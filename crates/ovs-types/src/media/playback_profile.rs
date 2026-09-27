//! What the server will play, said precisely enough to check.
//!
//! Two formats reach this server and no others (owner, 2026-09-23):
//!
//! | material | format |
//! |---|---|
//! | no alpha | HEVC, 8-bit or 10-bit |
//! | alpha | HAP Alpha |
//!
//! Everything else is transcoded by the client **before** it is uploaded, and
//! refused here if it arrives anyway. The server is the one that decides,
//! because it is the one that has to play it.
//!
//! # Why a contract and not just a codec name
//!
//! "HEVC" is not a specification. Two encoders both producing valid HEVC can
//! produce files that behave completely differently in a media server: one
//! with two-second GOPs and B-frames, where a slot change costs a seek back to
//! a keyframe and a decode forward; one with no colour tags, where the shader
//! guesses at a matrix and the picture is subtly wrong; one at a variable
//! frame rate, where frame *n* is not at time *n*.
//!
//! The client picks its encoder backend by what the machine has — NVENC where
//! there is an NVIDIA card, Media Foundation otherwise — and the operator
//! never sees that choice. So the backend must not be visible in the result
//! either. **Equivalent input produces functionally equivalent assets
//! whichever backend encoded it**, and this module is what that sentence means
//! in checkable terms.
//!
//! # The contract
//!
//! Every field below is a property the renderer relies on, and each one is
//! here because the renderer would be slower or wrong without it:
//!
//! - **Pixel format.** `yuv420p` or `yuv420p10le`, planar and eight or ten
//!   bits. The renderer uploads planes to the card as they are; anything else
//!   goes through a colour conversion for every frame of every layer.
//! - **Colour metadata present and explicit.** Primaries, transfer and matrix
//!   all tagged, and the range tagged. A file that leaves them out makes the
//!   renderer guess, and the two guesses differ by more than a grade.
//! - **Constant frame rate.** The playhead is arithmetic on a frame index; at
//!   a variable rate frame *n* is not where the arithmetic says it is.
//! - **Closed GOPs, no B-frames, and a keyframe at least every second.** A
//!   slot change is a seek, and a seek costs the distance back to the last
//!   keyframe. B-frames additionally make presentation order differ from
//!   decode order, which is a second place for a frame index to go wrong.
//! - **Square pixels.** The mapping from a composition rectangle to a wall is
//!   already the piece's job; a non-square sample aspect would be a second,
//!   invisible one.
//! - **Progressive.** There is no deinterlacer in this renderer and there will
//!   not be one.
//!
//! # What is deliberately not in here
//!
//! Quality. A backend that cannot reach the contract does not get to produce
//! something worse and call it done — it falls back to another format this
//! server plays, or says that it cannot (owner, 2026-09-23). Which means this
//! module never has to encode a judgement about how good a picture looks, and
//! that is the one judgement software should not be making on its own.

use serde::{Deserialize, Serialize};

/// How many bits a sample has.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BitDepth {
    /// Eight bits, `yuv420p`.
    Eight,
    /// Ten bits, `yuv420p10le`.
    Ten,
}

impl BitDepth {
    /// FFmpeg's name for the pixel format this depth requires.
    #[must_use]
    pub const fn pixel_format(self) -> &'static str {
        match self {
            Self::Eight => "yuv420p",
            Self::Ten => "yuv420p10le",
        }
    }
}

/// The most bits a second any clip may carry, as the owner set it.
///
/// **Sixty megabits, whatever the depth and whatever the raster** (owner,
/// 2026-09-25: "einfach cq18 max. 60MBit immer"). A ceiling and nothing else: a
/// clip that needs less is not padded up to it, and what decides how many bits one
/// actually gets is the encoder's quality target.
///
/// It was two numbers keyed on the depth — sixty at eight and eighty at ten —
/// which was my reading of the owner's "max 60 bei HD und 80 bei UHD" put on the
/// wrong axis: their two numbers were about the raster. Asked which of the two
/// readings held, the answer was neither, and one number is a rule an operator can
/// hold in their head.
pub const MOST_BITS_PER_SECOND: u64 = 60_000_000;

