//! Sources that make their own pixels.
//!
//! A generator implements the same [`FrameSource`] the decoders will, so a layer
//! plays one exactly as it plays a clip and nothing above this knows the
//! difference. What it produces is decided by the manifest's
//! parameters, checked when the show was saved rather than trusted here.
//!
//! # Most of these are still images, and two of them are not
//!
//! Six of the eight are a function of position and nothing else, so each is a
//! source with one frame and no duration — the same shape as a still. That means
//! no playhead, no decoder thread, and one upload when the slot changes rather
//! than a picture regenerated sixty times a second for a result that is
//! identical every time.
//!
//! Noise is the one that invites the opposite assumption, so it is worth saying
//! plainly: value noise at a fixed scale is a function of position and nothing
//! else. It is a still. Making it move would mean a playhead, a frame count, a
//! per-frame fill of the whole composition and a rate to argue about, and
//! nothing in the specification asks for any of that. When an animated variant
//! is genuinely wanted it is a new manifest with a speed parameter, published
//! beside this one — not an edit to this one.
//!
//! The shape invites it too, and gets the same answer. A shape at a fixed size
//! and a fixed rotation is a function of position: its rotation is an angle an
//! operator sets, not a speed. One that turned by itself would need the playhead
//! and the rate that noise was just refused, for a motion nothing asked for.
//!
//! The clock and the countdown are the exceptions, and they are exceptions for a
//! reason nothing else here has: they are a function of position *and time*.
//!
//! # How a clock fits a contract built for clips
//!
//! It fits it exactly, without bending it. [`FrameSource`] asks for a length and
//! a frame per index, and that is what these are:
//!
//! * A clock is a source **one day long** with one frame per second — 86 400 of
//!   them — and frame *n* is the second *n* of the day. With seconds turned off
//!   the picture only changes once a minute, so it is 1 440 frames instead: the
//!   frame count is the *granularity of the picture*, and making it finer than
//!   the picture would be redrawing sixty times for one result.
//! * A countdown is a source as long as the countdown, one frame per second, and
//!   frame *n* is *n* seconds in. What happens at zero falls out of the length
//!   rather than needing a rule: stopping at zero is a source that ends there,
//!   and running on into negative is a source that continues. Both then hold
//!   their last frame, which is what every source in this renderer already does.
//!
//! So `frame(index)` stays a pure function of the index — the same index draws
//! the same picture on any machine, at any time, in any test — and no clock is
//! read anywhere inside the drawing.
//!
//! # What it costs, and what has to change outside this module
//!
//! Two things the still generators did not need.
//!
//! First, **somebody has to ask for the right index**. A clip's index comes from
//! the beat, because a clip is timed to the music; a clock is not, and a clock
//! that sped up when an operator tapped a faster tempo would be worse than no
//! clock. So these sources say which index they should be showing, given the
//! wall clock and how long the slot has been up — [`GeneratorSource::tick_at`] —
//! and the caller asks for that rather than deriving one from the playhead.
//!
//! Second, **the picture is redrawn, so the buffer is kept**. A time-varying
//! generator holds one composition-sized image and writes the new second into
//! it; [`GeneratorSource::redraw_at`] answers `None` when the second has not
//! moved, so a pass that changes nothing costs a comparison and no upload. A
//! still keeps no buffer at all: it is drawn once, uploaded, and never asked
//! again.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ovs_types::config::generators::{
    manifest, GeneratorManifest, ParameterDefinition, MAX_STROBE_HZ, MIN_STROBE_HZ,
};
use ovs_types::config::slot::{ParameterMap, ParameterValue};
use ovs_types::geometry::{FrameRate, SizePx};

use crate::backend::Generated;
use crate::media::bands::in_bands;
use crate::media::source::{Frame, FrameSource};
use crate::media::still::{DecodedImage, MediaError};
use crate::media::text::{self, Pen};
use crate::media::timeline::SourceLength;
use crate::pattern::ServicePattern;

/// The largest generated frame.
///
/// A generator fills a buffer on the thread that asks for it, so its cost is a
/// memory write proportional to this. Matched to the composition sizes this
/// product targets rather than to anything a caller might ask for.
pub const MAX_GENERATED_SIDE: u32 = 8192;

/// Seconds in a day: how long a clock's source is.
const SECONDS_IN_A_DAY: u32 = 86_400;

/// The longest run `HH:MM:SS` can say, which is what bounds a countdown.
///
/// One second short of a day. The manifest refuses anything longer when a
/// show is saved; this is what a show written against another build is
/// clamped to, because a countdown that had wrapped round would be a number
/// nobody could act on.
const MAX_COUNTDOWN_SECONDS: u32 = SECONDS_IN_A_DAY - 1;

/// The longest run `MM:SS` can say.
const MAX_MINUTES_SECONDS: u32 = 3_599;

/// Below this, a countdown is written as minutes and seconds rather than hours.
///
/// Fixed for the whole run from the duration rather than chosen each second, so
/// the digits do not change size when the last hour ticks away.
const COUNTDOWN_HOURS_FROM: u32 = 3_600;

/// How much of the picture's height the lettering takes.
const CAP_FRACTION: f32 = 0.42;

/// How much of the picture's width a line is allowed to fill.
///
/// A margin either side, because these are drawn edge to edge on a wall and a
/// clock that touched the frame would read as one that had been cut off.
const LINE_FRACTION: f32 = 0.86;

/// How heavy the strokes are, as a fraction of the cap height.
///
/// Heavier than the test card's caption: a clock is read at a glance from the
/// side of a stage, often past a lighting rig, and a thin stroke at that
/// distance is a grey smudge.
const STROKE_WEIGHT: f32 = 0.14;

/// Space between characters, as a fraction of the cap height.
const TRACKING: f32 = 0.06;

/// What a generator that changes over time needs to know about now.
///
/// Two clocks rather than one, because the two generators are anchored to
/// different things and neither can be derived from the other: the time of day
/// is on the wall clock, and how long a speaker has been talking is measured
/// from the moment somebody fired the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    /// Wall-clock time, for a clock.
    pub wall: SystemTime,
    /// How long this generator has been on stage, for a countdown.
    pub since_anchor: Duration,
}

/// A picture kept so the next second can be written into it.
///
/// Only a generator that changes over time has one. A still is drawn once,
/// uploaded and never asked again, so holding a composition-sized buffer for it
/// would be up to two hundred megabytes kept for nothing.
#[derive(Debug)]
struct Live {
    image: DecodedImage,
    /// Which tick is in the buffer, or `None` before anything has been drawn.
    drawn: Option<u32>,
}

/// A source that draws itself.
#[derive(Debug)]
pub struct GeneratorSource {
    kind: Kind,
    size: SizePx,
    live: Option<Live>,
    /// What makes this instance's noise its own.
    ///
    /// Two noise layers on one wall must not draw the same field, and they
    /// would: the field is a function of position and time, and two layers
    /// share both. A number per instance is the cheapest thing that separates
    /// them, and noise is the one kind here where "which number" cannot
    /// matter.
    seed: u64,
    /// The service pattern, built the first time it is asked for.
    ///
    /// Built there rather than here because it needs the rate of the surface
    /// it is drawn for, which is the composition's and can change while a show
    /// is open — and a caption naming a rate nobody is running is worse than
    /// no caption at all.
    pattern: Option<Box<ServicePattern>>,
}

/// What a generator draws.
/// Not `Copy`: the text generator carries the words an operator typed, and a
/// kind that could be copied by accident would be a string copied sixty times a
/// second (house rule §5).
#[derive(Debug, Clone)]
enum Kind {
    /// One colour, everywhere.
    SolidColour { colour: [f32; 4] },
    /// A grid, optional colour bars and a centre cross.
    TestPattern { grid: u32, bars: bool },
    /// This system's own test pattern, the one an output and a composition
    /// show. Only the caption is carried here: everything else about the
    /// picture is the surface it is drawn for, and the pattern keeps its own
    /// buffers, so the drawing itself lives in [`GeneratorSource::pattern`].
    ServicePattern { caption: String },
    /// A flash at a rate: the colour, then nothing, then the colour.
    Strobe { colour: [f32; 4], hz: f32 },
    /// A linear ramp between two colours, along an angle.
    Gradient {
        from: [f32; 4],
        to: [f32; 4],
        angle_deg: f32,
    },
    /// Ruled lines at a spacing, in one colour on another.
    Grid {
        spacing: u32,
        line: u32,
        line_colour: [f32; 4],
        background: [f32; 4],
    },
    /// One shape, centred, over a background.
    ///
    /// A function of position and nothing else, like everything here but the two
    /// timepieces: the same picture at every index, because a size and a
    /// rotation are numbers an operator set rather than a motion (see the module
    /// documentation).
    Shape {
        figure: Figure,
        /// Across, in composition pixels.
        width_px: f32,
        /// Down, in composition pixels.
        height_px: f32,
        /// Clockwise on screen, in degrees.
        rotation_deg: f32,
        colour: [f32; 4],
        background: [f32; 4],
    },
    /// Value noise at a cell size.
    ///
    /// A function of position and nothing else: no seed and no time. No seed
    /// because a show that looked different every night would be a show
    /// an operator could not check in the afternoon, which is the same reason a
    /// layer's placeholder colour is derived rather than random. No time because
    /// this is a still (see the module documentation).
    Noise {
        scale: u32,
        coloured: bool,
        amount: f32,
    },
    /// Words an operator typed.
    Text {
        /// What to set, already upper-cased and split into lines.
        lines: Vec<String>,
        /// Cap height in composition pixels.
        cap_px: f32,
        /// Stroke width, as a fraction of the cap height.
        weight: f32,
        /// Extra space after each character, as a fraction of the cap height.
        tracking: f32,
        /// Which edge the lines are set against.
        align: Align,
        text: [f32; 4],
        background: [f32; 4],
    },
    /// The time of day.
    Clock {
        hour_24: bool,
        show_seconds: bool,
        text: [f32; 4],
        background: [f32; 4],
        /// The venue's offset from UTC, in minutes.
        offset_minutes: i32,
    },
    /// Time remaining, counting down.
    Countdown {
        /// How long the run is, in seconds.
        duration_s: u32,
        /// Whether firing the pad starts it again.
        ///
        /// Nothing in here acts on it: the anchor is supplied by whoever drives
        /// the source, and this is what tells them whether a trigger moves it.
        from_trigger: bool,
        /// Whether it runs on into negative rather than stopping at zero.
        count_past_zero: bool,
        text: [f32; 4],
        background: [f32; 4],
    },
}

/// Which shape [`Kind::Shape`] draws.
///
/// The identifiers are the manifest's, and they have to stay the manifest's: a
/// show stores one of these strings and expects the same shape back
/// (owner decision Q17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Figure {
    /// A disc, or an oval when the two sizes differ.
    Ellipse,
    /// A block, or a band when the two sizes differ.
    Rectangle,
    /// An isosceles triangle, apex upwards, filling the size it was given.
    Triangle,
}

impl Figure {
    /// Reads the `shape` parameter.
    ///
    /// A name this build does not know is only reachable from a show written
    /// against another one — this build refuses it on save — and the published
    /// first shape is the answer to it that is not invented in the renderer.
    fn from_id(id: &str) -> Self {
        match id {
            "rectangle" => Self::Rectangle,
            "triangle" => Self::Triangle,
            _ => Self::Ellipse,
        }
    }

    /// How far a point is from this shape's boundary, in pixels.
    ///
    /// Negative inside it. The point has already been turned into the shape's
    /// own frame, where the shape is centred on the origin and upright.
    fn distance(self, x: f32, y: f32, half_width: f32, half_height: f32) -> f32 {
        match self {
            Self::Ellipse => ellipse_distance(x, y, half_width, half_height),
            Self::Rectangle => rectangle_distance(x, y, half_width, half_height),
            Self::Triangle => triangle_distance(x, y, half_width, half_height),
        }
    }
}

impl Kind {
    /// How long a source of this kind is, and how finely it is divided.
    ///
    /// The frame count is the granularity of the *picture*, not of the show: a
    /// clock without seconds changes 1 440 times a day and a clock with them
    /// 86 400 times, and asking for a finer index than that would be redrawing a
    /// composition for a result identical to the last one.
    fn length(&self) -> SourceLength {
        match *self {
            Self::Clock { show_seconds, .. } => SourceLength {
                duration_ms: SECONDS_IN_A_DAY as u64 * 1_000,
                frame_count: if show_seconds {
                    SECONDS_IN_A_DAY
                } else {
                    SECONDS_IN_A_DAY / 60
                },
            },
            Self::Countdown {
                duration_s,
                count_past_zero,
                ..
            } => {
                let frames = countdown_frames(duration_s, count_past_zero);
                SourceLength {
                    duration_ms: frames as u64 * 1_000,
                    frame_count: frames,
                }
            }
            // Noise has no period at all: every index is an independent
            // field. The index is milliseconds, as the service pattern's is,
            // and this is the span it runs for before coming round.
            Self::Noise { .. } => SourceLength {
                duration_ms: u32::MAX as u64,
                frame_count: u32::MAX,
            },
            // **The index is milliseconds, and the picture never repeats.**
            // The drift wraps over the width — about nineteen seconds across
            // and twenty-seven down at 1920x1080, both of which scale with the
            // surface — and the sweep over a cycle of its own that depends on
            // the diagonal. Neither divides the other, so there is no shorter
            // honest number here than the span the index itself runs before it
            // comes round, which is about forty-nine days.
            Self::ServicePattern { .. } => SourceLength {
                duration_ms: u32::MAX as u64,
                frame_count: u32::MAX,
            },
            // Two pictures -- lit and dark -- and one cycle to show them in.
            // The frame count is the granularity of the picture here as it is
            // everywhere else in this match; a strobe simply has only two.
            Self::Strobe { hz, .. } => {
                let rate = clamp_rate(hz);
                SourceLength {
                    duration_ms: (1_000.0 / rate) as u64,
                    frame_count: 2,
                }
            }
            // No duration: a still has no timeline, so there is nothing for a
            // playhead to run along.
            _ => SourceLength {
                duration_ms: 0,
                frame_count: 1,
            },
        }
    }

    /// Whether the picture depends on the time as well as on the position.
    const fn changes_over_time(&self) -> bool {
        matches!(
            *self,
            Self::Clock { .. }
                | Self::Countdown { .. }
                | Self::Strobe { .. }
                | Self::ServicePattern { .. }
                | Self::Noise { .. }
        )
    }
}

/// The rate a strobe actually runs at.
///
/// Held inside the published range rather than trusted: a show written against
/// another build can name anything, and a rate of zero would be a division by
/// it. The ceiling is what an output can show rather than what somebody can
/// type -- see `MAX_STROBE_HZ`.
fn clamp_rate(hz: f32) -> f64 {
    f64::from(hz).clamp(MIN_STROBE_HZ, MAX_STROBE_HZ)
}

/// How finely the service pattern's time is divided, in ticks a second.
///
/// A millisecond, which is finer than any rate this drives presents at — so
/// every pass gets an index of its own whatever the wall is running at, and
/// the drift does not judder against a number this picked. The cost of the
/// fine division is nothing: this pattern is redrawn every pass regardless,
/// which is what it is for.
const PATTERN_TICKS_PER_SECOND: u32 = 1_000;

/// How many frames a countdown holds.
///
/// One more than the duration, because zero is a second somebody watches rather
/// than an instant it passes through: a ten-minute countdown shows `00:00` for
/// the six hundred and first second and then holds it.
///
/// A countdown that runs on past zero carries as much again as the format it is
/// written in can express, and then holds. There has to be an end — a source has
/// a frame count — and the honest place for it is where the writing runs out.
const fn countdown_frames(duration_s: u32, count_past_zero: bool) -> u32 {
    // Only reachable from a show written against another build: the manifest
    // refuses anything outside this when a show is saved.
    let duration = if duration_s > MAX_COUNTDOWN_SECONDS {
        MAX_COUNTDOWN_SECONDS
    } else if duration_s < 1 {
        1
    } else {
        duration_s
    };

    let overrun = if count_past_zero {
        if duration >= COUNTDOWN_HOURS_FROM {
            MAX_COUNTDOWN_SECONDS
        } else {
            MAX_MINUTES_SECONDS
        }
    } else {
        0
    };

    duration + 1 + overrun
}

