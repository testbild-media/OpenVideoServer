//! Encoding a surface as H.264, for a client to watch while the show runs.
//!
//! A picture a second is enough to answer "is it running". Cueing a show is not
//! that: an operator watching a clip land on a beat needs the preview to move
//! like the wall does, which means sixty frames a second over a network, which
//! means a video codec.
//!
//! # Why H.264, and why the card
//!
//! H.264 is the one codec every machine of the last decade decodes in hardware,
//! which is what makes a preview cheap for the person *watching* it as well as
//! for the server. Newer codecs are smaller on the wire and narrower in what can
//! play them; that trade is wrong for a preview whose whole job is to be
//! available.
//!
//! Encoding it costs the renderer almost nothing because it does not happen on
//! the part of the card that draws. Measured on the test machine with a 1080p
//! ProRes clip playing to a 1080p output: the encode engine sits at **0%** and
//! the decode engine at **0%**, because ProRes is decoded on the processor
//! and nothing else was asking.
//!
//! # When the sessions run out
//!
//! The card has a limit and `ENCODERS` is the answer to reaching it: hardware
//! first, then `libx264`. The owner's decision is that the ninth preview falls
//! back rather than being refused (owner decision Q13).
//!
//! **Measured on the test machine, 2026-09-22.** Twelve preview encoders opened
//! in one process, at 320x180:
//!
//! ```text
//! h264_nvenc x7, then libx264 x5
//! ```
//!
//! Nothing was refused, the hardware sessions were used first, and the fall to
//! software happened where the card ran out. Worth noting against what this
//! comment used to say: **seven**, not the eight that was assumed from an
//! earlier run. One more reason the count is not a number to design against —
//! it is a driver's, it varies with what else on the machine is encoding, and
//! the only safe design is the one that does not need to know it.
//!
//! # One stream, many watchers
//!
//! A surface is encoded once, whoever is looking. The picture is the same for
//! everybody — a client, a second client, the System screen — so encoding it
//! per viewer would be paying twice for one answer. What is *per viewer* is the
//! sending, because a socket is a socket; the encode is not.
//!
//! # Unix only, for now
//!
//! Same reason as the decoder: the FFmpeg libraries are a Unix build dependency
//! and the machine this runs on is Debian.

// One `unsafe impl` lives here, on the colour converter, and is explained where
// it is written — the same one the decoder carries, for the same reason.
#![allow(unsafe_code)]

use ovs_types::geometry::{FrameRate, SizePx};

use ffmpeg_next as ffmpeg;

use crate::media::DecodedImage;

/// The encoders to try, in the order they are worth having.
///
/// Hardware first, because it costs the show nothing; `libx264` last, because a
/// preview that costs a core is a preview worth switching off, and switching it
/// off is worse than having it.
///
/// VAAPI and Quick Sync are deliberately absent until there is a machine to try
/// them on. An untested path that claims to work is worse than one that says it
/// is not there.
///
/// `libopenh264` before `libx264` for two reasons, and the licence is the
/// second one. Measured on 2026-09-22 at the same bitrate, it costs less than
/// half the processor and gives up 0.14 dB — so it is the better software
/// encoder for a preview whichever build this is. It is also BSD where
/// `libx264` is GPL, and the server *links* libavcodec rather than running it,
/// so this list is also the server's licence (owner decision Q18).
///
/// All three are tried in order and a name this build does not have is skipped,
/// so one binary suits an FFmpeg with openh264 in it, one with x264, and one
/// with both.
pub const ENCODERS: [&str; 3] = ["h264_nvenc", "libopenh264", "libx264"];

/// Whether an encoder runs on the card.
///
/// One place, because two things depend on it and they must not disagree: the
/// frame-rate cap is only paid by software, and a watcher is told which it got.
/// A client deciding this for itself by matching on the name would have to be
/// taught about every encoder added to [`ENCODERS`] afterwards.
#[must_use]
pub fn is_hardware(name: &str) -> bool {
    name == "h264_nvenc"
}

/// The fastest a software-encoded preview is allowed to run, in frames a second.
///
/// # Why a preview in software is capped and one in hardware is not
///
/// Measured with `ovs-preview-cost` on the test machine, one 720p surface at
/// six megabits:
///
/// | encoder | cores a surface |
/// |---|---|
/// | `h264_nvenc` | 0.69 |
/// | `libx264` | **2.52** |
///
/// Hardware costs what it costs to hand frames over — the encoding happens on a
/// part of the card that draws nothing. Software costs two and a half cores,
/// which on the six-core reference server is most of half the machine for one
/// person watching one wall.
///
/// The owner's answer to Q13 is that a preview may have its rate
/// reduced rather than threaten the show, and that it must not simply be
/// refused. Halving the rate halves the cost almost exactly, because an encoder
/// is paid per frame; fifteen is a quarter of the price of sixty and is still a
/// picture somebody can check a mapping against, read a caption on and see a
/// cue land in.
///
/// It is a cap rather than a target: a show running at 25 gets 15, and one
/// already below it is left alone.
const SOFTWARE_PREVIEW_FPS: u32 = 15;