/// What the server plays, and therefore what an upload has to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlaybackProfile {
    /// HEVC without alpha, at one of the two depths.
    Hevc {
        /// Eight or ten bits.
        depth: BitDepth,
    },
    /// HAP Alpha: `RGBA_DXT5` blocks, which the card samples as they are.
    HapAlpha,
}

impl PlaybackProfile {
    /// The profile material with or without alpha belongs in.
    ///
    /// One line, and it is the whole of the ingest decision: alpha goes to
    /// HAP because HAP was made for exactly this and arrives as a texture
    /// already; everything else goes to HEVC because it is what this
    /// generation of hardware decodes fastest by a wide margin.
    #[must_use]
    pub const fn for_material(has_alpha: bool, depth: BitDepth) -> Self {
        if has_alpha {
            Self::HapAlpha
        } else {
            Self::Hevc { depth }
        }
    }

    /// Which of the three a file is claiming to be.
    ///
    /// Not whether it *is* one — that is [`faults`], and this is what to ask it
    /// about. A file is checked against the profile it is nearest to, so that
    /// the list an operator reads is the list of things to change rather than
    /// "this is not the other thing".
    ///
    /// # HAP, and which HAP
    ///
    /// Anything HAP is asked the HAP questions, and the three variants are not
    /// told apart. They cannot be, from a probe: FFmpeg decodes all of them to
    /// RGBA, so plain Hap and Hap Q look like Hap Alpha from outside, and
    /// telling them apart means parsing a frame's own type byte.
    ///
    /// It would also be refusing material this server is *best* at. The
    /// renderer reads all three natively — DXT1, DXT5 and YCoCg-DXT5 — and none
    /// of them costs a decoder at all. The owner's rule of 2026-09-23 routes
    /// the **client's** transcode (alpha to HAP Alpha, everything else to HEVC);
    /// a plain Hap file somebody uploads deliberately is not the thing that rule
    /// exists to keep off the machine.
    #[must_use]
    pub fn of(video: &ProbedVideo) -> Self {
        if video.codec == Self::HapAlpha.codec() {
            return Self::HapAlpha;
        }
        // Read off the pixel format rather than off the alpha flag: a file
        // without alpha is HEVC either way, and which *depth* of HEVC is the
        // question that decides both the pixel format clause and the ceiling.
        let depth = if video.pixel_format == BitDepth::Ten.pixel_format() {
            BitDepth::Ten
        } else {
            BitDepth::Eight
        };
        Self::Hevc { depth }
    }

    /// The codec name a probe reports for this profile.
    #[must_use]
    pub const fn codec(self) -> &'static str {
        match self {
            Self::Hevc { .. } => "hevc",
            Self::HapAlpha => "hap",
        }
    }
}

/// The longest a clip may go without a keyframe, as a fraction of a second.
///
/// One second. A slot change is a seek, and a seek costs the distance back to
/// the last keyframe — so this is the worst case an operator waits when they
/// change their mind, and it is chosen to be short enough not to be felt
/// rather than long enough to save bitrate.
pub const MOST_SECONDS_BETWEEN_KEYFRAMES: f64 = 1.0;

/// What a probe found in a file, in the terms this contract is written in.
///
/// Filled by whatever ran the probe — the server on an upload, a test on an
/// encoder's output — so that the check below is one piece of arithmetic with
/// no knowledge of FFmpeg in it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbedVideo {
    /// FFmpeg's codec name, lowercase: `hevc`, `hap`, `prores`.
    pub codec: String,
    /// FFmpeg's pixel format name, or empty when it said none.
    pub pixel_format: String,
    /// Whether the stream carries alpha.
    pub has_alpha: bool,
    /// Colour primaries as tagged, or empty when untagged.
    pub primaries: String,
    /// Transfer characteristics as tagged, or empty when untagged.
    pub transfer: String,
    /// Colour matrix as tagged, or empty when untagged.
    pub matrix: String,
    /// Whether the range is tagged, and as what.
    pub range: String,
    /// Frames a second, as the container's average.
    pub frame_rate: f64,
    /// Frames a second, as the container's *nominal* rate.
    ///
    /// Equal to [`frame_rate`](Self::frame_rate) for a constant-rate file, and
    /// that equality is how a variable-rate one is caught.
    pub nominal_frame_rate: f64,
    /// The longest run of frames between two keyframes, in frames.
    pub keyframe_interval: u32,
    /// Whether any frame is coded out of presentation order.
    pub has_b_frames: bool,
    /// The sample aspect ratio, as a fraction.
    pub sample_aspect: (u32, u32),
    /// Whether the stream is tagged as interlaced.
    pub interlaced: bool,
    /// Bits a second, as the container reports.
    pub bits_per_second: u64,
}