impl GeneratorSource {
    /// Builds the generator a slot asks for.
    ///
    /// Returns `None` when this build does not publish that type and version,
    /// which is the same answer a show validated against this build should
    /// never be able to produce — and the honest one when it does.
    #[must_use]
    pub fn new(
        type_id: &str,
        version: u32,
        parameters: &ParameterMap,
        size: SizePx,
    ) -> Option<Self> {
        let manifest = manifest(type_id, version)?;
        let kind = match manifest.type_id {
            "ovs.solid_colour" => Kind::SolidColour {
                colour: colour_of(manifest, parameters, "colour"),
            },
            // **Version two draws a different picture, deliberately.** A
            // show that names version one still gets the grid and bars it was
            // saved with; a source list offers only the newest, so this is
            // what an operator adds today.
            "ovs.test_pattern" if manifest.version >= 2 => Kind::ServicePattern {
                caption: text_of(manifest, parameters, "caption"),
            },
            "ovs.test_pattern" => Kind::TestPattern {
                grid: number_of(manifest, parameters, "grid_px") as u32,
                bars: flag_of(manifest, parameters, "bars"),
            },
            "ovs.strobe" => Kind::Strobe {
                colour: colour_of(manifest, parameters, "colour"),
                hz: number_of(manifest, parameters, "hz") as f32,
            },
            "ovs.gradient" => Kind::Gradient {
                from: colour_of(manifest, parameters, "colour_a"),
                to: colour_of(manifest, parameters, "colour_b"),
                angle_deg: number_of(manifest, parameters, "angle_deg") as f32,
            },
            "ovs.grid" => Kind::Grid {
                spacing: number_of(manifest, parameters, "spacing_px") as u32,
                line: number_of(manifest, parameters, "line_px") as u32,
                line_colour: colour_of(manifest, parameters, "line_colour"),
                background: colour_of(manifest, parameters, "background_colour"),
            },
            "ovs.shape" => Kind::Shape {
                figure: Figure::from_id(&choice_of(manifest, parameters, "shape")),
                width_px: number_of(manifest, parameters, "width_px") as f32,
                height_px: number_of(manifest, parameters, "height_px") as f32,
                rotation_deg: number_of(manifest, parameters, "rotation_deg") as f32,
                colour: colour_of(manifest, parameters, "colour"),
                background: colour_of(manifest, parameters, "background_colour"),
            },
            "ovs.noise" => Kind::Noise {
                scale: number_of(manifest, parameters, "scale_px") as u32,
                coloured: flag_of(manifest, parameters, "coloured"),
                amount: number_of(manifest, parameters, "amount") as f32,
            },
            "ovs.text" => Kind::Text {
                lines: set_out(&text_of(manifest, parameters, "text")),
                cap_px: number_of(manifest, parameters, "cap_px") as f32,
                weight: number_of(manifest, parameters, "weight") as f32,
                tracking: number_of(manifest, parameters, "tracking") as f32,
                align: Align::from_id(&choice_of(manifest, parameters, "align")),
                text: colour_of(manifest, parameters, "text_colour"),
                background: colour_of(manifest, parameters, "background_colour"),
            },
            "ovs.clock" => Kind::Clock {
                hour_24: flag_of(manifest, parameters, "hour_24"),
                show_seconds: flag_of(manifest, parameters, "show_seconds"),
                text: colour_of(manifest, parameters, "text_colour"),
                background: colour_of(manifest, parameters, "background_colour"),
                offset_minutes: number_of(manifest, parameters, "utc_offset_minutes") as i32,
            },
            "ovs.countdown" => Kind::Countdown {
                duration_s: number_of(manifest, parameters, "duration_s") as u32,
                from_trigger: flag_of(manifest, parameters, "from_trigger"),
                count_past_zero: flag_of(manifest, parameters, "count_past_zero"),
                text: colour_of(manifest, parameters, "text_colour"),
                background: colour_of(manifest, parameters, "background_colour"),
            },
            // Published but not implemented here would be a manifest describing
            // something nothing can draw, which is a mistake in this build
            // rather than in a show.
            _ => return None,
        };

        let size = SizePx::new(
            size.width.clamp(1, MAX_GENERATED_SIDE),
            size.height.clamp(1, MAX_GENERATED_SIDE),
        );

        // The buffer the next second gets written into. Allocated here, when the
        // slot changes, because the alternative is allocating a composition on
        // the render thread once a second (house rule §5). A still gets none: it
        // is uploaded once and never asked again.
        // The service pattern keeps its own buffers — the bars and the
        // caption are drawn once and copied — so a second one the size of the
        // composition would be up to two hundred megabytes held for nothing.
        let live =
            (kind.changes_over_time() && !matches!(kind, Kind::ServicePattern { .. })).then(|| {
                Live {
                    image: blank(size),
                    drawn: None,
                }
            });

        // Counted rather than drawn from the system's randomness: this has to
        // differ between instances and has no reason to differ between runs,
        // and a counter cannot fail or block.
        static NEXT_SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

        Some(Self {
            kind,
            size,
            live,
            seed: NEXT_SEED.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            pattern: None,
        })
    }

    /// Whether the picture depends on the time as well as on the position.
    ///
    /// What tells a caller whether one upload when the slot changes is enough.
    #[must_use]
    pub const fn changes_over_time(&self) -> bool {
        self.kind.changes_over_time()
    }

    /// Whether firing the pad again should start this generator over.
    ///
    /// Only a countdown has an opinion, and only because an operator said so: a
    /// speaker's clock is reset between talks, and one running against something
    /// in the world must not be disturbed by somebody bringing it up on a
    /// screen. Nothing else here is anchored to anything a trigger could move.
    #[must_use]
    pub const fn restarts_on_trigger(&self) -> bool {
        match self.kind {
            Kind::Countdown { from_trigger, .. } => from_trigger,
            _ => false,
        }
    }

    /// Which frame this generator should be showing now.
    ///
    /// `None` for a still, which has one frame and is already showing it. This
    /// is the whole of the coupling between these sources and real time: the
    /// drawing itself reads no clock, so the same index is the same picture on
    /// any machine and in any test.
    #[must_use]
    pub fn tick_at(&self, moment: Moment) -> Option<u32> {
        match &self.kind {
            Kind::Clock {
                show_seconds,
                offset_minutes,
                ..
            } => {
                let second = second_of_day(moment.wall, *offset_minutes);
                Some(if *show_seconds { second } else { second / 60 })
            }
            Kind::Noise { .. } => {
                // Free-running, like the pattern below and for a plainer
                // reason: there is nothing in noise for a trigger to restart.
                // What keeps two noise layers from drawing the same field as
                // each other is the seed, not the clock.
                let ms = moment
                    .wall
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                Some((ms % u128::from(u32::MAX)) as u32)
            }
            Kind::ServicePattern { .. } => {
                // **Free-running, on a clock of its own** (owner, 2026-10-12:
                // "Das Testbild läuft immer Freerun, es hat seine eigene
                // Zeit"). The wall clock rather than how long the layer has
                // been up, which is what everything else here is timed by:
                // this one must not restart when a slot is switched back to
                // it, when a caption is edited or when the composition is
                // resized, and two layers showing it have to be in step. It is
                // what somebody checks a wall with — "um nicht in die WebGUI
                // oder an den Server gehen zu müssen" — rather than something
                // a show is built out of, so nothing about the show reaches
                // it: not the trigger, not the transport, not the master
                // speed.
                //
                // A clock somebody steps, by hand or by NTP, steps this
                // picture with it. That is the cost of a time of its own and
                // it is a jump in a test pattern, which is the cheapest place
                // in this system for one to land.
                //
                // Wrapped rather than held at the end: a test pattern that
                // stopped moving would be exactly the lie it exists to expose
                // — a frozen output and a live one showing a still look the
                // same from across a room.
                let ms = moment
                    .wall
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                Some((ms % u128::from(u32::MAX)) as u32)
            }
            Kind::Strobe { hz, .. } => {
                // The parity of the half-cycle, and nothing more: a strobe has
                // two pictures, so that is the whole index. Counting
                // half-cycles would be a number climbing all evening to pick
                // between two things, and it would eventually run out.
                let halves = moment.since_anchor.as_secs_f64() * clamp_rate(*hz) * 2.0;
                Some((halves as u64 % 2) as u32)
            }
            Kind::Countdown { .. } => {
                let last = self.kind.length().frame_count.saturating_sub(1);
                let elapsed = moment.since_anchor.as_secs();
                Some(u32::try_from(elapsed).unwrap_or(u32::MAX).min(last))
            }
            _ => None,
        }
    }

    /// Draws a tick into the kept buffer, if it is not the one already drawn.
    ///
    /// Returns the picture to upload, or `None` when what is on the GPU is still
    /// right — which is every pass but one in sixty, and the reason a clock on a
    /// wall costs a comparison rather than a composition.
    ///
    /// Also `None` for a still, which has no kept buffer: it is drawn through
    /// [`FrameSource::frame`] once when the slot changes.
    pub fn redraw_at(&mut self, tick: u32, rate: FrameRate) -> Option<&DecodedImage> {
        if matches!(self.kind, Kind::ServicePattern { .. }) {
            return self.redraw_pattern(tick, rate);
        }

        let Self {
            kind,
            size,
            live,
            seed,
            ..
        } = self;
        let live = live.as_mut()?;
        if live.drawn == Some(tick) {
            return None;
        }

        draw_kind(kind, *size, live.image.pixels_mut(), tick, *seed);
        live.drawn = Some(tick);
        Some(&live.image)
    }

    /// Draws the service pattern, rebuilding it when the surface has changed.
    ///
    /// `matches` is three comparisons a pass. The caption names the surface's
    /// size and mode, so a composition somebody resizes or re-rates has to get
    /// a new caption rather than keep one that has stopped being true.
    ///
    /// There is no "already drawn" comparison here as there is for a clock:
    /// this picture is different on every pass, which is the whole of what it
    /// is for.
    fn redraw_pattern(&mut self, tick: u32, rate: FrameRate) -> Option<&DecodedImage> {
        let stale = match (&self.kind, &self.pattern) {
            (Kind::ServicePattern { caption }, Some(pattern)) => {
                !pattern.matches(self.size, rate, caption)
            }
            _ => true,
        };
        if stale {
            if let Kind::ServicePattern { caption } = &self.kind {
                self.pattern = Some(Box::new(ServicePattern::new(self.size, rate, caption)));
            }
        }

        let seconds = tick as f32 / PATTERN_TICKS_PER_SECOND as f32;
        self.pattern.as_mut().map(|pattern| pattern.draw(seconds))
    }

    /// What the card would have to draw for this generator, if it can.
    ///
    /// `None` for every kind that is still the processor's: the service
    /// pattern, which is a prepared tile and a caption; and the ones that
    /// write letters, which need the glyphs. Those are either cheap or rare -
    /// a clock redraws when its second moves - and the one that was neither
    /// is here.
    ///
    /// Measured on PC05 on 2026-10-15: `ovs.noise` cost the render thread
    /// 25,532 microseconds a pass against six for a 4K clip decoded on the
    /// card. That is what this is for.
    #[must_use]
    pub fn on_card(&self, tick: u32) -> Option<Generated> {
        match &self.kind {
            Kind::Noise {
                scale,
                coloured,
                amount,
            } => Some(Generated::Noise {
                scale: *scale,
                coloured: *coloured,
                amount: *amount,
                field: noise_field(self.seed, tick),
            }),
            Kind::SolidColour { colour } => Some(Generated::Solid { colour: *colour }),
            Kind::TestPattern { grid, bars } => Some(Generated::TestPattern {
                grid: *grid,
                bars: *bars,
            }),
            Kind::Gradient {
                from,
                to,
                angle_deg,
            } => Some(Generated::Gradient {
                from: *from,
                to: *to,
                // The processor falls back to ninety for an angle that is not
                // a number, rather than filling a wall with whatever `as u8`
                // makes of NaN. Decided here, so both paths fall back alike.
                angle_deg: if angle_deg.is_finite() {
                    *angle_deg
                } else {
                    90.0
                },
            }),
            Kind::Grid {
                spacing,
                line,
                line_colour,
                background,
            } => Some(Generated::Grid {
                spacing: *spacing,
                line: *line,
                line_colour: *line_colour,
                background: *background,
            }),
            Kind::Shape {
                figure,
                width_px,
                height_px,
                rotation_deg,
                colour,
                background,
            } => Some(Generated::Shape {
                figure: match figure {
                    Figure::Ellipse => 0,
                    Figure::Rectangle => 1,
                    Figure::Triangle => 2,
                },
                // A size that is not a finite number above zero encloses
                // nothing; zero reaches the shader as "draw the bed and stop",
                // which is what the processor does with it.
                extent: [
                    if width_px.is_finite() { *width_px } else { 0.0 },
                    if height_px.is_finite() {
                        *height_px
                    } else {
                        0.0
                    },
                ],
                rotation_deg: if rotation_deg.is_finite() {
                    *rotation_deg
                } else {
                    0.0
                },
                colour: *colour,
                background: *background,
            }),
            Kind::Strobe { colour, .. } => {
                // Lit on one half of the cycle and clear on the other, decided
                // here exactly as the processor decides it. Clear rather than
                // black: a strobe belongs over whatever is under it.
                let lit = tick.is_multiple_of(2);
                Some(Generated::Solid {
                    colour: if lit {
                        *colour
                    } else {
                        [colour[0], colour[1], colour[2], 0.0]
                    },
                })
            }
            _ => None,
        }
    }

    /// How big a picture this generator draws.
    #[must_use]
    pub const fn size(&self) -> SizePx {
        self.size
    }

    /// What the card needs to draw this pass of the service pattern.
    ///
    /// `None` for every other kind. Otherwise the pattern's unmoving parts are
    /// brought up to date — the bars and the caption, which only change when
    /// the surface does — and handed back **only when they were rebuilt**, so
    /// that eight megabytes crosses the bus on the pass a mode changed and on
    /// no other. The moment beside it is where the drift and the sweep have
    /// got to, which is all that is different from one pass to the next.
    ///
    /// Measured on PC05 on 2026-10-15: drawing this on the processor was
    /// 45% of the render thread, and the whole of it was moving bytes about.
    pub fn pattern_on_card(
        &mut self,
        tick: u32,
        rate: FrameRate,
    ) -> Option<(Option<DecodedImage>, crate::pattern::PatternMoment)> {
        let Kind::ServicePattern { caption } = &self.kind else {
            return None;
        };
        // Cloned rather than borrowed, because the pattern below is written
        // through `self` while this is read from it. A short string, and only
        // on the pass that rebuilds.
        let caption = caption.clone();
        let stale = self
            .pattern
            .as_ref()
            .is_none_or(|pattern| !pattern.matches(self.size, rate, &caption));
        if stale {
            self.pattern = Some(Box::new(ServicePattern::new(self.size, rate, &caption)));
        }

        let pattern = self.pattern.as_ref()?;
        let seconds = tick as f32 / PATTERN_TICKS_PER_SECOND as f32;
        Some((
            stale.then(|| pattern.source_picture()),
            pattern.moment(seconds),
        ))
    }

    /// Forgets which tick is on the GPU, so the next redraw draws again.
    ///
    /// For when something other than this generator has written the layer's
    /// texture. The kept buffer is still right; what is on the GPU is not, and
    /// `redraw_at` compares against the former.
    pub fn forget_drawn(&mut self) {
        if let Some(live) = self.live.as_mut() {
            live.drawn = None;
        }
    }

    /// Draws the frame at an index.
    fn draw(&self, index: u32) -> DecodedImage {
        let mut image = blank(self.size);
        draw_kind(&self.kind, self.size, image.pixels_mut(), index, self.seed);
        image
    }
}

impl FrameSource for GeneratorSource {
    fn length(&self) -> SourceLength {
        self.kind.length()
    }

    fn frame(&mut self, index: u32) -> Result<Frame, MediaError> {
        // Clamped rather than refused, as every source here is: a scheduler that
        // asked past the end has made a rounding error, and holding the last
        // frame is the right answer to it. For a still every index is the same
        // picture, which is what it always was.
        let index = index.min(self.kind.length().frame_count.saturating_sub(1));

        // **The side door, and nothing in the live path comes through it.** A
        // generator that changes over time is drawn through `redraw_at`, which
        // is given the surface's real rate; this is what a test or an exporter
        // reaches for. Built fresh and at the rate a composition is published
        // with, because there is no surface here to ask — stated rather than
        // invented, and the caption says so on the picture.
        if let Kind::ServicePattern { caption } = &self.kind {
            let mut pattern = ServicePattern::new(self.size, FrameRate::FPS_60, caption);
            let seconds = index as f32 / PATTERN_TICKS_PER_SECOND as f32;
            return Ok(Frame::new(index, pattern.draw(seconds).clone()));
        }

        Ok(Frame::new(index, self.draw(index)))
    }
}

/// Everything one field of noise is made of.
///
/// One value rather than five arguments, so a band can be handed the field it
/// is drawing a piece of.
#[derive(Clone, Copy)]
struct NoiseField {
    /// How many pixels one cell of the lattice covers.
    scale: u32,
    /// Whether the three channels get independent fields.
    coloured: bool,
    /// How far from mid grey the field reaches, 0 to 1.
    amount: f32,
    /// Which frame of the source this is.
    tick: u32,
    /// The generator's own seed, so two noise layers do not move together.
    seed: u64,
}