/// How far apart keyframes are, in seconds.
///
/// A watcher who opens a preview sees nothing until the next one, so this is
/// how long "it is loading" lasts. Two seconds is short enough not to be
/// noticed and long enough that keyframes are not most of the bitrate.
const KEYFRAME_SECONDS: u32 = 2;

/// Why a stream could not be encoded.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// No encoder this build can use is installed.
    #[error("none of {} can be opened on this machine", ENCODERS.join(", "))]
    NoEncoder,

    /// The encoder was opened but would not take a frame.
    #[error("the encoder refused a frame: {0}")]
    Refused(String),

    /// The picture could not be turned into what the encoder wants.
    #[error("cannot convert a frame for the encoder: {0}")]
    Unconvertible(String),
}

/// A colour converter that can be moved to the thread that encodes.
///
/// The same wrapper, and the same reasoning, as the decoder's: an `SwsContext`
/// is owned exclusively here and never shared, and the binding does not say so.
struct Scaler(ffmpeg::software::scaling::Context);

// SAFETY: exclusive ownership and no aliasing. The pointer inside is made in
// `open`, reachable only through `&mut PreviewEncoder`, and freed by its own
// `Drop`.
unsafe impl Send for Scaler {}

/// One surface, being encoded.
pub struct PreviewEncoder {
    encoder: ffmpeg::encoder::video::Encoder,
    scaler: Scaler,
    /// The picture in the encoder's own colour space, reused every frame.
    staged: ffmpeg::frame::Video,
    size: SizePx,
    rate: FrameRate,
    /// Which encoder was opened, for the log line and for telemetry.
    name: &'static str,
    /// Encode one frame in this many. Always 1 for a hardware encoder.
    ///
    /// See [`SOFTWARE_PREVIEW_FPS`]. A skipped frame costs a comparison and
    /// nothing else: the colour conversion, the copy and the encode all happen
    /// after this.
    stride: i64,
    /// Frames handed over so far, which is the presentation clock.
    index: i64,
}

