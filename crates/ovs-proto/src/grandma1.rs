//! The personalities as grandMA1 fixture types.
//!
//! The owner, 2026-10-17: "Kannst du mir für GrandMA 1 alle Fixtures bauen als
//! OVS Gen NAME und OVS FX NAME mit Hersteller testbild.media inkl. aller
//! Slots etc.?"
//!
//! A grandMA1 imports a fixture type from a text file: `Setup → Full Access →
//! Fixture Types → Import → External`. This renders every personality the
//! server publishes into one, from the same catalogue `reference/FIXTURES.md`
//! is rendered from, so the desk and the document cannot disagree about a
//! channel.
//!
//! # Where the format comes from
//!
//! There is no published specification. Everything here was read off the 610
//! fixture types MA ships with grandMA1 onPC 6.808
//! (`Data/System/newfixtures`), and every choice below is one those files make:
//!
//! - A `_CHANTYPE` block per DMX channel, in address order. A sixteen-bit
//!   control is its coarse channel, carrying everything, followed by a second
//!   block with the same attribute and `_TYPE FINE`.
//! - A named value (`_CHANSET`) is given in per cent of the **sixteen-bit**
//!   range, whatever the channel's width: DMX 1 on an eight-bit channel is
//!   `[0.3906%,0.7797%]`, which is 256 to 511 of 65535. A set one value wide
//!   is legal — MA's own VPU camera has `[50.0000%,50.0000%]` — and is what a
//!   home on a sixteen-bit channel has to be, or a scale of one comes out as
//!   one and a bit.
//! - Attribute, feature and label names are at most eighteen characters: that
//!   is where MA's own `KEYSTONE BOTTOM RO` stops. An attribute is whatever
//!   name a fixture gives it, and the same name sits under different features
//!   in different fixture types of the library, so the grouping is per type.
//! - Nine preset types: `DIMMER`, `PAN/TILT`, `COLOR`, `GOBO`, `BEAM`,
//!   `FOCUS`, `CONTROL`, `SHAPERS` and `VIDEO`.
//!
//! # How the controls are laid out on a desk
//!
//! Where a control is the same thing as one a lighting desk already knows, it
//! takes that attribute, so the desk's own tools work on it: a dimmer is
//! `DIM`, the first colour of a module is `RED`, `GREEN` and `BLUE` with the
//! colour-mix effect (so the colour picker drives it), a layer's position,
//! turn and size are `X POS`, `Z ROT` and `X SCALE` under `PAN/TILT` the way
//! the media-server fixtures in the library have them, and a layer's slot is
//! `SLOT` under `GOBO`, which is where those fixtures put the clip a layer
//! plays. Everything else is named after the control and sits under `VIDEO`,
//! grouped by the module it belongs to.

use ovs_types::config::fixtures::{definition, ColourPart, ParameterSlot};
use ovs_types::config::{ParameterDefinition, ParameterKind};

use crate::fixtures::{
    catalogue, ChannelBand, ChannelEncodingInfo, Personality, PersonalityChannel, PersonalityTarget,
};

/// Who the desk lists these under.
pub const MANUFACTURER: &str = "testbild.media";

/// The longest attribute, feature or label name a grandMA1 keeps.
pub const NAME_FIELD: usize = 18;

/// One fixture type, ready to be written to disk and imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureFile {
    /// The file's name, in the library's own `MANUFACTURER@NAME` form.
    pub file_name: String,
    /// What the desk's fixture list calls it.
    pub name: String,
    /// The file, with the CRLF line ends MA's own files have.
    pub contents: String,
}

/// Every fixture type a desk needs for this build.
///
/// The master, the layer, and one per version of every effect and generator
/// that declares DMX — less the retired ones, which no slot or chain can be
/// given any more.
#[must_use]
pub fn fixture_files() -> Vec<FixtureFile> {
    catalogue()
        .fixtures
        .iter()
        .filter(|personality| !retired(personality))
        .map(|personality| FixtureFile {
            file_name: file_name(personality),
            name: fixture_name(personality),
            contents: fixture_type(personality),
        })
        .collect()
}

