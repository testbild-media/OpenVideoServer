//! Effects: what they are, and what they can be asked for.
//!
//! An effect consumes a picture and produces one. That is the
//! whole contract: it sits after a slot's source or after a whole layer, and
//! what comes out of it is drawn where what went in would have been. It cannot
//! see other layers, it cannot see the composition, and it cannot decide when it
//! runs — which is what makes a chain of them predictable.
//!
//! # Why effects earn their place
//!
//! A rig plays a small number of clips at once, because every simultaneous clip
//! is a decoder. Effects multiply what those clips can look like without adding
//! one: four layers and a chain each is a great many looks from four
//! streams.
//!
//! # The same manifest rules as everything else
//!
//! Published once, never edited; a new parameter is a new version. A show
//! stores a type id and a version, and this build either has that exact thing or
//! says it does not. See [`crate::config::manifest`].
//!
//! # No effect master
//!
//! Deliberately. Each effect exposes its own parameters, and a chain
//! is ordered rather than mixed: there is no global "effects" fader to explain,
//! and nothing that silently scales what an operator set.

use crate::config::fixtures::{FixtureDefinition, ParameterSlot};
use crate::config::manifest::{Choice, DmxChannel, ParameterDefinition, ParameterKind};
use crate::config::slot::{ParameterMap, ParameterValue};
use crate::id::MediaItemId;

/// A published effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectManifest {
    /// Stable type identifier, for example `ovs.colour_correct`.
    pub type_id: &'static str,
    /// Version. A new parameter is a new version, never a changed one.
    pub version: u32,
    /// What a client's effect list shows.
    pub name: &'static str,
    /// One line describing what it does.
    pub description: &'static str,
    /// The parameters it declares.
    pub parameters: &'static [ParameterDefinition],
}

/// Every published effect's personality, built once.
///
/// Generating one leaks (`generated_personality` says why), so it is generated
/// once per effect rather than once per lookup — and a lookup is what the DMX
/// mapper does for every patched fixture on every frame.
static PERSONALITIES: std::sync::OnceLock<Vec<&'static FixtureDefinition>> =
    std::sync::OnceLock::new();

/// The personality a published effect describes, if it describes one.
#[must_use]
pub fn personality(type_id: &str, version: u32) -> Option<&'static FixtureDefinition> {
    PERSONALITIES
        .get_or_init(|| {
            PUBLISHED
                .iter()
                .filter_map(EffectManifest::personality)
                .collect()
        })
        .iter()
        .find(|definition| definition.type_id == type_id && definition.version == version)
        .copied()
}

impl EffectManifest {
    /// The personality this effect's own declarations describe.
    ///
    /// `None` when it exposes nothing to DMX, which most of what a plugin
    /// declares should be: a parameter nobody would ride during a show is a
    /// channel a patch has to find room for and an operator has to programme.
    #[must_use]
    pub fn personality(&'static self) -> Option<&'static FixtureDefinition> {
        crate::config::fixtures::generated_personality(
            self.type_id,
            self.version,
            self.name,
            self.parameters,
            ParameterSlot::Effect,
        )
    }

    /// Returns a parameter by key.
    #[must_use]
    pub fn parameter(&self, key: &str) -> Option<&'static ParameterDefinition> {
        crate::config::manifest::parameter(self.parameters, key)
    }

    /// Returns the value for a key, falling back to the declared default.
    #[must_use]
    pub fn value(&self, values: &ParameterMap, key: &str) -> Option<ParameterValue> {
        crate::config::manifest::value(self.parameters, values, key)
    }

    /// Returns the value for a key as a number, or the declared default.
    ///
    /// For the renderer, which wants a number rather than a tagged value: every
    /// numeric parameter has a default, so there is always an answer, and a
    /// value of the wrong type cannot reach here on a show that validated.
    #[must_use]
    pub fn number(&self, values: &ParameterMap, key: &str) -> f64 {
        match self.value(values, key) {
            Some(ParameterValue::Float(number)) => number,
            Some(ParameterValue::Int(number)) => number as f64,
            _ => 0.0,
        }
    }