impl std::fmt::Debug for PreviewEncoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreviewEncoder")
            .field("encoder", &self.name)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl PreviewEncoder {
    /// Opens the best encoder this machine has for one surface.
    ///
    /// `bitrate` is in bits per second: about 1.5 Mbit for 360p, 6 for 720p and
    /// 12 for 1080p at sixty frames a second, measured on the test machine with
    /// a moving picture.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::NoEncoder`] when nothing this build knows how to
    /// drive is installed.
    pub fn open(size: SizePx, rate: FrameRate, bitrate: u32) -> Result<Self, StreamError> {
        // Even sides, because every H.264 profile worth using is 4:2:0 and
        // 4:2:0 has half-resolution chroma. A wall with an odd width is a
        // legitimate thing; an encoder that refuses it is not.
        let size = SizePx::new(size.width.max(2) & !1, size.height.max(2) & !1);

        for name in ENCODERS {
            match Self::open_with(name, size, rate, bitrate) {
                Ok(opened) => {
                    tracing::info!(
                        encoder = name,
                        width = size.width,
                        height = size.height,
                        bitrate_kbps = bitrate / 1000,
                        "preview stream encoder opened"
                    );
                    return Ok(opened);
                }
                Err(error) => {
                    // Not a fault yet: the next one in the list may be there.
                    // The last failure is reported by the caller as no encoder
                    // at all.
                    tracing::debug!(encoder = name, "cannot open this encoder: {error}");
                }
            }
        }

        Err(StreamError::NoEncoder)
    }

    /// Opens one named encoder, by `FFmpeg`'s own name for it.
    ///
    /// # Not the way a preview is opened
    ///
    /// A preview opens with [`open`](Self::open), which walks [`ENCODERS`] and
    /// takes the best one available — that is the policy, and a caller that
    /// named an encoder would be a caller deciding it somewhere else.
    ///
    /// This is public for the one thing that has to name one: measuring what
    /// each costs. `ovs-preview-cost` compares hardware against software, and
    /// it cannot do that through a function whose whole job is to hide the
    /// difference.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Refused`] when this build has no such encoder, or
    /// when it has one and it will not open — a hardware encoder whose sessions
    /// are all taken looks the same as one that is not installed, which is why
    /// [`open`](Self::open) treats both as "try the next".
    ///
    /// [`open`]: Self::open
    pub fn open_with(
        name: &'static str,
        size: SizePx,
        rate: FrameRate,
        bitrate: u32,
    ) -> Result<Self, StreamError> {
        let codec =
            ffmpeg::encoder::find_by_name(name).ok_or_else(|| StreamError::Refused(name.into()))?;

        let context = ffmpeg::codec::context::Context::new_with_codec(codec);
        let mut encoder = context
            .encoder()
            .video()
            .map_err(|error| StreamError::Refused(error.to_string()))?;

        encoder.set_width(size.width);
        encoder.set_height(size.height);
        encoder.set_format(ffmpeg::format::Pixel::YUV420P);
        encoder.set_bit_rate(bitrate as usize);
        // Frames arrive at the show's rate and are stamped in frames, so the
        // time base is one tick per frame.
        encoder.set_time_base(ffmpeg::Rational::new(
            rate.denominator.max(1) as i32,
            rate.numerator.max(1) as i32,
        ));
        encoder.set_frame_rate(Some(ffmpeg::Rational::new(
            rate.numerator.max(1) as i32,
            rate.denominator.max(1) as i32,
        )));
        encoder.set_gop(KEYFRAME_SECONDS * rate.as_f64().round().max(1.0) as u32);
        // No reordering. A B-frame is a frame the decoder must wait for, and
        // waiting is the one thing this is not allowed to do.
        encoder.set_max_b_frames(0);

        // What a bitrate is worth depends entirely on the encoder being told to
        // keep to it. Measured before this was set: a stream asked for six
        // megabits produced eleven and a half, because an encoder given a
        // bitrate and no rate control picks its own — usually a constant
        // quality, which is a bitrate that depends on the picture. On a venue's
        // network that is the difference between a preview and a problem.
        //
        // A buffer of half a second, because a longer one lets a burst through
        // and a shorter one starves the picture on a cut.
        let bitrate = bitrate.to_string();
        let buffer = (bitrate.parse::<u32>().unwrap_or(6_000_000) / 2).to_string();

        let mut options = ffmpeg::Dictionary::new();
        match name {
            "h264_nvenc" => {
                // The fastest preset, tuned for ultra-low latency, and no
                // lookahead: every frame is finished before the next arrives.
                options.set("preset", "p1");
                options.set("tune", "ull");
                options.set("delay", "0");
                options.set("zerolatency", "1");
                options.set("rc", "cbr");
                options.set("maxrate", &bitrate);
                options.set("bufsize", &buffer);
            }
            _ => {
                options.set("preset", "ultrafast");
                options.set("tune", "zerolatency");
                options.set("maxrate", &bitrate);
                options.set("bufsize", &buffer);
            }
        }

        let encoder = encoder
            .open_with(options)
            .map_err(|error| StreamError::Refused(error.to_string()))?;

        // RGBA in, 4:2:0 out. On the processor for now: at a preview's size it
        // is a fraction of a millisecond, and moving it onto the card means
        // keeping the frame there in the first place, which is a larger change
        // than this one.
        let scaler = ffmpeg::software::scaling::Context::get(
            ffmpeg::format::Pixel::RGBA,
            size.width,
            size.height,
            ffmpeg::format::Pixel::YUV420P,
            size.width,
            size.height,
            ffmpeg::software::scaling::Flags::BILINEAR,
        )
        .map_err(|error| StreamError::Unconvertible(error.to_string()))?;

        Ok(Self {
            encoder,
            scaler: Scaler(scaler),
            staged: ffmpeg::frame::Video::new(
                ffmpeg::format::Pixel::YUV420P,
                size.width,
                size.height,
            ),
            size,
            rate,
            name,
            stride: Self::stride_for(name, rate),
            index: 0,
        })
    }

    /// Which encoder this is using.
    #[must_use]
    pub const fn encoder(&self) -> &'static str {
        self.name
    }

    /// How many frames a second this preview actually encodes.
    ///
    /// The show's rate for a hardware encoder, and at most
    /// [`SOFTWARE_PREVIEW_FPS`] for a software one. What a client should be
    /// told, so that a preview running at a quarter speed reads as a decision
    /// rather than as a fault.
    #[must_use]
    pub fn encoded_fps(&self) -> f64 {
        self.rate.as_f64() / self.stride.max(1) as f64
    }

    /// The size frames must arrive at.
    #[must_use]
    pub const fn size(&self) -> SizePx {
        self.size
    }

    /// Encodes one picture, appending what comes out.
    ///
    /// The bytes are Annex-B: start codes and NAL units, with the parameter
    /// sets in front of every keyframe. That is what a browser's `VideoDecoder`
    /// takes without a container and without being told anything first, which
    /// is the whole point of sending it this way.
    ///
    /// Returns whether what was appended contains a keyframe — the frame a
    /// watcher who has just opened the preview can start from.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError`] when the picture is the wrong size for this
    /// encoder, cannot be converted, or the encoder refuses it.
    pub fn encode(
        &mut self,
        picture: &DecodedImage,
        into: &mut Vec<u8>,
    ) -> Result<bool, StreamError> {
        if picture.size() != self.size {
            return Err(StreamError::Unconvertible(format!(
                "this encoder wants {}x{} and was given {}x{}",
                self.size.width,
                self.size.height,
                picture.size().width,
                picture.size().height
            )));
        }

        // Before anything is converted or copied: a skipped frame has to cost a
        // comparison and nothing else, which is the whole point of skipping it.
        //
        // Counted on frames offered rather than on frames taken, so the rate is
        // steady — every second frame, not every second one that happened to
        // arrive while the encoder was free.
        let offered = self.index;
        self.index += 1;
        if self.stride > 1 && offered % self.stride != 0 {
            return Ok(false);
        }

        let mut source = ffmpeg::frame::Video::new(
            ffmpeg::format::Pixel::RGBA,
            self.size.width,
            self.size.height,
        );
        copy_rows(picture.pixels(), &mut source, self.size);

        self.scaler
            .0
            .run(&source, &mut self.staged)
            .map_err(|error| StreamError::Unconvertible(error.to_string()))?;
        // The stamp is the frame's own position in the show, not a count of
        // what this encoder took. A stream whose stamps were consecutive would
        // play at a quarter speed rather than at a quarter of the frames.
        self.staged.set_pts(Some(offered));

        self.encoder
            .send_frame(&self.staged)
            .map_err(|error| StreamError::Refused(error.to_string()))?;

        self.drain(into)
    }

    /// How many frames a second a preview encodes, given which encoder it opened.
    ///
    /// One in one for hardware. For software, the smallest whole number of frames
    /// to skip that brings the rate to [`SOFTWARE_PREVIEW_FPS`] or below — a whole
    /// number because a preview whose frames are unevenly spaced looks worse than
    /// one that is simply slower.
    fn stride_for(name: &str, rate: FrameRate) -> i64 {
        if is_hardware(name) {
            return 1;
        }
        let fps = rate.as_f64();
        if !fps.is_finite() || fps <= f64::from(SOFTWARE_PREVIEW_FPS) {
            // A show already at or below the cap is left alone: there is nothing to
            // save and a preview slower than the wall is harder to read.
            return 1;
        }
        (fps / f64::from(SOFTWARE_PREVIEW_FPS)).ceil() as i64
    }

    /// Takes whatever the encoder has finished.
    fn drain(&mut self, into: &mut Vec<u8>) -> Result<bool, StreamError> {
        let mut keyframe = false;
        let mut packet = ffmpeg::Packet::empty();

        // An encoder with nothing ready says so by refusing, which is not an
        // error: at zero latency it usually has exactly one frame for us, and
        // at startup it may have none.
        while self.encoder.receive_packet(&mut packet).is_ok() {
            if let Some(data) = packet.data() {
                into.extend_from_slice(data);
            }
            keyframe |= packet.is_key();
        }

        Ok(keyframe)
    }

    /// The rate this was opened for.
    #[must_use]
    pub const fn rate(&self) -> FrameRate {
        self.rate
    }
}