/// Whether a personality belongs to a module nobody can add any more.
fn retired(personality: &Personality) -> bool {
    let type_id = personality.type_id.as_str();
    match personality.target {
        PersonalityTarget::Effect => ovs_types::config::effects::RETIRED.contains(&type_id),
        PersonalityTarget::Generator => ovs_types::config::generators::RETIRED.contains(&type_id),
        PersonalityTarget::Master | PersonalityTarget::Layer => false,
    }
}

/// What the desk's fixture list calls a personality.
///
/// `OVS FX` and `OVS Gen` in front of the module's own name, as the owner
/// asked. **Version one carries no number and every later version does**, so
/// a name, once in a show file, never has to change: when a module gains a
/// second version the first keeps the name it was imported under.
#[must_use]
pub fn fixture_name(personality: &Personality) -> String {
    let base = match personality.target {
        // Already "OVS Master" and "OVS Layer".
        PersonalityTarget::Master | PersonalityTarget::Layer => personality.name.clone(),
        PersonalityTarget::Effect => format!("OVS FX {}", personality.name),
        PersonalityTarget::Generator => format!("OVS Gen {}", personality.name),
    };
    with_version(base, personality.version)
}

/// The same, short, for the columns that have less room.
fn short_name(personality: &Personality) -> String {
    let base = match personality.target {
        PersonalityTarget::Master | PersonalityTarget::Layer => personality.name.clone(),
        PersonalityTarget::Effect => format!("FX {}", personality.name),
        PersonalityTarget::Generator => format!("Gen {}", personality.name),
    };
    with_version(base, personality.version)
}

fn with_version(base: String, version: u32) -> String {
    if version > 1 {
        format!("{base} v{version}")
    } else {
        base
    }
}

/// The file's name.
fn file_name(personality: &Personality) -> String {
    format!("{MANUFACTURER}@{}.txt", fixture_name(personality))
}

/// Renders one personality as a grandMA1 fixture type.
#[must_use]
pub fn fixture_type(personality: &Personality) -> String {
    let mut out = String::new();
    let line = |out: &mut String, text: &str| {
        out.push_str(text);
        out.push_str("\r\n");
    };

    line(&mut out, "_FIXTURETYPE");
    line(&mut out, "{");
    line(
        &mut out,
        &format!("\t_NAME       {}", quoted(&fixture_name(personality))),
    );
    line(&mut out, &format!("\t_MANUFAC    {}", quoted(MANUFACTURER)));
    line(
        &mut out,
        &format!("\t_SHORTNAME  {}", quoted(&short_name(personality))),
    );
    line(&mut out, "\t_VERSION    ORIGINAL");
    line(
        &mut out,
        &format!("\t_COMMENT    {}", quoted(&comment(personality))),
    );
    // What the desk's own visualiser draws it as. Every video fixture in the
    // library carries a pair; the number means nothing to a wall.
    line(&mut out, "\t_BRIGHTMINW 5000.000000");
    line(&mut out, "\t_BRIGHTMAXW 5000.000000");
    line(&mut out, "\t_CHANTYPE");

    let layout = Layout::of(personality);
    for (channel, attribute) in personality.channels.iter().zip(&layout.attributes) {
        channel_block(&mut out, personality, channel, attribute);
    }

    line(&mut out, "}");
    out
}

/// What the fixture says about itself in the desk's comment field.
fn comment(personality: &Personality) -> String {
    let target = match personality.target {
        PersonalityTarget::Master => "the show master, one per show",
        PersonalityTarget::Layer => "one layer",
        PersonalityTarget::Effect => "one effect instance in a chain",
        PersonalityTarget::Generator => "one generator instance in a slot",
    };
    format!(
        "OpenVideoServer {} v{}, {} channels. Patch in OVS to {target}, at the same address. \
         Generated from the OVS fixture catalogue; reference/FIXTURES.md lists every value.",
        personality.type_id, personality.version, personality.footprint
    )
}