/// Draws one field of noise on the processor, for a test on the card to
/// compare itself against.
///
/// The card draws generators now, and the processor still draws them for the
/// export path and for the tests that decide what these pictures are. Two
/// implementations of one picture is exactly the pair that drifts, so this is
/// the way the other one is reachable from the test that checks them against
/// each other.
#[cfg(test)]
pub(crate) fn noise_for_test(
    size: SizePx,
    scale: u32,
    coloured: bool,
    amount: f32,
    tick: u32,
    seed: u64,
) -> Vec<u8> {
    let mut pixels = vec![0_u8; size.width as usize * size.height as usize * 4];
    let field = NoiseField {
        scale,
        coloured,
        amount,
        tick,
        seed,
    };
    draw_noise(&mut pixels, size.width as usize, 0, field);
    pixels
}

/// Draws one card-drawable generator on the processor, for a test on the card
/// to compare itself against.
///
/// The description goes back to the kind it came from, so what this draws is
/// what a show would have drawn before any of these moved to the card. The
/// two kinds that are not here — noise, which has its own helper, and the
/// service pattern, which keeps its own buffer — are drawn through their own
/// paths.
#[cfg(test)]
pub(crate) fn generator_for_test(what: Generated, size: SizePx) -> Vec<u8> {
    let kind = match what {
        Generated::TestPattern { grid, bars } => Kind::TestPattern { grid, bars },
        Generated::Gradient {
            from,
            to,
            angle_deg,
        } => Kind::Gradient {
            from,
            to,
            angle_deg,
        },
        Generated::Grid {
            spacing,
            line,
            line_colour,
            background,
        } => Kind::Grid {
            spacing,
            line,
            line_colour,
            background,
        },
        Generated::Shape {
            figure,
            extent,
            rotation_deg,
            colour,
            background,
        } => Kind::Shape {
            figure: match figure {
                1 => Figure::Rectangle,
                2 => Figure::Triangle,
                _ => Figure::Ellipse,
            },
            width_px: extent[0],
            height_px: extent[1],
            rotation_deg,
            colour,
            background,
        },
        Generated::Solid { colour } => Kind::SolidColour { colour },
        other => panic!("{other:?} is not drawn through `draw_kind`"),
    };

    let mut pixels = vec![0_u8; size.width as usize * size.height as usize * 4];
    draw_kind(&kind, size, &mut pixels, 0, 0);
    pixels
}

/// An empty picture of a size, to be drawn into.
fn blank(size: SizePx) -> DecodedImage {
    let pixels = vec![0_u8; size.width as usize * size.height as usize * 4];
    DecodedImage::new(pixels, size).expect("a buffer the size it was made for")
}

/// Draws one kind of generator at one index, into a buffer of the right size.
///
/// `tick` is which frame of the source this is. Every still ignores it, because
/// every still has one frame.
fn draw_kind(kind: &Kind, size: SizePx, pixels: &mut [u8], tick: u32, seed: u64) {
    let width = size.width as usize;
    let height = size.height as usize;

    match kind {
        Kind::SolidColour { colour } => {
            let rgba = to_bytes(*colour);
            for pixel in pixels.as_chunks_mut::<4>().0 {
                *pixel = rgba;
            }
        }
        Kind::TestPattern { grid, bars } => draw_test_pattern(pixels, width, height, *grid, *bars),
        // Never reached: the service pattern keeps its own buffer and is
        // drawn through `redraw_at`. Left as it was found rather than
        // panicking — a layer that is black is something an operator can see
        // and say, and a renderer that stopped is not.
        Kind::ServicePattern { .. } => {}
        Kind::Strobe { colour, .. } => {
            // Lit on one half of the cycle and clear on the other. Clear
            // rather than black: a strobe belongs over whatever is under it,
            // and filling with black would make it a blackout with a flash in
            // it. The layer's own opacity still scales the flash.
            let lit = tick.is_multiple_of(2);
            let rgba = to_bytes(if lit {
                *colour
            } else {
                [colour[0], colour[1], colour[2], 0.0]
            });
            for pixel in pixels.as_chunks_mut::<4>().0 {
                *pixel = rgba;
            }
        }
        Kind::Gradient {
            from,
            to,
            angle_deg,
        } => draw_gradient(pixels, width, height, *from, *to, *angle_deg),
        Kind::Grid {
            spacing,
            line,
            line_colour,
            background,
        } => draw_grid(pixels, width, *spacing, *line, *line_colour, *background),
        Kind::Shape {
            figure,
            width_px,
            height_px,
            rotation_deg,
            colour,
            background,
        } => draw_shape(
            pixels,
            size,
            *figure,
            (*width_px, *height_px),
            *rotation_deg,
            *colour,
            *background,
        ),
        Kind::Noise {
            scale,
            coloured,
            amount,
        } => {
            let noise = NoiseField {
                scale: *scale,
                coloured: *coloured,
                amount: *amount,
                tick,
                seed,
            };
            in_bands(pixels, width, |band, first_row| {
                draw_noise(band, width, first_row, noise);
            });
        }
        Kind::Text {
            lines,
            cap_px,
            weight,
            tracking,
            align,
            text,
            background,
        } => draw_text(
            pixels,
            size,
            lines,
            TextPen {
                cap_px: *cap_px,
                weight: *weight,
                tracking: *tracking,
                align: *align,
                colour: *text,
                background: *background,
            },
        ),
        Kind::Clock {
            hour_24,
            show_seconds,
            text,
            background,
            ..
        } => {
            let written = clock_text(tick, *hour_24, *show_seconds);
            draw_written(pixels, size, written.as_bytes(), *text, *background);
        }
        Kind::Countdown {
            duration_s,
            count_past_zero,
            text,
            background,
            ..
        } => {
            let written = countdown_text(*duration_s, *count_past_zero, tick);
            draw_written(pixels, size, written.as_bytes(), *text, *background);
        }
    }
}

/// Returns what the manifest published this parameter as.
///
/// The answer when a show holds a value of the wrong type for a key. That
/// cannot happen on a show validated against this build, so reaching here
/// means a show written somewhere else — and the published default is the
/// only answer that is not invented in the renderer. A constant
/// written here instead would be a number from one generator quietly applied to
/// another.
fn declared(manifest: &GeneratorManifest, key: &str) -> Option<ParameterValue> {
    manifest
        .parameter(key)
        .map(ParameterDefinition::default_value)
}

/// Returns a colour parameter, or the manifest's default.
fn colour_of(manifest: &GeneratorManifest, values: &ParameterMap, key: &str) -> [f32; 4] {
    match manifest
        .value(values, key)
        .filter(|value| matches!(value, ParameterValue::Color(_)))
        .or_else(|| declared(manifest, key))
    {
        Some(ParameterValue::Color(colour)) => colour,
        _ => [1.0, 1.0, 1.0, 1.0],
    }
}

/// Which edge a block of text is set against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Centre,
    Right,
}

impl Align {
    /// Reads the `align` parameter.
    ///
    /// A name this build does not know is only reachable from a show written
    /// against another one — this build refuses it on save — and the published
    /// default is the answer to it that is not invented in the renderer.
    fn from_id(id: &str) -> Self {
        match id {
            "left" => Self::Left,
            "right" => Self::Right,
            _ => Self::Centre,
        }
    }
}

/// How one block of text is set.
struct TextPen {
    cap_px: f32,
    weight: f32,
    tracking: f32,
    align: Align,
    colour: [f32; 4],
    background: [f32; 4],
}

/// How far apart two lines sit, as a fraction of the cap height.
///
/// Cap height plus half of it again. Tighter and the descenders of one line
/// meet the caps of the next; looser and two lines stop reading as one block.
const LINE_SPACING: f32 = 1.5;

/// Turns what an operator typed into the lines this font can set.
///
/// Upper-cased, because the font has no lower case and the alternative is a
/// caption that silently loses most of itself. Split on line breaks, because
/// somebody who pressed Enter meant a line break; nothing wraps, because
/// wrapping is layout and layout is a font engine's job.
///
/// A carriage return is dropped rather than set: text arriving from a client on
/// Windows carries them, and a box at the end of every line would be this
/// renderer punishing an operator for their operating system.
fn set_out(words: &str) -> Vec<String> {
    words
        .replace('\r', "")
        .split('\n')
        .map(|line| line.trim_end().to_uppercase())
        .collect()
}

/// Draws a block of upper-cased lines, centred vertically.
fn draw_text(pixels: &mut [u8], size: SizePx, lines: &[String], pen: TextPen) {
    let rgba = to_bytes(pen.background);
    for pixel in pixels.as_chunks_mut::<4>().0 {
        *pixel = rgba;
    }

    let cap = pen.cap_px.max(1.0);
    let step = cap * LINE_SPACING;
    let width = size.width as f32;
    // The block is centred on the picture, so the first line's centre is half a
    // step above the middle for every line after the first.
    let first = size.height as f32 / 2.0 - step * (lines.len().saturating_sub(1) as f32) / 2.0;
    // A margin, so text set against an edge is not against the very edge of the
    // composition — which on a wall is usually a pixel behind a bezel.
    let margin = cap / 2.0;

    for (index, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let bytes = line.as_bytes();
        let run = text::line_width(bytes.len(), pen.tracking) * cap;
        let centre_x = match pen.align {
            Align::Left => margin + run / 2.0,
            Align::Centre => width / 2.0,
            Align::Right => width - margin - run / 2.0,
        };
        text::write_centred(
            pixels,
            size,
            bytes,
            &Pen {
                cap_px: cap,
                weight: pen.weight,
                tracking: pen.tracking,
                colour: to_bytes(pen.colour),
                // The one caller that wants the box: these are an operator's
                // own words, so a character this font has not got is theirs to
                // see rather than ours to hide.
                missing: text::Missing::Box,
            },
            (centre_x, first + step * index as f32),
        );
    }
}

/// Returns a text parameter's value, or the manifest's default.
fn text_of(manifest: &GeneratorManifest, values: &ParameterMap, key: &str) -> String {
    match manifest
        .value(values, key)
        .filter(|value| matches!(value, ParameterValue::Text(_)))
        .or_else(|| declared(manifest, key))
    {
        Some(ParameterValue::Text(words)) => words,
        _ => String::new(),
    }
}

/// Returns a choice parameter's identifier, or the manifest's default.
///
/// The empty string when a show holds something that is not a choice at all,
/// which no show this build saved can — and which every reader of one of
/// these turns into the published first entry rather than into a panic.
fn choice_of(manifest: &GeneratorManifest, values: &ParameterMap, key: &str) -> String {
    match manifest
        .value(values, key)
        .filter(|value| matches!(value, ParameterValue::Choice(_)))
        .or_else(|| declared(manifest, key))
    {
        Some(ParameterValue::Choice(id)) => id,
        _ => String::new(),
    }
}

/// Returns a number parameter, or the manifest's default.
fn number_of(manifest: &GeneratorManifest, values: &ParameterMap, key: &str) -> f64 {
    as_number(manifest.value(values, key))
        .or_else(|| as_number(declared(manifest, key)))
        .unwrap_or(0.0)
}

/// Reads a value as a number, whichever numeric form it is stored in.
fn as_number(value: Option<ParameterValue>) -> Option<f64> {
    match value {
        Some(ParameterValue::Float(number)) => Some(number),
        Some(ParameterValue::Int(number)) => Some(number as f64),
        _ => None,
    }
}

/// Returns a toggle parameter, or the manifest's default.
fn flag_of(manifest: &GeneratorManifest, values: &ParameterMap, key: &str) -> bool {
    matches!(
        manifest
            .value(values, key)
            .filter(|value| matches!(value, ParameterValue::Bool(_)))
            .or_else(|| declared(manifest, key)),
        Some(ParameterValue::Bool(true))
    )
}

/// Converts linear RGBA to the eight-bit values a texture holds.
///
/// `+ 0.5` and a truncating cast rather than `round`, which is a call into
/// libm: on the owner's machine on 2026-10-14, `roundf` was **a fifth of the
/// render thread** -- four calls a pixel, two million pixels, sixty times a
/// second. For a value already clamped into `0..=1` the two agree exactly,
/// because rounding half away from zero and rounding half up are the same
/// thing when there is no negative side.
fn to_bytes(colour: [f32; 4]) -> [u8; 4] {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [
        channel(colour[0]),
        channel(colour[1]),
        channel(colour[2]),
        channel(colour[3]),
    ]
}

/// The eight bars, left to right, as a commissioning pattern has them.
const BARS: [[u8; 3]; 8] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
    [0, 0, 0],
];

/// Draws the commissioning pattern.
///
/// Three things, each for a different question an operator is asking. The grid
/// finds missing panels and scaling errors — a wall that is scaling will show
/// uneven lines. The bars check colour and order. The cross finds the centre,
/// which is what an alignment is measured from.
fn draw_test_pattern(pixels: &mut [u8], width: usize, height: usize, grid: u32, bars: bool) {
    let grid = (grid as usize).max(2);
    let bar_height = if bars { height / 6 } else { 0 };

    // The edge and the cross are drawn in proportion to the canvas rather than
    // one pixel wide. A single pixel on a 1920-wide wall disappears in the
    // console's preview, which is where somebody checks it from before walking
    // over to look — a line that only exists at full size is a line that is not
    // there when it is needed.
    let stroke = (width.max(height) / 240).max(1);

    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;

            let on_grid = x % grid == 0 || y % grid == 0;
            let on_edge = x < stroke || y < stroke || x + stroke >= width || y + stroke >= height;
            let on_cross = x.abs_diff(width / 2) <= stroke || y.abs_diff(height / 2) <= stroke;

            let rgb = if y < bar_height {
                BARS[(x * BARS.len() / width.max(1)).min(BARS.len() - 1)]
            } else if on_edge || on_cross {
                // The edge and the centre in red: both are what somebody is
                // looking for when they are lining a wall up, and neither
                // should be mistaken for the grid.
                [255, 0, 0]
            } else if on_grid {
                [128, 128, 128]
            } else {
                [16, 16, 16]
            };

            pixels[offset] = rgb[0];
            pixels[offset + 1] = rgb[1];
            pixels[offset + 2] = rgb[2];
            pixels[offset + 3] = 255;
        }
    }
}

/// Draws a linear gradient between two colours.
///
/// The angle is clockwise from left-to-right, the same direction everything else
/// in this product turns, so zero puts the first colour at the left edge and
/// ninety puts it at the top.
///
/// The ramp is normalised across the span of the canvas's own pixel centres
/// along that direction, so the furthest pixels each way land exactly on the two
/// declared colours whatever the angle and whatever the composition's shape. A
/// gradient whose corners fell short of its own end colour would be an operator
/// setting a colour and never seeing it on the wall.
///
/// The mix happens in the space the colours are declared in, so either end of
/// the ramp is byte-for-byte what [`Kind::SolidColour`] would have drawn from
/// the same value — the two generators agree where they overlap.
fn draw_gradient(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    from: [f32; 4],
    to: [f32; 4],
    angle_deg: f32,
) {
    // A console or an import can carry a nonsense angle into a slot. Falling
    // back to the published default is better than a canvas of NaN, which would
    // fill the wall with whatever `as u8` happens to produce.
    let angle = if angle_deg.is_finite() {
        angle_deg
    } else {
        90.0
    };

    // Image space here: `y` grows downward, as it does in the pixel buffer, so a
    // clockwise turn on the wall is the ordinary rotation matrix.
    let (sin, cos) = angle.to_radians().sin_cos();
    let half_width = width as f32 / 2.0;
    let half_height = height as f32 / 2.0;

    // How far the outermost pixel centres reach along the gradient's direction.
    // Zero only for a canvas one pixel wide across that direction, which has no
    // ramp in it at all: every pixel is then the midpoint of the two colours,
    // which is the honest answer rather than a division by nothing.
    let span = match ((width - 1) as f32 * cos).abs() + ((height - 1) as f32 * sin).abs() {
        span if span > 0.0 => span,
        _ => 1.0,
    };

    for y in 0..height {
        for x in 0..width {
            let along = (x as f32 + 0.5 - half_width) * cos + (y as f32 + 0.5 - half_height) * sin;
            let position = (along / span + 0.5).clamp(0.0, 1.0);

            let mixed = [
                from[0] + (to[0] - from[0]) * position,
                from[1] + (to[1] - from[1]) * position,
                from[2] + (to[2] - from[2]) * position,
                from[3] + (to[3] - from[3]) * position,
            ];
            let offset = (y * width + x) * 4;
            pixels[offset..offset + 4].copy_from_slice(&to_bytes(mixed));
        }
    }
}