/// Copies tightly packed pixels into a frame that may be padded.
///
/// FFmpeg pads every row to suit itself, and a picture copied in one block
/// lands diagonally. The same rule as on the way out of the decoder.
fn copy_rows(pixels: &[u8], into: &mut ffmpeg::frame::Video, size: SizePx) {
    let row_bytes = size.width as usize * 4;
    let stride = into.stride(0);
    let data = into.data_mut(0);

    for row in 0..size.height as usize {
        let from = row * row_bytes;
        let to = row * stride;
        let (Some(source), Some(destination)) = (
            pixels.get(from..from + row_bytes),
            data.get_mut(to..to + row_bytes),
        ) else {
            return;
        };
        destination.copy_from_slice(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture that changes, so the encoder has something to compress.
    pub(super) fn picture(size: SizePx, frame: u32) -> DecodedImage {
        let mut pixels = vec![0_u8; size.width as usize * size.height as usize * 4];
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = (index % size.width as usize) as u32;
            let y = (index / size.width as usize) as u32;
            pixel[0] = ((x + frame * 7) % 256) as u8;
            pixel[1] = ((y + frame * 3) % 256) as u8;
            pixel[2] = ((x ^ y) % 256) as u8;
            pixel[3] = 0xFF;
        }
        DecodedImage::new(pixels, size).expect("a picture")
    }

    #[test]
    fn a_software_preview_is_capped_and_a_hardware_one_is_not() {
        // Measured with `ovs-preview-cost`: a 720p surface costs 0.69 cores on
        // `h264_nvenc` and 2.52 on `libx264`. On the six-core reference server
        // that is most of half the machine for one person watching one wall,
        // which is what the owner's "may have its rate reduced" is about
        // (owner decision Q13).
        assert_eq!(
            PreviewEncoder::stride_for("h264_nvenc", FrameRate::FPS_60),
            1,
            "hardware is not capped"
        );

        // Sixty into fifteen is one in four; fifty into fifteen is one in four
        // as well, because the stride is whole.
        assert_eq!(PreviewEncoder::stride_for("libx264", FrameRate::FPS_60), 4);
        assert_eq!(
            PreviewEncoder::stride_for("libx264", FrameRate::new(50, 1)),
            4
        );
        assert_eq!(
            PreviewEncoder::stride_for("libx264", FrameRate::new(30, 1)),
            2
        );

        // A show already at or below the cap is left alone: there is nothing to
        // save and a preview slower than the wall is harder to read.
        assert_eq!(
            PreviewEncoder::stride_for("libx264", FrameRate::new(15, 1)),
            1
        );
        assert_eq!(
            PreviewEncoder::stride_for("libx264", FrameRate::new(10, 1)),
            1
        );
    }

    #[test]
    fn a_capped_preview_encodes_fewer_frames_and_says_so() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let size = SizePx::new(320, 180);
        let Ok(mut encoder) =
            PreviewEncoder::open_with("libx264", size, FrameRate::FPS_60, 1_000_000)
        else {
            println!("skipped: this build has no libx264");
            return;
        };

        assert!(
            (encoder.encoded_fps() - 15.0).abs() < 0.01,
            "a sixty hertz show previews at {} in software",
            encoder.encoded_fps()
        );

        // Sixty frames offered at one in four is fifteen taken. Counted by how
        // many produced anything, because a frame that was skipped produces no
        // bytes and a frame that was taken very nearly always does.
        let mut stream = Vec::new();
        let mut produced = 0;
        for frame in 0..60 {
            let before = stream.len();
            encoder
                .encode(&picture(size, frame), &mut stream)
                .expect("encodes");
            if stream.len() > before {
                produced += 1;
            }
        }

        assert!(
            (10..=16).contains(&produced),
            "sixty frames at one in four produced {produced}"
        );
    }

    #[test]
    fn the_ninth_preview_falls_back_to_software_rather_than_being_refused() {
        // The owner's answer to Q13, as an acceptance check.
        // The fallback has been in `ENCODERS` since previews were built and
        // nobody had ever reached it: this card offers eight NVENC sessions,
        // and nothing in the tree had opened nine.
        //
        // Serialised with the GPU lock. It is a Vulkan lock and this is not
        // Vulkan, but it is the same card and the same scarce resource, and a
        // test that took every encoder session while another test wanted one
        // would fail whichever of them lost.
        let _gpu = crate::vulkan::one_gpu_at_a_time();

        let size = SizePx::new(320, 180);
        let rate = FrameRate::FPS_60;

        // Twelve, which is more than this card's eight and more surfaces than
        // any rig this product targets: four walls, a composition, and room.
        let mut open = Vec::new();
        for index in 0..12 {
            match PreviewEncoder::open(size, rate, 2_000_000) {
                Ok(encoder) => open.push(encoder),
                Err(error) => panic!("preview {} was refused: {error}", index + 1),
            }
        }

        let names: Vec<&str> = open.iter().map(PreviewEncoder::encoder).collect();
        println!("twelve previews opened as {names:?}");

        if names[0] != "h264_nvenc" {
            // No hardware encoder on this machine, so there is no fallback to
            // reach. What is still proved is the part that matters most: the
            // ninth preview opened.
            println!("skipped the fallback half: this build has no h264_nvenc");
            return;
        }

        // Something other than the card, whichever software encoder this
        // build has. Naming one here made this fail against an FFmpeg built
        // without `libx264` in it — which is the whole point of the LGPL
        // server build, and not a thing the fallback cares about.
        assert!(
            names.iter().any(|name| *name != "h264_nvenc"),
            "every one of twelve took a hardware session, which this card does \
             not have: {names:?}"
        );
        assert!(
            names
                .iter()
                .take_while(|name| **name == "h264_nvenc")
                .count()
                >= 4,
            "the hardware sessions were not used first: {names:?}"
        );
    }

    #[test]
    fn a_surface_is_encoded_as_something_a_browser_can_decode() {
        let size = SizePx::new(320, 180);
        let mut encoder =
            PreviewEncoder::open(size, FrameRate::FPS_60, 1_000_000).expect("an encoder");

        let mut stream = Vec::new();
        let mut keyframes = 0;
        for frame in 0..30 {
            if encoder
                .encode(&picture(size, frame), &mut stream)
                .expect("encodes")
            {
                keyframes += 1;
            }
        }

        assert!(
            keyframes >= 1,
            "a watcher has to be able to start somewhere"
        );
        assert!(stream.len() > 100, "and there has to be a picture in it");
        // Annex-B: a start code, then a NAL. Sent without a container and
        // without a description, which is what `VideoDecoder` reads directly.
        assert_eq!(
            &stream[..4],
            &[0, 0, 0, 1],
            "the stream starts with a start code"
        );
    }

    #[test]
    fn a_picture_of_the_wrong_size_is_refused_rather_than_stretched() {
        // A surface that changed mode gets a new encoder, not a squashed
        // picture: the caption on a test pattern would be a lie and so would
        // the preview.
        let mut encoder = PreviewEncoder::open(SizePx::new(320, 180), FrameRate::FPS_60, 1_000_000)
            .expect("an encoder");

        let wrong = picture(SizePx::new(160, 90), 0);
        let error = encoder
            .encode(&wrong, &mut Vec::new())
            .expect_err("must not take it");

        assert!(error.to_string().contains("320x180"), "{error}");
    }

    #[test]
    fn an_odd_sized_surface_is_made_even() {
        // Every H.264 profile worth using is 4:2:0, and 4:2:0 has half-
        // resolution chroma. A wall with an odd width is a legitimate thing.
        let encoder = PreviewEncoder::open(SizePx::new(641, 361), FrameRate::FPS_60, 1_000_000)
            .expect("an encoder");

        assert_eq!(encoder.size(), SizePx::new(640, 360));
    }
}