/// Where one channel sits on a desk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attribute {
    attribute: String,
    feature: String,
    preset: &'static str,
    attribute_label: String,
    feature_label: String,
    /// What the desk's own tools take the channel for.
    effect: &'static str,
    effect_sub: Option<&'static str>,
}

/// Every channel's attribute, worked out together, because the first colour
/// of a module is told apart from the others by being first.
struct Layout {
    attributes: Vec<Attribute>,
}

impl Layout {
    fn of(personality: &Personality) -> Self {
        let parameters = parameters_of(personality);
        let slots: Vec<Option<ParameterSlot>> = personality
            .channels
            .iter()
            .map(|channel| slot_of(personality, channel))
            .collect();
        // The key of the first colour, which is the one the colour picker gets.
        let first_colour = slots.iter().find_map(|slot| match slot {
            Some(ParameterSlot::Effect(parameter) | ParameterSlot::Generator(parameter))
                if parameter.component.is_some() =>
            {
                Some(parameter.key)
            }
            _ => None,
        });

        let attributes = personality
            .channels
            .iter()
            .zip(&slots)
            .map(|(channel, slot)| {
                attribute_of(personality, channel, *slot, first_colour, parameters)
            })
            .collect();
        Self { attributes }
    }
}

/// The manifest's parameters, for an effect or a generator.
fn parameters_of(personality: &Personality) -> &'static [ParameterDefinition] {
    ovs_types::config::effects::manifest(&personality.type_id, personality.version)
        .map(|manifest| manifest.parameters)
        .or_else(|| {
            ovs_types::config::generators::manifest(&personality.type_id, personality.version)
                .map(|manifest| manifest.parameters)
        })
        .unwrap_or(&[])
}

/// What a channel of the catalogue drives, from the definition it came from.
fn slot_of(personality: &Personality, channel: &PersonalityChannel) -> Option<ParameterSlot> {
    definition(&personality.type_id, personality.version)?
        .channels
        .iter()
        .find(|defined| defined.offset == channel.offset)
        .map(|defined| defined.parameter)
}

/// The fixed attributes of the two personalities the server publishes itself.
///
/// By control name, so a control added to a later version of either finds no
/// row here and is laid out like any other control rather than taking another
/// one's attribute.
const FIXED: [(&str, &str, &str, &str, &str, &str); 13] = [
    // control, attribute, feature, preset, attribute label, feature label
    ("master_dimmer", "DIM", "DIMMER", "DIMMER", "Dim", "Dimmer"),
    (
        "master_freeze",
        "FREEZE",
        "CONTROL",
        "CONTROL",
        "Freeze",
        "Control",
    ),
    (
        "master_tap_tempo",
        "BPM TAP",
        "SPEED",
        "CONTROL",
        "BPM Tap",
        "Speed",
    ),
    (
        "master_reset_phase",
        "BPM SYNC",
        "SPEED",
        "CONTROL",
        "BPM Sync",
        "Speed",
    ),
    (
        "master_speed_scale",
        "BPM SCALE",
        "SPEED",
        "CONTROL",
        "BPM Scale",
        "Speed",
    ),
    (
        "master_crossfader",
        "CROSSFADE A/B",
        "TRANSITION",
        "VIDEO",
        "Crossfade A/B",
        "Transition",
    ),
    ("layer_opacity", "DIM", "DIMMER", "DIMMER", "Dim", "Dimmer"),
    ("layer_slot", "SLOT", "MEDIA", "GOBO", "Slot", "Media"),
    (
        "layer_position_x",
        "X POS",
        "POSITION",
        "PAN/TILT",
        "X Pos",
        "Position",
    ),
    (
        "layer_position_y",
        "Y POS",
        "POSITION",
        "PAN/TILT",
        "Y Pos",
        "Position",
    ),
    (
        "layer_rotation",
        "Z ROT",
        "ROTATION",
        "PAN/TILT",
        "Z Rot",
        "Rotation",
    ),
    (
        "layer_scale_x",
        "X SCALE",
        "SCALE",
        "PAN/TILT",
        "X Scale",
        "Scale",
    ),
    (
        "layer_scale_y",
        "Y SCALE",
        "SCALE",
        "PAN/TILT",
        "Y Scale",
        "Scale",
    ),
];

