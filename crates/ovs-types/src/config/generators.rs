//! Generators: what they are, and what they can be asked for.
//!
//! A generator implements the source contract — a layer plays one exactly as it
//! plays a clip — so what has to be described here is only the part a clip does
//! not have: the parameters an operator can set, with their types, defaults and
//! ranges.
//!
//! # A manifest is published once and never changes
//!
//! The same rule as a fixture personality, for the same reason: a show stores
//! a type and a version and expects the same thing back. Adding a parameter is a
//! new version, not an edit, because a show saved last year must open looking
//! the way it was left.
//!
//! The parameter model itself is shared with effects, which declare theirs the
//! same way: see [`crate::config::manifest`].
//!
//! # Values are checked against the manifest, not assumed
//!
//! A show can hold any [`ParameterValue`] for any name. What makes a generator
//! usable is that the values it is given are the ones it declared, in the ranges
//! it declared, and that anything else is refused when the show is saved
//! rather than producing a picture nobody asked for.

pub use crate::config::fixtures::FixtureDefinition;
pub use crate::config::manifest::{Choice, DmxChannel, ParameterDefinition, ParameterKind};

use crate::config::slot::{ParameterMap, ParameterValue};

/// A published generator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeneratorManifest {
    /// Stable type identifier, for example `ovs.solid_colour`.
    pub type_id: &'static str,
    /// Version. A new parameter is a new version, never a changed one.
    pub version: u32,
    /// What a client's source list shows.
    pub name: &'static str,
    /// One line describing what it draws.
    pub description: &'static str,
    /// The parameters it declares.
    pub parameters: &'static [ParameterDefinition],
}

/// Every published generator's personality, built once.
///
/// The same registry effects have, for the same reason: generating one leaks,
/// and the DMX mapper looks one up for every patched fixture on every frame.
static PERSONALITIES: std::sync::OnceLock<Vec<&'static FixtureDefinition>> =
    std::sync::OnceLock::new();

/// The personality a published generator describes, if it describes one.
#[must_use]
pub fn personality(type_id: &str, version: u32) -> Option<&'static FixtureDefinition> {
    PERSONALITIES
        .get_or_init(|| {
            PUBLISHED
                .iter()
                .filter_map(GeneratorManifest::personality)
                .collect()
        })
        .iter()
        .find(|definition| definition.type_id == type_id && definition.version == version)
        .copied()
}

impl GeneratorManifest {
    /// The personality this generator's own declarations describe.
    ///
    /// `None` when it exposes nothing to DMX. A generator is a plugin like an
    /// effect and the rule is the same: it brings channels or it brings none.
    #[must_use]
    pub fn personality(&'static self) -> Option<&'static FixtureDefinition> {
        crate::config::fixtures::generated_personality(
            self.type_id,
            self.version,
            self.name,
            self.parameters,
            crate::config::fixtures::ParameterSlot::Generator,
        )
    }

    /// Says why a value is not usable for one of this generator's parameters.
    ///
    /// The same rule an effect's values are held to, and for the same reason:
    /// this is an API request, which can say anything, where a DMX channel is
    /// eight bits that cannot express an impossible value in the first place.
    #[must_use]
    pub fn refuse(&self, key: &str, value: &ParameterValue) -> Option<String> {
        match self.parameter(key) {
            Some(definition) => definition.refuse(value),
            None => Some(format!(
                "{} version {} has no such parameter",
                self.type_id, self.version
            )),
        }
    }

    /// Returns a parameter by key.
    #[must_use]
    pub fn parameter(&self, key: &str) -> Option<&'static ParameterDefinition> {
        crate::config::manifest::parameter(self.parameters, key)
    }

    /// Returns the value for a key, falling back to the declared default.
    ///
    /// A show that leaves a parameter out is not incomplete: it is a show
    /// that wants what the generator was published with, which is exactly what
    /// a default is for.
    #[must_use]
    pub fn value(&self, values: &ParameterMap, key: &str) -> Option<ParameterValue> {
        crate::config::manifest::value(self.parameters, values, key)
    }

    /// Returns every problem with a set of values.
    ///
    /// A value for something this generator does not declare is a problem too:
    /// it is nearly always a typo or a show written against another version,
    /// and silently ignoring it means an operator setting something and seeing
    /// nothing happen.
    #[must_use]
    pub fn problems(&self, values: &ParameterMap) -> Vec<(String, String)> {
        crate::config::manifest::problems(
            self.parameters,
            &format!("{} version {}", self.type_id, self.version),
            values,
        )
    }
}

/// A flat colour.
///
/// The reference generator: the smallest thing that is genuinely useful on a
/// wall — a colour wash, a blackout bed, a background under a key — and enough
/// to exercise the whole contract, because it has a parameter that matters.
pub const SOLID_COLOUR_V1: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.solid_colour",
    version: 1,
    name: "Solid Colour",
    description: "Fills the layer with one colour.",
    parameters: &[ParameterDefinition {
        key: "colour",
        name: "Colour",
        kind: ParameterKind::Colour {
            default: [1.0, 1.0, 1.0, 1.0],
        },
        dmx: Some(DmxChannel::Coarse),
    }],
};