/// Why a file is not something this server will play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFault {
    /// What was wrong, in the operator's terms.
    pub what: String,
    /// What the contract asks for.
    pub wanted: String,
    /// What the file has.
    pub found: String,
}

impl ProfileFault {
    fn new(what: &str, wanted: impl Into<String>, found: impl Into<String>) -> Self {
        Self {
            what: what.to_owned(),
            wanted: wanted.into(),
            found: found.into(),
        }
    }
}

/// Checks a probed file against the profile it claims to be.
///
/// Returns every fault rather than the first, because an operator fixing a
/// file wants the whole list and a client choosing a backend wants to know
/// whether one fault or eight stand between it and a usable asset.
///
/// The list is empty for a file this server will play.
#[must_use]
pub fn faults(video: &ProbedVideo, profile: PlaybackProfile) -> Vec<ProfileFault> {
    let mut out = Vec::new();

    if video.codec != profile.codec() {
        // And nothing else. A file in the wrong codec fails every question
        // below it as well — a HAP file has no pixel format and no colour
        // matrix — and listing those would bury the one fault that matters
        // under seven that are only consequences of it.
        return vec![ProfileFault::new(
            "codec",
            profile.codec(),
            video.codec.clone(),
        )];
    }

    // Everything below this line is about HEVC. A HAP file is a texture
    // already: it has no pixel format to get wrong, no colour matrix to leave
    // out, every frame is a keyframe and there is nothing to reorder.
    let PlaybackProfile::Hevc { depth } = profile else {
        return out;
    };

    if video.pixel_format != depth.pixel_format() {
        out.push(ProfileFault::new(
            "pixel format",
            depth.pixel_format(),
            video.pixel_format.clone(),
        ));
    }
    if video.has_alpha {
        out.push(ProfileFault::new(
            "alpha",
            "none; material with alpha is HAP Alpha",
            "present",
        ));
    }

    // Tagged, not merely plausible. An untagged file is one the renderer has
    // to guess about, and two renderers guessing differently is a picture that
    // changes when the machine does.
    for (what, value) in [
        ("colour primaries", &video.primaries),
        ("transfer characteristics", &video.transfer),
        ("colour matrix", &video.matrix),
        ("colour range", &video.range),
    ] {
        if value.is_empty() || value == "unknown" {
            out.push(ProfileFault::new(what, "tagged", "untagged"));
        }
    }

    if !rates_agree(video.frame_rate, video.nominal_frame_rate) {
        out.push(ProfileFault::new(
            "frame rate",
            "constant",
            format!(
                "variable: {:.3} average against {:.3} nominal",
                video.frame_rate, video.nominal_frame_rate
            ),
        ));
    }

    let most = keyframes_at_most(video.nominal_frame_rate);
    if video.keyframe_interval > most {
        out.push(ProfileFault::new(
            "keyframe interval",
            format!("at most {most} frames, which is one second"),
            format!("{} frames", video.keyframe_interval),
        ));
    }

    // **B-frames are not a fault.** They were, until the clause was measured
    // on the owner's own material on 2026-09-23 and bought nothing at all.
    //
    // Three encodes of one 1080p50 file on the reference hardware, through
    // `hevc_cuvid`: a five-second GOP with B-frames seeks in 297 ms, a
    // one-second GOP without them in 230 ms, and a one-second GOP *with*
    // them in 228 ms. The whole improvement is the keyframe interval below;
    // the reordering is worth nothing either way. Sequential playback is the
    // same story — seven parallel streams at fifty a second, nothing held,
    // for all three.
    //
    // Keeping it would have cost something real: a file that already has
    // one-second keyframes and uses B-frames would be re-encoded for no
    // gain, and a re-encode is a generation of quality thrown away. A clause
    // that refuses material has to earn it.
    //
    // `ProbedVideo::has_b_frames` stays: it is worth reporting, and it is
    // the thing to look at first if a decoder ever does start caring.

    if video.sample_aspect != (1, 1) {
        out.push(ProfileFault::new(
            "sample aspect",
            "1:1",
            format!("{}:{}", video.sample_aspect.0, video.sample_aspect.1),
        ));
    }

    if video.interlaced {
        out.push(ProfileFault::new("scan", "progressive", "interlaced"));
    }

    let ceiling = MOST_BITS_PER_SECOND;
    if video.bits_per_second > ceiling {
        out.push(ProfileFault::new(
            "bitrate",
            format!("at most {} Mbit/s", ceiling / 1_000_000),
            format!("{:.1} Mbit/s", video.bits_per_second as f64 / 1_000_000.0),
        ));
    }

    out
}