/// Draws ruled lines at a spacing.
///
/// Lines start at the top-left corner and repeat, so a grid on a composition and
/// the same grid on a piece cut out of it line up: the spacing is measured from
/// the picture's own origin rather than from its centre, which would move every
/// line whenever the composition was resized.
///
/// A line wider than the spacing fills the picture with the line colour. That is
/// what was asked for, so that is what is drawn — the manifest cannot express
/// one parameter's range in terms of another, and quietly capping it would mean
/// an operator dragging a slider and seeing it stop.
fn draw_grid(
    pixels: &mut [u8],
    width: usize,
    spacing: u32,
    line: u32,
    line_colour: [f32; 4],
    background: [f32; 4],
) {
    // The manifest's range starts at four, so this only bites on a show from
    // somewhere else. A spacing of zero would divide by nothing.
    let spacing = (spacing as usize).max(1);
    let line = (line as usize).max(1);

    let line_rgba = to_bytes(line_colour);
    let background_rgba = to_bytes(background);

    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = index % width;
        let y = index / width;
        let on_line = x % spacing < line || y % spacing < line;
        *pixel = if on_line { line_rgba } else { background_rgba };
    }
}

/// Draws one shape, centred, over a background.
///
/// # Why the edge is softened
///
/// Deciding for each pixel whether it is inside the shape or outside it puts a
/// staircase on every edge that is not square to the pixel grid, and an LED wall
/// is the least forgiving display there is for one: the pixels are large, the
/// audience is close, and a stepped circle is the thing people notice about a
/// show that was not meant to be noticed at all. So each pixel is asked *how
/// much* of it the shape covers instead, which is a subtraction and a clamp more
/// than the test it replaces and is the same softening the lettering in
/// [`crate::media::text`] already does.
///
/// The coverage is estimated from the distance to the boundary rather than
/// integrated over the pixel: a pixel half a pixel inside is covered, one half a
/// pixel outside is not, and the band between them is the ramp. At the scale a
/// wall is looked at, the two are indistinguishable.
///
/// # Only the pixels the shape can reach
///
/// The background is written across the picture and then the shape is drawn into
/// the upright box the turned shape fits in, so a small shape on a large
/// composition costs the background fill and its own area — not a rotation and
/// three distances for every pixel of the wall (house rule §5).
fn draw_shape(
    pixels: &mut [u8],
    size: SizePx,
    figure: Figure,
    extent: (f32, f32),
    rotation_deg: f32,
    colour: [f32; 4],
    background: [f32; 4],
) {
    let background_rgba = to_bytes(background);
    for pixel in pixels.as_chunks_mut::<4>().0 {
        *pixel = background_rgba;
    }

    // Half extents: everything below measures from the centre outwards. A size
    // that is not a finite number above zero encloses nothing, so the background
    // is the whole of the answer — and asking an ellipse how far a point is from
    // a boundary of no radius would be a division by nothing. Anything but a
    // finite size is only reachable from a show written against another
    // build, because this one refuses it on save.
    let (half_width, half_height) = (extent.0 / 2.0, extent.1 / 2.0);
    if !(half_width.is_finite() && half_height.is_finite() && half_width > 0.0 && half_height > 0.0)
    {
        return;
    }

    // A console or an import can carry a nonsense angle into a slot. Unturned is
    // what the generator was published with, and better than a canvas of NaN.
    let rotation = if rotation_deg.is_finite() {
        rotation_deg
    } else {
        0.0
    };

    // Image space: `y` grows downward, as it does in the pixel buffer, so a
    // clockwise turn on the wall is the ordinary rotation matrix — the same
    // direction the gradient's angle and a layer transform turn.
    let (sin, cos) = rotation.to_radians().sin_cos();

    let width = size.width as usize;
    let height = size.height as usize;
    let centre_x = size.width as f32 / 2.0;
    let centre_y = size.height as f32 / 2.0;

    // The upright box the turned shape fits in, and one pixel more each way for
    // the coverage ramp. Held inside the picture before it is made into an
    // index, so a shape far larger than the composition is a full canvas rather
    // than a number no index can hold.
    let reach_x = (half_width * cos).abs() + (half_height * sin).abs() + 1.0;
    let reach_y = (half_width * sin).abs() + (half_height * cos).abs() + 1.0;
    let left = (centre_x - reach_x).clamp(0.0, width as f32) as usize;
    let right = (centre_x + reach_x).clamp(0.0, width as f32).ceil() as usize;
    let top = (centre_y - reach_y).clamp(0.0, height as f32) as usize;
    let bottom = (centre_y + reach_y).clamp(0.0, height as f32).ceil() as usize;

    for y in top..bottom {
        for x in left..right {
            let across = x as f32 + 0.5 - centre_x;
            let down = y as f32 + 0.5 - centre_y;

            // The pixel is turned back into the shape's frame rather than the
            // shape being turned into the picture's: one rotation either way,
            // and in the shape's own frame every boundary below is arithmetic.
            let local_x = across * cos + down * sin;
            let local_y = down * cos - across * sin;

            let coverage =
                (0.5 - figure.distance(local_x, local_y, half_width, half_height)).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }

            let at = (y * width + x) * 4;
            pixels[at..at + 4].copy_from_slice(&to_bytes(over(background, colour, coverage)));
        }
    }
}

/// Mixes a shape's colour over a background by how much of the pixel it covers.
///
/// Each colour is scaled by its own alpha for the mix and the result is taken
/// back out of it afterwards, because what a generator hands over is straight
/// alpha and never premultiplied (see [`DecodedImage`]).
///
/// Mixing the four channels as they are written would be wrong in exactly the
/// case the shape exists for. Over a transparent background it would drag a
/// half-covered edge pixel towards the background's *colour* as well as towards
/// its transparency, which after the shader has premultiplied it is a dark rim
/// around everything an operator meant to float over the layer below. Where both
/// colours are opaque this is the ordinary mix, so a shape on a solid bed is
/// untouched by the correction.
fn over(background: [f32; 4], shape: [f32; 4], coverage: f32) -> [f32; 4] {
    let alpha = mix(background[3], shape[3], coverage);
    if alpha <= 0.0 {
        // Nothing of either colour is visible, so which colour it is cannot be
        // seen. Mixing them is as good an answer as any and a stable one.
        return [
            mix(background[0], shape[0], coverage),
            mix(background[1], shape[1], coverage),
            mix(background[2], shape[2], coverage),
            0.0,
        ];
    }

    let channel = |index: usize| {
        mix(
            background[index] * background[3],
            shape[index] * shape[3],
            coverage,
        ) / alpha
    };
    [channel(0), channel(1), channel(2), alpha]
}

/// How far a point is from the boundary of an upright ellipse at the origin.
///
/// An estimate, and deliberately. The exact distance to an ellipse has no closed
/// form and is found by iterating, which would be several roots per pixel for a
/// number that is only ever used within half a pixel of the boundary. This is
/// the first-order one — how far the ellipse's own equation is from zero,
/// divided by how fast it is changing there — and inside that half-pixel band it
/// is accurate to a fraction of what a coverage ramp can show.
fn ellipse_distance(x: f32, y: f32, half_width: f32, half_height: f32) -> f32 {
    let across = x / half_width;
    let down = y / half_height;
    let equation = across * across + down * down - 1.0;
    let slope = 2.0 * (across / half_width).hypot(down / half_height);

    if slope > 0.0 {
        equation / slope
    } else {
        // The exact centre, where the equation has no slope at all. The nearest
        // boundary is the shorter semi-axis away.
        -half_width.min(half_height)
    }
}

/// How far a point is from the boundary of an upright rectangle at the origin.
///
/// Exact in both directions: outside, the distance to the nearest point of the
/// rectangle; inside, how far away the nearer pair of sides is. Both matter,
/// because a corner softened by the wrong number is a corner that reads as a
/// bevel.
fn rectangle_distance(x: f32, y: f32, half_width: f32, half_height: f32) -> f32 {
    let across = x.abs() - half_width;
    let down = y.abs() - half_height;
    let outside = across.max(0.0).hypot(down.max(0.0));
    let inside = across.max(down).min(0.0);
    outside + inside
}

/// How far a point is from the boundary of an upright triangle at the origin.
///
/// Isosceles, apex upwards, filling the size it was given — which is the wedge
/// and the arrowhead a show uses, and what an operator would have to build out
/// of anything more general.
///
/// The distance is taken to each of the three edges and the sign from which side
/// of them the point is on, rather than from the three edge *lines*: taking the
/// nearer line would round the corners off by up to a pixel, and the corner is
/// the whole of what makes a triangle read as one.
fn triangle_distance(x: f32, y: f32, half_width: f32, half_height: f32) -> f32 {
    let apex = (0.0, -half_height);
    let right = (half_width, half_height);
    let left = (-half_width, half_height);
    let point = (x, y);

    let mut nearest = f32::INFINITY;
    let mut inside = true;
    for (from, to) in [(apex, right), (right, left), (left, apex)] {
        nearest = nearest.min(crate::media::geometry::distance_to_segment(point, from, to));

        // The three edges are wound the same way round, so a point inside is on
        // the same side of all three of them.
        let along = (to.0 - from.0, to.1 - from.1);
        let offset = (point.0 - from.0, point.1 - from.1);
        inside &= along.0 * offset.1 - along.1 * offset.0 >= 0.0;
    }

    if inside {
        -nearest
    } else {
        nearest
    }
}

/// Draws value noise.
///
/// Value noise rather than white noise: a random value per cell, smoothly
/// interpolated between cells, which is the difference between a texture and
/// television static. Static at any scale looks the same on a wall — which is to
/// say it looks like a fault.
///
/// `amount` fades towards mid grey rather than towards black, so taking the
/// texture out of a bed under a key does not also take the light out of it.
/// `first_row` is where this band starts in the picture, so a band drawn by a
/// thread of its own lands on exactly the rows it would have had alone.
///
/// The field's five settings travel as one value rather than five arguments,
/// which is also what keeps a band's call to this honest: a thread is handed
/// the field and its own rows, and nothing else.
fn draw_noise(pixels: &mut [u8], width: usize, first_row: usize, noise: NoiseField) {
    let NoiseField {
        scale,
        coloured,
        amount,
        tick,
        seed,
    } = noise;
    let field = noise_field(seed, tick);
    let scale = (scale as f32).max(1.0);
    let amount = if amount.is_finite() {
        amount.clamp(0.0, 1.0)
    } else {
        1.0
    };

    // **The four corners belong to the cell, not to the pixel.** `value_noise`
    // hashes all four of them for every pixel it is asked about, and a cell
    // covers `scale x scale` pixels -- so at the default scale the same four
    // numbers were hashed sixty-four times over. Each hash is three 64-bit
    // multiplies, and a coloured field needs four hashes, three fields deep.
    //
    // Measured on the owner's machine on 2026-10-14: this generator alone took
    // the wall to **9 fps**, with `draw_kind` and `roundf` together two thirds
    // of the render thread. The arithmetic below is the same arithmetic in the
    // same order; what changes is how often the hashing happens.
    //
    // The grey field is also no longer drawn when colouring. It was computed
    // for every pixel and then thrown away, which is a quarter of the work of a
    // coloured field for nothing.
    let fade = |t: f32| t * t * (3.0 - 2.0 * t);
    let fields: [u64; 3] = if coloured {
        [
            field.wrapping_add(1),
            field.wrapping_add(2),
            field.wrapping_add(3),
        ]
    } else {
        [field, field, field]
    };
    let depth = if coloured { 3 } else { 1 };

    for (row, line) in pixels.as_chunks_mut::<4>().0.chunks_mut(width).enumerate() {
        let y = (first_row + row) as f32 / scale;
        let cell_y = y.floor();
        let top = cell_y as i64;
        let fraction_y = fade(y - cell_y);

        // The cell this row is walking through, and its corners. `left` only
        // ever grows along a row, so one comparison a pixel finds the edge.
        let mut held = i64::MIN;
        let mut corners = [[0.0_f32; 4]; 3];

        for (column, pixel) in line.iter_mut().enumerate() {
            let x = column as f32 / scale;
            let cell_x = x.floor();
            let left = cell_x as i64;
            if left != held {
                for (corner, channel) in corners.iter_mut().zip(fields).take(depth) {
                    *corner = [
                        lattice(left, top, channel),
                        lattice(left + 1, top, channel),
                        lattice(left, top + 1, channel),
                        lattice(left + 1, top + 1, channel),
                    ];
                }
                held = left;
            }

            let fraction_x = fade(x - cell_x);
            let mut rgb = [0.0_f32; 3];
            for (channel, corner) in rgb.iter_mut().zip(&corners).take(depth) {
                let value = mix(
                    mix(corner[0], corner[1], fraction_x),
                    mix(corner[2], corner[3], fraction_x),
                    fraction_y,
                );
                *channel = 0.5 + (value - 0.5) * amount;
            }
            if !coloured {
                rgb = [rgb[0], rgb[0], rgb[0]];
            }

            *pixel = to_bytes([rgb[0], rgb[1], rgb[2], 1.0]);
        }
    }
}

/// Returns the noise value at a point, in `0.0..=1.0`.
///
/// A random value at each lattice point, interpolated with a smoothstep so the
/// cells blend into one another instead of showing as squares. `channel`
/// selects one of several independent fields, which is how coloured noise gets
/// three that do not move together.
///
/// **Only the test uses this now.** `draw_noise` asked it per pixel, and so
/// hashed each cell's four corners once for every pixel inside that cell; it
/// walks the cells itself since 2026-10-14. This is kept as the thing that has
/// to agree, because an optimisation nobody can check against what it replaced
/// is a rewrite.
#[cfg(test)]
fn value_noise(x: f32, y: f32, channel: u64) -> f32 {
    let cell_x = x.floor();
    let cell_y = y.floor();
    let (left, top) = (cell_x as i64, cell_y as i64);

    // Smoothstep: zero gradient at both ends, so neighbouring cells meet without
    // the visible creases linear interpolation leaves.
    let fade = |t: f32| t * t * (3.0 - 2.0 * t);
    let fraction_x = fade(x - cell_x);
    let fraction_y = fade(y - cell_y);

    let top_edge = mix(
        lattice(left, top, channel),
        lattice(left + 1, top, channel),
        fraction_x,
    );
    let bottom_edge = mix(
        lattice(left, top + 1, channel),
        lattice(left + 1, top + 1, channel),
        fraction_x,
    );
    mix(top_edge, bottom_edge, fraction_y)
}

/// Which four noise fields a tick uses.
///
/// Four per frame, so the three colour channels keep the separation they
/// always had and the next frame starts past all of them.
///
/// **In one place because two things read it.** The processor draws noise with
/// this, and the card is handed the answer: the shader has no 64-bit multiply
/// to work it out with, and it is one number a frame rather than one a pixel.
/// If these two ever disagreed, the same show would be a different picture
/// depending on which drew it.
const fn noise_field(seed: u64, tick: u32) -> u64 {
    seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((tick as u64).wrapping_mul(4))
}

/// Returns the value at one lattice point, in `0.0..=1.0`.
///
/// A hash rather than a stored table: a table large enough not to repeat across
/// a wall would be a megabyte to carry, and a hash is the same picture every
/// time the show is opened without carrying anything at all.
fn lattice(x: i64, y: i64, channel: u64) -> f32 {
    let mut value = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ channel.wrapping_mul(0x1656_67B1_9E37_79F9);
    value ^= value >> 33;
    value = value.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    value ^= value >> 33;

    // The top 24 bits, which is more resolution than eight-bit output can show.
    (value >> 40) as f32 / 16_777_216.0
}

/// Linear interpolation between two values.
fn mix(from: f32, to: f32, position: f32) -> f32 {
    from + (to - from) * position
}

/// The longest line either of the timepieces writes: `12:34:56 PM`.
const MAX_WRITTEN: usize = 12;

/// A short line of ASCII, held without allocating.
///
/// Twelve bytes on the stack rather than a `String`, because this is built on
/// the render thread once a second and a formatting machine that allocates
/// belongs nowhere near it (house rule §5).
struct Written {
    bytes: [u8; MAX_WRITTEN],
    len: usize,
}

impl Written {
    /// An empty line.
    const fn new() -> Self {
        Self {
            bytes: [b' '; MAX_WRITTEN],
            len: 0,
        }
    }

