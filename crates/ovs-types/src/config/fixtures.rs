//! DMX fixture personalities.
//!
//! A fixture definition is what a lighting console has programmed into its show
//! file: a stable type identifier, a version, a footprint and a channel layout.
//! It is published, and once published it never changes — an operator patched a
//! console against version 1 and that console will keep sending version 1
//! forever, whatever this build thinks. A new control means a new
//! version, never an edited one.
//!
//! # Definitions are transport-blind
//!
//! Nothing here knows whether a value arrived over Art-Net or sACN. The two
//! protocols feed the same universes, and a fixture that behaved differently
//! depending on how the packet reached the machine would be a fixture nobody
//! could reason about.
//!
//! # And they only ever produce parameters
//!
//! A channel decodes to a [`Parameter`] and a value. It does not call anything.
//! Everything live-controllable goes through one typed model, so
//! that a console, a client and a script all reach the show the same way and a
//! new transport is a new source of parameters rather than a new path into the
//! renderer.

use serde::{Deserialize, Serialize};

use crate::config::manifest::{DmxChannel, ParameterDefinition, ParameterKind};
use crate::config::patch::FixtureTarget;
use crate::id::{EffectInstanceId, GeneratorInstanceId, LayerId};

/// Something a control source can change.
///
/// The whole of what is live-controllable, as one type. A value that is not
/// here cannot be driven from a console, and that is the point: the list is
/// short, deliberate and the same for every transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "parameter", rename_all = "snake_case")]
pub enum Parameter {
    /// Master dimmer, applied after the buses are combined.
    MasterDimmer,
    /// Global A/B crossfader.
    MasterCrossfader,
    /// The one master tempo.
    MasterBpm,
    /// Global speed scale.
    MasterSpeedScale,
    /// Whether the visible output is held.
    MasterFreeze,
    /// One tap of the tempo.
    MasterTapTempo,

    /// One layer's live opacity.
    LayerOpacity {
        /// The layer.
        layer_id: LayerId,
    },
    /// Which slot a layer is showing.
    LayerSlot {
        /// The layer.
        layer_id: LayerId,
    },
    /// Start the layer's source again.
    LayerTrigger {
        /// The layer.
        layer_id: LayerId,
    },
    /// Horizontal offset, as a fraction of half the composition.
    LayerPositionX {
        /// The layer.
        layer_id: LayerId,
    },
    /// Vertical offset, as a fraction of half the composition.
    LayerPositionY {
        /// The layer.
        layer_id: LayerId,
    },
    /// Horizontal scale.
    ///
    /// Two axes rather than one. The earlier fixture had a single uniform
    /// scale and an argument for it — that an operator reaching for scale wants
    /// the layer bigger rather than stretched — and the owner's layout has
    /// both, which is what a fixture driving a mapped wall needs.
    LayerScaleX {
        /// The layer.
        layer_id: LayerId,
    },
    /// Vertical scale.
    LayerScaleY {
        /// The layer.
        layer_id: LayerId,
    },
    /// Clockwise rotation in degrees.
    LayerRotation {
        /// The layer.
        layer_id: LayerId,
    },
    /// One parameter of one effect instance.
    ///
    /// Unlike the variants above, what this names is not fixed by this crate:
    /// an effect is a plugin and its parameters are whatever its manifest
    /// declares. The key is the manifest's own.
    EffectParameter {
        /// Which instance in which chain.
        effect_instance_id: EffectInstanceId,
        /// The manifest key it moves.
        key: &'static str,
        /// Which part of a colour, when the parameter is one.
        component: Option<ColourPart>,
    },
    /// One parameter of one generator instance.
    ///
    /// The same as [`EffectParameter`](Self::EffectParameter) for the other
    /// kind of module.
    GeneratorParameter {
        /// Which instance in which slot.
        generator_instance_id: GeneratorInstanceId,
        /// The manifest key it moves.
        key: &'static str,
        /// Which part of a colour, when the parameter is one.
        component: Option<ColourPart>,
    },
}

impl Parameter {
    /// A stable machine name for this parameter.
    ///
    /// Separate from what a console shows, which belongs to the channel in a
    /// personality: two personalities may label the same parameter differently,
    /// and a client matching on the label would break the first time one of
    /// them was reworded.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::MasterDimmer => "master_dimmer",
            Self::MasterCrossfader => "master_crossfader",
            Self::MasterBpm => "master_bpm",
            Self::MasterSpeedScale => "master_speed_scale",
            Self::MasterFreeze => "master_freeze",
            Self::MasterTapTempo => "master_tap_tempo",
            Self::LayerOpacity { .. } => "layer_opacity",
            Self::LayerSlot { .. } => "layer_slot",
            Self::LayerTrigger { .. } => "layer_trigger",
            Self::LayerPositionX { .. } => "layer_position_x",
            Self::LayerPositionY { .. } => "layer_position_y",
            Self::LayerScaleX { .. } => "layer_scale_x",
            Self::LayerScaleY { .. } => "layer_scale_y",
            Self::LayerRotation { .. } => "layer_rotation",
            // The manifest's own key, which is what the rest of the system
            // already calls it. A composed name would have to be built at
            // runtime, and this is the one thing here that is never a constant.
            Self::EffectParameter { key, .. } | Self::GeneratorParameter { key, .. } => key,
        }
    }

    /// Returns true when this parameter is fired rather than set.
    ///
    /// Two of them are moments, not values: a tap and a layer trigger. A
    /// transport turns a channel *crossing* its threshold into one of
    /// these arriving at all, so a momentary parameter reaching
    /// [`crate::config::Parameter`]'s consumer means "it happened", never "it is
    /// still on". Without that distinction a console holding a trigger channel
    /// high would retrigger its layer forty-four times a second, and the layer
    /// would sit on its first frame for as long as the operator's finger was
    /// down.
    #[must_use]
    pub const fn is_momentary(self) -> bool {
        matches!(self, Self::MasterTapTempo | Self::LayerTrigger { .. })
    }
}

/// A decoded channel value, in the parameter's own terms.
///
/// Named for the channel rather than the parameter, because
/// [`crate::config::ParameterValue`] already means something else: what an
/// effect's manifest declares. One is a byte off the wire made sense of; the
/// other is a number in a show file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChannelValue {
    /// A number, already in the range the parameter uses.
    Number(f64),
    /// A whole number, such as a slot index.
    Index(u16),
    /// On or off.
    Flag(bool),
}