/// A way to measure the thing, for whoever tunes it next.
///
/// The cost that matters is the whole path — copying the picture in, converting
/// its colours, and the encode — at the size and rate a wall actually runs at.
/// This runs it when `OVS_STREAM_BENCH` is set and does nothing otherwise:
///
/// ```text
/// OVS_STREAM_BENCH=1280x720 cargo test -p ovs-renderer --lib stream -- --nocapture
/// ```
#[cfg(test)]
mod worker {
    use super::tests::picture;
    use super::*;

    /// Waits for a worker's encoder to come up, or gives up.
    ///
    /// Returns false where this machine has no encoder, which is a reason to
    /// skip a test rather than to fail one.
    fn opened(worker: &mut StreamWorker) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut chunks = Vec::new();
        while std::time::Instant::now() < deadline {
            worker.collect(&mut chunks);
            if worker.failure().is_some() {
                return false;
            }
            if worker.opened() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    }

    #[test]
    fn starting_a_stream_does_not_wait_for_the_card() {
        // Taking a session on the card is tens of milliseconds, and this is
        // called from the thread that owes a frame every sixteen. Measured on
        // the test machine before this changed: opening a preview cost the wall
        // two frames and restarting the core cost it eighteen.
        let size = SizePx::new(320, 180);
        let worker = StreamWorker::start(size, FrameRate::FPS_60, 1_000_000).expect("a thread");

        assert!(
            !worker.opened(),
            "the encoder cannot have been opened on this thread"
        );
        assert_eq!(
            worker.size(),
            size,
            "until it says otherwise, it is what was asked for"
        );
        assert!(
            worker.encoding().is_none(),
            "and it cannot say what it opened as before it has opened"
        );
    }