    /// Adds one character, or nothing if the line is already full.
    ///
    /// Silently full rather than panicking: every caller here writes a line
    /// whose length it knows, so a truncation would be a fault in this file, and
    /// a partly-written clock on a wall says so more usefully than a renderer
    /// that stopped.
    fn push(&mut self, byte: u8) {
        if self.len < MAX_WRITTEN {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }

    /// Adds a two-digit field, padded in the tens column with `pad`.
    ///
    /// Always two columns, whatever the number, because the font is monospaced
    /// and the whole value of that is digits that do not move sideways when the
    /// hour goes from nine to ten.
    fn push_two(&mut self, value: u32, pad: u8) {
        let tens = (value / 10) % 10;
        self.push(if value < 10 { pad } else { b'0' + tens as u8 });
        self.push(b'0' + (value % 10) as u8);
    }

    /// What was written.
    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Which second of the local day a wall-clock time is.
///
/// The offset is the venue's, from the manifest, because nothing in this
/// workspace carries a time-zone database and a clock that was an hour out twice
/// a year would be worse than one nobody had to think about.
fn second_of_day(wall: SystemTime, offset_minutes: i32) -> u32 {
    // A machine whose clock reads before 1970 has not had its time set at all.
    // Midnight is as good an answer as any, and better than refusing to draw.
    let since_epoch = wall
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0);

    let local = since_epoch
        .saturating_add(i64::from(offset_minutes) * 60)
        .rem_euclid(i64::from(SECONDS_IN_A_DAY));
    u32::try_from(local).unwrap_or(0)
}

/// Writes the time of day at one second (or minute) of the day.
fn clock_text(tick: u32, hour_24: bool, show_seconds: bool) -> Written {
    let (hour, minute, second) = if show_seconds {
        (tick / 3_600 % 24, tick / 60 % 60, Some(tick % 60))
    } else {
        (tick / 60 % 24, tick % 60, None)
    };

    let mut written = Written::new();
    if hour_24 {
        written.push_two(hour, b'0');
    } else {
        // Twelve rather than zero, and padded with a space rather than a zero,
        // because that is how a clock face is written and how a crew reads one.
        let on_the_dial = if hour % 12 == 0 { 12 } else { hour % 12 };
        written.push_two(on_the_dial, b' ');
    }

    written.push(b':');
    written.push_two(minute, b'0');
    if let Some(second) = second {
        written.push(b':');
        written.push_two(second, b'0');
    }

    if !hour_24 {
        written.push(b' ');
        written.push(if hour < 12 { b'A' } else { b'P' });
        written.push(b'M');
    }

    written
}

/// Writes how long is left at one second of a countdown.
///
/// The sign has a column of its own whether or not there is anything in it, so
/// the digits do not jump sideways the moment a run goes over. Hours are in the
/// line or out of it for the whole run, decided by the duration rather than by
/// where the count has got to, for the same reason.
fn countdown_text(duration_s: u32, count_past_zero: bool, tick: u32) -> Written {
    let duration = duration_s.clamp(1, MAX_COUNTDOWN_SECONDS);
    let with_hours = duration >= COUNTDOWN_HOURS_FROM;

    // Clamped here as well as in the length, so a caller asking for a tick past
    // the end gets the last second the run can say rather than a number that has
    // wrapped.
    let tick = tick.min(countdown_frames(duration_s, count_past_zero).saturating_sub(1));
    let remaining = i64::from(duration) - i64::from(tick);
    let left = u32::try_from(remaining.unsigned_abs()).unwrap_or(MAX_COUNTDOWN_SECONDS);

    let mut written = Written::new();
    written.push(if remaining < 0 { b'-' } else { b' ' });
    if with_hours {
        written.push_two(left / 3_600, b'0');
        written.push(b':');
        written.push_two(left / 60 % 60, b'0');
    } else {
        written.push_two(left / 60, b'0');
    }
    written.push(b':');
    written.push_two(left % 60, b'0');

    written
}

/// Fills the picture and writes one line across the middle of it.
///
/// Sized to the picture rather than to a number of pixels: these are what the
/// wall is for, so the lettering is as large as the shorter way across allows
/// with a margin — which is also what keeps a clock legible on a 1024x600 panel
/// and on a 4K wall without anybody setting a size.
fn draw_written(
    pixels: &mut [u8],
    size: SizePx,
    written: &[u8],
    colour: [f32; 4],
    background: [f32; 4],
) {
    let rgba = to_bytes(background);
    for pixel in pixels.as_chunks_mut::<4>().0 {
        *pixel = rgba;
    }

    let width = size.width as f32;
    let height = size.height as f32;
    let by_height = height * CAP_FRACTION;
    let by_width = width * LINE_FRACTION / text::line_width(written.len(), TRACKING);

    text::write_centred(
        pixels,
        size,
        written,
        &Pen {
            cap_px: by_height.min(by_width),
            weight: STROKE_WEIGHT,
            tracking: TRACKING,
            colour: to_bytes(colour),
            // A clock and a countdown set digits, a colon and a minus, all of
            // which this font has. A box here could only be a fault this
            // renderer invented.
            missing: text::Missing::Nothing,
        },
        (width / 2.0, height / 2.0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The noise, asked pixel by pixel, which is how it used to be drawn.
    ///
    /// `draw_noise` hashes a cell's four corners once and walks the pixels
    /// inside it; before 2026-10-14 it hashed them again for every pixel, which
    /// cost the owner's wall three quarters of its frame rate. The arithmetic is
    /// meant to be the same arithmetic, so this says that in a test rather than
    /// in a comment.
    fn noise_per_pixel(
        width: usize,
        height: usize,
        scale: u32,
        coloured: bool,
        amount: f32,
        tick: u32,
        seed: u64,
    ) -> Vec<u8> {
        let field = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(u64::from(tick).wrapping_mul(4));
        let scale = (scale as f32).max(1.0);
        let amount = if amount.is_finite() {
            amount.clamp(0.0, 1.0)
        } else {
            1.0
        };

        let mut pixels = vec![0_u8; width * height * 4];
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = (index % width) as f32 / scale;
            let y = (index / width) as f32 / scale;

            let grey = 0.5 + (value_noise(x, y, field) - 0.5) * amount;
            let rgb = if coloured {
                [
                    0.5 + (value_noise(x, y, field.wrapping_add(1)) - 0.5) * amount,
                    0.5 + (value_noise(x, y, field.wrapping_add(2)) - 0.5) * amount,
                    0.5 + (value_noise(x, y, field.wrapping_add(3)) - 0.5) * amount,
                ]
            } else {
                [grey, grey, grey]
            };

            *pixel = to_bytes([rgb[0], rgb[1], rgb[2], 1.0]);
        }
        pixels
    }

    #[test]
    fn the_quicker_noise_draws_the_same_picture() {
        // Sizes that are not multiples of the scale on purpose: the last cell of
        // a row is a partial one, and that is where a loop walking cells rather
        // than pixels goes wrong.
        for (width, height) in [(64_usize, 48_usize), (31, 17)] {
            for scale in [1_u32, 3, 8, 64] {
                for coloured in [false, true] {
                    for amount in [1.0_f32, 0.35] {
                        let mut quick = vec![0_u8; width * height * 4];
                        let field = NoiseField {
                            scale,
                            coloured,
                            amount,
                            tick: 7,
                            seed: 0xABCD,
                        };
                        draw_noise(&mut quick, width, 0, field);
                        let slow =
                            noise_per_pixel(width, height, scale, coloured, amount, 7, 0xABCD);
                        assert_eq!(
                            quick, slow,
                            "{width}x{height}, scale {scale}, coloured {coloured}, amount {amount}"
                        );

                        // And the same picture again when the rows are shared
                        // out between threads. A band draws the rows it was
                        // given and nothing else, so how many bands there were
                        // must not be visible in the result — if it is, the
                        // wall shows seams where the threads met.
                        let mut split = vec![0_u8; width * height * 4];
                        for count in [2_usize, 3, 7, height + 4] {
                            split.fill(0);
                            crate::media::bands::in_rows(
                                &mut split,
                                width,
                                count,
                                |band, first_row| draw_noise(band, width, first_row, field),
                            );
                            assert_eq!(
                                split, quick,
                                "{width}x{height} in {count} bands, scale {scale}, \
                                 coloured {coloured}, amount {amount}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_quicker_conversion_rounds_the_same_way() {
        // `round` is a call into libm and was a fifth of the render thread.
        // What replaced it has to agree with it everywhere, not nearly
        // everywhere: a channel one off is a visible band on a wall.
        for step in 0..=2_000_u32 {
            let value = step as f32 / 1_000.0 - 0.5;
            let was = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            assert_eq!(to_bytes([value, 0.0, 0.0, 1.0])[0], was, "at {value}");
        }
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0] {
            let was = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            assert_eq!(to_bytes([value, 0.0, 0.0, 1.0])[0], was, "at {value}");
        }
    }

    fn size() -> SizePx {
        SizePx::new(64, 64)
    }

    fn pixel(image: &DecodedImage, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * image.size().width + x) * 4) as usize;
        let pixels = image.pixels();
        [
            pixels[offset],
            pixels[offset + 1],
            pixels[offset + 2],
            pixels[offset + 3],
        ]
    }

    #[test]
    fn a_solid_colour_is_the_colour_the_show_asked_for() {
        let parameters = ParameterMap::from([(
            "colour".to_owned(),
            ParameterValue::Color([1.0, 0.0, 0.5, 1.0]),
        )]);
        let mut source =
            GeneratorSource::new("ovs.solid_colour", 1, &parameters, size()).expect("published");

        let frame = source.frame(0).expect("draws");
        assert_eq!(pixel(&frame.image, 10, 10), [255, 0, 128, 255]);
        assert_eq!(pixel(&frame.image, 63, 63), [255, 0, 128, 255]);
    }

    #[test]
    fn a_generator_with_no_parameters_set_draws_its_defaults() {
        // A show that adds a generator and sets nothing is a show that
        // wants what the generator was published with.
        let mut source = GeneratorSource::new("ovs.solid_colour", 1, &ParameterMap::new(), size())
            .expect("published");
        let frame = source.frame(0).expect("draws");
        assert_eq!(pixel(&frame.image, 1, 1), [255, 255, 255, 255]);
    }

    /// The six generators whose picture is a function of position alone.
    const STILLS: [&str; 5] = [
        "ovs.solid_colour",
        "ovs.test_pattern",
        "ovs.gradient",
        "ovs.grid",
        "ovs.shape",
    ];

    /// The ones whose picture is a function of time as well as position.
    const TIMEPIECES: [&str; 4] = ["ovs.clock", "ovs.countdown", "ovs.strobe", "ovs.noise"];

    #[test]
    fn a_still_generator_has_one_frame_and_no_timeline() {
        // Six of the eight do not change over time, so there is nothing for a
        // playhead to run along and no buffer to keep. Noise included: value
        // noise at a fixed scale is a function of position, and giving it a
        // timeline would cost a fill of the whole composition every frame for a
        // motion nothing asked for. The shape included for the same reason: its
        // rotation is an angle somebody set, not a speed.
        for type_id in STILLS {
            let source =
                GeneratorSource::new(type_id, 1, &ParameterMap::new(), size()).expect("published");
            assert_eq!(source.length().frame_count, 1, "{type_id}");
            assert_eq!(source.length().duration_ms, 0, "{type_id}");
            assert!(!source.changes_over_time(), "{type_id}");
        }
    }

    #[test]
    fn a_generator_that_changes_over_time_says_so_and_has_a_timeline() {
        // The distinction everything above this acts on: one upload when the
        // slot changes is enough for a still and is not enough for a clock.
        for type_id in TIMEPIECES {
            let source =
                GeneratorSource::new(type_id, 1, &ParameterMap::new(), size()).expect("published");
            assert!(source.changes_over_time(), "{type_id}");
            assert!(source.length().frame_count > 1, "{type_id}");
            assert!(source.length().duration_ms > 0, "{type_id}");
        }
    }

    /// What a strobe is showing at a moment, as its top-left pixel.
    fn strobe_at(source: &mut GeneratorSource, seconds: f64) -> [u8; 4] {
        let tick = source
            .tick_at(Moment {
                wall: UNIX_EPOCH,
                since_anchor: Duration::from_secs_f64(seconds),
            })
            .expect("a strobe knows which half of its cycle it is in");
        // Forgotten first: `redraw_at` answers `None` for a tick it has
        // already drawn, and this asks about moments out of order.
        source.forget_drawn();
        let image = source
            .redraw_at(tick, FrameRate::FPS_60)
            .expect("a picture");
        pixel(image, 0, 0)
    }

    fn strobe(hz: f64) -> GeneratorSource {
        GeneratorSource::new(
            "ovs.strobe",
            1,
            &ParameterMap::from([
                (
                    "colour".to_owned(),
                    ParameterValue::Color([1.0, 0.0, 0.0, 1.0]),
                ),
                ("hz".to_owned(), ParameterValue::Float(hz)),
            ]),
            size(),
        )
        .expect("published")
    }

    #[test]
    fn a_strobe_is_lit_for_half_of_each_cycle() {
        // Ten hertz: a cycle is a tenth of a second, lit for the first
        // twentieth of it and clear for the second.
        let mut source = strobe(10.0);

        assert_eq!(strobe_at(&mut source, 0.00), [255, 0, 0, 255], "lit");
        assert_eq!(strobe_at(&mut source, 0.04), [255, 0, 0, 255], "still lit");
        assert_eq!(strobe_at(&mut source, 0.06)[3], 0, "clear");
        assert_eq!(strobe_at(&mut source, 0.10), [255, 0, 0, 255], "lit again");
        assert_eq!(strobe_at(&mut source, 0.16)[3], 0, "and clear again");
    }

    #[test]
    fn the_dark_half_is_clear_rather_than_black() {
        // A strobe belongs over whatever is under it. Filling with black would
        // make it a blackout with a flash in it, and the colour is kept so the
        // layer does not flicker through a hue on its way out.
        let mut source = strobe(10.0);
        let dark = strobe_at(&mut source, 0.06);

        assert_eq!(dark[3], 0, "nothing of it is drawn");
        assert_eq!(
            [dark[0], dark[1], dark[2]],
            [255, 0, 0],
            "and it is still red"
        );
    }

    #[test]
    fn a_strobe_flashes_as_many_times_a_second_as_it_was_asked_to() {
        // Counted off the passes the render loop actually makes rather than
        // asserted about the arithmetic: a rate that was out by a factor of
        // two would satisfy every test that only looks at one moment.
        let source = strobe(10.0);

        let mut flashes = 0;
        let mut was_lit = false;
        for frame in 0..60 {
            let lit = source
                .tick_at(Moment {
                    wall: UNIX_EPOCH,
                    since_anchor: Duration::from_secs_f64(f64::from(frame) / 60.0),
                })
                .expect("a tick")
                .is_multiple_of(2);
            if lit && !was_lit {
                flashes += 1;
            }
            was_lit = lit;
        }

        assert_eq!(flashes, 10, "ten flashes in a second at ten hertz");
    }

    #[test]
    fn a_strobe_from_another_build_cannot_divide_by_its_rate() {
        // A show written against something else can name any number, and zero
        // would be a division by it. The published floor is the answer.
        let source = strobe(0.0);

        assert_eq!(
            source.length().duration_ms,
            2_000,
            "held at the slowest published rate"
        );
        assert!(source
            .tick_at(Moment {
                wall: UNIX_EPOCH,
                since_anchor: Duration::from_secs(1),
            })
            .is_some());
    }

    fn noise() -> GeneratorSource {
        GeneratorSource::new("ovs.noise", 1, &ParameterMap::new(), size()).expect("published")
    }

    #[test]
    fn noise_is_a_different_field_every_frame() {
        // The owner, 2026-10-12: "Ist aktuell Static, sollte aber wirklich
        // zufälliger Noise sein jedes Frame." A field that stood still was a
        // texture with a grain, which is not what the word means.
        let mut source = noise();

        let first = source
            .redraw_at(0, FrameRate::FPS_60)
            .expect("a picture")
            .pixels()
            .to_vec();
        let next = source
            .redraw_at(17, FrameRate::FPS_60)
            .expect("a picture")
            .pixels()
            .to_vec();

        assert_ne!(first, next, "one frame later and the same grain");
    }

    #[test]
    fn two_noise_layers_do_not_draw_the_same_field() {
        // The field is a function of position and time, and two layers share
        // both — so without something of their own they would be the same
        // picture twice, which is the one thing noise must never be.
        let mut one = noise();
        let mut other = noise();

        let at = |source: &mut GeneratorSource| {
            source
                .redraw_at(1_000, FrameRate::FPS_60)
                .expect("a picture")
                .pixels()
                .to_vec()
        };

        assert_ne!(at(&mut one), at(&mut other), "the same moment, one picture");
    }

    #[test]
    fn noise_runs_free_rather_than_from_when_the_layer_started() {
        // Nothing in noise restarts, so nothing about the layer reaches it.
        let source = noise();
        let at = |wall_seconds: u64, layer_seconds: u64| {
            source
                .tick_at(Moment {
                    wall: UNIX_EPOCH + Duration::from_secs(wall_seconds),
                    since_anchor: Duration::from_secs(layer_seconds),
                })
                .expect("a tick")
        };

        assert_eq!(at(7, 0), at(7, 600), "the layer's age does not reach it");
        assert_ne!(at(7, 0), at(8, 0), "and the clock does");
    }

    fn service_pattern() -> GeneratorSource {
        GeneratorSource::new(
            "ovs.test_pattern",
            2,
            &ParameterMap::new(),
            SizePx::new(192, 108),
        )
        .expect("published")
    }

    #[test]
    fn the_test_pattern_a_slot_offers_now_is_the_service_pattern() {
        // The owner, 2026-10-12: "den möchte ich da sehen und keinen
        // Generischen". Version one is still published and still draws its
        // grid, so a show saved with it opens unchanged; version two is what a
        // source list offers, and it is the picture an output shows.
        let still = GeneratorSource::new("ovs.test_pattern", 1, &ParameterMap::new(), size())
            .expect("published");
        assert!(!still.changes_over_time(), "version one is a still");

        let service = service_pattern();
        assert!(service.changes_over_time(), "version two moves");
        assert!(service.length().frame_count > 1);
        assert!(service.length().duration_ms > 0);
    }

    #[test]
    fn the_service_pattern_is_a_different_picture_at_a_different_moment() {
        // The whole reason it moves: a frozen output and a live one showing a
        // still look identical from across a room. A pattern that gave the
        // same pixels two seconds apart would be no better than the still it
        // replaced.
        let mut source = service_pattern();

        let first = source
            .redraw_at(0, FrameRate::FPS_60)
            .expect("a picture")
            .pixels()
            .to_vec();
        let later = source
            .redraw_at(2_000, FrameRate::FPS_60)
            .expect("a picture")
            .pixels()
            .to_vec();

        assert_ne!(first, later, "two seconds apart and the same pixels");
        assert!(
            first.iter().any(|byte| *byte > 0),
            "and it is a picture rather than a black field"
        );
    }

    #[test]
    fn the_caption_follows_the_rate_the_surface_is_running_at() {
        // The caption names the surface and its mode, which is the whole point
        // of putting one on: an engineer reads what the wall is doing off the
        // wall instead of taking somebody's word for it. A composition whose
        // rate somebody changes has to get a new caption rather than keep one
        // that has stopped being true.
        let mut source = service_pattern();

        let sixty = source
            .redraw_at(0, FrameRate::FPS_60)
            .expect("a picture")
            .pixels()
            .to_vec();
        let fifty = source
            .redraw_at(0, FrameRate::FPS_50)
            .expect("a picture")
            .pixels()
            .to_vec();

        assert_ne!(
            sixty, fifty,
            "the same moment, and the caption did not move"
        );
    }

    #[test]
    fn the_service_pattern_runs_free_rather_than_from_when_the_layer_started() {
        // The owner's rule for this one generator: "Das Testbild läuft immer
        // Freerun, es hat seine eigene Zeit." So the layer's own age must not
        // reach it — otherwise it restarts every time the generator is rebuilt
        // and two layers showing it fall out of step.
        let source = service_pattern();
        let at = |wall_seconds: u64, layer_seconds: u64| {
            source
                .tick_at(Moment {
                    wall: UNIX_EPOCH + Duration::from_secs(wall_seconds),
                    since_anchor: Duration::from_secs(layer_seconds),
                })
                .expect("a tick")
        };

        assert_eq!(
            at(100, 0),
            at(100, 9_999),
            "the same moment, two layers of different ages, one picture"
        );
        assert_eq!(at(100, 0), 100_000, "a millisecond a tick");
        assert_ne!(at(100, 0), at(102, 0), "and it moves with the clock");
    }

    #[test]
    fn a_clock_is_a_source_one_day_long_with_a_frame_a_second() {
        // How a clock fits a contract written for clips, without bending it: a
        // length, and a frame per index, where the index is the second of the
        // day.
        let source =
            GeneratorSource::new("ovs.clock", 1, &ParameterMap::new(), size()).expect("published");

        assert_eq!(source.length().frame_count, 86_400);
        assert_eq!(source.length().duration_ms, 86_400_000);
    }

    #[test]
    fn a_clock_without_seconds_has_a_frame_a_minute_rather_than_a_frame_a_second() {
        // The frame count is the granularity of the picture. Sixty indices that
        // all draw the same minute would be fifty-nine redraws of a composition
        // for a result identical to the last one.
        let parameters =
            ParameterMap::from([("show_seconds".to_owned(), ParameterValue::Bool(false))]);
        let source = GeneratorSource::new("ovs.clock", 1, &parameters, size()).expect("published");

        assert_eq!(source.length().frame_count, 1_440);
        assert_eq!(source.length().duration_ms, 86_400_000, "still a day long");
    }

    #[test]
    fn the_clock_at_one_second_is_not_the_clock_at_the_next() {
        // The whole difference between this and every generator before it. A
        // clock whose picture did not move would be a photograph of a clock,
        // which on a backstage monitor is worse than nothing.
        let mut source =
            GeneratorSource::new("ovs.clock", 1, &ParameterMap::new(), SizePx::new(256, 128))
                .expect("published");

        let noon = source.frame(43_200).expect("draws").image.pixels().to_vec();
        let after = source.frame(43_201).expect("draws");
        assert_ne!(noon.as_slice(), after.image.pixels());
    }

    #[test]
    fn the_same_second_is_the_same_picture_on_any_machine() {
        // `frame` reads no clock: the index decides the picture and nothing
        // else, which is what makes a clock testable at all.
        let draw = || {
            let mut source =
                GeneratorSource::new("ovs.clock", 1, &ParameterMap::new(), SizePx::new(128, 64))
                    .expect("published");
            source.frame(45_296).expect("draws").image.pixels().to_vec()
        };

        assert_eq!(draw(), draw());
    }

    #[test]
    fn a_clock_shows_the_second_of_the_day_the_wall_clock_is_on() {
        // Half past twelve in the afternoon, UTC, and the same instant in a
        // venue an hour east of it.
        let at = |offset: f64| {
            let parameters = ParameterMap::from([(
                "utc_offset_minutes".to_owned(),
                ParameterValue::Float(offset),
            )]);
            let source =
                GeneratorSource::new("ovs.clock", 1, &parameters, size()).expect("published");
            source
                .tick_at(Moment {
                    wall: UNIX_EPOCH + Duration::from_secs(45_296),
                    since_anchor: Duration::ZERO,
                })
                .expect("a clock knows which frame it is on")
        };

        assert_eq!(clock_text(at(0.0), true, true).as_bytes(), b"12:34:56");
        assert_eq!(clock_text(at(60.0), true, true).as_bytes(), b"13:34:56");
        assert_eq!(clock_text(at(-330.0), true, true).as_bytes(), b"07:04:56");
    }

    #[test]
    fn a_clock_rolls_over_at_midnight_rather_than_running_past_it() {
        // Ten seconds before midnight in a venue five hours west of UTC, which
        // is a different day from the one the machine's clock is on.
        let parameters = ParameterMap::from([(
            "utc_offset_minutes".to_owned(),
            ParameterValue::Float(-300.0),
        )]);
        let source = GeneratorSource::new("ovs.clock", 1, &parameters, size()).expect("published");

        let tick = source
            .tick_at(Moment {
                // 04:59:50 UTC, which is 23:59:50 the evening before.
                wall: UNIX_EPOCH + Duration::from_secs(17_990),
                since_anchor: Duration::ZERO,
            })
            .expect("a frame");
        assert_eq!(clock_text(tick, true, true).as_bytes(), b"23:59:50");
        assert!(tick < 86_400, "and inside the day it is a frame of");
    }

    #[test]
    fn a_machine_whose_clock_was_never_set_draws_midnight_rather_than_refusing() {
        // A server that came up without a network and before an RTC battery. A
        // wrong clock is visible and recoverable; a black layer during a show
        // is neither.
        let source =
            GeneratorSource::new("ovs.clock", 1, &ParameterMap::new(), size()).expect("published");

        let tick = source
            .tick_at(Moment {
                wall: UNIX_EPOCH - Duration::from_secs(86_400),
                since_anchor: Duration::ZERO,
            })
            .expect("a frame");
        assert_eq!(tick, 0);
    }

    #[test]
    fn a_twelve_hour_clock_says_which_half_of_the_day_it_is() {
        // Nine in the morning and nine at night are the same face and different
        // calls, so the dial has to carry the difference.
        assert_eq!(clock_text(32_400, false, true).as_bytes(), b" 9:00:00 AM");
        assert_eq!(clock_text(75_600, false, true).as_bytes(), b" 9:00:00 PM");
        assert_eq!(
            clock_text(30, false, true).as_bytes(),
            b"12:00:30 AM",
            "midnight is twelve, not zero"
        );
        assert_eq!(clock_text(43_200, false, true).as_bytes(), b"12:00:00 PM");
    }

    #[test]
    fn a_clock_without_seconds_is_written_without_them() {
        assert_eq!(clock_text(754, true, false).as_bytes(), b"12:34");
        assert_eq!(clock_text(754, false, false).as_bytes(), b"12:34 PM");
    }

    #[test]
    fn a_countdown_counts_down_from_its_duration_and_reaches_zero() {
        // Ten minutes, the published default: the number a chair is watching.
        let mut source = GeneratorSource::new("ovs.countdown", 1, &ParameterMap::new(), size())
            .expect("published");

        assert_eq!(countdown_text(600, false, 0).as_bytes(), b" 10:00");
        assert_eq!(countdown_text(600, false, 1).as_bytes(), b" 09:59");
        assert_eq!(countdown_text(600, false, 600).as_bytes(), b" 00:00");

        // Zero is the last frame the source holds, so nothing above it needs a
        // rule about what happens afterwards.
        assert_eq!(source.length().frame_count, 601);
        assert_eq!(source.frame(600).expect("draws").index, 600);
        assert_eq!(
            source.frame(5_000).expect("draws").index,
            600,
            "and it holds there"
        );
    }

    #[test]
    fn a_countdown_told_to_run_on_past_zero_runs_on_into_negative() {
        // The other instrument: not "your time is up" but "you are four minutes
        // over", which is what a chair needs in order to act.
        let parameters =
            ParameterMap::from([("count_past_zero".to_owned(), ParameterValue::Bool(true))]);
        let source =
            GeneratorSource::new("ovs.countdown", 1, &parameters, size()).expect("published");

        assert_eq!(countdown_text(600, true, 600).as_bytes(), b" 00:00");
        assert_eq!(countdown_text(600, true, 660).as_bytes(), b"-01:00");
        assert!(
            source.length().frame_count > 601,
            "the source has to be long enough to say it"
        );
    }

    #[test]
    fn what_a_countdown_does_at_zero_is_the_length_of_its_source() {
        // No rule anywhere about what happens at the end: stopping is a source
        // that ends there and running on is a source that continues, and both
        // then hold their last frame like everything else in this renderer.
        let holding = GeneratorSource::new(
            "ovs.countdown",
            1,
            &ParameterMap::from([("count_past_zero".to_owned(), ParameterValue::Bool(false))]),
            size(),
        )
        .expect("published");
        let running_on = GeneratorSource::new(
            "ovs.countdown",
            1,
            &ParameterMap::from([("count_past_zero".to_owned(), ParameterValue::Bool(true))]),
            size(),
        )
        .expect("published");

        assert_eq!(holding.length().frame_count, 601);
        assert_eq!(
            running_on.length().frame_count,
            601 + 3_599,
            "as far again as minutes and seconds can say"
        );
    }

    #[test]
    fn a_countdown_of_an_hour_or_more_is_written_with_hours_for_the_whole_run() {
        // Chosen from the duration rather than from where the count has got to,
        // so the digits do not change size when the last hour ticks away.
        assert_eq!(countdown_text(3_600, false, 0).as_bytes(), b" 01:00:00");
        assert_eq!(countdown_text(3_600, false, 3_000).as_bytes(), b" 00:10:00");
        assert_eq!(countdown_text(3_599, false, 0).as_bytes(), b" 59:59");
    }

    #[test]
    fn a_countdown_longer_than_it_can_be_written_is_clamped_to_the_longest_it_can() {
        // The manifest refuses this when a show is saved, so it is only
        // reachable from a show written against another build. A run that had
        // wrapped round to a small number would be a stage clock telling a
        // speaker the opposite of the truth.
        let parameters =
            ParameterMap::from([("duration_s".to_owned(), ParameterValue::Float(200_000.0))]);
        let mut source =
            GeneratorSource::new("ovs.countdown", 1, &parameters, size()).expect("published");

        assert_eq!(source.length().frame_count, 86_400, "a day, and no more");
        assert_eq!(countdown_text(200_000, false, 0).as_bytes(), b" 23:59:59");
        assert_eq!(source.frame(0).expect("draws").index, 0);
    }

    #[test]
    fn a_countdown_of_nothing_at_all_still_has_a_frame_to_show() {
        // Nothing above this can divide by a run of no length.
        let parameters =
            ParameterMap::from([("duration_s".to_owned(), ParameterValue::Float(0.0))]);
        let source =
            GeneratorSource::new("ovs.countdown", 1, &parameters, size()).expect("published");

        assert!(source.length().frame_count >= 2);
        assert_eq!(countdown_text(0, false, 0).as_bytes(), b" 00:01");
    }

    #[test]
    fn a_countdown_restarts_on_a_trigger_only_when_it_was_asked_to() {
        // A speaker's clock is reset between talks; one running against
        // something in the world must not be disturbed by somebody bringing it
        // up on a screen.
        let with = |from_trigger: bool| {
            let parameters = ParameterMap::from([(
                "from_trigger".to_owned(),
                ParameterValue::Bool(from_trigger),
            )]);
            GeneratorSource::new("ovs.countdown", 1, &parameters, size())
                .expect("published")
                .restarts_on_trigger()
        };

        assert!(with(true));
        assert!(!with(false));

        // Nothing else here is anchored to anything a trigger could move.
        for type_id in STILLS.into_iter().chain(["ovs.clock"]) {
            let source =
                GeneratorSource::new(type_id, 1, &ParameterMap::new(), size()).expect("published");
            assert!(!source.restarts_on_trigger(), "{type_id}");
        }
    }

    #[test]
    fn a_countdown_is_at_the_second_of_its_run_the_slot_has_been_up() {
        let source = GeneratorSource::new("ovs.countdown", 1, &ParameterMap::new(), size())
            .expect("published");
        let at = |seconds| {
            source
                .tick_at(Moment {
                    wall: UNIX_EPOCH,
                    since_anchor: Duration::from_secs(seconds),
                })
                .expect("a frame")
        };

        assert_eq!(at(0), 0);
        assert_eq!(at(90), 90);
        assert_eq!(at(10_000), 600, "and holds at the end of its run");
    }

    #[test]
    fn a_picture_is_redrawn_when_the_second_moves_and_not_otherwise() {
        // What stops a clock costing a composition sixty times a second: a pass
        // in which nothing has changed is a comparison and no upload.
        let mut source =
            GeneratorSource::new("ovs.clock", 1, &ParameterMap::new(), SizePx::new(128, 64))
                .expect("published");

        assert!(
            source.redraw_at(45_296, FrameRate::FPS_60).is_some(),
            "the first one is drawn"
        );
        assert!(
            source.redraw_at(45_296, FrameRate::FPS_60).is_none(),
            "and the same second is not drawn again"
        );
        assert!(
            source.redraw_at(45_297, FrameRate::FPS_60).is_some(),
            "the next one is"
        );
    }

    #[test]
    fn a_still_keeps_no_buffer_to_redraw_into() {
        // Up to two hundred megabytes at the sizes this will draw. A still is
        // uploaded once and never asked again, so holding one would be memory
        // kept for nothing.
        let mut solid = GeneratorSource::new("ovs.solid_colour", 1, &ParameterMap::new(), size())
            .expect("published");
        assert!(solid.redraw_at(0, FrameRate::FPS_60).is_none());
    }

    #[test]
    fn a_redrawn_picture_is_the_same_one_the_frame_contract_gives() {
        // Two ways to the same drawing — one that allocates and one that does
        // not — and a wall that differed between them would be a bug nobody
        // could see coming.
        let mut source = GeneratorSource::new(
            "ovs.countdown",
            1,
            &ParameterMap::new(),
            SizePx::new(96, 48),
        )
        .expect("published");

        let allocated = source.frame(123).expect("draws").image.pixels().to_vec();
        let in_place = source
            .redraw_at(123, FrameRate::FPS_60)
            .expect("draws")
            .pixels()
            .to_vec();
        assert_eq!(allocated, in_place);
    }

    #[test]
    fn the_colours_a_timepiece_was_given_are_the_ones_on_the_wall() {
        // A stage clock is often amber on black, because a white one throws
        // light onto the front of a speaker.
        let parameters = ParameterMap::from([
            (
                "text_colour".to_owned(),
                ParameterValue::Color([1.0, 0.5, 0.0, 1.0]),
            ),
            (
                "background_colour".to_owned(),
                ParameterValue::Color([0.0, 0.0, 0.25, 1.0]),
            ),
        ]);
        let mut source = GeneratorSource::new("ovs.clock", 1, &parameters, SizePx::new(256, 128))
            .expect("published");
        let frame = source.frame(45_296).expect("draws");

        let colours: std::collections::BTreeSet<[u8; 4]> = frame
            .image
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .collect();
        assert!(
            colours.contains(&[0, 0, 64, 255]),
            "the background the operator chose"
        );
        assert!(
            colours.contains(&[255, 128, 0, 255]),
            "and the lettering they chose"
        );
    }

    #[test]
    fn every_published_generator_can_actually_be_drawn() {
        // A manifest with no drawing behind it would be a generator a client
        // offers, a show saves and the wall refuses — a fault in this build
        // rather than in anybody's show.
        for manifest in &ovs_types::config::generators::PUBLISHED {
            let mut source = GeneratorSource::new(
                manifest.type_id,
                manifest.version,
                &ParameterMap::new(),
                size(),
            )
            .unwrap_or_else(|| panic!("{} is published but not drawn", manifest.type_id));
            let frame = source.frame(0).expect("draws");
            assert_eq!(
                frame.image.pixels().len(),
                64 * 64 * 4,
                "{}",
                manifest.type_id
            );
        }
    }

    #[test]
    fn every_generator_makes_a_picture_from_its_published_defaults_alone() {
        // An operator adds a generator and sets nothing. What appears has to be
        // something they recognise, which at minimum means it is not one flat
        // value across the whole canvas for anything but the solid colour.
        for type_id in [
            "ovs.test_pattern",
            "ovs.gradient",
            "ovs.grid",
            "ovs.shape",
            "ovs.noise",
            "ovs.clock",
            "ovs.countdown",
        ] {
            // Half a thousand across, because one of these draws a single object
            // at a published size rather than a repeating texture, and a canvas
            // smaller than that object would be a flat field for a reason that
            // has nothing to do with the defaults being wrong.
            let mut source =
                GeneratorSource::new(type_id, 1, &ParameterMap::new(), SizePx::new(512, 512))
                    .expect("published");
            let frame = source.frame(0).expect("draws");

            let distinct: std::collections::BTreeSet<[u8; 4]> = frame
                .image
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .copied()
                .collect();
            assert!(
                distinct.len() > 1,
                "{type_id} draws one flat colour from its own defaults"
            );
        }
    }

    #[test]
    fn a_test_pattern_has_an_edge_a_grid_and_a_centre() {
        // The three things somebody lining up a wall is looking for.
        let parameters = ParameterMap::from([
            ("grid_px".to_owned(), ParameterValue::Float(16.0)),
            ("bars".to_owned(), ParameterValue::Bool(false)),
        ]);
        let mut source =
            GeneratorSource::new("ovs.test_pattern", 1, &parameters, SizePx::new(64, 64))
                .expect("published");
        let frame = source.frame(0).expect("draws");

        assert_eq!(pixel(&frame.image, 0, 30), [255, 0, 0, 255], "the edge");
        assert_eq!(pixel(&frame.image, 32, 40), [255, 0, 0, 255], "the centre");
        assert_eq!(
            pixel(&frame.image, 63, 30),
            [255, 0, 0, 255],
            "and the far edge"
        );
        assert_eq!(
            pixel(&frame.image, 16, 20),
            [128, 128, 128, 255],
            "the grid"
        );
        assert_eq!(pixel(&frame.image, 20, 20), [16, 16, 16, 255], "the field");
    }

    #[test]
    fn the_edge_and_the_cross_are_thicker_on_a_bigger_canvas() {
        // A one-pixel line on a 1920-wide wall is invisible in a 240-wide
        // preview, which is where it is looked at first.
        let parameters = ParameterMap::from([("bars".to_owned(), ParameterValue::Bool(false))]);
        let mut source =
            GeneratorSource::new("ovs.test_pattern", 1, &parameters, SizePx::new(1920, 1080))
                .expect("published");
        let frame = source.frame(0).expect("draws");

        // Eight pixels either side of the centre line, at 1920 wide.
        for offset in 0..8 {
            assert_eq!(
                pixel(&frame.image, 960 + offset, 400),
                [255, 0, 0, 255],
                "the cross at +{offset}"
            );
            assert_eq!(
                pixel(&frame.image, offset, 400),
                [255, 0, 0, 255],
                "the edge"
            );
        }
        assert_ne!(
            pixel(&frame.image, 200, 400),
            [255, 0, 0, 255],
            "and not the whole picture"
        );
    }

    #[test]
    fn colour_bars_are_drawn_across_the_top_when_asked_for() {
        let parameters = ParameterMap::from([("bars".to_owned(), ParameterValue::Bool(true))]);
        let mut source =
            GeneratorSource::new("ovs.test_pattern", 1, &parameters, SizePx::new(64, 64))
                .expect("published");
        let frame = source.frame(0).expect("draws");

        // White at the left, black at the right, in the band across the top.
        assert_eq!(pixel(&frame.image, 1, 3), [255, 255, 255, 255]);
        assert_eq!(pixel(&frame.image, 62, 3), [0, 0, 0, 255]);
    }

    #[test]
    fn a_gradient_starts_at_one_colour_and_ends_at_the_other() {
        // An operator setting the two ends has to get exactly those two ends. A
        // ramp that fell short of its own end colour would be somebody choosing
        // a colour and never seeing it on the wall.
        let parameters = ParameterMap::from([
            (
                "colour_a".to_owned(),
                ParameterValue::Color([1.0, 0.0, 0.0, 1.0]),
            ),
            (
                "colour_b".to_owned(),
                ParameterValue::Color([0.0, 0.0, 1.0, 1.0]),
            ),
            ("angle_deg".to_owned(), ParameterValue::Float(90.0)),
        ]);
        let mut source = GeneratorSource::new("ovs.gradient", 1, &parameters, SizePx::new(64, 64))
            .expect("published");
        let frame = source.frame(0).expect("draws");

        assert_eq!(pixel(&frame.image, 32, 0), [255, 0, 0, 255], "the top");
        assert_eq!(pixel(&frame.image, 32, 63), [0, 0, 255, 255], "the bottom");
    }

    #[test]
    fn a_gradient_at_ninety_degrees_runs_down_and_at_zero_runs_right() {
        // The angle turns clockwise from left-to-right, the same direction a
        // layer transform turns. An operator who has learnt one has learnt both.
        let ends = |angle: f64| {
            let parameters = ParameterMap::from([
                (
                    "colour_a".to_owned(),
                    ParameterValue::Color([0.0, 0.0, 0.0, 1.0]),
                ),
                (
                    "colour_b".to_owned(),
                    ParameterValue::Color([1.0, 1.0, 1.0, 1.0]),
                ),
                ("angle_deg".to_owned(), ParameterValue::Float(angle)),
            ]);
            let mut source =
                GeneratorSource::new("ovs.gradient", 1, &parameters, SizePx::new(64, 64))
                    .expect("published");
            let frame = source.frame(0).expect("draws");
            (
                pixel(&frame.image, 0, 32)[0],
                pixel(&frame.image, 63, 32)[0],
                pixel(&frame.image, 32, 0)[0],
                pixel(&frame.image, 32, 63)[0],
            )
        };

        let (left, right, top, bottom) = ends(0.0);
        assert!(left < right, "at zero the ramp runs left to right");
        assert_eq!(top, bottom, "and not top to bottom");

        let (left, right, top, bottom) = ends(90.0);
        assert!(top < bottom, "at ninety the ramp runs top to bottom");
        assert_eq!(left, right, "and not left to right");
    }

    #[test]
    fn a_gradient_between_two_equal_colours_is_the_solid_colour_of_that_value() {
        // Where the two generators overlap they must agree, or an operator
        // fading one into the other sees a step.
        let colour = ParameterValue::Color([0.25, 0.5, 0.75, 1.0]);
        let mut gradient = GeneratorSource::new(
            "ovs.gradient",
            1,
            &ParameterMap::from([
                ("colour_a".to_owned(), colour.clone()),
                ("colour_b".to_owned(), colour.clone()),
            ]),
            size(),
        )
        .expect("published");
        let mut solid = GeneratorSource::new(
            "ovs.solid_colour",
            1,
            &ParameterMap::from([("colour".to_owned(), colour)]),
            size(),
        )
        .expect("published");

        assert_eq!(
            gradient.frame(0).expect("draws").image.pixels(),
            solid.frame(0).expect("draws").image.pixels()
        );
    }

    #[test]
    fn a_grid_puts_its_lines_at_the_spacing_and_leaves_the_background_between_them() {
        let parameters = ParameterMap::from([
            ("spacing_px".to_owned(), ParameterValue::Float(16.0)),
            ("line_px".to_owned(), ParameterValue::Float(2.0)),
            (
                "line_colour".to_owned(),
                ParameterValue::Color([1.0, 0.0, 0.0, 1.0]),
            ),
            (
                "background_colour".to_owned(),
                ParameterValue::Color([0.0, 0.0, 1.0, 1.0]),
            ),
        ]);
        let mut source = GeneratorSource::new("ovs.grid", 1, &parameters, SizePx::new(64, 64))
            .expect("published");
        let frame = source.frame(0).expect("draws");

        assert_eq!(pixel(&frame.image, 16, 8), [255, 0, 0, 255], "a line");
        assert_eq!(pixel(&frame.image, 17, 8), [255, 0, 0, 255], "two wide");
        assert_eq!(pixel(&frame.image, 18, 8), [0, 0, 255, 255], "and no wider");
        assert_eq!(pixel(&frame.image, 8, 32), [255, 0, 0, 255], "and across");
        assert_eq!(pixel(&frame.image, 9, 9), [0, 0, 255, 255], "the field");
    }

    #[test]
    fn a_grid_draws_nothing_the_commissioning_pattern_would_add() {
        // A grid is content. If it acquired the test pattern's red edge or
        // centre cross it would be a diagnostic somebody had put on the wall
        // during a show without meaning to.
        let parameters = ParameterMap::from([
            ("spacing_px".to_owned(), ParameterValue::Float(16.0)),
            (
                "line_colour".to_owned(),
                ParameterValue::Color([1.0, 1.0, 1.0, 1.0]),
            ),
            (
                "background_colour".to_owned(),
                ParameterValue::Color([0.0, 0.0, 0.0, 1.0]),
            ),
        ]);
        let mut source = GeneratorSource::new("ovs.grid", 1, &parameters, SizePx::new(64, 64))
            .expect("published");
        let frame = source.frame(0).expect("draws");

        let colours: std::collections::BTreeSet<[u8; 4]> = frame
            .image
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .collect();
        assert_eq!(
            colours,
            std::collections::BTreeSet::from([[0, 0, 0, 255], [255, 255, 255, 255]]),
            "only the two colours the operator chose"
        );
    }

    /// How many pixels of a picture are exactly one colour.
    fn count(image: &DecodedImage, rgba: [u8; 4]) -> usize {
        image
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| **pixel == rgba)
            .count()
    }

    /// The three shapes, as the manifest numbers them.
    const ELLIPSE: &str = "ellipse";
    const RECTANGLE: &str = "rectangle";
    const TRIANGLE: &str = "triangle";

    /// A shape of one size, in one colour on another, turned by an angle.
    fn shape(
        figure: &str,
        extent: (f64, f64),
        rotation_deg: f64,
        colour: [f32; 4],
        background: [f32; 4],
    ) -> ParameterMap {
        ParameterMap::from([
            (
                "shape".to_owned(),
                ParameterValue::Choice(figure.to_owned()),
            ),
            ("width_px".to_owned(), ParameterValue::Float(extent.0)),
            ("height_px".to_owned(), ParameterValue::Float(extent.1)),
            (
                "rotation_deg".to_owned(),
                ParameterValue::Float(rotation_deg),
            ),
            ("colour".to_owned(), ParameterValue::Color(colour)),
            (
                "background_colour".to_owned(),
                ParameterValue::Color(background),
            ),
        ])
    }

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
    const RED_BYTES: [u8; 4] = [255, 0, 0, 255];

    /// A caption of one line, in white on black, drawn into a wide picture.
    fn caption(words: &str, over: &[(&str, ParameterValue)]) -> std::sync::Arc<DecodedImage> {
        let mut parameters = ParameterMap::from([
            ("text".to_owned(), ParameterValue::Text(words.to_owned())),
            ("cap_px".to_owned(), ParameterValue::Float(24.0)),
            (
                "background_colour".to_owned(),
                ParameterValue::Color([0.0, 0.0, 0.0, 1.0]),
            ),
        ]);
        for (key, value) in over {
            parameters.insert((*key).to_owned(), value.clone());
        }
        let mut source = GeneratorSource::new("ovs.text", 1, &parameters, SizePx::new(256, 128))
            .expect("published");
        source.frame(0).expect("draws").image
    }

    /// How many pixels of a picture are not the background.
    fn ink(image: &DecodedImage) -> usize {
        image
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| **pixel != [0, 0, 0, 255])
            .count()
    }

    /// The mean x of the ink, as a fraction of the width.
    fn ink_centre(image: &DecodedImage) -> f32 {
        let width = image.size().width as usize;
        let mut total = 0.0;
        let mut seen = 0.0;
        for (index, pixel) in image.pixels().as_chunks::<4>().0.iter().enumerate() {
            if *pixel != [0, 0, 0, 255] {
                total += (index % width) as f32;
                seen += 1.0;
            }
        }
        if seen == 0.0 {
            return 0.5;
        }
        total / seen / width as f32
    }

    #[test]
    fn a_caption_sets_the_words_it_was_given() {
        let drawn = caption("HELLO", &[]);
        assert!(ink(&drawn) > 0, "nothing was set");

        // And an empty one sets nothing at all, rather than a row of boxes.
        assert_eq!(ink(&caption("", &[])), 0);
    }

    #[test]
    fn a_caption_is_set_in_capitals_whatever_was_typed() {
        // The font is thirty-eight characters and has no lower case
        // (`pattern::font`). Upper-casing is the difference between a caption
        // an operator typed normally and a row of boxes.
        assert_eq!(
            caption("hello", &[]).pixels(),
            caption("HELLO", &[]).pixels(),
            "lower case is set as capitals"
        );
    }

    #[test]
    fn a_character_this_font_has_not_got_is_visible_rather_than_a_gap() {
        // An operator's own words, so a character that will not set is theirs
        // to see — in the preview, rather than at the get-in
        // (owner decision Q11).
        let alone = ink(&caption("A", &[]));
        let with_stranger = ink(&caption("A\u{20ac}", &[]));

        assert!(alone > 0, "the letter drew nothing");
        assert!(
            with_stranger > alone,
            "the stranger added nothing: {alone} against {with_stranger}"
        );
    }

    #[test]
    fn a_line_break_is_a_line_break() {
        // Somebody who pressed Enter meant a line break. Nothing wraps: that is
        // layout, and layout is a font engine's job rather than this one's.
        let one = caption("AB CD", &[]);
        let two = caption("AB\nCD", &[]);

        assert!(ink(&two) > 0);
        assert_ne!(one.pixels(), two.pixels(), "two lines are not one line");

        // The two lines straddle the middle, so there is ink in both halves.
        let half = (two.size().height / 2) as usize * two.size().width as usize * 4;
        let above = two.pixels()[..half]
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| **pixel != [0, 0, 0, 255])
            .count();
        assert!(above > 0 && above < ink(&two), "one line each side");
    }

    #[test]
    fn alignment_moves_the_words_to_the_edge_it_names() {
        let left = ink_centre(&caption(
            "AB",
            &[("align", ParameterValue::Choice("left".to_owned()))],
        ));
        let centre = ink_centre(&caption("AB", &[]));
        let right = ink_centre(&caption(
            "AB",
            &[("align", ParameterValue::Choice("right".to_owned()))],
        ));

        assert!(left < centre, "left {left} against centre {centre}");
        assert!(centre < right, "centre {centre} against right {right}");
        assert!((centre - 0.5).abs() < 0.05, "centred at {centre}");
    }

    #[test]
    fn a_caption_has_no_timeline_and_is_drawn_once() {
        // A still: one frame, nothing to run along, and nothing that would make
        // the renderer redraw it sixty times a second.
        let source =
            GeneratorSource::new("ovs.text", 1, &ParameterMap::new(), size()).expect("published");

        assert!(!source.changes_over_time());
        assert_eq!(source.length().frame_count, 1);
        assert_eq!(source.length().duration_ms, 0);
    }
    const BLUE_BYTES: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn a_shape_from_its_published_defaults_alone_is_a_disc_on_nothing() {
        // An operator adds a shape and sets nothing. What appears is the thing
        // somebody means by "put a circle up", over whatever the layer below is
        // playing — not over a black bed they then have to turn off.
        let mut source =
            GeneratorSource::new("ovs.shape", 1, &ParameterMap::new(), SizePx::new(512, 512))
                .expect("published");
        let frame = source.frame(0).expect("draws");

        assert_eq!(
            pixel(&frame.image, 0, 0),
            [0, 0, 0, 0],
            "the corner is empty"
        );
        assert_eq!(
            pixel(&frame.image, 256, 256),
            [255, 255, 255, 255],
            "and the middle is the shape"
        );
        assert_eq!(
            pixel(&frame.image, 356, 256),
            [255, 255, 255, 255],
            "a hundred pixels out, which is inside a shape 256 across"
        );
        assert_eq!(
            pixel(&frame.image, 456, 256),
            [0, 0, 0, 0],
            "and two hundred, which is outside it"
        );
    }

    #[test]
    fn the_shape_is_where_it_says_it_is_and_the_background_is_everywhere_else() {
        // Centred, and the size it was given: a band forty pixels across and
        // eight down, on a canvas of sixty-four.
        let parameters = shape(RECTANGLE, (40.0, 8.0), 0.0, RED, BLUE);
        let mut source = GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(64, 64))
            .expect("published");
        let frame = source.frame(0).expect("draws");

        assert_eq!(pixel(&frame.image, 32, 32), RED_BYTES, "the middle");
        assert_eq!(pixel(&frame.image, 50, 32), RED_BYTES, "and out along it");
        assert_eq!(pixel(&frame.image, 54, 32), BLUE_BYTES, "and past its end");
        assert_eq!(pixel(&frame.image, 32, 40), BLUE_BYTES, "and below it");
        assert_eq!(pixel(&frame.image, 0, 0), BLUE_BYTES, "and the corner");
    }

    #[test]
    fn a_rectangle_turned_a_quarter_of_a_turn_lies_the_other_way() {
        // The same band, stood on its end. An operator who turns a bar ninety
        // degrees expects a column, and gets one.
        let upright = shape(RECTANGLE, (40.0, 8.0), 90.0, RED, BLUE);
        let mut source =
            GeneratorSource::new("ovs.shape", 1, &upright, SizePx::new(64, 64)).expect("published");
        let frame = source.frame(0).expect("draws");

        assert_eq!(
            pixel(&frame.image, 32, 50),
            RED_BYTES,
            "the long way is down"
        );
        assert_eq!(pixel(&frame.image, 32, 14), RED_BYTES, "and up");
        assert_eq!(
            pixel(&frame.image, 50, 32),
            BLUE_BYTES,
            "and no longer across"
        );
        assert_eq!(pixel(&frame.image, 14, 32), BLUE_BYTES, "either way");
    }

    #[test]
    fn a_triangle_points_the_way_it_was_turned_to_point() {
        // The rotation is not a rectangle's alone: a wedge pointing at the next
        // thing to happen is most of what a triangle is for.
        let corners = |rotation: f64| {
            let parameters = shape(TRIANGLE, (48.0, 48.0), rotation, RED, BLUE);
            let mut source = GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(64, 64))
                .expect("published");
            let frame = source.frame(0).expect("draws");
            (pixel(&frame.image, 12, 12), pixel(&frame.image, 12, 52))
        };

        assert_eq!(
            corners(0.0),
            (BLUE_BYTES, RED_BYTES),
            "apex upwards: the base is at the bottom"
        );
        assert_eq!(
            corners(180.0),
            (RED_BYTES, BLUE_BYTES),
            "and turned over, it is at the top"
        );
    }