/// How a channel's bytes become a value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChannelEncoding {
    /// One channel across a range, `0` to `255`.
    Ranged8 {
        /// The value at `0`.
        min: f64,
        /// The value at `255`.
        max: f64,
    },
    /// Two channels, coarse then fine, across a range.
    ///
    /// Sixteen bits for anything an operator fades by hand. Eight bits of
    /// dimmer is 256 steps across a whole fade, and the steps are visible on a
    /// wall as banding in the last few percent.
    Ranged16 {
        /// The value at `0`.
        min: f64,
        /// The value at `65535`.
        max: f64,
    },
    /// One channel as a whole number, `0` to `255`.
    Index8,
    /// One channel as a switch.
    ///
    /// Below the threshold is off. A console's "on" is rarely exactly 255 and
    /// an operator's fader is never exactly anything, so a band is what this
    /// has to be.
    Switch {
        /// The lowest value that counts as on.
        threshold: u8,
    },
    /// One channel divided into three-value bands, one per slot.
    ///
    /// The bottom four values are Black, and every slot after that owns three:
    /// slot 1 is 4–6, slot 2 is 7–9, and slot 84 is 253–255. Three rather than
    /// one because a desk's fader does not land on a number — an operator
    /// nudging a wheel past a slot would otherwise skip it, and a console
    /// rounding a percentage to a byte would select whichever slot it rounded
    /// to.
    ///
    /// Eighty-four is what a channel holds at three values each, and it is also
    /// what the visible matrix reaches: seven banks of twelve (Q4).
    SlotBands,
    /// One channel divided into bands, one per master scale step.
    ///
    /// The show's own speed, and the only speed there is: six steps from an
    /// eighth to four times, each a doubling ([`MASTER_SCALE_STEPS`]). Layers
    /// have no rate of their own — a slot either runs free at 1x or is locked to
    /// a number of beats, and this scales the tempo those beats are counted
    /// against (owner, 2026-09-23).
    ScaleLadder,
    /// One channel divided into this many even bands, one per named choice.
    ///
    /// The generic form of the ladders above, for a plugin's own list: a shape
    /// generator's three shapes are three bands of eighty-five values, and an
    /// eight-choice list is eight of thirty-two. Which choice a band means is
    /// the manifest's business — this answers with the band's number and the
    /// caller looks it up, because a fixture definition holds no plugin's
    /// vocabulary.
    Choices {
        /// How many there are. Never zero; a manifest with an empty choice list
        /// is refused by the catalogue's own test.
        count: u16,
    },
}

/// The master scale steps, in the order a fixture's bands run.
///
/// Six, each twice the one below it, with `1x` at the fourth — so a desk at
/// half travel is at the show's own tempo, and the two ends are the slowest
/// and the fastest the server will run.
///
/// It was eight, topping out at sixteen. The owner capped it at four on
/// 2026-09-23, from the wall rather than from the specification: a clip asked
/// to play faster than that stutters whatever it is encoded in, so travel
/// above four is travel that only produces a worse picture. Six bands over 256
/// values is 42 or 43 apiece, which puts `1x` at 128 — the one value on a
/// channel anybody can find without counting.
pub const MASTER_SCALE_STEPS: [f64; 6] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0];

/// The lowest DMX value that selects a slot rather than Black.
const FIRST_SLOT_VALUE: u8 = 4;

/// How many DMX values each slot owns.
const VALUES_PER_SLOT: u8 = 3;

/// The highest slot a single channel can address at [`VALUES_PER_SLOT`] each.
pub const MAX_ADDRESSABLE_SLOT: u16 = 84;

impl ChannelEncoding {
    /// Returns how many channels this encoding occupies.
    #[must_use]
    pub const fn width(self) -> u16 {
        match self {
            Self::Ranged16 { .. } => 2,
            _ => 1,
        }
    }

    /// Decodes this channel from a fixture's slice of a universe.
    ///
    /// `channels` starts at the fixture's first channel. Returns `None` when the
    /// slice is shorter than the channel needs, which means a universe arrived
    /// truncated — legal in both protocols, and not a reason to guess.
    #[must_use]
    pub fn decode(self, channels: &[u8], offset: u16) -> Option<ChannelValue> {
        let index = usize::from(offset.checked_sub(1)?);
        let coarse = *channels.get(index)?;

        match self {
            Self::Ranged8 { min, max } => Some(ChannelValue::Number(lerp(
                min,
                max,
                f64::from(coarse) / 255.0,
            ))),
            Self::Ranged16 { min, max } => {
                let fine = *channels.get(index + 1)?;
                let raw = (u32::from(coarse) << 8) | u32::from(fine);
                Some(ChannelValue::Number(lerp(
                    min,
                    max,
                    f64::from(raw) / 65_535.0,
                )))
            }
            Self::Index8 => Some(ChannelValue::Index(u16::from(coarse))),
            Self::Switch { threshold } => Some(ChannelValue::Flag(coarse >= threshold)),
            Self::SlotBands => Some(ChannelValue::Index(slot_of(coarse))),
            Self::Choices { count } => {
                let count = count.max(1);
                let band = (u32::from(coarse) * u32::from(count) / 256).min(u32::from(count) - 1);
                Some(ChannelValue::Index(band as u16))
            }
            Self::ScaleLadder => Some(ChannelValue::Number(band(coarse, MASTER_SCALE_STEPS))),
        }
    }
}

/// Returns the slot a channel value selects, with Black at the bottom.
///
/// Not [`band`]: the slots are not even bands across the channel. The bottom
/// four values are Black and every slot after that owns exactly three, which is
/// what makes slot 1 land on the same numbers whether a personality addresses
/// twelve slots or eighty-four.
const fn slot_of(value: u8) -> u16 {
    if value < FIRST_SLOT_VALUE {
        return 0;
    }
    let slot = (value - FIRST_SLOT_VALUE) / VALUES_PER_SLOT + 1;
    if slot as u16 > MAX_ADDRESSABLE_SLOT {
        MAX_ADDRESSABLE_SLOT
    } else {
        slot as u16
    }
}

/// The lowest DMX value that selects a slot, for a console programming one.
///
/// The middle of the slot's band rather than its bottom, so a desk that is a
/// value out either way still lands on the slot that was meant.
#[must_use]
pub const fn dmx_for_slot(slot: u16) -> u8 {
    if slot == 0 {
        return 0;
    }
    let slot = if slot > MAX_ADDRESSABLE_SLOT {
        MAX_ADDRESSABLE_SLOT
    } else {
        slot
    };
    FIRST_SLOT_VALUE + (slot as u8 - 1) * VALUES_PER_SLOT + 1
}

/// Returns the band a channel value falls into.
fn band<T: Copy, const N: usize>(value: u8, bands: [T; N]) -> T {
    // Even bands across the whole channel, so a console's percentage readout
    // maps to a step without the operator having to know the boundaries.
    let index = usize::from(value) * N / 256;
    bands[index.min(N - 1)]
}