/// The service pattern, on a layer.
///
/// **There is no version 1, and the gap is the point.** It was a grid, bars
/// and a centre cross, drawn once and left alone — and beside it this system
/// already had a test pattern of its own, the one an output and a composition
/// show. Two answers to one question, and the lesser one was what a layer
/// offered (owner, 2026-10-12: "den möchte ich da sehen und keinen
/// Generischen"), so version 2 drew the service pattern instead. Three days
/// later the first went (owner, 2026-10-15: "Testpattern v1 soll nicht
/// existieren. Nur noch v2 und das wird zum normalen Testpattern").
///
/// **The number stays 2 although nothing is below it.** A version is what a
/// saved show names — four slots of the show on the machine in the hall name
/// this one — and renumbering it would be editing those shows from here. A
/// manifest is immutable once published; removing one is a decision about what
/// this build offers, and renumbering another is a decision about documents
/// that are not ours.
///
/// What the picture is for: colour bars over a black-level ramp, a picture
/// that drifts, a sweep every five seconds and a caption naming the surface
/// and its mode.
///
/// **What the drift and the sweep are for**, and why they are worth what they
/// cost: a frozen output and a live one showing a still look identical from
/// across a room, and a pattern that moves tells them apart. The sweep is one
/// bright diagonal edge with a fading trail, which is what tearing, frame
/// doubling and a cable that cannot carry the rate all look wrong on.
///
/// **It is drawn every frame and that is not free.** The whole surface is
/// copied twice a row and blended over, on the processor, and the finished
/// picture crosses the bus whole — about fifteen megabytes a frame at 1080p.
/// That is the price of the thing this is for, and it is paid while somebody
/// is lining a wall up rather than during a show.
///
/// One parameter, because one is all the pattern has to be told: everything
/// else about the picture is the surface it is drawn for. `COMPOSITION` as
/// published because that is this system's own word for the surface a layer
/// fills, and it is what the caption on a composition already says.
pub const TEST_PATTERN_V2: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.test_pattern",
    version: 2,
    name: "Test Pattern",
    description: "The service pattern: bars, a drift, a sweep and a caption.",
    parameters: &[ParameterDefinition {
        key: "caption",
        name: "Caption",
        kind: ParameterKind::Text {
            default: "COMPOSITION",
            max_chars: 32,
        },
        dmx: None,
    }],
};

/// The slowest flash that is still a flash, in hertz.
///
/// **My choice, not a measurement.** Below this it is a light being switched
/// rather than a strobe, and a parameter has to have a bottom.
pub const MIN_STROBE_HZ: f64 = 0.5;

/// The fastest flash any supported output can actually show, in hertz.
///
/// Derived rather than picked: a flash needs one frame lit and one frame dark,
/// so the most an output can show is half its own rate, and the fastest mode
/// this server drives is sixty. Above half of whatever the output is really
/// running at the picture aliases -- a 25p wall asked for twenty hertz shows
/// something, but not twenty flashes -- and that is a fact about the wall
/// rather than a rule this can enforce.
pub const MAX_STROBE_HZ: f64 = 30.0;