/// Works out where one channel sits.
fn attribute_of(
    personality: &Personality,
    channel: &PersonalityChannel,
    slot: Option<ParameterSlot>,
    first_colour: Option<&'static str>,
    parameters: &'static [ParameterDefinition],
) -> Attribute {
    if let Some(&(_, attribute, feature, preset, attribute_label, feature_label)) =
        FIXED.iter().find(|row| row.0 == channel.control)
    {
        let dimmer = attribute == "DIM";
        return Attribute {
            attribute: attribute.to_owned(),
            feature: feature.to_owned(),
            preset,
            attribute_label: attribute_label.to_owned(),
            feature_label: feature_label.to_owned(),
            effect: if dimmer { "DIMMER" } else { "NONE" },
            effect_sub: None,
        };
    }

    let module = field(&personality.name);
    let plugin = match slot {
        Some(ParameterSlot::Effect(parameter) | ParameterSlot::Generator(parameter)) => {
            Some(parameter)
        }
        _ => None,
    };

    if let Some(parameter) = plugin {
        if let Some(part) = parameter.component {
            let name = parameters
                .iter()
                .find(|defined| defined.key == parameter.key)
                .map_or(parameter.key, |defined| defined.name);
            // The first colour is the desk's own: RED, GREEN and BLUE with the
            // colour-mix effect, which is what puts it under the picker.
            if first_colour == Some(parameter.key) {
                let (attribute, effect, effect_sub) = match part {
                    ColourPart::Red => ("RED", "COLOR_MIX", Some("RED")),
                    ColourPart::Green => ("GREEN", "COLOR_MIX", Some("GREEN")),
                    ColourPart::Blue => ("BLUE", "COLOR_MIX", Some("BLUE")),
                    ColourPart::Alpha => ("ALPHA", "NONE", None),
                };
                return Attribute {
                    attribute: attribute.to_owned(),
                    feature: "RGB".to_owned(),
                    preset: "COLOR",
                    attribute_label: part.label().to_owned(),
                    feature_label: "RGB".to_owned(),
                    effect,
                    effect_sub,
                };
            }
            // A second colour has its own name. Two sets of RED in one
            // fixture would be one attribute twice.
            return Attribute {
                attribute: field(&format!("{name} {}", part.label())).to_uppercase(),
                feature: field(name).to_uppercase(),
                preset: "COLOR",
                attribute_label: field(&format!("{name} {}", part.label())),
                feature_label: field(name),
                effect: "NONE",
                effect_sub: None,
            };
        }
    }

    // Everything else: named after itself, grouped by its module.
    Attribute {
        attribute: field(&channel.name).to_uppercase(),
        feature: module.to_uppercase(),
        preset: if plugin.is_some() { "VIDEO" } else { "CONTROL" },
        attribute_label: field(&channel.name),
        feature_label: module,
        effect: "NONE",
        effect_sub: None,
    }
}