/// Linear interpolation, used for every ranged channel.
fn lerp(min: f64, max: f64, position: f64) -> f64 {
    min + (max - min) * position
}

/// One channel of a fixture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelDefinition {
    /// One-based offset within the fixture.
    pub offset: u16,
    /// What a console shows for this channel.
    pub name: &'static str,
    /// What it controls, given the fixture's target.
    pub parameter: ParameterSlot,
    /// How its bytes become a value.
    pub encoding: ChannelEncoding,
    /// What a console should send to put this channel at rest.
    ///
    /// Raw, in the units the channel is sent in: the whole sixteen-bit number
    /// for a `Ranged16` channel, the low byte for every other kind. That is
    /// what a personality file has to contain, because a console homes a
    /// fixture by sending numbers rather than by understanding them.
    ///
    /// # Why this exists, and what it is not
    ///
    /// A console sends a whole universe, and every channel it has not been
    /// programmed for arrives as zero. Zero scale is a layer that vanishes and
    /// zero opacity is a layer nobody can see, so a freshly patched fixture on
    /// an unprogrammed desk shows nothing and looks broken.
    ///
    /// The fix somebody always reaches for is to make zero mean "neutral", and
    /// the owner refused it (owner decision Q15): a patched fixture belongs to
    /// the console, the universe is authoritative, and OpenVideoServer must not
    /// guess whether a zero it received was intended. **Zero stays a real
    /// value.** Scale 0 is the minimum scale and opacity 0 is off.
    ///
    /// This is the other half of that answer. Nothing here changes how a
    /// received value is read; it is published so a console can put the fixture
    /// somewhere sensible *itself*, once, with its own home command.
    pub home: u16,
}

impl ChannelDefinition {
    /// What homing this channel produces, for a definition's own tests.
    ///
    /// The same path a received value takes, so a home that decodes to
    /// something unintended is a failing test rather than a layer that homes
    /// to the wrong size.
    #[must_use]
    pub fn homed(&self) -> Option<ChannelValue> {
        // Laid out as a console would send it, so this walks exactly the path a
        // received value walks rather than a shortcut that could agree with
        // nothing. A wide channel sends its coarse byte first; a narrow one
        // sends one byte, and its home is that byte rather than the low half of
        // a pair — which is the mistake this comment exists to have caught.
        let universe = if self.encoding.width() == 2 {
            self.home.to_be_bytes()
        } else {
            [self.home as u8, 0]
        };
        self.encoding.decode(&universe, 1)
    }
}

/// What a channel controls, before it is bound to a target.
///
/// A definition is written once and patched many times, so its channels name a
/// role rather than a layer. Binding happens when a patch entry says which layer
/// this instance drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterSlot {
    /// A global control. The same whatever the instance.
    Master(Parameter),
    /// A layer control, bound to whichever layer the instance targets.
    Layer(LayerParameter),
    /// One parameter of whichever effect instance the patch points at.
    Effect(PluginParameter),
    /// One parameter of whichever generator instance the patch points at.
    Generator(PluginParameter),
}

impl ParameterSlot {
    /// A stable machine name for what this channel controls.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Master(parameter) => parameter.name(),
            Self::Layer(parameter) => parameter.name(),
            // The manifest's own key. A plugin's parameter has no name in this
            // crate to give it, and the key is what every other part of the
            // system already calls it.
            Self::Effect(parameter) | Self::Generator(parameter) => parameter.key,
        }
    }

    /// Binds this channel to what a patch entry points at.
    ///
    /// `None` when the personality does not fit the target: a layer personality
    /// aimed at the master would give one layer's opacity fader the master
    /// dimmer. The rule lives here because patch validation and the DMX
    /// receiver both need it, and a rule those two disagreed about would refuse
    /// a patch that works or accept one that does not.
    #[must_use]
    pub const fn bind(self, target: FixtureTarget) -> Option<Parameter> {
        match (self, target) {
            (Self::Master(parameter), FixtureTarget::Master) => Some(parameter),
            (Self::Layer(parameter), FixtureTarget::Layer { layer_id }) => {
                Some(parameter.bind(layer_id))
            }
            (
                Self::Effect(parameter),
                FixtureTarget::Effect {
                    effect_instance_id, ..
                },
            ) => Some(Parameter::EffectParameter {
                effect_instance_id,
                key: parameter.key,
                component: parameter.component,
            }),
            (
                Self::Generator(parameter),
                FixtureTarget::Generator {
                    generator_instance_id,
                    ..
                },
            ) => Some(Parameter::GeneratorParameter {
                generator_instance_id,
                key: parameter.key,
                component: parameter.component,
            }),
            _ => None,
        }
    }
}

/// One parameter of a plugin, before it knows which instance.
///
/// A plugin — an effect, and in time a generator — declares which of its
/// parameters a console may drive (`ParameterDefinition::dmx`). This is one of
/// those, waiting for a patch entry to say whose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginParameter {
    /// The manifest key it moves.
    pub key: &'static str,
    /// Which part of a colour, when the parameter is one.
    ///
    /// A colour is four channels and they are four separate faders on a desk,
    /// so each carries one part and the value is assembled from whatever has
    /// arrived. `None` for every other kind, which is one channel or two.
    pub component: Option<ColourPart>,
}

/// Which of a colour's four numbers a channel carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColourPart {
    /// Red.
    Red,
    /// Green.
    Green,
    /// Blue.
    Blue,
    /// Alpha.
    Alpha,
}

impl ColourPart {
    /// Its index into a linear RGBA array.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Red => 0,
            Self::Green => 1,
            Self::Blue => 2,
            Self::Alpha => 3,
        }
    }

    /// What a console's channel list shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Green => "Green",
            Self::Blue => "Blue",
            Self::Alpha => "Alpha",
        }
    }

    /// The four, in the order they are sent.
    pub const ALL: [Self; 4] = [Self::Red, Self::Green, Self::Blue, Self::Alpha];
}