    /// Returns the value for a key as a colour, or black where it is not one.
    #[must_use]
    pub fn colour(&self, values: &ParameterMap, key: &str) -> [f32; 4] {
        match self.value(values, key) {
            Some(ParameterValue::Color(colour)) => colour,
            _ => [0.0, 0.0, 0.0, 1.0],
        }
    }

    /// Returns the value for a key as a switch, or off where it is not one.
    #[must_use]
    pub fn flag(&self, values: &ParameterMap, key: &str) -> bool {
        matches!(self.value(values, key), Some(ParameterValue::Bool(true)))
    }

    /// Returns the value for a key as a choice's id, or an empty string.
    ///
    /// Empty only where the key is not a choice at all: a choice always has a
    /// default, so a show that validated against this manifest has an answer.
    #[must_use]
    pub fn choice(&self, values: &ParameterMap, key: &str) -> String {
        match self.value(values, key) {
            Some(ParameterValue::Choice(id)) => id,
            _ => String::new(),
        }
    }

    /// Returns every problem with a set of values.
    #[must_use]
    pub fn problems(&self, values: &ParameterMap) -> Vec<(String, String)> {
        crate::config::manifest::problems(
            self.parameters,
            &format!("{} version {}", self.type_id, self.version),
            values,
        )
    }

    /// Says why one value cannot be accepted for a key, or `None` when it can.
    ///
    /// The same rules [`problems`](Self::problems) applies to a show being
    /// saved, one value at a time, for the live path: a value of the wrong type,
    /// one outside its declared range, and one for a parameter this effect never
    /// declared are all refused.
    ///
    /// Live values are held to the show's rules rather than to a control
    /// source's, where out of range is clamped to the endpoint (house rule §17).
    /// The difference is what is on the other end. A DMX channel is eight bits
    /// that cannot express an impossible value in the first place, so clamping
    /// there only ever corrects a badly scaled fader. This is an API request,
    /// which can say anything, and a live control that accepted a blur radius of
    /// four thousand would be a way to stop a show from a client.
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

    /// Returns what one instance's parameters currently are: live over configured.
    ///
    /// `live` is what a control source has moved since the show was opened,
    /// or `None` for an instance nobody has touched.
    #[must_use]
    pub const fn live_over<'a>(
        &'a self,
        configured: &'a ParameterMap,
        live: Option<&'a ParameterMap>,
    ) -> EffectValues<'a> {
        EffectValues {
            manifest: self,
            configured,
            live,
        }
    }
}

/// What one effect instance's parameters currently are.
///
/// Three places are asked in order — what a control source has moved, what the
/// show configured, what the effect was published with — and the first answer
/// wins. Nothing is merged into a fourth map, deliberately: the renderer builds
/// one of these per effect per frame and the render path may not allocate
/// (house rule §5). Borrowing three references and choosing between them costs
/// nothing; merging two maps into a third would cost an allocation sixty times a
/// second for every effect on stage.
#[derive(Debug, Clone, Copy)]
pub struct EffectValues<'a> {
    manifest: &'a EffectManifest,
    configured: &'a ParameterMap,
    live: Option<&'a ParameterMap>,
}