/// A flash at a rate.
///
/// The one generator here that is about time rather than about a picture: two
/// pictures, the colour and nothing, alternating. An operator reaches for it to
/// punch a chorus, and it is the thing they would otherwise build out of a
/// solid colour and a lot of pad pressing.
///
/// Published at ten hertz because that is unmistakably a strobe and is still
/// below half of every rate this server drives -- 25p, 50p and 60p all show ten
/// flashes a second honestly. **The number itself is mine**; the operator moves
/// it and the show remembers where they left it.
///
/// Worth knowing rather than enforced here: flashing light between roughly
/// three and thirty hertz is the band photosensitive epilepsy is triggered in,
/// and venues often have a house limit. That is a decision for whoever is
/// running the room, so this publishes the range and says so instead of
/// quietly refusing part of it.
pub const STROBE_V1: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.strobe",
    version: 1,
    name: "Strobe",
    description: "Flashes one colour at a rate.",
    parameters: &[
        ParameterDefinition {
            key: "colour",
            name: "Colour",
            kind: ParameterKind::Colour {
                default: [1.0, 1.0, 1.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "hz",
            name: "Rate",
            kind: ParameterKind::Number {
                min: MIN_STROBE_HZ,
                max: MAX_STROBE_HZ,
                default: 10.0,
            },
            dmx: Some(DmxChannel::Coarse),
        },
    ],
};

/// A flash that can take the picture under it instead of covering it.
///
/// The owner, 2026-10-16: "erweitere den strobe um ein kanal. normal strobe
/// mit weiß und einen modus für invert color strobe, wo der weiße blitz dann
/// inverted color ist."
///
/// **One more channel, and version one is untouched.** A manifest is published
/// once; a show saved with the strobe it had keeps flashing exactly as it did,
/// and a source list offers this.
///
/// # What the second mode is
///
/// `colour` is the flash as it has always been: the colour, over what is
/// under it. `invert` composites the lit frame as a *difference* instead, so
/// white — the published colour — gives the picture underneath with every
/// channel turned round, and the clear half of the cycle leaves it alone.
///
/// The colour still means something in that mode rather than being ignored:
/// a difference against white is a full inversion, and against anything else
/// it is an inversion of part of the spectrum. That is a knob worth having
/// and it costs nothing to keep.
///
/// **It needs a card that can blend that way.** Difference is one of the two
/// advanced blend operations, and a device without the extension draws them
/// as Normal — on such a machine an inverting strobe flashes its colour
/// instead. The renderer says so once when it finds out.
pub const STROBE_V2: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.strobe",
    version: 2,
    name: "Strobe",
    description: "Flashes one colour at a rate, over the picture or against it.",
    parameters: &[
        ParameterDefinition {
            key: "colour",
            name: "Colour",
            kind: ParameterKind::Colour {
                default: [1.0, 1.0, 1.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "hz",
            name: "Rate",
            kind: ParameterKind::Number {
                min: MIN_STROBE_HZ,
                max: MAX_STROBE_HZ,
                default: 10.0,
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "mode",
            name: "Mode",
            kind: ParameterKind::Choice {
                choices: &[
                    Choice {
                        id: "colour",
                        name: "Colour",
                    },
                    Choice {
                        id: "invert",
                        name: "Invert",
                    },
                ],
                default: "colour",
            },
            dmx: Some(DmxChannel::Coarse),
        },
    ],
};

/// A linear gradient between two colours.
///
/// The background this business actually asks for. A flat wash reads
/// as a lit surface; one that falls off reads as depth, which is what somebody
/// wants behind a band or under a key, and it costs no decoder to get.
///
/// Published as a white-to-black ramp running top to bottom, because that is the
/// picture in which the angle is legible at a glance — an operator can see
/// immediately which way round the two colours are — and it is the neutral thing
/// they then recolour.
pub const GRADIENT_V1: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.gradient",
    version: 1,
    name: "Gradient",
    description: "A linear gradient between two colours, at an angle.",
    parameters: &[
        ParameterDefinition {
            key: "colour_a",
            name: "From",
            kind: ParameterKind::Colour {
                default: [1.0, 1.0, 1.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "colour_b",
            name: "To",
            kind: ParameterKind::Colour {
                default: [0.0, 0.0, 0.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        // Clockwise on screen from left-to-right, which is the direction every
        // other angle in this product turns (see the layer transform). A full
        // turn is the range because every angle in it is a different picture:
        // there is no part of the slider that does nothing.
        ParameterDefinition {
            key: "angle_deg",
            name: "Angle",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 360.0,
                default: 90.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// Ruled lines on a background.
///
/// Content rather than a diagnostic, which is what separates it from
/// [`TEST_PATTERN_V2`]: a grid behind a band, a floor receding, a scaffold
/// behind a key. All of those want a grid and none of them want bars, a drift
/// and a caption drawn over it, so the two are different generators rather
/// than one with a switch.
///
/// Published as white lines on black at sixty-four pixels, which reads as a grid
/// on every composition size this product targets rather than as a pale field on
/// a big one or three lines on a small one.
pub const GRID_V1: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.grid",
    version: 1,
    name: "Grid",
    description: "Lines at a spacing, in one colour on another.",
    parameters: &[
        // In composition pixels, like every other size in the system, so a look
        // set up on one composition means the same thing after the pieces are
        // mapped onto a wall of a different size.
        ParameterDefinition {
            key: "spacing_px",
            name: "Spacing",
            kind: ParameterKind::Number {
                min: 4.0,
                max: 512.0,
                default: 64.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "line_px",
            name: "Line width",
            kind: ParameterKind::Number {
                min: 1.0,
                max: 64.0,
                default: 2.0,
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "line_colour",
            name: "Line colour",
            kind: ParameterKind::Colour {
                default: [1.0, 1.0, 1.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "background_colour",
            name: "Background",
            kind: ParameterKind::Colour {
                default: [0.0, 0.0, 0.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
    ],
};

/// Value noise.
///
/// Cheap texture behind a key. A flat colour behind cut-out lettering
/// looks like a slide; the same colour with a little grain in it looks like a
/// surface, and unlike a grain clip it costs no decoder and no storage.
///
/// Published as monochrome grain at the finest scale and full amount. The
/// owner, 2026-10-18: "Noise sollte standard scale 0 haben, weil das wirklich
/// noise ist" — the bottom of the scale, static rather than blobs.
///
/// **No DMX.** The same message: "danach hat noise keine DMX Parameter mehr".
/// It is set up in the client and faded with its layer's dimmer, so a desk
/// patches nothing for it and there is no fixture type to import.
pub const NOISE_V1: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.noise",
    version: 1,
    name: "Noise",
    description: "Value noise at a scale, monochrome or coloured.",
    parameters: &[
        // The size of one cell of the noise, in composition pixels. Two is
        // nearly per-pixel static and five hundred is a few soft blobs across a
        // wall; everything an operator reaches for is between them.
        ParameterDefinition {
            key: "scale_px",
            name: "Scale",
            kind: ParameterKind::Number {
                min: 2.0,
                max: 512.0,
                default: 2.0,
            },
            dmx: None,
        },
        ParameterDefinition {
            key: "coloured",
            name: "Coloured",
            kind: ParameterKind::Toggle { default: false },
            dmx: None,
        },
        // How far the grain swings either side of mid grey. Published at the top
        // of its range because a generator an operator has just added should
        // look like what it is; turning it down fades towards flat grey rather
        // than towards black, so a bed under a key does not get darker as the
        // texture comes out of it.
        ParameterDefinition {
            key: "amount",
            name: "Amount",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 1.0,
            },
            dmx: None,
        },
    ],
};

/// One shape, centred, over a background.
///
/// The element rather than the picture. Everything published before
/// this fills the frame: a wash, a ramp, a grid, a texture. A show also wants
/// one *thing* on the wall — a disc behind a head, a bar under a caption, a
/// wedge pointing at whatever happens next — and until now that meant somebody
/// opening a design application, exporting a still and putting it through
/// ingest, for a picture the operator could not then resize in the room.
///
/// # Three shapes, because a show uses three
///
/// An ellipse, a rectangle and a triangle. Between them they cover the dot, the
/// circle, the band, the block, the arrow and the wedge, which is what actually
/// goes up. Left out, deliberately: a ring, which needs a thickness and is a
/// second size to explain; a star or an n-gon, which needs a point count and an
/// inner radius and would make the triangle a special case of itself; and a
/// line, which is a rectangle an operator has already been given. None of them
/// is refused on principle — each is a manifest of its own when somebody asks
/// for it, which is cheaper than publishing a shape generator nobody can
/// predict the controls of.
///
/// # Which shape, as a list
///
/// It was a number, because a parameter could be a number, a colour or a toggle
/// and nothing else — so the shapes were 0, 1 and 2, with their names written
/// into the label because there was nowhere else to put them. That is what
/// owner decision Q17 asked about and the owner's answer was to add the list:
/// each shape has an identifier a show stores and a name a client shows, and
/// a client draws a menu.
///
/// Ordered by what a show reaches for first, so the published default is the
/// one anybody means when they say "put a shape up".
///
/// Published as a white ellipse on nothing, a quarter of a 1080-line
/// composition across: the shape an operator means when they say "put a circle
/// up", at a size that is unmistakably an object and obviously theirs to drag.
pub const SHAPE_V1: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.shape",
    version: 1,
    name: "Shape",
    description: "One shape, centred, in a colour on a background.",
    parameters: &[
        // A show that names a fourth shape is refused on save rather than
        // drawn as whichever one it rounds to — which is what the number this
        // replaced could not promise, because 2.6 is a number in range.
        ParameterDefinition {
            key: "shape",
            name: "Shape",
            kind: ParameterKind::Choice {
                choices: &[
                    Choice {
                        id: "ellipse",
                        name: "Ellipse",
                    },
                    Choice {
                        id: "rectangle",
                        name: "Rectangle",
                    },
                    Choice {
                        id: "triangle",
                        name: "Triangle",
                    },
                ],
                default: "ellipse",
            },
            dmx: Some(DmxChannel::Coarse),
        },
        // In composition pixels, like every other size in the system, so a look
        // set up on one composition means the same thing after the pieces are
        // mapped onto a wall of a different size. It matters
        // more here than for the grid: a shape is usually placed *with*
        // something else — a disc beside a grid, a bar under a clock — and a
        // size that was a fraction of the canvas while the others were pixels
        // would be the one element that moved when the composition was resized.
        //
        // Two of them rather than one, because a rectangle whose sides are
        // equal is a square and the thing a show actually puts on a wall is a
        // band across it.
        //
        // Zero is in range and draws nothing. A shape taken out of a look by
        // dragging its size to nothing is one the operator can bring back
        // without having to remember what it was, which is the same reason the
        // blur publishes a radius of zero.
        //
        // The top of the range is the largest frame this build generates. A
        // shape bigger than the picture it is drawn into is a filled canvas,
        // which is a legitimate thing to pass through on the way to a size.
        ParameterDefinition {
            key: "width_px",
            name: "Width",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 8192.0,
                default: 256.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "height_px",
            name: "Height",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 8192.0,
                default: 256.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        // Clockwise on screen, the direction every other angle in this product
        // turns (see the gradient, and the layer transform). A full turn,
        // because the triangle does not come back to itself before one — and a
        // slider whose useful length depended on which shape was selected would
        // be worse than one quarter of which repeats for two of the three.
        ParameterDefinition {
            key: "rotation_deg",
            name: "Rotation",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 360.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "colour",
            name: "Colour",
            kind: ParameterKind::Colour {
                default: [1.0, 1.0, 1.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        // Transparent, which is what separates this from the grid and the two
        // timepieces: those are pictures in their own right and this is an
        // element, and an element is nearly always wanted over whatever the
        // layer below is playing. A published black bed would mean every
        // operator's first act was to turn it off.
        ParameterDefinition {
            key: "background_colour",
            name: "Background",
            kind: ParameterKind::Colour {
                default: [0.0, 0.0, 0.0, 0.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
    ],
};

/// The time of day.
///
/// What goes on a backstage monitor. The first generator whose
/// picture depends on something other than where the pixel is, which is why it
/// declares no size, no position and no format beyond the two questions anybody
/// actually asks about a clock: is it on the twenty-four hour dial, and does it
/// show seconds.
///
/// Published as a white twenty-four hour clock with seconds on black, because
/// that is the one a crew reads from the side of a stage and the one that cannot
/// be misread by an hour.
///
/// # Why an offset rather than a time zone
///
/// Nothing in this workspace carries a time-zone database, and a clock that was
/// wrong by an hour twice a year would be worse than one nobody had to think
/// about. The offset is the venue's, set once, and it is the only part of a
/// clock this build cannot work out for itself. The alternative was showing UTC
/// on a wall in a building that does not run on it.
pub const CLOCK_V1: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.clock",
    version: 1,
    name: "Clock",
    description: "The time of day, drawn large.",
    parameters: &[
        ParameterDefinition {
            key: "hour_24",
            name: "24-hour",
            kind: ParameterKind::Toggle { default: true },
            dmx: None,
        },
        ParameterDefinition {
            key: "show_seconds",
            name: "Seconds",
            kind: ParameterKind::Toggle { default: true },
            dmx: None,
        },
        ParameterDefinition {
            key: "text_colour",
            name: "Text",
            kind: ParameterKind::Colour {
                default: [1.0, 1.0, 1.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "background_colour",
            name: "Background",
            kind: ParameterKind::Colour {
                default: [0.0, 0.0, 0.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        // The real range of civil offsets, in minutes so the three-quarter-hour
        // and half-hour zones can be reached at all.
        ParameterDefinition {
            key: "utc_offset_minutes",
            name: "UTC offset",
            kind: ParameterKind::Number {
                min: -720.0,
                max: 840.0,
                default: 0.0,
            },
            dmx: None,
        },
    ],
};

/// Time remaining, counting down.
///
/// The conference stage clock: a speaker is given twenty minutes, an
/// operator fires the slot when they start, and the number on the downstage
/// monitor is the only thing in the room that everybody agrees about.
///
/// # What happens at zero is a decision, not a detail
///
/// A countdown that stops at zero and one that keeps going into negative are two
/// different instruments. The first says "your time is up"; the second says "you
/// are four minutes over", which is what a chair needs in order to act. Neither
/// is a sensible default for the other, so it is a switch.
///
/// Published at ten minutes, counting from the trigger, stopping at zero, in
/// white on black — the segment clock, which is the one somebody reaches for
/// without configuring anything.
///
/// # Why the duration stops short of a day
///
/// It is drawn as hours, minutes and seconds, and a day does not fit in those.
/// A longer countdown is refused when the show is saved rather than drawn
/// wrapping round to a number nobody could act on.
pub const COUNTDOWN_V1: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.countdown",
    version: 1,
    name: "Countdown",
    description: "Time remaining, counting down to zero.",
    parameters: &[
        // One second short of a day: the longest run `HH:MM:SS` can say.
        ParameterDefinition {
            key: "duration_s",
            name: "Duration",
            kind: ParameterKind::Number {
                min: 1.0,
                max: 86_399.0,
                default: 600.0,
            },
            dmx: None,
        },
        // Whether firing the pad starts it again. On, for the speaker's clock an
        // operator resets between talks; off for a countdown that is running
        // against something in the world and must not be disturbed by somebody
        // bringing it up on a screen.
        ParameterDefinition {
            key: "from_trigger",
            name: "Start on trigger",
            kind: ParameterKind::Toggle { default: true },
            dmx: None,
        },
        ParameterDefinition {
            key: "count_past_zero",
            name: "Count up past zero",
            kind: ParameterKind::Toggle { default: false },
            dmx: None,
        },
        ParameterDefinition {
            key: "text_colour",
            name: "Text",
            kind: ParameterKind::Colour {
                default: [1.0, 1.0, 1.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "background_colour",
            name: "Background",
            kind: ParameterKind::Colour {
                default: [0.0, 0.0, 0.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
    ],
};

/// Words, set large and centred.
///
/// The one generator whose value is what an operator typed rather than a number
/// they chose: a holding caption, a room name, a speaker's name under a
/// countdown. owner decision Q11 asked where those words come from and the
/// owner's answer is this — the show, edited in the desktop client, with no
/// filesystem watcher anywhere.
///
/// # Built so the words can move later
///
/// The other half of that answer, and the half that decides the shape: the same
/// generator has to be able to take its text from the API and the live-state
/// system later, **without becoming a second generator type and without
/// changing how it draws**. So the text is an ordinary parameter, keyed like
/// every other, and a future live path sets it exactly as a live effect value
/// is set today. Nothing here is a static-text special case that a dynamic one
/// would have to be added beside.
///
/// # Upper case, because the font is
///
/// The strokes come from the test card's font (`pattern::font`), which is
/// thirty-eight characters and no lower case — an argument that module makes
/// for itself and which holds here: a font file means a font engine, a licence
/// and a megabyte of outlines. The renderer therefore sets what it is given in
/// capitals, and a character the font does not have is drawn as a hollow box
/// rather than as a gap, so a caption that will not set is visible in the
/// preview instead of at the get-in.
///
/// A line break in the text is a line break on the wall. There is no automatic
/// wrapping: that is layout, and layout is a font engine's job rather than this
/// one's.
///
/// Published as one line of white capitals on nothing, a tenth of a 1080-line
/// composition tall — a caption over whatever the layer below is playing.
pub const TEXT_V1: GeneratorManifest = GeneratorManifest {
    type_id: "ovs.text",
    version: 1,
    name: "Text",
    description: "Words, set large and centred.",
    parameters: &[
        // Long enough for a title and a couple of names under it, short enough
        // that a show cannot become a document nobody can open. A line break
        // counts as a character, which is what somebody typing one expects.
        ParameterDefinition {
            key: "text",
            name: "Text",
            kind: ParameterKind::Text {
                default: "OPENVIDEOSERVER",
                max_chars: 240,
            },
            dmx: None,
        },
        // In composition pixels, like every other size in the system, so a look
        // set up on one composition means the same thing after the pieces are
        // mapped onto a wall of a different size.
        ParameterDefinition {
            key: "cap_px",
            name: "Cap height",
            kind: ParameterKind::Number {
                min: 4.0,
                max: 1024.0,
                default: 108.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        // As a fraction of the cap height, so the weight of the lettering holds
        // when the size is changed. The published value is what the test card
        // uses, which is a weight that reads across a room.
        ParameterDefinition {
            key: "weight",
            name: "Stroke weight",
            kind: ParameterKind::Number {
                min: 0.02,
                max: 0.4,
                default: 0.12,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "tracking",
            name: "Tracking",
            kind: ParameterKind::Number {
                min: -0.2,
                max: 1.0,
                default: 0.12,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "align",
            name: "Alignment",
            kind: ParameterKind::Choice {
                choices: &[
                    Choice {
                        id: "centre",
                        name: "Centre",
                    },
                    Choice {
                        id: "left",
                        name: "Left",
                    },
                    Choice {
                        id: "right",
                        name: "Right",
                    },
                ],
                default: "centre",
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "text_colour",
            name: "Text",
            kind: ParameterKind::Colour {
                default: [1.0, 1.0, 1.0, 1.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
        // Nothing, not black: a caption is usually over something. A published
        // black bed would mean every operator's first act was to turn it off.
        ParameterDefinition {
            key: "background_colour",
            name: "Background",
            kind: ParameterKind::Colour {
                default: [0.0, 0.0, 0.0, 0.0],
            },
            dmx: Some(DmxChannel::Coarse),
        },
    ],
};

/// Every generator this build publishes.
pub const PUBLISHED: [GeneratorManifest; 11] = [
    SOLID_COLOUR_V1,
    // Version 2 and no version 1, which is the one place in this file where a
    // manifest was removed rather than retired: see its own documentation.
    TEST_PATTERN_V2,
    // Both, and the catalogue offers the newer: a show saved against version
    // one keeps the flash it was saved with.
    STROBE_V1,
    STROBE_V2,
    GRADIENT_V1,
    GRID_V1,
    SHAPE_V1,
    NOISE_V1,
    CLOCK_V1,
    COUNTDOWN_V1,
    TEXT_V1,
];

/// The types a source list no longer offers.
///
/// **Retired, not deleted.** Deleting a manifest takes the layer off every
/// show that already names it — the generator cannot be built, so the layer
/// draws its placeholder — and an operator opening last month's show would
/// find a hole in it with nothing to say why. Retiring costs nothing: these
/// leave the list, and [`manifest`] still answers for a show that has one.
///
/// The owner, 2026-10-12: "Was sowieso dann weg kann und mir keinen Mehrwert
/// bringt: Generator Countdown, Generator Clock". Both draw the time, and the
/// time is not what this system is for. And 2026-10-18: "Grid Generator wird
/// nicht benötigt und sollte entfernt sein, dadurch auch kein DMX haben" — a
/// desk is given no fixture type for a retired generator.
pub const RETIRED: [&str; 3] = ["ovs.clock", "ovs.countdown", "ovs.grid"];

/// Returns a published generator by type and version.
///
/// A show naming a version this build does not have is refused rather than
/// served the nearest one, for the same reason a fixture is: the nearest one
/// draws something, and nobody would know it was not what the show asked for.
#[must_use]
pub fn manifest(type_id: &str, version: u32) -> Option<&'static GeneratorManifest> {
    PUBLISHED
        .iter()
        .find(|manifest| manifest.type_id == type_id && manifest.version == version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_show_that_sets_nothing_gets_what_the_generator_was_published_with() {
        let values = ParameterMap::new();
        assert_eq!(
            SOLID_COLOUR_V1.value(&values, "colour"),
            Some(ParameterValue::Color([1.0, 1.0, 1.0, 1.0]))
        );
        assert!(SOLID_COLOUR_V1.problems(&values).is_empty());
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused() {
        // A colour parameter holding a number is a show written against
        // something else, and drawing *something* would be worse than saying so.
        let values = ParameterMap::from([("colour".to_owned(), ParameterValue::Float(0.5))]);
        let problems = SOLID_COLOUR_V1.problems(&values);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("colour"));
    }

    #[test]
    fn a_number_outside_its_range_is_refused_rather_than_clamped() {
        // Clamping is what a live control does with a fader that has gone too
        // far. This is a show being saved, where the right answer is to say
        // what is wrong.
        let values = ParameterMap::from([("spacing_px".to_owned(), ParameterValue::Float(4096.0))]);
        let problems = GRID_V1.problems(&values);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("between 4 and 512"));
    }

    #[test]
    fn a_parameter_the_generator_does_not_have_is_a_problem() {
        // Nearly always a typo or a show written against another version.
        // Ignoring it means an operator setting something and seeing nothing.
        let values = ParameterMap::from([("colur".to_owned(), ParameterValue::Color([0.0; 4]))]);
        let problems = SOLID_COLOUR_V1.problems(&values);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("no such parameter"));
    }

    #[test]
    fn a_colour_outside_the_unit_range_is_refused() {
        let values = ParameterMap::from([(
            "colour".to_owned(),
            ParameterValue::Color([2.0, 0.0, 0.0, 1.0]),
        )]);
        assert_eq!(SOLID_COLOUR_V1.problems(&values).len(), 1);
    }

    #[test]
    fn a_version_this_build_does_not_publish_is_not_found() {
        assert!(manifest("ovs.solid_colour", 1).is_some());
        assert!(manifest("ovs.solid_colour", 2).is_none());
        assert!(manifest("something.else", 1).is_none());
    }

    #[test]
    fn every_published_generator_declares_usable_defaults() {
        // A default that its own manifest would refuse would be a generator
        // that cannot be added to a show without being configured first.
        for manifest in &PUBLISHED {
            let values: ParameterMap = manifest
                .parameters
                .iter()
                .map(|parameter| (parameter.key.to_owned(), parameter.default_value()))
                .collect();
            assert!(
                manifest.problems(&values).is_empty(),
                "{} rejects its own defaults",
                manifest.type_id
            );
        }
    }

    #[test]
    fn every_published_number_has_a_range_a_slider_can_be_dragged_across() {
        // A client draws a number as a fader, which needs two distinct ends and
        // a handle somewhere between them. A range of zero width, or a default
        // outside it, is a control nobody can use.
        for manifest in &PUBLISHED {
            for parameter in manifest.parameters {
                if let ParameterKind::Number { min, max, default } = parameter.kind {
                    assert!(
                        min < max,
                        "{} {} has no range to drag",
                        manifest.type_id,
                        parameter.key
                    );
                    assert!(
                        (min..=max).contains(&default),
                        "{} {} starts outside its own range",
                        manifest.type_id,
                        parameter.key
                    );
                }
            }
        }
    }

    #[test]
    fn every_published_generator_is_found_by_the_type_and_version_a_show_stores() {
        for published in &PUBLISHED {
            assert_eq!(
                manifest(published.type_id, published.version),
                Some(published),
                "{} v{} is published but cannot be looked up",
                published.type_id,
                published.version
            );
        }
    }

    #[test]
    fn a_gradient_angle_outside_a_full_turn_is_refused() {
        let values = ParameterMap::from([("angle_deg".to_owned(), ParameterValue::Float(400.0))]);
        let problems = GRADIENT_V1.problems(&values);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("between 0 and 360"));
    }

    #[test]
    fn a_noise_amount_above_one_is_refused() {
        let values = ParameterMap::from([("amount".to_owned(), ParameterValue::Float(1.5))]);
        let problems = NOISE_V1.problems(&values);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("between 0 and 1"));
    }

    #[test]
    fn a_grid_is_a_separate_generator_from_the_service_pattern() {
        // One is content and one is a diagnostic. A show asking for a grid
        // behind a band must not get colour bars and a caption with it.
        assert_ne!(GRID_V1.type_id, TEST_PATTERN_V2.type_id);
        assert!(GRID_V1.parameter("caption").is_none());
        assert!(GRID_V1.parameter("line_colour").is_some());
    }

    #[test]
    fn a_clock_is_published_with_the_dial_that_cannot_be_misread_by_an_hour() {
        // What an operator gets by adding a clock and setting nothing: the
        // twenty-four hour dial, with seconds, so a crew at the side of a stage
        // reads it the way they would read a watch.
        let values = ParameterMap::new();
        assert_eq!(
            CLOCK_V1.value(&values, "hour_24"),
            Some(ParameterValue::Bool(true))
        );
        assert_eq!(
            CLOCK_V1.value(&values, "show_seconds"),
            Some(ParameterValue::Bool(true))
        );
        assert!(CLOCK_V1.problems(&values).is_empty());
    }

    #[test]
    fn a_clock_offset_outside_the_range_a_venue_can_be_in_is_refused() {
        // Minutes rather than hours, because the three-quarter-hour zones exist;
        // and bounded, because a clock eighteen hours out is a typo.
        let values =
            ParameterMap::from([("utc_offset_minutes".to_owned(), ParameterValue::Float(60.0))]);
        assert!(CLOCK_V1.problems(&values).is_empty(), "an hour east");

        let absurd = ParameterMap::from([(
            "utc_offset_minutes".to_owned(),
            ParameterValue::Float(1_080.0),
        )]);
        let problems = CLOCK_V1.problems(&absurd);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("between -720 and 840"));
    }

    #[test]
    fn a_countdown_longer_than_it_can_be_written_is_refused() {
        // It is drawn as hours, minutes and seconds, and a day does not fit in
        // those. Saying so on save beats drawing a number that has wrapped.
        let a_day =
            ParameterMap::from([("duration_s".to_owned(), ParameterValue::Float(86_400.0))]);
        let problems = COUNTDOWN_V1.problems(&a_day);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("between 1 and 86399"));

        let a_second_short =
            ParameterMap::from([("duration_s".to_owned(), ParameterValue::Float(86_399.0))]);
        assert!(COUNTDOWN_V1.problems(&a_second_short).is_empty());
    }

    #[test]
    fn a_countdown_is_published_stopping_at_zero_rather_than_running_on() {
        // Two different instruments. The one somebody reaches for without
        // configuring anything is the segment clock that stops.
        let values = ParameterMap::new();
        assert_eq!(
            COUNTDOWN_V1.value(&values, "count_past_zero"),
            Some(ParameterValue::Bool(false))
        );
        assert_eq!(
            COUNTDOWN_V1.value(&values, "from_trigger"),
            Some(ParameterValue::Bool(true))
        );
        assert_eq!(
            COUNTDOWN_V1.value(&values, "duration_s"),
            Some(ParameterValue::Float(600.0))
        );
    }

    #[test]
    fn a_clock_and_a_countdown_are_separate_generators() {
        // One says what time it is and the other says how long is left. A slot
        // asking for a countdown must not be able to receive a wall clock.
        assert_ne!(CLOCK_V1.type_id, COUNTDOWN_V1.type_id);
        assert!(CLOCK_V1.parameter("duration_s").is_none());
        assert!(COUNTDOWN_V1.parameter("hour_24").is_none());
        assert!(manifest("ovs.clock", 1).is_some());
        assert!(manifest("ovs.countdown", 1).is_some());
    }

    #[test]
    fn a_shape_is_published_as_an_element_rather_than_as_a_picture() {
        // The difference between this and the grid or the clock: those fill the
        // frame, and this is one thing meant to float over whatever the layer
        // below is playing. A published black bed would mean every operator's
        // first act was to turn it off.
        let values = ParameterMap::new();
        assert_eq!(
            SHAPE_V1.value(&values, "background_colour"),
            Some(ParameterValue::Color([0.0, 0.0, 0.0, 0.0]))
        );
        assert_eq!(
            SHAPE_V1.value(&values, "shape"),
            Some(ParameterValue::Choice("ellipse".to_owned())),
            "the ellipse, which is what somebody means by putting a shape up"
        );
        assert!(SHAPE_V1.problems(&values).is_empty());
    }

    #[test]
    fn a_shape_this_build_does_not_draw_is_refused_rather_than_rounded_to_one_it_does() {
        // What the number could not promise: 2.6 was a number in range, and
        // whichever shape it rounded to was the renderer's decision rather than
        // the operator's. A name is either one of the three or it is not.
        let values = ParameterMap::from([(
            "shape".to_owned(),
            ParameterValue::Choice("star".to_owned()),
        )]);
        let problems = SHAPE_V1.problems(&values);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("ellipse, rectangle, triangle"));

        // And a number is no longer a shape at all, however plausible it looks.
        let values = ParameterMap::from([("shape".to_owned(), ParameterValue::Float(1.0))]);
        assert_eq!(SHAPE_V1.problems(&values).len(), 1);

        for shape in ["ellipse", "rectangle", "triangle"] {
            let values = ParameterMap::from([(
                "shape".to_owned(),
                ParameterValue::Choice(shape.to_owned()),
            )]);
            assert!(SHAPE_V1.problems(&values).is_empty(), "{shape}");
        }
    }

    #[test]
    fn a_shape_of_no_size_is_a_look_it_has_been_taken_out_of_rather_than_a_mistake() {
        // Dragging the size to nothing removes the shape and keeps everything
        // else the operator set, which is the same reason the blur publishes a
        // radius of zero.
        let values = ParameterMap::from([
            ("width_px".to_owned(), ParameterValue::Float(0.0)),
            ("height_px".to_owned(), ParameterValue::Float(0.0)),
        ]);
        assert!(SHAPE_V1.problems(&values).is_empty());
    }

    #[test]
    fn a_shape_is_sized_in_the_same_pixels_as_every_other_size_in_the_system() {
        // Not a fraction of the canvas: the range runs to the largest frame this
        // build generates, which is a count of pixels and could not be anything
        // else. A shape is usually placed beside something else, and the one
        // element that moved when the composition was resized would be the one
        // measured differently from its neighbours.
        for key in ["width_px", "height_px"] {
            let Some(ParameterKind::Number { min, max, .. }) =
                SHAPE_V1.parameter(key).map(|parameter| parameter.kind)
            else {
                panic!("{key} is a size and has to be a number");
            };
            assert_eq!(min, 0.0, "{key}");
            assert_eq!(max, 8192.0, "{key}");
        }

        let wider_than_any_fraction =
            ParameterMap::from([("width_px".to_owned(), ParameterValue::Float(1920.0))]);
        assert!(SHAPE_V1.problems(&wider_than_any_fraction).is_empty());
    }

    #[test]
    fn no_two_published_generators_share_a_type_and_version() {
        for (index, first) in PUBLISHED.iter().enumerate() {
            for second in PUBLISHED.iter().skip(index + 1) {
                assert!(
                    first.type_id != second.type_id || first.version != second.version,
                    "{} v{} is published twice",
                    first.type_id,
                    first.version
                );
            }
        }
    }
}