/// Builds the personality a plugin's parameters describe.
///
/// # Why this is generated rather than written
///
/// There is no OVS Effect Personality and no OVS Generator Personality. An
/// effect and a generator are plugins, third-party ones are expected,
/// and a fixed layout decided in this crate would be wrong for every
/// plugin that did not exist when it was written. So a module declares its own
/// parameters and, per parameter, whether a console may have it and how finely
/// — and the personality falls out of that.
///
/// The rules are a reading of what a manifest already says, so nothing new is
/// invented and a personality cannot drift from the module it describes:
///
/// | the manifest says | the channel is |
/// |---|---|
/// | a number, `Fine` | sixteen bits across its range |
/// | a number, `Coarse` | eight bits across its range |
/// | a switch | one channel, on at 128 |
/// | a colour | four channels: red, green, blue, alpha |
/// | a list of choices | one channel in that many even bands |
/// | text | nothing — a desk cannot send words |
///
/// Channels are numbered in declaration order, so a plugin author reads their
/// own manifest down the page and sees the console's channel list.
///
/// Returns `None` when the module declares no DMX at all, which is a module
/// saying it does not want to be driven that way rather than a gap.
///
/// # Leaked, once, on purpose
///
/// A [`FixtureDefinition`] is `&'static` because the published ones are
/// constants. A generated one is built from `&'static` inputs but cannot be a
/// constant, so it is built once per module and leaked. The number of modules
/// is bounded by what this build publishes plus what it loads, and a plugin
/// loaded once is a personality kept for as long as it is loadable.
#[must_use]
pub fn generated_personality(
    type_id: &'static str,
    version: u32,
    name: &'static str,
    parameters: &'static [ParameterDefinition],
    slot: fn(PluginParameter) -> ParameterSlot,
) -> Option<&'static FixtureDefinition> {
    let mut channels: Vec<ChannelDefinition> = Vec::new();
    let mut offset = 1u16;

    for parameter in parameters {
        let Some(width) = parameter.dmx else {
            continue;
        };
        match parameter.kind {
            // A picture is chosen in the client, never sent down a wire. The
            // manifest declares no channel for it; this is the second door and
            // it says the same thing.
            ParameterKind::Text { .. } | ParameterKind::Image => {}
            ParameterKind::Colour { default } => {
                for part in ColourPart::ALL {
                    channels.push(ChannelDefinition {
                        offset,
                        name: leak(format!("{} {}", parameter.name, part.label())),
                        parameter: slot(PluginParameter {
                            key: parameter.key,
                            component: Some(part),
                        }),
                        encoding: ChannelEncoding::Ranged8 { min: 0.0, max: 1.0 },
                        home: u16::from(byte_of(default[part.index()])),
                    });
                    offset += 1;
                }
            }
            ParameterKind::Number { min, max, default } => {
                let (encoding, home) = match width {
                    DmxChannel::Coarse => (
                        ChannelEncoding::Ranged8 { min, max },
                        raw_of(default, min, max, 255.0),
                    ),
                    DmxChannel::Fine => (
                        ChannelEncoding::Ranged16 { min, max },
                        raw_of(default, min, max, 65_535.0),
                    ),
                };
                channels.push(ChannelDefinition {
                    offset,
                    name: parameter.name,
                    parameter: slot(PluginParameter {
                        key: parameter.key,
                        component: None,
                    }),
                    encoding,
                    home,
                });
                offset += encoding.width();
            }
            ParameterKind::Toggle { default } => {
                channels.push(ChannelDefinition {
                    offset,
                    name: parameter.name,
                    parameter: slot(PluginParameter {
                        key: parameter.key,
                        component: None,
                    }),
                    encoding: ChannelEncoding::Switch { threshold: 128 },
                    home: if default { 255 } else { 0 },
                });
                offset += 1;
            }
            ParameterKind::Choice { choices, default } => {
                let count = choices.len().max(1) as u16;
                // The middle of the default's own band, which is where a desk
                // should sit to mean it: an edge is one value from the next
                // choice along.
                let index = choices
                    .iter()
                    .position(|choice| choice.id == default)
                    .unwrap_or(0) as u32;
                let band = 256 / u32::from(count);
                channels.push(ChannelDefinition {
                    offset,
                    name: parameter.name,
                    parameter: slot(PluginParameter {
                        key: parameter.key,
                        component: None,
                    }),
                    encoding: ChannelEncoding::Choices { count },
                    home: (index * band + band / 2).min(255) as u16,
                });
                offset += 1;
            }
        }
    }

    if channels.is_empty() {
        return None;
    }

    let footprint = offset - 1;
    Some(Box::leak(Box::new(FixtureDefinition {
        type_id,
        version,
        name,
        footprint,
        channels: Box::leak(channels.into_boxed_slice()),
    })))
}

/// One `0.0..=1.0` component as the byte a desk sends.
fn byte_of(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Where a value sits in a channel's raw range.
fn raw_of(value: f64, min: f64, max: f64, full: f64) -> u16 {
    let span = max - min;
    if span.abs() < f64::EPSILON {
        return 0;
    }
    (((value - min) / span).clamp(0.0, 1.0) * full).round() as u16
}

/// Keeps a generated name for as long as the personality that uses it.
fn leak(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// A layer control, before it knows which layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerParameter {
    /// Live opacity.
    Opacity,
    /// Which slot is showing.
    Slot,
    /// Start the source again.
    Trigger,
    /// Horizontal offset.
    PositionX,
    /// Vertical offset.
    PositionY,
    /// Horizontal scale.
    ScaleX,
    /// Vertical scale.
    ScaleY,
    /// Clockwise rotation.
    Rotation,
}

impl LayerParameter {
    /// A stable machine name, the same one the bound parameter carries.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Opacity => "layer_opacity",
            Self::Slot => "layer_slot",
            Self::Trigger => "layer_trigger",
            Self::PositionX => "layer_position_x",
            Self::PositionY => "layer_position_y",
            Self::ScaleX => "layer_scale_x",
            Self::ScaleY => "layer_scale_y",
            Self::Rotation => "layer_rotation",
        }
    }

    /// Binds this control to a layer.
    #[must_use]
    pub const fn bind(self, layer_id: LayerId) -> Parameter {
        match self {
            Self::Opacity => Parameter::LayerOpacity { layer_id },
            Self::Slot => Parameter::LayerSlot { layer_id },
            Self::Trigger => Parameter::LayerTrigger { layer_id },
            Self::PositionX => Parameter::LayerPositionX { layer_id },
            Self::PositionY => Parameter::LayerPositionY { layer_id },
            Self::ScaleX => Parameter::LayerScaleX { layer_id },
            Self::ScaleY => Parameter::LayerScaleY { layer_id },
            Self::Rotation => Parameter::LayerRotation { layer_id },
        }
    }
}

/// A published fixture personality.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixtureDefinition {
    /// Stable type identifier, for example `ovs.master`.
    pub type_id: &'static str,
    /// Version. Published definitions never change; a new control is a new one.
    pub version: u32,
    /// What a console's personality list shows.
    pub name: &'static str,
    /// How many channels an instance occupies.
    pub footprint: u16,
    /// The channels, in offset order.
    pub channels: &'static [ChannelDefinition],
}