impl<'a> EffectValues<'a> {
    /// Returns the effect these values belong to.
    #[must_use]
    pub const fn manifest(&self) -> &'a EffectManifest {
        self.manifest
    }

    /// Returns the value for a key, or the declared default.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<ParameterValue> {
        if let Some(value) = self.live.and_then(|live| live.get(key)) {
            // A live value cannot be for a parameter the effect does not
            // declare, or of the wrong type: it was checked against this same
            // manifest on the way in.
            return Some(value.clone());
        }
        self.manifest.value(self.configured, key)
    }

    /// Returns the value for a key as a number, or the declared default.
    #[must_use]
    pub fn number(&self, key: &str) -> f64 {
        match self.value(key) {
            Some(ParameterValue::Float(number)) => number,
            Some(ParameterValue::Int(number)) => number as f64,
            _ => 0.0,
        }
    }

    /// Returns the value for a key as a colour, or black where it is not one.
    #[must_use]
    pub fn colour(&self, key: &str) -> [f32; 4] {
        match self.value(key) {
            Some(ParameterValue::Color(colour)) => colour,
            _ => [0.0, 0.0, 0.0, 1.0],
        }
    }

    /// Returns the value for a key as a switch, or off where it is not one.
    #[must_use]
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.value(key), Some(ParameterValue::Bool(true)))
    }

    /// Returns the value for a key as a choice's id, or an empty string.
    #[must_use]
    pub fn choice(&self, key: &str) -> String {
        match self.value(key) {
            Some(ParameterValue::Choice(id)) => id,
            _ => String::new(),
        }
    }

    /// Returns the media item a picture parameter names, where it names one.
    ///
    /// The parse is the cost of keeping the value a string
    /// ([`ParameterKind::Image`]). It is twenty-six characters of base32 and
    /// allocates nothing, and it happens once per effect per frame rather than
    /// per pixel — but it is a parse on the render path, so if a second
    /// parameter of this kind ever appears it is worth measuring rather than
    /// assuming.
    #[must_use]
    pub fn media(&self, key: &str) -> Option<MediaItemId> {
        match self.value(key) {
            Some(ParameterValue::Text(name)) => name.parse().ok(),
            _ => None,
        }
    }
}