/// Writes one channel, and its fine half when it has one.
fn channel_block(
    out: &mut String,
    personality: &Personality,
    channel: &PersonalityChannel,
    attribute: &Attribute,
) {
    let wide = channel.width > 1;
    let stepped = matches!(
        channel.encoding,
        ChannelEncodingInfo::Switch { .. } | ChannelEncodingInfo::Stepped { .. }
    );

    let mut header = format!(
        "\t{{ _ATTRIBUT {} _FEATURE {} _PRESET {} _ATT_LABEL {} _FEA_LABEL {} _DEFAULT {}",
        quoted(&attribute.attribute),
        quoted(&attribute.feature),
        quoted(attribute.preset),
        quoted(&attribute.attribute_label),
        quoted(&attribute.feature_label),
        percent_of_home(channel),
    );
    if attribute.effect == "DIMMER" {
        // Highlight is full, as on every dimmer in the library.
        header.push_str(" _HIGHLITE 100.0000%");
    }
    if stepped {
        // A step is a step: it snaps rather than fading through the ones
        // between, which on a slot would play every clip on the way.
        header.push_str(" _ETIME 0.000000 _SNAP YES");
    } else {
        header.push_str(" _ETIME -1.000000");
    }
    header.push_str(" \t\t_CHANFUNC\r\n");
    out.push_str(&header);

    let (min, max) = match channel.encoding {
        ChannelEncodingInfo::Ranged8 { min, max } | ChannelEncodingInfo::Ranged16 { min, max } => {
            (min, max)
        }
        ChannelEncodingInfo::Index8 => (0.0, 255.0),
        _ => (0.0, 1.0),
    };
    let mut function = format!(
        "\t\t{{ _NAME {} _RANGE [  0,255] _EFF {}",
        padded(&quoted("Function")),
        attribute.effect
    );
    if let Some(sub) = attribute.effect_sub {
        function.push_str(&format!(" _EFFSUB {sub}"));
    }
    function.push_str(&format!(" _PHYS [{min:8.2},{max:8.2}]"));

    let sets = channel_sets(personality, channel, attribute);
    if sets.is_empty() {
        function.push_str(" }\r\n");
        out.push_str(&function);
    } else {
        function.push_str(" \t\t\t_CHANSET\r\n");
        out.push_str(&function);
        for (name, from, to) in sets {
            out.push_str(&format!(
                "\t\t\t{{ _NAME {} _RANGE [{from:.4}%,{to:.4}%] }}\r\n",
                padded(&quoted(&name))
            ));
        }
        out.push_str("\t\t}\r\n");
    }
    out.push_str("\t}\r\n");

    if wide {
        out.push_str(&format!(
            "\t{{ _ATTRIBUT {} _FEATURE {} _PRESET {} _TYPE FINE }}\r\n",
            quoted(&attribute.attribute),
            quoted(&attribute.feature),
            quoted(attribute.preset),
        ));
    }
}

/// The named values on a channel, as a name and a range in per cent.
fn channel_sets(
    personality: &Personality,
    channel: &PersonalityChannel,
    attribute: &Attribute,
) -> Vec<(String, f64, f64)> {
    match &channel.encoding {
        ChannelEncodingInfo::Stepped { bands } => bands
            .iter()
            .map(|band| {
                let (from, to) = byte_band(band.from, band.to);
                (step_name(personality, channel, band), from, to)
            })
            .collect(),
        ChannelEncodingInfo::Switch { threshold } => {
            let (off, on) = if channel.momentary {
                ("Idle", "Fire")
            } else {
                ("Off", "On")
            };
            let below = threshold.saturating_sub(1);
            let (low_from, low_to) = byte_band(0, below);
            let (high_from, high_to) = byte_band(*threshold, 255);
            vec![
                (off.to_owned(), low_from, low_to),
                (on.to_owned(), high_from, high_to),
            ]
        }
        // A colour and a dimmer are faders and nothing else, as they are on
        // every fixture in the library.
        ChannelEncodingInfo::Ranged8 { .. } | ChannelEncodingInfo::Ranged16 { .. }
            if attribute.preset == "COLOR" || attribute.effect == "DIMMER" =>
        {
            Vec::new()
        }
        ChannelEncodingInfo::Ranged8 { min, max } => landmarks(*min, *max, 255, channel.home)
            .into_iter()
            .map(|(name, dmx)| {
                let (from, to) = byte_band(dmx as u8, dmx as u8);
                (name, from, to)
            })
            .collect(),
        // Exactly the value, not a coarse step's worth of them: a desk sends
        // the start of a set, and a home of a scale of one has to be one.
        ChannelEncodingInfo::Ranged16 { min, max } => landmarks(*min, *max, 65_535, channel.home)
            .into_iter()
            .map(|(name, dmx)| {
                let at = f64::from(dmx) * 100.0 / 65_535.0;
                (name, at, at)
            })
            .collect(),
        ChannelEncodingInfo::Index8 => Vec::new(),
    }
}