impl FixtureDefinition {
    /// Returns true when every channel of this personality fits a target.
    #[must_use]
    pub fn drives(&self, target: FixtureTarget) -> bool {
        self.channels
            .iter()
            .all(|channel| channel.parameter.bind(target).is_some())
    }

    /// Returns the channel at a one-based offset, if there is one.
    #[must_use]
    pub fn channel(&self, offset: u16) -> Option<&ChannelDefinition> {
        self.channels
            .iter()
            .find(|channel| channel.offset == offset)
    }
}

/// The single show-wide master fixture, version 1.
///
/// Five channels, every one of them eight bits: the six the owner set down on
/// 2026-09-22, less sync. What was here before was this renderer's guess at the
/// same thing — ten channels, sixteen bits for the dimmer, the crossfader and a
/// BPM *value* — and it was wrong in ways worth naming, because each of them
/// says something about how a desk is actually used:
///
/// - **There is no BPM value channel.** A tempo is tapped, not typed into a
///   fader: a sixteen-bit channel across 20 to 999 BPM puts about a seventh of
///   a beat per step in the range anybody works in, and an operator who wants
///   128 cannot get it by hand.
/// - **There is no sync channel either**, since 2026-10-18: "BPM Sync
///   entfernen wir komplett aus DMX und existiert nur noch im Clienten." The
///   client's phase reset is a request of its own, and a desk has a tap.
/// - **The scale is a ladder, not a range.** Eight steps, each a doubling, so a
///   console's percentage readout lands on a step instead of between two.
/// - **Eight bits throughout.** Sixteen was reserved for what an operator fades
///   by hand, and on reflection that is the layer's transform rather than
///   anything here.
///
/// Nothing has been released, so this replaces version 1 rather than becoming
/// version 2 — the same reasoning the owner gave for the slot transform
/// (owner decision Q14). After the first release a channel layout is physically
/// programmed into a desk's show file and a new control is a new version.
///
/// Taking sync out moved the scale and the crossfader down one channel in
/// version 1 itself, after release, on the owner's word that every desk is
/// being programmed again for the new fixture types (2026-10-18: "Wir
/// definieren gerade neu, was alles umschmeißt und das Pult muss eh neu
/// gemacht werden").
pub const MASTER_V1: FixtureDefinition = FixtureDefinition {
    type_id: "ovs.master",
    version: 1,
    name: "OVS Master",
    footprint: 5,
    channels: &[
        ChannelDefinition {
            offset: 1,
            name: "Master Dimmer",
            parameter: ParameterSlot::Master(Parameter::MasterDimmer),
            encoding: ChannelEncoding::Ranged8 { min: 0.0, max: 1.0 },
            // Full. A homed master that blacks the show is a master nobody
            // would home twice.
            home: 255,
        },
        ChannelDefinition {
            offset: 2,
            name: "Freeze Output",
            parameter: ParameterSlot::Master(Parameter::MasterFreeze),
            encoding: ChannelEncoding::Switch { threshold: 128 },
            // Off. Homing a desk must not stop the show.
            home: 0,
        },
        ChannelDefinition {
            offset: 3,
            name: "BPM Tap",
            parameter: ParameterSlot::Master(Parameter::MasterTapTempo),
            encoding: ChannelEncoding::Switch { threshold: 128 },
            // A trigger at rest.
            home: 0,
        },
        ChannelDefinition {
            offset: 4,
            name: "BPM Scale",
            parameter: ParameterSlot::Master(Parameter::MasterSpeedScale),
            encoding: ChannelEncoding::ScaleLadder,
            // 1x, which since the ladder was capped at four is the fourth of
            // six bands: 128 to 170. Half travel is the show's own tempo, and
            // 128 is the one value on a channel anybody can find without
            // counting.
            home: 128,
        },
        ChannelDefinition {
            offset: 5,
            name: "Crossfade A/B",
            parameter: ParameterSlot::Master(Parameter::MasterCrossfader),
            encoding: ChannelEncoding::Ranged8 { min: 0.0, max: 1.0 },
            // All the way to A, which is the bus a show is built on before
            // anybody crossfades away from it.
            home: 0,
        },
    ],
};

/// The layer fixture, version 1.
///
/// Twelve channels, as the owner set them down on 2026-09-22. One per patched
/// layer. What changed from this renderer's earlier guess, and why each change
/// is the right way round:
///
/// - **Selecting a slot is what plays it.** There is no separate trigger
///   channel. That costs one thing and it is worth saying out loud: a *re*
///   trigger cannot be sent over DMX, because re-selecting the slot that is
///   already running does not move the channel. An operator who wants a clip
///   restarted from a desk goes to Black and back.
/// - **A slot is a band, not a number.** Three DMX values each, so a wheel
///   nudged past a slot does not skip it and a console rounding a percentage to
///   a byte lands where it meant to.
/// - **Scale is two channels, not one.** The earlier version had a single
///   uniform scale and an argument for it — that an operator reaching for scale
///   wants the layer bigger rather than stretched. The owner wants both axes,
///   which is what a fixture driving a mapped wall actually needs.
/// - **Rotation runs from -180 to 180**, centred at 32768, rather than 0 to
///   360. A desk's wheel at rest is in the middle of its travel, and upright is
///   what rest should mean.
/// - **No rate and no trigger grid.** Both are live values a client sets; the
///   owner's layout does not carry them, so a console does not drive them.
///
/// It carries no A/B/M assignment. That is a live client-side decision and
/// deliberately not part of the standard personality.
pub const LAYER_V1: FixtureDefinition = FixtureDefinition {
    type_id: "ovs.layer",
    version: 1,
    name: "OVS Layer",
    footprint: 12,
    channels: &[
        ChannelDefinition {
            offset: 1,
            name: "Layer Dimmer",
            parameter: ParameterSlot::Layer(LayerParameter::Opacity),
            encoding: ChannelEncoding::Ranged8 { min: 0.0, max: 1.0 },
            // Full. A freshly patched layer showing nothing is the whole
            // complaint that home values answer (owner decision Q15).
            home: 255,
        },
        ChannelDefinition {
            offset: 2,
            name: "Slot Playback",
            parameter: ParameterSlot::Layer(LayerParameter::Slot),
            encoding: ChannelEncoding::SlotBands,
            // Black. Homing a desk must not start content playing in front of
            // an audience.
            home: 0,
        },
        ChannelDefinition {
            offset: 3,
            name: "Transform Position X",
            parameter: ParameterSlot::Layer(LayerParameter::PositionX),
            encoding: ChannelEncoding::Ranged16 {
                min: -2.0,
                max: 2.0,
            },
            // Centred.
            home: 32_768,
        },
        ChannelDefinition {
            offset: 5,
            name: "Transform Position Y",
            parameter: ParameterSlot::Layer(LayerParameter::PositionY),
            encoding: ChannelEncoding::Ranged16 {
                min: -2.0,
                max: 2.0,
            },
            // Centred.
            home: 32_768,
        },
        ChannelDefinition {
            offset: 7,
            name: "Transform Rotation Z",
            parameter: ParameterSlot::Layer(LayerParameter::Rotation),
            encoding: ChannelEncoding::Ranged16 {
                min: -180.0,
                max: 180.0,
            },
            // Upright, which is the middle of the channel rather than its end.
            home: 32_768,
        },
        ChannelDefinition {
            offset: 9,
            name: "Transform Scale X",
            parameter: ParameterSlot::Layer(LayerParameter::ScaleX),
            encoding: ChannelEncoding::Ranged16 {
                min: 0.0,
                max: MAX_FIXTURE_SCALE,
            },
            // 1.0, which on a 0-to-10 range is a tenth of the channel.
            home: 6_554,
        },
        ChannelDefinition {
            offset: 11,
            name: "Transform Scale Y",
            parameter: ParameterSlot::Layer(LayerParameter::ScaleY),
            encoding: ChannelEncoding::Ranged16 {
                min: 0.0,
                max: MAX_FIXTURE_SCALE,
            },
            // 1.0, as above.
            home: 6_554,
        },
    ],
};