/// The most frames a clip at this rate may go without a keyframe.
///
/// At least one, because a rate a container reported as zero must not turn
/// into "no keyframes required".
#[must_use]
pub fn keyframes_at_most(frame_rate: f64) -> u32 {
    if !frame_rate.is_finite() || frame_rate <= 0.0 {
        return 1;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let frames = (frame_rate * MOST_SECONDS_BETWEEN_KEYFRAMES).round() as u32;
    frames.max(1)
}

/// Whether two reported rates are the same rate.
///
/// Within a thousandth, because a container stores a rate as a fraction and
/// two ways of asking it can disagree in the last place — 60000/1001 is not
/// exactly 59.94 and neither reading is wrong.
fn rates_agree(one: f64, other: f64) -> bool {
    if !one.is_finite() || !other.is_finite() {
        return false;
    }
    (one - other).abs() <= 0.001 * one.abs().max(other.abs()).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file that meets the contract, which the tests below spoil one field
    /// of at a time.
    /// The owner's `HEVC.mp4` **after** it has been transcoded to the profile.
    ///
    /// Every number here was read off a real file with `ovs-media`'s own probe
    /// on 2026-09-23: 1920x1080 at 50, `hevc_nvenc` at a one-second GOP, tagged
    /// BT.709 throughout, 48.6 Mbit — except the bitrate, which is the *source*
    /// file's 59.5 Mbit, kept because it is the interesting number: just inside
    /// the sixty the owner set, so a ceiling that moved would show here.
    fn good() -> ProbedVideo {
        ProbedVideo {
            codec: "hevc".to_owned(),
            pixel_format: "yuv420p".to_owned(),
            has_alpha: false,
            primaries: "bt709".to_owned(),
            transfer: "bt709".to_owned(),
            matrix: "bt709".to_owned(),
            range: "tv".to_owned(),
            frame_rate: 50.0,
            nominal_frame_rate: 50.0,
            keyframe_interval: 50,
            has_b_frames: false,
            sample_aspect: (1, 1),
            interlaced: false,
            bits_per_second: 59_467_186,
        }
    }

    const EIGHT: PlaybackProfile = PlaybackProfile::Hevc {
        depth: BitDepth::Eight,
    };

    fn what(faults: &[ProfileFault]) -> Vec<&str> {
        faults.iter().map(|fault| fault.what.as_str()).collect()
    }

    #[test]
    fn the_owners_own_test_file_does_not_meet_the_contract_and_says_why() {
        // The point of the whole policy, on a real file. `HEVC.mp4` is HEVC, it
        // is 8-bit, it is tagged, it is progressive, it is constant-rate and it
        // is 59.5 Mbit — inside the sixty. And it is still not an OVS asset,
        // because its keyframes are 253 frames apart, which at fifty a second is
        // five seconds: every slot change on it pays a five-second GOP.
        //
        // "HEVC is allowed" is not the same as "any HEVC is allowed", and this
        // is the file that makes the difference concrete. Measured, not assumed:
        // the interval comes from `ovs_media::profile_of` reading the packets.
        let as_delivered = ProbedVideo {
            keyframe_interval: 253,
            has_b_frames: true,
            ..good()
        };
        assert_eq!(
            what(&faults(&as_delivered, EIGHT)),
            vec!["keyframe interval"],
            "the keyframes, and nothing else — the B-frames are fine"
        );
    }

    #[test]
    fn the_owners_file_transcoded_to_the_profile_is_accepted() {
        // The other half, and the one that proves the policy is a door rather
        // than a wall: the same file through `hevc_nvenc` at a one-second GOP
        // came back at 48.6 Mbit and passes every clause. These are that file's
        // own numbers.
        let transcoded = ProbedVideo {
            bits_per_second: 48_600_000,
            ..good()
        };
        assert_eq!(faults(&transcoded, EIGHT), Vec::new());
        assert_eq!(faults(&good(), EIGHT), Vec::new());
    }

    #[test]
    fn a_file_is_asked_about_the_profile_it_is_nearest_to() {
        // So that the list an operator reads is the list of things to change.
        // A ten-bit file asked the eight-bit questions would be told its pixel
        // format is wrong, which is true of the wrong profile and useless.
        assert_eq!(PlaybackProfile::of(&good()), EIGHT);

        let ten = ProbedVideo {
            pixel_format: "yuv420p10le".to_owned(),
            ..good()
        };
        assert_eq!(
            PlaybackProfile::of(&ten),
            PlaybackProfile::Hevc {
                depth: BitDepth::Ten
            }
        );

        // Every HAP variant, including the ones with no alpha in them: they
        // cannot be told apart from a probe, and all three are native.
        for has_alpha in [true, false] {
            let hap = ProbedVideo {
                codec: "hap".to_owned(),
                pixel_format: "rgba".to_owned(),
                has_alpha,
                ..good()
            };
            assert_eq!(PlaybackProfile::of(&hap), PlaybackProfile::HapAlpha);
        }
    }

    #[test]
    fn alpha_material_is_hap_and_is_asked_nothing_else() {
        // A HAP file has no pixel format to get wrong, no matrix to leave out,
        // every frame is a keyframe and nothing is reordered. Asking it the
        // HEVC questions would be inventing faults.
        let hap = ProbedVideo {
            codec: "hap".to_owned(),
            pixel_format: String::new(),
            has_alpha: true,
            primaries: String::new(),
            transfer: String::new(),
            matrix: String::new(),
            range: String::new(),
            keyframe_interval: 1,
            ..good()
        };
        assert_eq!(faults(&hap, PlaybackProfile::HapAlpha), Vec::new());

        // And the same file offered as HEVC is refused for being HAP, once.
        assert_eq!(what(&faults(&hap, EIGHT)), vec!["codec"]);
    }

    #[test]
    fn material_goes_to_the_profile_its_alpha_decides() {
        assert_eq!(
            PlaybackProfile::for_material(true, BitDepth::Eight),
            PlaybackProfile::HapAlpha
        );
        assert_eq!(
            PlaybackProfile::for_material(false, BitDepth::Ten),
            PlaybackProfile::Hevc {
                depth: BitDepth::Ten
            }
        );
    }

    #[test]
    fn an_untagged_colour_is_a_fault_for_every_tag_that_is_missing() {
        // The whole list, because an operator fixing a file wants to know how
        // many things are wrong rather than discovering them one re-encode at
        // a time.
        let untagged = ProbedVideo {
            primaries: String::new(),
            transfer: "unknown".to_owned(),
            matrix: String::new(),
            range: String::new(),
            ..good()
        };
        assert_eq!(
            what(&faults(&untagged, EIGHT)),
            vec![
                "colour primaries",
                "transfer characteristics",
                "colour matrix",
                "colour range"
            ]
        );
    }

    #[test]
    fn a_variable_frame_rate_is_refused_because_the_playhead_is_arithmetic() {
        let variable = ProbedVideo {
            frame_rate: 47.3,
            nominal_frame_rate: 50.0,
            ..good()
        };
        assert_eq!(what(&faults(&variable, EIGHT)), vec!["frame rate"]);

        // And 60000/1001 against 59.94 is the same rate twice, not a fault.
        let ntsc = ProbedVideo {
            frame_rate: 60000.0 / 1001.0,
            nominal_frame_rate: 59.94,
            keyframe_interval: 60,
            ..good()
        };
        assert_eq!(faults(&ntsc, EIGHT), Vec::new());
    }

    #[test]
    fn a_long_gop_is_refused_because_a_slot_change_is_a_seek() {
        // Two seconds at 50 fps. What an operator would feel is the pad they
        // pressed taking twice as long to arrive as the one before it.
        let long = ProbedVideo {
            keyframe_interval: 100,
            ..good()
        };
        assert_eq!(what(&faults(&long, EIGHT)), vec!["keyframe interval"]);

        // Exactly a second is the contract, not one frame under it.
        assert_eq!(keyframes_at_most(50.0), 50);
        assert_eq!(keyframes_at_most(60000.0 / 1001.0), 60);
        assert_eq!(keyframes_at_most(0.0), 1, "never zero");
    }

    #[test]
    fn b_frames_are_accepted_because_the_measurement_said_to_accept_them() {
        // They were refused until 2026-09-23, when three encodes of the owner's
        // own 1080p50 file were measured through `hevc_cuvid`: a five-second GOP
        // with B-frames seeks in 297 ms, a one-second GOP without them in 230 ms,
        // and a one-second GOP *with* them in 228 ms. The keyframe interval is
        // the whole of the difference.
        //
        // A clause that refuses material and buys nothing costs a re-encode, and
        // a re-encode is a generation of quality thrown away.
        let reordered = ProbedVideo {
            has_b_frames: true,
            ..good()
        };
        assert!(faults(&reordered, EIGHT).is_empty());
    }

    #[test]
    fn ten_bit_has_its_own_pixel_format_and_shares_the_one_ceiling() {
        // **One ceiling** (owner, 2026-09-25: "einfach cq18 max. 60MBit immer").
        //
        // This asserted that a 79 Mbit/s ten-bit file conforms, which it did while
        // the ceiling was eighty at ten bits. What ten-bit has of its own is the
        // pixel format; what it shares with everything else is the sixty megabits.
        let ten = PlaybackProfile::Hevc {
            depth: BitDepth::Ten,
        };
        // An eight-bit file offered as ten-bit is wrong about one thing.
        assert_eq!(what(&faults(&good(), ten)), vec!["pixel format"]);

        let proper = ProbedVideo {
            pixel_format: "yuv420p10le".to_owned(),
            ..good()
        };
        assert_eq!(faults(&proper, ten), Vec::new());

        // Over the ceiling is over the ceiling, and being ten-bit buys nothing.
        let fat = ProbedVideo {
            bits_per_second: MOST_BITS_PER_SECOND + 1,
            ..proper.clone()
        };
        assert_eq!(what(&faults(&fat, ten)), vec!["bitrate"]);
        assert_eq!(
            what(&faults(&fat, EIGHT)),
            vec!["pixel format", "bitrate"],
            "and the eight-bit reading of it is wrong about the format as well"
        );
    }

    #[test]
    fn a_file_that_is_wrong_in_several_ways_says_all_of_them() {
        // What a client gets back when it points a backend at the wrong
        // settings entirely: every fault, so one fix can address all of them.
        //
        // The codec is right here on purpose. A wrong codec answers on its own
        // and stops, because everything else would only be a consequence of
        // it; this is the case where the format is right and the settings are
        // not, which is the one a client can actually act on.
        let bad = ProbedVideo {
            pixel_format: "yuv422p".to_owned(),
            matrix: String::new(),
            keyframe_interval: 250,
            sample_aspect: (16, 15),
            interlaced: true,
            bits_per_second: 120_000_000,
            ..good()
        };
        assert_eq!(
            what(&faults(&bad, EIGHT)),
            vec![
                "pixel format",
                "colour matrix",
                "keyframe interval",
                "sample aspect",
                "scan",
                "bitrate"
            ]
        );
    }
}