/// The bottom, the middle, the top and home of a continuous channel, as named
/// values: the numbers somebody at a desk asked for (owner, 2026-10-16: "wo
/// Mitte ist, welche Scale wo liegt"). Home only where it is none of the
/// other three.
///
/// Each is named after what the server makes of the number sent, not after
/// the round figure it is near: the middle of an eight-bit channel is 128 of
/// 255, which is not quite half way.
fn landmarks(min: f64, max: f64, top: u32, home: u16) -> Vec<(String, u32)> {
    let middle = top.div_ceil(2);
    let value = |dmx: u32| min + (max - min) * f64::from(dmx) / f64::from(top);
    let mut sets = vec![
        (format!("Min {}", number(value(0))), 0),
        (format!("Center {}", number(value(middle))), middle),
        (format!("Max {}", number(value(top))), top),
    ];
    let home = u32::from(home);
    if home != 0 && home != middle && home != top {
        sets.push((format!("Home {}", number(value(home))), home));
    }
    sets
}

/// A band of eight-bit values as the desk's per cent of sixteen bits.
fn byte_band(from: u8, to: u8) -> (f64, f64) {
    (
        f64::from(from) * 256.0 * 100.0 / 65_535.0,
        (f64::from(to) * 256.0 + 255.0) * 100.0 / 65_535.0,
    )
}

/// What one step of a stepped channel is called on the desk.
fn step_name(
    personality: &Personality,
    channel: &PersonalityChannel,
    band: &ChannelBand,
) -> String {
    match channel.control.as_str() {
        "layer_slot" => {
            if band.value == "black" {
                "Black".to_owned()
            } else {
                format!("Slot {}", band.value)
            }
        }
        "master_speed_scale" => format!("{}x", band.value.trim_end_matches(".0")),
        _ => {
            // A module's choice: its own display name, which is what the
            // client shows beside the same channel.
            let named = parameters_of(personality).iter().find_map(|parameter| {
                let ParameterKind::Choice { choices, .. } = parameter.kind else {
                    return None;
                };
                if parameter.name != channel.name {
                    return None;
                }
                choices
                    .iter()
                    .find(|choice| choice.id == band.value)
                    .map(|choice| choice.name)
            });
            ascii(named.unwrap_or(&band.value))
        }
    }
}

/// The percentage a desk is told a channel rests at.
fn percent_of_home(channel: &PersonalityChannel) -> String {
    let home = f64::from(channel.home);
    let percent = if channel.width > 1 {
        home * 100.0 / 65_535.0
    } else {
        home * 256.0 * 100.0 / 65_535.0
    };
    format!("{percent:.4}%")
}

/// A number as a set's name shows it: two decimals at most, trimmed.
///
/// Two, because a name is read on a desk's screen: the home of a grid's
/// spacing is sent as 63.997 and reads as 64, which is what it is for.
/// `reference/FIXTURES.md` has the third decimal for anybody who needs it.
fn number(value: f64) -> String {
    let mut shown = format!("{value:.2}");
    if shown.contains('.') {
        shown = shown.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    if shown == "-0" {
        shown = "0".to_owned();
    }
    shown
}

/// A name cut to what the desk keeps, in characters it can show.
fn field(name: &str) -> String {
    ascii(name)
        .chars()
        .take(NAME_FIELD)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Only what a grandMA1 text file can carry.
///
/// The library is ASCII bar ten files, and those ten are one manufacturer's
/// accented names. A dash of ours would arrive as two characters of noise.
fn ascii(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\u{2013}' | '\u{2014}' => '-',
            '"' => '\'',
            c if c.is_ascii() && !c.is_ascii_control() => c,
            _ => '?',
        })
        .collect()
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", ascii(text))
}