    #[test]
    fn only_a_software_encoder_pays_the_frame_rate_cap() {
        // The two have to agree: a watcher is told `hardware: false` and a
        // reduced rate by the same decision, and if they ever disagreed the
        // preview would either be capped and claim it was not, or run at full
        // rate and apologise for something it did not do.
        for name in ENCODERS {
            let capped = PreviewEncoder::stride_for(name, FrameRate::FPS_60) > 1;
            assert_eq!(
                is_hardware(name),
                !capped,
                "{name}: hardware is {} and the cap is {capped}",
                is_hardware(name)
            );
        }
    }

    #[test]
    fn a_worker_says_what_it_opened_once_it_has() {
        let size = SizePx::new(322, 181);
        let mut worker = StreamWorker::start(size, FrameRate::FPS_60, 1_000_000).expect("a thread");

        if !opened(&mut worker) {
            // No encoder on this machine; the rest of the renderer still works.
            return;
        }

        assert_ne!(worker.encoder(), "opening");
        assert_eq!(
            worker.size(),
            SizePx::new(322, 180),
            "an odd side is made even, and the worker says so"
        );

        // And what leaves the renderer for a watcher (owner decision Q13).
        let encoding = worker.encoding().expect("it has opened");
        assert_eq!(encoding.encoder, worker.encoder());
        assert_eq!(encoding.hardware, is_hardware(worker.encoder()));
        assert!(
            encoding.fps > 0.0 && encoding.fps <= 60.0,
            "a sixty hertz surface is encoded at {} frames a second",
            encoding.fps
        );
        if !encoding.hardware {
            assert!(
                encoding.fps <= f64::from(SOFTWARE_PREVIEW_FPS),
                "a software preview said it was running at {}",
                encoding.fps
            );
        }
    }