    #[test]
    fn an_edge_is_a_ramp_between_the_two_colours_rather_than_a_staircase() {
        // A circle drawn by asking each pixel whether it is inside would be
        // every pixel one colour or the other, which on an LED wall is a
        // staircase an audience can count the steps of. Softening it costs a
        // subtraction per pixel.
        let parameters = shape(TRIANGLE, (40.0, 40.0), 0.0, RED, BLUE);
        let mut triangle = GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(64, 64))
            .expect("published");
        let parameters = shape(ELLIPSE, (40.0, 40.0), 0.0, RED, BLUE);
        let mut disc = GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(64, 64))
            .expect("published");

        for (what, frame) in [
            ("a disc", disc.frame(0).expect("draws")),
            ("a triangle", triangle.frame(0).expect("draws")),
        ] {
            let between = frame
                .image
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel[0] > 0 && pixel[0] < 255)
                .count();
            assert!(
                between > 24,
                "{what} has only {between} pixels between the two colours, which is a staircase"
            );
        }
    }

    #[test]
    fn an_edge_over_a_transparent_background_fades_the_alpha_and_not_the_colour() {
        // What makes a shape usable as an element on a layer above another. A
        // half-covered edge pixel is the shape's colour at half alpha; if it
        // were half the shape's colour instead, the shader would premultiply it
        // again and put a dark rim around everything the operator floated over
        // the picture below.
        let parameters = shape(ELLIPSE, (40.0, 40.0), 0.0, RED, [0.0, 0.0, 0.0, 0.0]);
        let mut source = GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(64, 64))
            .expect("published");
        let frame = source.frame(0).expect("draws");

        assert_eq!(pixel(&frame.image, 32, 32), RED_BYTES, "the shape is solid");
        assert_eq!(
            pixel(&frame.image, 0, 0),
            [0, 0, 0, 0],
            "and around it is nothing"
        );

        let edges: Vec<[u8; 4]> = frame
            .image
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .filter(|pixel| pixel[3] > 0 && pixel[3] < 255)
            .collect();
        assert!(!edges.is_empty(), "there is an edge to look at");
        for edge in edges {
            assert_eq!(
                [edge[0], edge[1], edge[2]],
                [255, 0, 0],
                "the colour stays the colour and only the alpha comes off it"
            );
        }
    }

    #[test]
    fn a_shape_of_no_size_draws_no_shape_rather_than_dividing_by_it() {
        // A size dragged to nothing takes the shape out of the look and leaves
        // everything else the operator set, so it can be brought back. An
        // ellipse of no radius would otherwise be asked how far a point is from
        // a boundary it does not have.
        for extent in [(0.0, 40.0), (40.0, 0.0), (0.0, 0.0)] {
            let parameters = shape(ELLIPSE, extent, 30.0, RED, BLUE);
            let mut source = GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(64, 64))
                .expect("published");
            let frame = source.frame(0).expect("draws");

            assert_eq!(
                count(&frame.image, BLUE_BYTES),
                64 * 64,
                "{extent:?} draws something"
            );
        }
    }

    #[test]
    fn a_shape_bigger_than_the_picture_fills_it_and_one_of_no_number_at_all_does_not() {
        // Both only reachable from a show written against another build,
        // because this one refuses a size outside the manifest's range on save.
        // A shape larger than the composition is a filled canvas, which is what
        // was asked for; a size that is not a number is not a size.
        let fill = |width: f64| {
            let parameters = shape(RECTANGLE, (width, 8192.0), 0.0, RED, BLUE);
            let mut source = GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(64, 64))
                .expect("published");
            let frame = source.frame(0).expect("draws");
            (
                count(&frame.image, RED_BYTES),
                count(&frame.image, BLUE_BYTES),
            )
        };

        assert_eq!(fill(8192.0), (64 * 64, 0), "larger than the canvas");
        assert_eq!(fill(f64::INFINITY), (0, 64 * 64), "and not a size at all");
    }

    #[test]
    fn a_shape_is_sized_in_composition_pixels_rather_than_in_a_fraction_of_the_canvas() {
        // The reason the grid's spacing and the pixelate effect's block are in
        // the same unit: a look set up on one composition has to mean the same
        // thing after the pieces are mapped onto a wall of another size. A
        // shape measured as a fraction would be the one element that moved when
        // everything around it did not.
        let parameters = shape(RECTANGLE, (32.0, 32.0), 0.0, RED, BLUE);
        let on = |side: u32| {
            let mut source =
                GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(side, side))
                    .expect("published");
            count(&source.frame(0).expect("draws").image, RED_BYTES)
        };

        assert_eq!(
            on(64),
            32 * 32,
            "thirty-two pixels across, on a small canvas"
        );
        assert_eq!(on(128), 32 * 32, "and the same thirty-two on a larger one");
    }

    #[test]
    fn each_of_the_three_shapes_is_the_shape_it_says_it_is() {
        // Told apart by how much of the same box each of them fills: a triangle
        // half of it, an ellipse about four fifths, a rectangle all of it.
        let area = |figure: &str| {
            let parameters = shape(figure, (48.0, 48.0), 0.0, RED, BLUE);
            let mut source = GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(64, 64))
                .expect("published");
            count(&source.frame(0).expect("draws").image, RED_BYTES)
        };

        assert_eq!(area(RECTANGLE), 48 * 48, "a rectangle fills its box");
        assert!(
            area(TRIANGLE) < area(ELLIPSE),
            "and a triangle the least of it"
        );
        assert!(area(ELLIPSE) < area(RECTANGLE));
        assert!(area(TRIANGLE) > 0, "all three draw something");
    }

    #[test]
    fn a_shape_is_the_same_picture_however_far_a_playhead_has_run() {
        // A size and a rotation are numbers somebody set, not a motion. A shape
        // that turned by itself would need a rate nothing has asked for, and it
        // would be a new manifest rather than this one.
        let parameters = shape(TRIANGLE, (40.0, 24.0), 20.0, RED, BLUE);
        let mut source = GeneratorSource::new("ovs.shape", 1, &parameters, SizePx::new(64, 64))
            .expect("published");

        let first = source.frame(0).expect("draws").image.pixels().to_vec();
        assert_eq!(
            first.as_slice(),
            source.frame(5_000).expect("draws").image.pixels()
        );
    }

    #[test]
    fn noise_asked_twice_for_one_moment_draws_it_twice_the_same() {
        // **This used to say the opposite**, and said why: a show that looked
        // different every night is a show nobody can check in the afternoon.
        // The owner decided otherwise on 2026-10-12 — "sollte aber wirklich
        // zufälliger Noise sein jedes Frame" — and what that gives up is
        // exactly that: the field now depends on the wall clock, so the same
        // show is not the same grain tomorrow.
        //
        // What is kept, and has to be, is that one moment is one picture. A
        // redraw that rolled fresh numbers every time it was asked would make
        // the buffer comparison in `redraw_at` meaningless and put a different
        // field on two outputs showing the same composition.
        let mut source =
            GeneratorSource::new("ovs.noise", 1, &ParameterMap::new(), size()).expect("published");

        assert_eq!(
            source.frame(42).expect("draws").image.pixels().to_vec(),
            source.frame(42).expect("draws").image.pixels().to_vec(),
        );
        assert_ne!(
            source.frame(42).expect("draws").image.pixels().to_vec(),
            source.frame(43).expect("draws").image.pixels().to_vec(),
            "and a different moment is a different field"
        );
    }

    #[test]
    fn noise_with_no_amount_is_a_flat_mid_grey() {
        // Turning the texture down fades towards grey, not towards black, so a
        // bed under a key does not get darker as the grain comes out of it.
        let parameters = ParameterMap::from([("amount".to_owned(), ParameterValue::Float(0.0))]);
        let mut source =
            GeneratorSource::new("ovs.noise", 1, &parameters, size()).expect("published");
        let frame = source.frame(0).expect("draws");

        for pixel in frame.image.pixels().as_chunks::<4>().0 {
            assert_eq!(*pixel, [128, 128, 128, 255]);
        }
    }

    #[test]
    fn monochrome_noise_is_grey_and_coloured_noise_is_not() {
        let draw = |coloured: bool| {
            let parameters =
                ParameterMap::from([("coloured".to_owned(), ParameterValue::Bool(coloured))]);
            let mut source =
                GeneratorSource::new("ovs.noise", 1, &parameters, size()).expect("published");
            source.frame(0).expect("draws")
        };

        let grey = draw(false);
        for pixel in grey.image.pixels().as_chunks::<4>().0 {
            assert_eq!(pixel[0], pixel[1], "monochrome noise is one value");
            assert_eq!(pixel[1], pixel[2]);
        }

        let colour = draw(true);
        assert!(
            colour
                .image
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[0] != pixel[1] || pixel[1] != pixel[2]),
            "coloured noise has three fields that do not move together"
        );
    }

    #[test]
    fn noise_at_a_larger_scale_has_larger_cells() {
        // What the scale parameter is for: a wall of fine grain and a wall of
        // soft blobs are different looks, and the slider has to be the
        // difference between them.
        let neighbours_that_differ = |scale: f64| {
            let parameters =
                ParameterMap::from([("scale_px".to_owned(), ParameterValue::Float(scale))]);
            let mut source = GeneratorSource::new("ovs.noise", 1, &parameters, SizePx::new(256, 1))
                .expect("published");
            let frame = source.frame(0).expect("draws");
            let row: Vec<u8> = frame
                .image
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect();
            row.windows(2)
                .filter(|pair| pair[0].abs_diff(pair[1]) > 2)
                .count()
        };

        assert!(
            neighbours_that_differ(4.0) > neighbours_that_differ(128.0),
            "fine grain must change faster across the picture than soft blobs"
        );
    }

    #[test]
    fn a_wrongly_typed_parameter_falls_back_to_what_the_generator_published() {
        // Only reachable from a show written against another build, because
        // this one refuses it on save. The published default is the only answer
        // that is not invented here — a constant in the renderer would be one
        // generator's number quietly applied to another.
        // A still generator, because the question is about parameters and
        // nothing else: noise now carries a seed of its own, so two instances
        // of it differ by design and could not answer this.
        let parameters = ParameterMap::from([("colour".to_owned(), ParameterValue::Float(3.0))]);
        let mut wrong =
            GeneratorSource::new("ovs.solid_colour", 1, &parameters, size()).expect("published");
        let mut published =
            GeneratorSource::new("ovs.solid_colour", 1, &ParameterMap::new(), size())
                .expect("published");

        assert_eq!(
            wrong.frame(0).expect("draws").image.pixels(),
            published.frame(0).expect("draws").image.pixels()
        );
    }

    #[test]
    fn a_generator_this_build_does_not_publish_is_not_drawn() {
        // The same answer a fixture gives: better nothing than something that
        // looks deliberate.
        assert!(
            GeneratorSource::new("ovs.solid_colour", 99, &ParameterMap::new(), size()).is_none()
        );
        assert!(
            GeneratorSource::new("third.party.plasma", 1, &ParameterMap::new(), size()).is_none()
        );
    }

    #[test]
    fn a_frame_is_the_size_it_says_even_when_the_composition_is_absurd() {
        // A generator fills a buffer on the thread that asks for it, so its
        // cost is bounded whatever a show asks for.
        let source = GeneratorSource::new(
            "ovs.solid_colour",
            1,
            &ParameterMap::new(),
            SizePx::new(0, 100_000),
        )
        .expect("published");
        assert_eq!(source.size.width, 1);
        assert_eq!(source.size.height, MAX_GENERATED_SIDE);
    }
}