/// The largest scale a layer fixture can ask for, as a multiplier.
///
/// Ten, which the owner's table writes as 1000%. Linear across the channel, so
/// 100% is a tenth of the way up it — their table put it at 5958, which is a
/// tenth of *eleven* times the channel, and they confirmed the line rather than
/// the number.
pub const MAX_FIXTURE_SCALE: f64 = 10.0;

/// Every definition this build publishes.
pub const PUBLISHED: [FixtureDefinition; 2] = [MASTER_V1, LAYER_V1];

/// Returns a published definition by type and version.
///
/// A patch naming a version this build does not have is refused rather than
/// served the nearest one: a console sending version 1 channels into a version 2
/// layout would move the wrong things, and slowly enough that nobody would
/// connect the two.
///
/// Answers for the two personalities this crate publishes and for the ones a
/// module generates from its own parameters — one lookup, because a patch
/// entry naming `ovs.blur` and one naming `ovs.layer` are the same kind of
/// thing to everything that reads a patch.
#[must_use]
pub fn definition(type_id: &str, version: u32) -> Option<&'static FixtureDefinition> {
    PUBLISHED
        .iter()
        .find(|definition| definition.type_id == type_id && definition.version == version)
        .or_else(|| crate::config::effects::personality(type_id, version))
        .or_else(|| crate::config::generators::personality(type_id, version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::show::{MAX_SPEED_SCALE, MIN_SPEED_SCALE};

    #[test]
    fn a_plugin_that_declares_nothing_has_no_personality() {
        // Which is the plugin saying it does not want to be driven that way.
        // A personality with no channels would be patchable, would occupy an
        // address, and would do nothing.
        use crate::config::manifest::ParameterDefinition;

        static SILENT: [ParameterDefinition; 1] = [ParameterDefinition {
            key: "words",
            name: "Words",
            kind: ParameterKind::Text {
                default: "",
                max_chars: 16,
            },
            dmx: None,
        }];

        assert!(
            generated_personality("test.silent", 1, "Silent", &SILENT, ParameterSlot::Effect)
                .is_none()
        );
    }

    #[test]
    fn text_never_becomes_a_channel_however_it_is_declared() {
        // A desk sends numbers. A parameter whose value *is* words has nothing
        // to send, and a manifest that asks for one anyway gets no channel
        // rather than a channel that cannot work.
        use crate::config::manifest::{DmxChannel, ParameterDefinition};

        static MIXED: [ParameterDefinition; 2] = [
            ParameterDefinition {
                key: "words",
                name: "Words",
                kind: ParameterKind::Text {
                    default: "",
                    max_chars: 16,
                },
                dmx: Some(DmxChannel::Coarse),
            },
            ParameterDefinition {
                key: "size",
                name: "Size",
                kind: ParameterKind::Number {
                    min: 0.0,
                    max: 10.0,
                    default: 5.0,
                },
                dmx: Some(DmxChannel::Coarse),
            },
        ];

        let personality =
            generated_personality("test.mixed", 1, "Mixed", &MIXED, ParameterSlot::Effect)
                .expect("one channel");
        assert_eq!(personality.footprint, 1);
        assert_eq!(personality.channels[0].name, "Size");
        assert_eq!(
            personality.channels[0].offset, 1,
            "the text left no gap behind it"
        );
    }

    #[test]
    fn every_published_effect_generates_the_personality_its_manifest_describes() {
        // The footprints are the arithmetic written out: a fine number is two
        // channels, a coarse one is one, a colour is four and a switch is one.
        // Written here so that changing a manifest's `dmx` shows up as a
        // changed number rather than as a quietly different patch.
        for (type_id, footprint) in [
            ("ovs.colour_correct", 8),
            ("ovs.tint", 7),
            ("ovs.pixelate", 2),
            ("ovs.blur", 4),
        ] {
            let personality = definition(type_id, 1)
                .unwrap_or_else(|| panic!("{type_id} generates no personality"));
            assert_eq!(personality.footprint, footprint, "{type_id}");
            assert_eq!(
                personality
                    .channels
                    .iter()
                    .map(|channel| channel.encoding.width())
                    .sum::<u16>(),
                footprint,
                "{type_id}: the channels do not fill the footprint"
            );
        }
    }

    #[test]
    fn a_generated_personality_drives_an_effect_and_nothing_else() {
        // The same rule the published ones live under: a personality aimed at
        // the wrong kind of thing would move something nobody asked for.
        let personality = definition("ovs.blur", 1).expect("a personality");
        let effect_instance_id = crate::id::EffectInstanceId::new();

        assert!(personality.drives(FixtureTarget::Effect { effect_instance_id }));
        assert!(!personality.drives(FixtureTarget::Master));
        assert!(!personality.drives(FixtureTarget::Layer {
            layer_id: LayerId::new()
        }));
    }

    #[test]
    fn a_colour_becomes_four_channels_that_say_which_part_they_are() {
        // Four faders on a desk, and each one has to carry enough to be put
        // back into the right place in the colour.
        let personality = definition("ovs.tint", 1).expect("a personality");
        let colour: Vec<&ChannelDefinition> = personality
            .channels
            .iter()
            .filter(|channel| channel.name.starts_with("Colour "))
            .collect();

        assert_eq!(colour.len(), 4);
        for (channel, part) in colour.iter().zip(ColourPart::ALL) {
            assert_eq!(*channel.name, *format!("Colour {}", part.label()));
            let ParameterSlot::Effect(parameter) = channel.parameter else {
                panic!("a colour channel of an effect");
            };
            assert_eq!(parameter.key, "colour");
            assert_eq!(parameter.component, Some(part));
        }
    }

    #[test]
    fn the_masters_ladder_reaches_exactly_as_far_as_the_show_allows() {
        // A desk that can send a value the show then refuses reads as
        // broken, and a ladder that stops short leaves travel that does
        // nothing. Both ends, therefore, are the same two numbers.
        let first = MASTER_SCALE_STEPS[0];
        let last = MASTER_SCALE_STEPS[MASTER_SCALE_STEPS.len() - 1];
        assert!(
            (first - MIN_SPEED_SCALE).abs() < f64::EPSILON,
            "the ladder starts at {first}, the show at {MIN_SPEED_SCALE}"
        );
        assert!(
            (last - MAX_SPEED_SCALE).abs() < f64::EPSILON,
            "the ladder ends at {last}, the show at {MAX_SPEED_SCALE}"
        );
        for pair in MASTER_SCALE_STEPS.windows(2) {
            assert!(
                (pair[1] - pair[0] * 2.0).abs() < f64::EPSILON,
                "{} is not twice {}",
                pair[1],
                pair[0]
            );
        }
        assert!(
            MASTER_SCALE_STEPS.contains(&1.0),
            "a scale channel that cannot ask for the show's own tempo"
        );
    }

    /// What homing one channel of a personality produces.
    fn homed(definition: &FixtureDefinition, name: &str) -> ChannelValue {
        definition
            .channels
            .iter()
            .find(|channel| channel.name == name)
            .unwrap_or_else(|| panic!("{} has no {name} channel", definition.type_id))
            .homed()
            .unwrap_or_else(|| panic!("{name} homes to nothing"))
    }

    /// Whether two numbers are the same to within a channel's own resolution.
    ///
    /// A sixteen-bit channel divides its range into 65535 steps, so a value in
    /// the middle of the range is the middle to within one of them and no
    /// closer. Asserting equality would be asserting that the range happened to
    /// divide evenly, which is a property of the numbers rather than of the
    /// home being right.
    #[track_caller]
    fn about(value: f64, wanted: f64, span: f64, what: &str) {
        let step = span / 65_535.0;
        assert!(
            (value - wanted).abs() <= step,
            "{what}: homed to {value}, wanted {wanted}"
        );
    }

    #[test]
    fn homing_a_layer_puts_it_where_an_operator_would_expect_to_find_it() {
        // The answer to Q15. Zero is still a real value — scale
        // zero is the minimum scale and a dimmer at zero is off — and this is
        // how a console gets a freshly patched layer somewhere sensible without
        // anybody redefining what zero means.
        let ChannelValue::Number(dimmer) = homed(&LAYER_V1, "Layer Dimmer") else {
            panic!("the dimmer is a number");
        };
        about(dimmer, 1.0, 1.0, "the layer dimmer");

        for axis in ["Transform Position X", "Transform Position Y"] {
            let ChannelValue::Number(position) = homed(&LAYER_V1, axis) else {
                panic!("{axis} is a number");
            };
            about(position, 0.0, 4.0, axis);
        }

        // Two axes rather than one: an operator who wants the layer stretched
        // has a channel for it, and one who wants it bigger moves both.
        for axis in ["Transform Scale X", "Transform Scale Y"] {
            let ChannelValue::Number(scale) = homed(&LAYER_V1, axis) else {
                panic!("{axis} is a number");
            };
            about(scale, 1.0, MAX_FIXTURE_SCALE, axis);
        }

        let ChannelValue::Number(rotation) = homed(&LAYER_V1, "Transform Rotation Z") else {
            panic!("rotation is a number");
        };
        about(rotation, 0.0, 360.0, "rotation");
    }

    #[test]
    fn homing_a_layer_neither_starts_content_nor_fires_anything() {
        // A home is sent to a rig that may be in front of an audience. It puts
        // the layer at its normal size and leaves it on black.
        assert_eq!(
            homed(&LAYER_V1, "Slot Playback"),
            ChannelValue::Index(0),
            "black"
        );

        // And there is nothing on this personality that could fire: selecting a
        // slot is what plays it, so the layout (2026-09-22) has no trigger.
        for channel in LAYER_V1.channels {
            assert!(
                !matches!(channel.encoding, ChannelEncoding::Switch { .. }),
                "{} is a switch, and a layer has none",
                channel.name
            );
        }
    }

    #[test]
    fn homing_the_master_leaves_the_show_running() {
        let ChannelValue::Number(dimmer) = homed(&MASTER_V1, "Master Dimmer") else {
            panic!("the dimmer is a number");
        };
        about(dimmer, 1.0, 1.0, "the master dimmer");

        let ChannelValue::Number(crossfader) = homed(&MASTER_V1, "Crossfade A/B") else {
            panic!("the crossfader is a number");
        };
        about(crossfader, 0.0, 1.0, "the crossfader");

        // A ladder lands on its step exactly, which is the point of making the
        // channel a ladder rather than a range: the show's own tempo is
        // reachable from a desk instead of being approached to within a
        // fraction of a per cent.
        let ChannelValue::Number(scale) = homed(&MASTER_V1, "BPM Scale") else {
            panic!("the speed scale is a number");
        };
        assert!(
            (scale - 1.0).abs() < f64::EPSILON,
            "the speed scale homed to {scale}"
        );

        for trigger in ["BPM Tap", "Freeze Output"] {
            assert_eq!(
                homed(&MASTER_V1, trigger),
                ChannelValue::Flag(false),
                "{trigger} is at rest"
            );
        }

        // There is no tempo channel. The master takes its BPM from the client
        // or from a tap, and a fader that could set it would be a second
        // source for the one number a show cannot have two of.
        assert!(
            !MASTER_V1
                .channels
                .iter()
                .any(|channel| channel.name == "BPM"),
            "the master personality has a BPM fader again"
        );
        // Nor a sync: "BPM Sync entfernen wir komplett aus DMX und existiert
        // nur noch im Clienten" (owner, 2026-10-18).
        assert!(
            !MASTER_V1
                .channels
                .iter()
                .any(|channel| channel.name.contains("Sync")),
            "the master personality has a sync channel again"
        );
        assert_eq!(MASTER_V1.footprint, 5);
    }

    #[test]
    fn every_published_channel_says_where_its_home_is_and_means_it() {
        // A channel whose home decodes to nothing is a personality file with a
        // number in it that the fixture would ignore.
        for definition in &PUBLISHED {
            for channel in definition.channels {
                assert!(
                    channel.homed().is_some(),
                    "{} channel {} homes to nothing",
                    definition.type_id,
                    channel.name
                );
                // An eight-bit channel is sent as one byte. A home above 255
                // would lose its top half on the wire and land somewhere else.
                if !matches!(channel.encoding, ChannelEncoding::Ranged16 { .. }) {
                    assert!(
                        channel.home <= 255,
                        "{} channel {} homes to {}, which does not fit in a byte",
                        definition.type_id,
                        channel.name,
                        channel.home
                    );
                }
            }
        }
    }

    #[test]
    fn a_layer_control_and_the_parameter_it_binds_to_have_one_name() {
        // Two spellings of the same control would mean a client that matched on
        // the catalogue's name not recognising the parameter that arrived.
        for parameter in [
            LayerParameter::Opacity,
            LayerParameter::Slot,
            LayerParameter::Trigger,
            LayerParameter::PositionX,
            LayerParameter::PositionY,
            LayerParameter::ScaleX,
            LayerParameter::ScaleY,
            LayerParameter::Rotation,
        ] {
            assert_eq!(parameter.name(), parameter.bind(LayerId::new()).name());
        }
    }

    #[test]
    fn every_definition_fills_its_footprint_exactly() {
        // A footprint that disagrees with the channels is a patch that collides
        // with its neighbour, or leaves a gap nobody notices until two fixtures
        // are addressed back to back.
        for definition in PUBLISHED {
            let last = definition
                .channels
                .iter()
                .map(|channel| channel.offset + channel.encoding.width() - 1)
                .max()
                .expect("a definition has channels");
            assert_eq!(
                last, definition.footprint,
                "{} v{} claims {} channels and uses {last}",
                definition.type_id, definition.version, definition.footprint
            );
        }
    }

    #[test]
    fn no_two_channels_of_a_definition_overlap() {
        for definition in PUBLISHED {
            let mut used = vec![false; usize::from(definition.footprint) + 1];
            for channel in definition.channels {
                for offset in channel.offset..channel.offset + channel.encoding.width() {
                    let slot = &mut used[usize::from(offset)];
                    assert!(
                        !*slot,
                        "{} v{}: channel {offset} is claimed twice",
                        definition.type_id, definition.version
                    );
                    *slot = true;
                }
            }
            assert!(
                used.iter().skip(1).all(|taken| *taken),
                "{} v{} leaves a gap",
                definition.type_id,
                definition.version
            );
        }
    }

    #[test]
    fn the_layer_fixture_carries_nothing_the_specification_excludes() {
        // BPM tap, BPM reset, effect master and layer black are explicitly not
        // part of it, and neither is A/B/M assignment.
        for channel in LAYER_V1.channels {
            assert!(
                matches!(channel.parameter, ParameterSlot::Layer(_)),
                "{} is not a layer control",
                channel.name
            );
            let name = channel.name.to_ascii_lowercase();
            for forbidden in ["bpm", "black", "effect master", "bus", "route"] {
                assert!(!name.contains(forbidden), "{} is excluded", channel.name);
            }
        }
    }

    #[test]
    fn a_full_channel_is_the_top_of_its_range_and_an_empty_one_the_bottom() {
        let dimmer = ChannelEncoding::Ranged16 { min: 0.0, max: 1.0 };
        assert_eq!(
            dimmer.decode(&[255, 255], 1),
            Some(ChannelValue::Number(1.0))
        );
        assert_eq!(dimmer.decode(&[0, 0], 1), Some(ChannelValue::Number(0.0)));

        let scale = ChannelEncoding::Ranged8 { min: 0.5, max: 4.0 };
        assert_eq!(scale.decode(&[0], 1), Some(ChannelValue::Number(0.5)));
        assert_eq!(scale.decode(&[255], 1), Some(ChannelValue::Number(4.0)));
    }

    #[test]
    fn sixteen_bits_is_finer_than_eight() {
        // The reason the dimmer is two channels. One step of the coarse channel
        // is 256 steps of the fine one, and the last few percent of a fade is
        // where an operator sees the difference.
        let dimmer = ChannelEncoding::Ranged16 { min: 0.0, max: 1.0 };
        let Some(ChannelValue::Number(low)) = dimmer.decode(&[0, 1], 1) else {
            panic!("decodes");
        };
        let Some(ChannelValue::Number(zero)) = dimmer.decode(&[0, 0], 1) else {
            panic!("decodes");
        };
        assert!(low > zero);
        assert!(low < 0.0001, "one fine step is tiny, which is the point");
    }

    #[test]
    fn a_switch_has_a_band_rather_than_a_value() {
        // A console's "on" is rarely exactly 255, and an operator's fader is
        // never exactly anything.
        let switch = ChannelEncoding::Switch { threshold: 128 };
        assert_eq!(switch.decode(&[0], 1), Some(ChannelValue::Flag(false)));
        assert_eq!(switch.decode(&[127], 1), Some(ChannelValue::Flag(false)));
        assert_eq!(switch.decode(&[128], 1), Some(ChannelValue::Flag(true)));
        assert_eq!(switch.decode(&[255], 1), Some(ChannelValue::Flag(true)));
    }

    #[test]
    fn a_truncated_universe_decodes_to_nothing_rather_than_a_guess() {
        // Both protocols allow a short universe, and a console that sends 24
        // channels is not saying the rest are zero.
        let dimmer = ChannelEncoding::Ranged16 { min: 0.0, max: 1.0 };
        assert_eq!(
            dimmer.decode(&[255], 1),
            None,
            "the fine channel is missing"
        );
        assert_eq!(dimmer.decode(&[], 1), None);
        assert_eq!(ChannelEncoding::Index8.decode(&[1, 2], 5), None);
    }

    #[test]
    fn a_definition_is_found_by_type_and_version_together() {
        assert!(definition("ovs.master", 1).is_some());
        assert!(definition("ovs.layer", 1).is_some());
        // A version this build does not have is refused rather than served the
        // nearest one, which would move the wrong things.
        assert!(definition("ovs.layer", 2).is_none());
        assert!(definition("ovs.spotlight", 1).is_none());
    }

    #[test]
    fn a_layer_control_becomes_a_parameter_when_it_is_bound() {
        let layer_id = LayerId::new();
        assert_eq!(
            LayerParameter::Opacity.bind(layer_id),
            Parameter::LayerOpacity { layer_id }
        );
        assert_eq!(
            LayerParameter::Slot.bind(layer_id),
            Parameter::LayerSlot { layer_id }
        );
    }
}