    #[test]
    fn a_surface_is_encoded_off_the_thread_that_drew_it() {
        let size = SizePx::new(320, 180);
        let mut worker = StreamWorker::start(size, FrameRate::FPS_60, 1_000_000).expect("a thread");

        if !opened(&mut worker) {
            return;
        }

        for frame in 0..20 {
            worker.submit(picture(size, frame));
            // The worker is a thread; giving it the processor is how a test
            // waits for one without pretending to know how long it takes.
            std::thread::yield_now();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let mut chunks = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline
            && !chunks.iter().any(|chunk: &Chunk| chunk.keyframe)
        {
            worker.collect(&mut chunks);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(
            chunks.iter().any(|chunk| chunk.keyframe),
            "a watcher has to be able to start somewhere"
        );
        assert!(
            chunks.iter().all(|chunk| !chunk.data.is_empty()),
            "and every chunk has a picture in it"
        );

        let stamps: Vec<u64> = chunks.iter().map(|chunk| chunk.timestamp_us).collect();
        assert!(
            stamps.windows(2).all(|pair| pair[1] > pair[0]),
            "time runs forwards: {stamps:?}"
        );
    }

    #[test]
    fn a_frame_the_encoder_is_too_busy_for_is_dropped_rather_than_queued() {
        // A preview that is behind is not a preview. A queue is a promise to
        // show somebody what was on the wall a moment ago, which is the one
        // thing a preview must not do.
        let size = SizePx::new(1920, 1080);
        let mut worker = StreamWorker::start(size, FrameRate::FPS_60, 6_000_000).expect("a thread");

        if !opened(&mut worker) {
            // No encoder on this machine; the rest of the renderer still works.
            return;
        }

        let frame = picture(size, 0);
        for _ in 0..200 {
            worker.submit(frame.clone());
        }

        let (submitted, dropped) = worker.counts();
        assert_eq!(submitted + dropped, 200);
        assert!(
            dropped > 0,
            "two hundred frames at once cannot all be taken"
        );
    }
}

#[cfg(test)]
mod bench {
    use super::tests::picture;
    use super::*;

    #[test]
    fn measure_when_asked_to() {
        let Ok(asked) = std::env::var("OVS_STREAM_BENCH") else {
            return;
        };
        let (width, height) = asked.split_once('x').expect("WIDTHxHEIGHT");
        let size = SizePx::new(
            width.parse().expect("width"),
            height.parse().expect("height"),
        );
        let frames = 300;

        let mut encoder =
            PreviewEncoder::open(size, FrameRate::FPS_60, 6_000_000).expect("an encoder");
        let pictures: Vec<DecodedImage> = (0..60).map(|frame| picture(size, frame)).collect();

        let mut stream = Vec::new();
        let started = std::time::Instant::now();
        for frame in 0..frames {
            encoder
                .encode(&pictures[frame as usize % pictures.len()], &mut stream)
                .expect("encodes");
        }
        let elapsed = started.elapsed();

        println!(
            "{} at {}x{}: {frames} frames in {:.2}s = {:.0} fps, {:.2} Mbit/s, \
             {:.1}% of one core at 60 fps",
            encoder.encoder(),
            size.width,
            size.height,
            elapsed.as_secs_f64(),
            f64::from(frames) / elapsed.as_secs_f64(),
            stream.len() as f64 * 8.0 / (f64::from(frames) / 60.0) / 1e6,
            100.0 * elapsed.as_secs_f64() / (f64::from(frames) / 60.0),
        );
    }
}

/// One encoded picture, on its way to whoever is watching.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Whether a watcher can start here.
    ///
    /// Somebody who opens a preview mid-show sees nothing until one of these
    /// arrives, which is what the keyframe interval decides.
    pub keyframe: bool,
    /// When this picture belongs, in microseconds since the stream started.
    pub timestamp_us: u64,
    /// The bytes, Annex-B.
    pub data: Vec<u8>,
}

/// How many frames may be waiting to be encoded before one is dropped.
///
/// Two. A preview that is behind is not a preview: an operator watching a clip
/// land on a beat needs what is on the wall *now*, and a queue is a promise to
/// show them what was on it a moment ago. When the encoder cannot keep up the
/// newest frame wins and the count says so.
const QUEUE: usize = 2;

/// What a worker says back.
#[derive(Debug)]
enum Event {
    /// The encoder came up, and this is what it turned out to be.
    ///
    /// Always the first thing a worker sends, so a chunk can never arrive
    /// before the size it was encoded at is known.
    Opened {
        /// Which encoder was taken.
        encoder: &'static str,
        /// What it is encoding at, which may be an even-sided version of what
        /// was asked for.
        size: SizePx,
        /// Frames a second it will really produce, which is the show's rate
        /// unless a software encoder capped it.
        fps: f64,
    },
    /// One encoded picture.
    Encoded(Chunk),
    /// It stopped, and why.
    Failed(String),
}

/// A surface being encoded, on a thread of its own.
///
/// Encoding is milliseconds and the render thread has sixteen of them for
/// everything, so this is the same shape as every other worker in the renderer:
/// hand it something, collect what is finished, never wait.
///
/// # Opening is not waiting either
///
/// The encoder is opened *on* the worker thread rather than before it starts.
/// Taking an NVENC session costs tens of milliseconds and it was being paid on
/// the render thread: measured on the test machine, somebody opening a preview
/// cost the wall two frames, and a core restart — which stops every stream and
/// starts it again — cost eighteen. A preview is a convenience and it may not
/// cost the show a single frame.
///
/// Until the encoder is up, [`size`](Self::size) is what was asked for and
/// [`encoder`](Self::encoder) is `"opening"`, and frames handed over are
/// dropped. That is a fifth of a second at worst and it is invisible: a watcher
/// cannot see anything until the first keyframe anyway.
#[derive(Debug)]
pub struct StreamWorker {
    frames: Option<std::sync::mpsc::SyncSender<DecodedImage>>,
    events: std::sync::mpsc::Receiver<Event>,
    size: SizePx,
    encoder: &'static str,
    /// What it is really encoding at. Zero until the encoder is up, which is
    /// the same moment `encoder` stops saying "opening".
    fps: f64,
    /// Why it stopped, once it has.
    failure: Option<String>,
    /// Frames handed over and frames dropped because the encoder was busy.
    submitted: u64,
    dropped: u64,
}

impl StreamWorker {
    /// Starts encoding a surface.
    ///
    /// Returns as soon as the thread exists. Whether an encoder could be opened
    /// at all is answered a moment later, through [`collect`](Self::collect):
    /// finding that out here would mean opening it here, and this is called
    /// from the thread that owes a frame every sixteen milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError`] when the thread cannot be spawned, which is the
    /// machine being out of threads rather than anything about encoding.
    pub fn start(size: SizePx, rate: FrameRate, bitrate: u32) -> Result<Self, StreamError> {
        let (frames_in, frames_out) = std::sync::mpsc::sync_channel::<DecodedImage>(QUEUE);
        let (events_in, events_out) = std::sync::mpsc::channel::<Event>();

        let micros_per_frame = 1_000_000.0 / rate.as_f64().max(1.0);
        std::thread::Builder::new()
            .name("ovs-stream".to_owned())
            .spawn(move || {
                let mut encoder = match PreviewEncoder::open(size, rate, bitrate) {
                    Ok(encoder) => encoder,
                    Err(error) => {
                        let _ = events_in.send(Event::Failed(error.to_string()));
                        return;
                    }
                };

                if events_in
                    .send(Event::Opened {
                        encoder: encoder.encoder(),
                        size: encoder.size(),
                        fps: encoder.encoded_fps(),
                    })
                    .is_err()
                {
                    return;
                }

                let mut index: u64 = 0;
                for picture in frames_out {
                    let mut data = Vec::new();
                    match encoder.encode(&picture, &mut data) {
                        Ok(keyframe) => {
                            if data.is_empty() {
                                continue;
                            }
                            let chunk = Chunk {
                                keyframe,
                                timestamp_us: (index as f64 * micros_per_frame) as u64,
                                data,
                            };
                            index += 1;
                            if events_in.send(Event::Encoded(chunk)).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            // Said once and then stopped: an encoder that has
                            // refused a frame refuses the next one too, and
                            // sixty lines a second would bury the one that
                            // explained it.
                            let _ = events_in.send(Event::Failed(error.to_string()));
                            return;
                        }
                    }
                }
            })
            .map_err(|error| StreamError::Refused(error.to_string()))?;

        Ok(Self {
            frames: Some(frames_in),
            events: events_out,
            size,
            encoder: "opening",
            fps: 0.0,
            failure: None,
            submitted: 0,
            dropped: 0,
        })
    }