/// Brightness, contrast, saturation and gamma.
///
/// The one effect every rig needs, because walls do not match: an LED panel run
/// next to a projector, or two panel batches from different years, are corrected
/// here rather than in the content. Every parameter is neutral at its default,
/// so adding this effect and touching nothing changes nothing.
pub const COLOUR_CORRECT_V1: EffectManifest = EffectManifest {
    type_id: "ovs.colour_correct",
    version: 1,
    name: "Colour Correct",
    description: "Brightness, contrast, saturation and gamma.",
    parameters: &[
        ParameterDefinition {
            key: "brightness",
            name: "Brightness",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "contrast",
            name: "Contrast",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "saturation",
            name: "Saturation",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "gamma",
            name: "Gamma",
            kind: ParameterKind::Number {
                min: 0.1,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// Neutral daylight, in kelvin: the temperature at which this changes nothing.
///
/// D65, which is what Rec. 709 is mastered against and therefore what every
/// picture reaching this is already balanced for. The gains are worked out
/// relative to it, so the published default is exactly no change rather than
/// nearly none.
pub const NEUTRAL_KELVIN: f64 = 6500.0;

/// Brightness, contrast, saturation, gamma, and then the white point.
///
/// **Version two adds temperature and tint, after the four that were already
/// here** (owner, 2026-10-12: "beides passiert erst nach Saturation und
/// Gamma"). The order is not a preference: brightness and contrast move levels
/// and gamma bends them, and a white point applied before those would be
/// stretched by them. Applied after, it is what an operator means by it —
/// the picture, warmed.
///
/// **Which is also the end of the Tint effect**, and the two senses of that
/// word are worth keeping apart. Tint here is the green–magenta axis, the one
/// that sits beside temperature on every grading desk and corrects the cast a
/// panel or a camera has. The *effect* called Tint pushed a whole picture
/// towards one colour, which is an artistic choice rather than a correction,
/// and that is what the Colorize effect is for.
///
/// Temperature runs from 2000 K to 10000 K: candlelight at one end, a heavily
/// overcast sky at the other, and everything a venue actually contains in
/// between. **The ends are mine**, picked to cover what a room does rather
/// than measured from anything.
pub const COLOUR_CORRECT_V2: EffectManifest = EffectManifest {
    type_id: "ovs.colour_correct",
    version: 2,
    name: "Colour Correct",
    description: "Brightness, contrast, saturation, gamma, and the white point.",
    parameters: &[
        ParameterDefinition {
            key: "brightness",
            name: "Brightness",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "contrast",
            name: "Contrast",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "saturation",
            name: "Saturation",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "gamma",
            name: "Gamma",
            kind: ParameterKind::Number {
                min: 0.1,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "temperature_k",
            name: "Temperature",
            kind: ParameterKind::Number {
                min: 2000.0,
                max: 10000.0,
                default: NEUTRAL_KELVIN,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "tint",
            name: "Tint",
            kind: ParameterKind::Number {
                min: -1.0,
                max: 1.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// Pushes a picture towards one colour.
///
/// What turns four clips into eight looks: the same content, one in the house
/// colour and one not. Separate from the correction above because it is an
/// artistic choice made during a show, not a calibration made once.
pub const TINT_V1: EffectManifest = EffectManifest {
    type_id: "ovs.tint",
    version: 1,
    name: "Tint",
    description: "Mixes the picture towards one colour.",
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
            key: "amount",
            name: "Amount",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "preserve_luma",
            name: "Keep brightness",
            kind: ParameterKind::Toggle { default: true },
            dmx: Some(DmxChannel::Coarse),
        },
    ],
};

/// Squares.
///
/// Cheap, and it reads from the back of a room where subtler things do not.
/// Sized in composition pixels rather than as a fraction, so a look set up on a
/// 1920-wide composition means the same thing after the pieces are mapped onto a
/// wall of a different size.
pub const PIXELATE_V1: EffectManifest = EffectManifest {
    type_id: "ovs.pixelate",
    version: 1,
    name: "Pixelate",
    description: "Averages the picture into square blocks.",
    parameters: &[ParameterDefinition {
        key: "block_px",
        name: "Block size",
        kind: ParameterKind::Number {
            min: 1.0,
            max: 256.0,
            default: 1.0,
        },
        dmx: Some(DmxChannel::Fine),
    }],
};

/// A separable blur.
///
/// Two passes, horizontal then vertical, because a square kernel costs the
/// square of what two lines cost and looks the same. A radius of zero is the
/// default and does nothing, so the effect can sit in a chain switched off in
/// all but name.
pub const BLUR_V1: EffectManifest = EffectManifest {
    type_id: "ovs.blur",
    version: 1,
    name: "Blur",
    description: "Softens the picture, horizontally and vertically.",
    parameters: &[
        ParameterDefinition {
            key: "radius_px",
            name: "Radius",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 64.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "amount",
            name: "Amount",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// Four edges off the picture.
///
/// What somebody reaches for when a camera feed has a letterbox in it, when a
/// capture carries a strip of somebody's desktop along one edge, or when a
/// piece of content has to be made to fit a panel it was not cut for. The
/// alternative in the room is asking whoever sent it to send it again.
///
/// Sized in composition pixels and running to 8192, which is the same range
/// and the same reason as the shape generator's size: a look set up on one
/// composition means the same thing after the pieces are mapped onto a wall of
/// another size, and an edge measured as a fraction would be the one element
/// that moved when somebody resized the composition.
///
/// What is cut away is cleared rather than blacked, so whatever is behind the
/// layer shows through there. Trimming a layer is about letting the layer
/// under it be seen; a black bar would be a second piece of content.
///
/// All four at zero is the published default, and it runs its pass anyway --
/// one full-screen read and write that returns its own input. Skipping it was
/// tried and taken back out: the renderer proves every published effect
/// neutral at its defaults by rendering it and comparing the picture, and an
/// effect that appends no pass is never asked. The blur pays the same at a
/// radius of zero, so this is the house price rather than a new one.
pub const CROP_V1: EffectManifest = EffectManifest {
    type_id: "ovs.crop",
    version: 1,
    name: "Crop",
    description: "Cuts a number of pixels off each edge.",
    parameters: &[
        ParameterDefinition {
            key: "left_px",
            name: "Left",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 8192.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "right_px",
            name: "Right",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 8192.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "top_px",
            name: "Top",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 8192.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "bottom_px",
            name: "Bottom",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 8192.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// Every effect this build publishes.
pub const PUBLISHED: [EffectManifest; 12] = [
    // Both versions, and the catalogue offers the newer: a show saved against
    // version one opens with the four controls it was saved with.
    COLOUR_CORRECT_V1,
    COLOUR_CORRECT_V2,
    TINT_V1,
    PIXELATE_V1,
    BLUR_V1,
    CROP_V1,
    MIRROR_V1,
    COLORIZE_V1,
    RGB_CORRECT_V1,
    RGB_SPLIT_V1,
    TV_GLITCH_V1,
    MASK_V1,
];

/// A picture from the library, used as a stencil.
///
/// The owner, 2026-10-13: "einen effekt namens mask wo man eine schwarz weiss
/// maske als bild hochladen kann und das ausschneidet oder multipliziert, dann
/// koennten es auch graustufen sein."
///
/// So: grey is not a special case, it is the point. The mask's brightness is
/// read as a number from nought to one and used whole, which makes a soft edge
/// in the picture wherever there is a soft edge in the mask.
///
/// **The two modes are not the same picture**, which is why they are a choice
/// rather than one behaviour. Multiplying darkens towards black and leaves the
/// layer covering what is under it; cutting out lowers the layer's own opacity,
/// so whatever is on the layer underneath comes through the hole. A hole in a
/// top layer is the usual reason to reach for this, and a vignette is the
/// usual reason for the other.
///
/// **It is stretched over the composition**, both axes independently, whatever
/// its own size. A mask is made for the picture it masks: one authored at the
/// composition's own size lands pixel for pixel, and one authored at any other
/// size still covers the same thing. Fitting rules that keep the proportions
/// would put a mask's own edge somewhere in the picture, which is a mask with
/// an edge nobody drew.
///
/// It starts with no picture, and with no picture it does nothing at all —
/// not a pass that reads a white texture, but no pass. That is what makes it
/// neutral at its published defaults, like every other effect here, on a
/// machine where nothing has been uploaded yet.
pub const MASK_V1: EffectManifest = EffectManifest {
    type_id: "ovs.mask",
    version: 1,
    name: "Mask",
    description: "A picture from the library, multiplied in or cut out.",
    parameters: &[
        ParameterDefinition {
            key: "image",
            name: "Picture",
            kind: ParameterKind::Image,
            // Not drivable from a console: a desk can send a number, and
            // which picture is in a show is not one.
            dmx: None,
        },
        ParameterDefinition {
            key: "mode",
            name: "Mode",
            kind: ParameterKind::Choice {
                choices: &[
                    Choice {
                        id: "multiply",
                        name: "Multiply",
                    },
                    Choice {
                        id: "cut_out",
                        name: "Cut out",
                    },
                ],
                default: "multiply",
            },
            dmx: None,
        },
        ParameterDefinition {
            key: "invert",
            name: "Invert",
            kind: ParameterKind::Toggle { default: false },
            dmx: None,
        },
        ParameterDefinition {
            key: "amount",
            name: "Amount",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// Folds one half of the picture onto the other.
///
/// An artistic effect, in the owner's words, and the one on this list with no
/// corrective use at all: what it makes is a kaleidoscope, and with both axes
/// on it is a quadrant mirrored twice.
///
/// Each axis says which half to keep rather than merely whether to fold,
/// because the two answers are different pictures and an operator wants both:
/// a logo in the left third and a logo in the right third survive opposite
/// choices.
pub const MIRROR_V1: EffectManifest = EffectManifest {
    type_id: "ovs.mirror",
    version: 1,
    name: "Mirror",
    description: "Folds one half of the picture onto the other.",
    parameters: &[
        ParameterDefinition {
            key: "horizontal",
            name: "Horizontal",
            kind: ParameterKind::Choice {
                choices: &[
                    Choice {
                        id: "off",
                        name: "Off",
                    },
                    Choice {
                        id: "left",
                        name: "Keep left",
                    },
                    Choice {
                        id: "right",
                        name: "Keep right",
                    },
                ],
                default: "off",
            },
            dmx: Some(DmxChannel::Coarse),
        },
        ParameterDefinition {
            key: "vertical",
            name: "Vertical",
            kind: ParameterKind::Choice {
                choices: &[
                    Choice {
                        id: "off",
                        name: "Off",
                    },
                    Choice {
                        id: "top",
                        name: "Keep top",
                    },
                    Choice {
                        id: "bottom",
                        name: "Keep bottom",
                    },
                ],
                default: "off",
            },
            dmx: Some(DmxChannel::Coarse),
        },
    ],
};

/// Throws away the colour and paints the brightness back in.
///
/// The picture's own luma carries the picture; a hue and a saturation carry
/// the colour. This is where the Tint effect's job went when it was retired —
/// pushing a whole picture towards one colour is an artistic choice, and this
/// is the control for it (owner, 2026-10-12).
///
/// Saturation at zero is the picture in monochrome, which is worth having on
/// its own and is the honest bottom end of "colour it in" rather than a dead
/// position on the control.
///
/// `amount` starts at zero, so adding this and touching nothing changes
/// nothing — the promise every effect here makes.
pub const COLORIZE_V1: EffectManifest = EffectManifest {
    type_id: "ovs.colorize",
    version: 1,
    name: "Colorize",
    description: "Keeps the brightness and paints one colour over it.",
    parameters: &[
        ParameterDefinition {
            key: "hue",
            name: "Hue",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 360.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "saturation",
            name: "Saturation",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "amount",
            name: "Amount",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// A black level and a white level, per channel.
///
/// Six controls: where each channel's black sits and where its white sits,
/// which is the oldest correction there is and the one that matches two panel
/// batches to each other. Black to `lift`, white to `gain`, and a straight line
/// between — so lift at zero and gain at one is the picture it was given.
///
/// Separate from Colour Correct because it is a different question. That one
/// asks what the picture should look like; this one asks what the panel is
/// doing to it.
pub const RGB_CORRECT_V1: EffectManifest = EffectManifest {
    type_id: "ovs.rgb_correct",
    version: 1,
    name: "RGB Correct",
    description: "A black level and a white level for each channel.",
    parameters: &[
        ParameterDefinition {
            key: "lift_r",
            name: "Red black",
            kind: ParameterKind::Number {
                min: -1.0,
                max: 1.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "lift_g",
            name: "Green black",
            kind: ParameterKind::Number {
                min: -1.0,
                max: 1.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "lift_b",
            name: "Blue black",
            kind: ParameterKind::Number {
                min: -1.0,
                max: 1.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "gain_r",
            name: "Red white",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "gain_g",
            name: "Green white",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "gain_b",
            name: "Blue white",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 4.0,
                default: 1.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// Pulls the channels apart along a line.
///
/// Red one way, blue the other, green where it was: what a lens does at its
/// edges and what a misaligned tube did all the time, and a look somebody
/// reaches for deliberately now.
///
/// In composition pixels and at an angle, so the same look survives being
/// mapped onto a wall of another size — the reason every size in this system
/// is in those pixels.
pub const RGB_SPLIT_V1: EffectManifest = EffectManifest {
    type_id: "ovs.rgb_split",
    version: 1,
    name: "RGB Split",
    description: "Offsets the red and blue channels in opposite directions.",
    parameters: &[
        ParameterDefinition {
            key: "offset_px",
            name: "Offset",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 256.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "angle_deg",
            name: "Angle",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 360.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// A picture coming apart the way a tube did.
///
/// Three faults an old set had, each with its own control, under one master:
/// lines that jump sideways now and then, snow over the picture, and an error
/// band travelling down it. The master is what an operator rides during a
/// show; the three underneath are how the look is set up beforehand.
///
/// **The first effect here that moves on its own.** It reads the clock a pass
/// now carries — free-running, shared by every pass in a frame, and reached by
/// nothing in the show. A glitch is a look rather than a cue, so the transport
/// and the master speed do not touch it, and two layers carrying it move
/// together.
///
/// The master starts at zero, so adding it changes nothing until somebody asks
/// for it; the three faults start half up, so that one control brings the
/// whole look in at once.
pub const TV_GLITCH_V1: EffectManifest = EffectManifest {
    type_id: "ovs.tv_glitch",
    version: 1,
    name: "TV Glitch",
    description: "Line jumps, snow and a rolling error, under one master.",
    parameters: &[
        ParameterDefinition {
            key: "amount",
            name: "Amount",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 0.0,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "tear",
            name: "Line jumps",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 0.5,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "noise",
            name: "Snow",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 0.5,
            },
            dmx: Some(DmxChannel::Fine),
        },
        ParameterDefinition {
            key: "roll",
            name: "Rolling error",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 1.0,
                default: 0.5,
            },
            dmx: Some(DmxChannel::Fine),
        },
    ],
};

/// The types an effect list no longer offers.
///
/// **Retired, not deleted**, for the reason [`crate::config::generators::RETIRED`]
/// gives: a show that already names one keeps working, because [`manifest`]
/// still answers for it.
///
/// Tint pushed a whole picture towards one colour. That is an artistic choice
/// rather than a correction, and it is what the Colorize effect is for — which
/// is the distinction that made it worth retiring rather than folding into
/// Colour Correct, where "tint" means the green–magenta axis instead (owner,
/// 2026-10-12).
pub const RETIRED: [&str; 1] = ["ovs.tint"];

/// Returns a published effect by type and version.
///
/// A show naming a version this build does not have is refused rather than
/// served the nearest one: the nearest one would draw something, and nobody
/// would know it was not what the show asked for.
#[must_use]
pub fn manifest(type_id: &str, version: u32) -> Option<&'static EffectManifest> {
    PUBLISHED
        .iter()
        .find(|manifest| manifest.type_id == type_id && manifest.version == version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_effect_that_was_added_and_not_touched_changes_nothing() {
        // The promise an operator is given by an effect list: adding one and
        // leaving it alone cannot alter a show that was already right.
        let nothing = ParameterMap::new();
        assert_eq!(COLOUR_CORRECT_V1.number(&nothing, "brightness"), 1.0);
        assert_eq!(COLOUR_CORRECT_V1.number(&nothing, "contrast"), 1.0);
        assert_eq!(COLOUR_CORRECT_V1.number(&nothing, "saturation"), 1.0);
        assert_eq!(COLOUR_CORRECT_V1.number(&nothing, "gamma"), 1.0);
        assert_eq!(TINT_V1.number(&nothing, "amount"), 0.0);
        assert_eq!(PIXELATE_V1.number(&nothing, "block_px"), 1.0);
        assert_eq!(BLUR_V1.number(&nothing, "radius_px"), 0.0);
    }

    #[test]
    fn a_version_this_build_does_not_have_is_refused() {
        assert!(manifest("ovs.blur", 1).is_some());
        assert!(manifest("ovs.blur", 2).is_none());
        assert!(manifest("ovs.kaleidoscope", 1).is_none());
    }

    #[test]
    fn every_published_effect_has_a_distinct_identity() {
        // Two manifests answering to one name is a show loading whichever
        // came first in an array, which is not a decision anybody made.
        for (index, manifest) in PUBLISHED.iter().enumerate() {
            for other in &PUBLISHED[index + 1..] {
                assert!(
                    manifest.type_id != other.type_id || manifest.version != other.version,
                    "{} version {} is published twice",
                    manifest.type_id,
                    manifest.version
                );
            }
        }
    }

    #[test]
    fn every_published_effect_declares_what_a_client_needs_to_draw_it() {
        for manifest in &PUBLISHED {
            assert!(manifest.type_id.starts_with("ovs."), "{}", manifest.type_id);
            assert!(manifest.version >= 1);
            assert!(!manifest.name.trim().is_empty());
            assert!(!manifest.description.trim().is_empty());
            for parameter in manifest.parameters {
                assert!(!parameter.key.trim().is_empty());
                assert!(!parameter.name.trim().is_empty());
            }
        }
    }

    #[test]
    fn a_parameter_the_effect_never_declared_is_a_problem() {
        let values = ParameterMap::from([("radius_px".to_owned(), ParameterValue::Float(4.0))]);
        let problems = COLOUR_CORRECT_V1.problems(&values);
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].1.contains("ovs.colour_correct version 1"),
            "{problems:?}"
        );
    }

    #[test]
    fn a_live_value_sits_on_top_of_the_configured_one_without_replacing_it() {
        // The whole relationship in one assertion: the show's map is what it
        // was afterwards, so closing the show and opening it again gives the
        // configured value back.
        let configured = ParameterMap::from([("radius_px".to_owned(), ParameterValue::Float(8.0))]);
        let live = ParameterMap::from([("radius_px".to_owned(), ParameterValue::Float(40.0))]);

        let values = BLUR_V1.live_over(&configured, Some(&live));
        assert_eq!(values.number("radius_px"), 40.0);
        assert_eq!(
            configured.get("radius_px"),
            Some(&ParameterValue::Float(8.0)),
            "the show is not what a live value writes to"
        );
    }

    #[test]
    fn a_parameter_no_live_value_names_stays_where_the_show_put_it() {
        // An operator moving one control must not quietly return the others to
        // their published defaults.
        let configured = ParameterMap::from([
            ("radius_px".to_owned(), ParameterValue::Float(8.0)),
            ("amount".to_owned(), ParameterValue::Float(0.25)),
        ]);
        let live = ParameterMap::from([("radius_px".to_owned(), ParameterValue::Float(1.0))]);

        let values = BLUR_V1.live_over(&configured, Some(&live));
        assert_eq!(values.number("radius_px"), 1.0);
        assert_eq!(values.number("amount"), 0.25);
    }

    #[test]
    fn an_instance_with_no_live_values_is_exactly_the_configured_one() {
        let configured = ParameterMap::from([("amount".to_owned(), ParameterValue::Float(0.5))]);
        let values = TINT_V1.live_over(&configured, None);

        assert_eq!(values.number("amount"), 0.5);
        assert_eq!(values.colour("colour"), [1.0, 1.0, 1.0, 1.0], "published");
        assert!(values.flag("preserve_luma"), "published");
    }

    #[test]
    fn a_live_value_outside_the_declared_range_is_refused() {
        // A live control that could put a blur radius at four thousand would be
        // a way to stop a show from a client.
        assert!(BLUR_V1
            .refuse("radius_px", &ParameterValue::Float(4_000.0))
            .is_some());
        assert!(BLUR_V1
            .refuse("radius_px", &ParameterValue::Float(f64::NAN))
            .is_some());
        assert!(BLUR_V1
            .refuse("radius_px", &ParameterValue::Float(12.0))
            .is_none());
    }

    #[test]
    fn a_live_value_for_a_parameter_the_effect_never_declared_is_refused() {
        let reason = BLUR_V1
            .refuse("brightness", &ParameterValue::Float(1.0))
            .expect("a blur has no brightness");
        assert!(reason.contains("ovs.blur version 1"), "{reason}");
    }

    #[test]
    fn a_live_value_of_the_wrong_type_is_refused() {
        assert!(BLUR_V1
            .refuse("radius_px", &ParameterValue::Bool(true))
            .is_some());
        assert!(TINT_V1
            .refuse("colour", &ParameterValue::Float(0.5))
            .is_some());
        assert!(TINT_V1
            .refuse("preserve_luma", &ParameterValue::Bool(false))
            .is_none());
    }

    #[test]
    fn a_colour_reads_back_as_a_colour() {
        let values = ParameterMap::from([(
            "colour".to_owned(),
            ParameterValue::Color([1.0, 0.0, 0.5, 1.0]),
        )]);
        assert_eq!(TINT_V1.colour(&values, "colour"), [1.0, 0.0, 0.5, 1.0]);
        assert!(TINT_V1.flag(&values, "preserve_luma"), "the default is on");
    }
}