/// A way to look at the ones that are judged by eye, for whoever adjusts them.
///
/// A clock is judged by being read from the side of a stage and an edge by
/// whether it steps, and no assertion does either. This writes one of each to a
/// directory when `OVS_GENERATOR_PNG` names one, and does nothing at all
/// otherwise:
///
/// ```text
/// OVS_GENERATOR_PNG=/tmp cargo test -p ovs-renderer --lib media::generator
/// ```
#[cfg(test)]
mod preview {
    use super::*;

    #[test]
    fn write_one_of_each_when_asked_to() {
        let path = std::env::var("OVS_GENERATOR_PNG").unwrap_or_default();
        if path.is_empty() {
            return;
        }

        let amber = ParameterValue::Color([1.0, 0.65, 0.1, 1.0]);
        for (name, type_id, tick, parameters) in [
            ("clock-24", "ovs.clock", 45_296, ParameterMap::new()),
            (
                "clock-12",
                "ovs.clock",
                75_599,
                ParameterMap::from([("hour_24".to_owned(), ParameterValue::Bool(false))]),
            ),
            (
                "countdown",
                "ovs.countdown",
                61,
                ParameterMap::from([("text_colour".to_owned(), amber)]),
            ),
            (
                "countdown-over",
                "ovs.countdown",
                705,
                ParameterMap::from([("count_past_zero".to_owned(), ParameterValue::Bool(true))]),
            ),
            // On a dark bed rather than on nothing, so the edge can be looked at
            // in whatever opens the file. Turned off the square, which is where
            // a staircase shows worst.
            (
                "shape-disc",
                "ovs.shape",
                0,
                ParameterMap::from([
                    ("width_px".to_owned(), ParameterValue::Float(520.0)),
                    ("height_px".to_owned(), ParameterValue::Float(520.0)),
                    (
                        "background_colour".to_owned(),
                        ParameterValue::Color([0.05, 0.05, 0.08, 1.0]),
                    ),
                ]),
            ),
            (
                "shape-triangle-turned",
                "ovs.shape",
                0,
                ParameterMap::from([
                    (
                        "shape".to_owned(),
                        ParameterValue::Choice("triangle".to_owned()),
                    ),
                    ("width_px".to_owned(), ParameterValue::Float(640.0)),
                    ("height_px".to_owned(), ParameterValue::Float(520.0)),
                    ("rotation_deg".to_owned(), ParameterValue::Float(17.0)),
                    (
                        "background_colour".to_owned(),
                        ParameterValue::Color([0.05, 0.05, 0.08, 1.0]),
                    ),
                ]),
            ),
        ] {
            let mut source = GeneratorSource::new(type_id, 1, &parameters, SizePx::new(1280, 720))
                .expect("published");
            let frame = source.frame(tick).expect("draws");
            image::save_buffer(
                std::path::Path::new(&format!("{path}/{name}.png")),
                frame.image.pixels(),
                frame.image.size().width,
                frame.image.size().height,
                image::ColorType::Rgba8,
            )
            .expect("writes");
        }
    }
}