/// A quoted name followed by the spacing MA's own files line names up with.
fn padded(quoted: &str) -> String {
    format!("{quoted:<12}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// One generated file, with every run of spaces made one, so a test reads
    /// what a line says rather than how MA's own files line it up.
    fn named(name: &str) -> String {
        let file = fixture_files()
            .into_iter()
            .find(|file| file.name == name)
            .unwrap_or_else(|| panic!("{name} is not generated"));
        let mut squeezed = String::with_capacity(file.contents.len());
        let mut last_space = false;
        for character in file.contents.chars() {
            if character == ' ' {
                if !last_space {
                    squeezed.push(' ');
                }
                last_space = true;
            } else {
                squeezed.push(character);
                last_space = false;
            }
        }
        squeezed
    }

    #[test]
    fn every_personality_a_desk_can_use_has_a_file_and_the_retired_ones_do_not() {
        let names: BTreeSet<String> = fixture_files().into_iter().map(|file| file.name).collect();
        for expected in [
            "OVS Master",
            "OVS Layer",
            "OVS FX Blur",
            "OVS FX Echo",
            "OVS FX Invert",
            "OVS FX Invert v2",
            "OVS Gen Solid Colour",
            "OVS Gen Strobe v2",
        ] {
            assert!(names.contains(expected), "{expected} is missing: {names:?}");
        }
        for retired in ["OVS FX Tint", "OVS Gen Clock", "OVS Gen Countdown"] {
            assert!(
                !names.contains(retired),
                "{retired} is retired and still generated"
            );
        }
    }

    #[test]
    fn a_file_has_one_block_per_channel_and_a_fine_block_per_wide_one() {
        // What a desk counts to work out the footprint. A fine half left out
        // shifts every channel after it by one, and nothing says so until a
        // fader moves the wrong thing.
        for personality in catalogue().fixtures.iter().filter(|p| !retired(p)) {
            let text = fixture_type(personality);
            let blocks = text.matches("\t{ _ATTRIBUT").count();
            assert_eq!(
                blocks,
                usize::from(personality.footprint),
                "{} has {blocks} channel blocks for a footprint of {}",
                personality.type_id,
                personality.footprint
            );
            let fine = text.matches("_TYPE FINE").count();
            let wide = personality.channels.iter().filter(|c| c.width > 1).count();
            assert_eq!(fine, wide, "{}", personality.type_id);
        }
    }

    #[test]
    fn names_fit_the_desk_and_no_attribute_is_used_twice_in_a_fixture() {
        for personality in catalogue().fixtures.iter().filter(|p| !retired(p)) {
            let layout = Layout::of(personality);
            let mut seen = BTreeSet::new();
            for attribute in &layout.attributes {
                for name in [
                    &attribute.attribute,
                    &attribute.feature,
                    &attribute.attribute_label,
                    &attribute.feature_label,
                ] {
                    assert!(
                        name.len() <= NAME_FIELD && !name.is_empty(),
                        "{}: {name:?} does not fit",
                        personality.type_id
                    );
                }
                assert!(
                    seen.insert(attribute.attribute.clone()),
                    "{}: {} twice",
                    personality.type_id,
                    attribute.attribute
                );
            }
        }
    }

    #[test]
    fn the_files_are_ascii_with_crlf_and_balanced_braces() {
        for file in fixture_files() {
            assert!(file.contents.is_ascii(), "{}", file.file_name);
            assert!(
                !file.contents.replace("\r\n", "").contains('\n'),
                "{} has a bare line feed",
                file.file_name
            );
            let open = file.contents.matches('{').count();
            let close = file.contents.matches('}').count();
            assert_eq!(open, close, "{}", file.file_name);
            assert!(file.contents.contains("_MANUFAC    \"testbild.media\""));
        }
    }

    #[test]
    fn the_layer_names_every_slot_where_the_server_reads_it() {
        // "inkl. aller Slots": Black and eighty-four slots, each where the
        // decoder puts it. Slot 1 is DMX 4 to 6, which is 1024 to 1791 of
        // 65535 on the desk.
        let layer = named("OVS Layer");
        assert!(
            layer.contains("{ _NAME \"Black\" _RANGE [0.0000%,1.5610%] }"),
            "{layer}"
        );
        assert!(
            layer.contains("{ _NAME \"Slot 1\" _RANGE [1.5625%,2.7329%] }"),
            "{layer}"
        );
        assert!(
            layer.contains("{ _NAME \"Slot 84\" _RANGE [98.8296%,100.0000%] }"),
            "{layer}"
        );
        assert_eq!(layer.matches("\"Slot ").count(), 84);
    }

    #[test]
    fn a_wide_home_is_the_exact_value_so_a_scale_of_one_is_one() {
        // 6554 of 65535 is a scale of 1.0001; one step either side is 0.9999
        // or 1.0003. A set as wide as a coarse step would land anywhere in
        // 6400 to 6655.
        let layer = named("OVS Layer");
        assert!(
            layer.contains("{ _NAME \"Home 1\" _RANGE [10.0008%,10.0008%] }"),
            "{layer}"
        );
        assert!(layer.contains(
            "_ATTRIBUT \"X SCALE\" _FEATURE \"SCALE\" _PRESET \"PAN/TILT\" \
             _ATT_LABEL \"X Scale\" _FEA_LABEL \"Scale\" _DEFAULT 10.0008%"
        ));
        // And the centre of a position is the middle the server reads as nought.
        assert!(layer.contains("{ _NAME \"Center 0\" _RANGE [50.0008%,50.0008%] }"));
    }

    #[test]
    fn the_first_colour_is_the_desks_own_and_a_second_has_its_own_name() {
        // The colour picker drives RED, GREEN and BLUE with the colour-mix
        // effect. A second colour on the same fixture cannot be those again.
        let grid = named("OVS Gen Grid");
        assert!(grid.contains("_ATTRIBUT \"RED\" _FEATURE \"RGB\" _PRESET \"COLOR\""));
        assert!(grid.contains("_EFF COLOR_MIX _EFFSUB RED"));
        assert_eq!(grid.matches("_ATTRIBUT \"RED\"").count(), 1);
        assert!(grid.contains("_ATTRIBUT \"BACKGROUND RED\""), "{grid}");
    }

    #[test]
    fn a_choice_is_named_as_the_client_names_it() {
        let echo = named("OVS FX Echo");
        assert!(
            echo.contains("{ _NAME \"Behind\" _RANGE [0.0000%,49.9992%] }"),
            "{echo}"
        );
        assert!(
            echo.contains("{ _NAME \"Lighten\" _RANGE [50.0008%,100.0000%] }"),
            "{echo}"
        );
    }

    #[test]
    fn the_master_names_its_speed_steps_and_its_triggers() {
        let master = named("OVS Master");
        assert!(master.contains("\"0.125x\""), "{master}");
        assert!(master.contains("\"1x\""), "{master}");
        assert!(master.contains("_ATTRIBUT \"BPM TAP\""));
        assert!(
            master.contains("{ _NAME \"Fire\" _RANGE [50.0008%,100.0000%] }"),
            "{master}"
        );
    }

    #[test]
    fn names_line_up_the_way_mas_own_files_line_them_up() {
        // A quoted name padded to twelve and one space, as in MA's own
        // `{ _NAME "min"        _RANGE [0.0000%,0.3891%] }`.
        let raw = fixture_files()
            .into_iter()
            .find(|file| file.name == "OVS Layer")
            .expect("the layer")
            .contents;
        assert!(raw.contains("\t\t{ _NAME \"Function\"   _RANGE [  0,255] _EFF DIMMER _PHYS [    0.00,    1.00] }\r\n"), "{raw}");
        assert!(
            raw.contains("\t\t\t{ _NAME \"Black\"      _RANGE [0.0000%,1.5610%] }\r\n"),
            "{raw}"
        );
    }
}