    /// Hands over a picture, or drops it if the encoder is still busy.
    ///
    /// Never waits. A render thread that blocked on an encoder would be a wall
    /// that stutters because somebody opened a preview.
    pub fn submit(&mut self, picture: DecodedImage) {
        let Some(frames) = &self.frames else {
            return;
        };

        match frames.try_send(picture) {
            Ok(()) => self.submitted += 1,
            Err(_) => self.dropped += 1,
        }
    }

    /// Collects whatever has been encoded since the last call.
    ///
    /// Also the place the encoder reports that it came up, and that it stopped:
    /// both arrive on the same channel as the pictures so that they cannot
    /// overtake them.
    pub fn collect(&mut self, into: &mut Vec<Chunk>) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Opened { encoder, size, fps } => {
                    self.encoder = encoder;
                    self.size = size;
                    self.fps = fps;
                }
                Event::Encoded(chunk) => into.push(chunk),
                Event::Failed(reason) => self.failure = Some(reason),
            }
        }
    }

    /// Why this stream stopped, once it has.
    ///
    /// Taken rather than read, because it is said to a watcher once. A worker
    /// that has failed encodes nothing further, so the caller's job on seeing
    /// this is to stop feeding it and let it go.
    #[must_use]
    pub fn failure(&mut self) -> Option<String> {
        self.failure.take()
    }

    /// Whether the encoder is up yet.
    #[must_use]
    pub fn opened(&self) -> bool {
        self.encoder != "opening"
    }

    /// The size this is encoding at.
    #[must_use]
    pub const fn size(&self) -> SizePx {
        self.size
    }

    /// Which encoder it opened.
    #[must_use]
    pub const fn encoder(&self) -> &'static str {
        self.encoder
    }

    /// What it turned out to be encoding with, once it is up.
    ///
    /// `None` until then. The answer to Q13's last clause: this
    /// is what leaves the renderer, so that a preview at a quarter speed can be
    /// shown as a decision rather than as a fault.
    #[must_use]
    pub fn encoding(&self) -> Option<ovs_types::telemetry::StreamEncoding> {
        self.opened().then(|| ovs_types::telemetry::StreamEncoding {
            encoder: self.encoder.to_owned(),
            hardware: is_hardware(self.encoder),
            fps: self.fps,
        })
    }

    /// How many pictures were handed over, and how many were dropped.
    #[must_use]
    pub const fn counts(&self) -> (u64, u64) {
        (self.submitted, self.dropped)
    }
}

impl Drop for StreamWorker {
    fn drop(&mut self) {
        // Closing the queue ends the loop, and the thread closes the encoder on
        // its way out. Deliberately not joined: this is dropped from the render
        // thread when the last watcher goes, and waiting for a card to tear down
        // a session there is how a preview closing costs the wall frames. The
        // encoder owns nothing the renderer shares, so there is nothing to be
        // in step with.
        self.frames = None;
    }
}
