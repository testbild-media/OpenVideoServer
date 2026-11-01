//! The personalities as fixture types a grandMA2 and a dot2 import.
//!
//! The owner, 2026-10-21: "du kannst jetzt bitte auch komplette Fixtures für
//! GrandMA 2 und 3 bauen, sowie dot2. Bitte implementiere mit bestem Gewissen."
//!
//! # One file for both desks
//!
//! A grandMA2 and a dot2 read the same XML: MA's own Fixture Builder writes one
//! file and puts it in both libraries, `gma2/library` and `dot2/library`
//! (help.malighting.com, Fixture Builder, "Export fixture"). So one file, made
//! here, committed under both `reference/grandma2/` and `reference/dot2/`.
//!
//! # Where the format comes from
//!
//! No grandMA2 or dot2 was at hand. Everything here is read off real files —
//! console exports from grandMA2 3.1.2 and 3.9.60, MA's own generic dimmer and
//! a Fixture Builder 1.9 file — and MA's help and forum; the schema's host
//! answers nothing any more. **Not yet imported on a console**, and
//! `reference/grandma2/README.md` lists what to look at first.
//!
//! - **The version a dot2 still takes.** A dot2 refuses a file tagged newer than
//!   itself, without a word ("nothing happens… no error messages", MA forum),
//!   and its last release is 1.9. These carry the Fixture Builder 1.9 header,
//!   `major_vers="3" minor_vers="2" stream_vers="2"`, which both desks import.
//! - **A sixteen-bit channel** is `coarse` and `fine`, the channel and the next.
//! - **Values on two scales.** A channel function's range is always 24-bit,
//!   `min_dmx_24` to `max_dmx_24`; a named value and a default are in the
//!   channel's own resolution, 0 to 255 or 0 to 65535.
//! - **An attribute the desk does not have is made on import**, with the
//!   feature and preset the file gives it; one it has keeps its own place
//!   (MA forum, 2009). So the owner's own names are prefixed where a desk might
//!   already have the plain word.
//!
//! # The same layout as the grandMA1
//!
//! Every channel goes where the owner put it on his grandMA1 (2026-10-18; see
//! `grandma1.rs`), under the names his desk gave them where a grandMA2 has no
//! name of its own for the thing:
//!
//! | Role | Attribute | Feature | Preset |
//! |---|---|---|---|
//! | dimmer | `DIM` | DIMMER | DIMMER |
//! | X, Y | `PAN`, `TILT` | POSITION | POSITION |
//! | rotation | `OVS_ROTATION` | POSITION | POSITION |
//! | scale | `OVS_SCALE_X`, `OVS_SCALE_Y` | OVS_SCALE | POSITION |
//! | parameter n | `PARA_`n | OVS_PARAMETER | VIDEO |
//! | colour | `COLORRGB1`, `2`, `3` | COLORRGB | COLOR |
//! | its alpha | `OVS_ALPHA` | OVS_ALPHA | COLOR |
//! | background | `OVS_BG_RED` … `OVS_BG_ALPHA` | OVS_RGBX_BG | COLOR |
//!
//! `DIM`, `PAN`, `TILT` and `COLORRGB1` to `3` are the desk's own, so its
//! masters, trackball and colour picker take them for what they are. The alpha
//! is a feature of its own rather than a fourth colour, because a colour
//! picker that thought it an emitter would set it to nought for every
//! saturated colour and make the picture transparent.
//!
//! # Named values
//!
//! Exactly the grandMA1's (owner, 2026-10-18: "Es ist immer von bis Wert"): a
//! step is its whole band, a landmark the eight-bit step it is in, in the order
//! they sit on the channel, and a dimmer or a colour has none. A desk shows the
//! channel's physical range beside its value, so a hue is typed in degrees.

use ovs_types::config::fixtures::ColourPart;

use crate::fixtures::Personality;
use crate::grandma1::{
    comment, current_personalities, desk_channels, full_name, short_full_name, DeskChannel, Role,
    MANUFACTURER,
};

/// One fixture type, ready to be written to disk and imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ma2File {
    /// `testbild_media@ovs_fx_blur@standard.xml`; see [`file_name`].
    pub file_name: String,
    /// What the desk's fixture list calls it.
    pub name: String,
    /// The file.
    pub contents: String,
}

/// The one mode every fixture here has.
const MODE: &str = "Standard";

/// Every fixture type a grandMA2 or a dot2 needs for this build.
#[must_use]
pub fn fixture_files() -> Vec<Ma2File> {
    current_personalities()
        .iter()
        .map(|personality| Ma2File {
            file_name: file_name(personality),
            name: full_name(personality),
            contents: fixture_type(personality),
        })
        .collect()
}

/// `manufacturer@fixture@mode.xml`, in lower case with underscores, as MA's
/// own library names them (`generic@dimmer@00`); the desk's import filters by
/// what is either side of an `@`.
#[must_use]
pub fn file_name(personality: &Personality) -> String {
    let part = |text: &str| {
        text.to_lowercase()
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!(
        "{}@{}@{}.xml",
        part(MANUFACTURER),
        part(&full_name(personality)),
        part(MODE)
    )
}

/// Whether a file in the fixture folder is one of these, of any vintage.
#[must_use]
pub fn is_fixture_file(name: &str) -> bool {
    name.starts_with("testbild_media@") && name.ends_with(".xml")
}

/// What the desk's short-name column shows: eight characters, as MA's Fixture
/// Builder cuts it.
#[must_use]
pub fn short_name(personality: &Personality) -> String {
    short_full_name(personality)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect()
}

/// Renders one personality as a grandMA2 fixture type.
#[must_use]
pub fn fixture_type(personality: &Personality) -> String {
    let mut out = String::new();
    let mut line = |depth: usize, text: &str| {
        out.push_str(&"  ".repeat(depth));
        out.push_str(text);
        out.push('\n');
    };

    line(0, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    // The Fixture Builder 1.9 header: what a dot2 takes, and a grandMA2 too.
    line(
        0,
        r#"<MA xmlns:xml="http://www.w3.org/XML/1998/namespace" major_vers="3" minor_vers="2" stream_vers="2">"#,
    );
    line(
        1,
        &format!(
            r#"<FixtureType index="0" name="{}" mode="{MODE}">"#,
            xml(&full_name(personality))
        ),
    );
    line(2, "<InfoItems>");
    line(3, &format!("<Info>{}</Info>", xml(&comment(personality))));
    line(2, "</InfoItems>");
    line(
        2,
        &format!("<short_name>{}</short_name>", xml(&short_name(personality))),
    );
    line(
        2,
        &format!("<manufacturer>{}</manufacturer>", xml(MANUFACTURER)),
    );
    line(2, "<short_manufacturer>testbild</short_manufacturer>");
    line(2, r#"<Modules index="0">"#);
    line(
        3,
        r#"<Module index="0" name="Main Module" class="None" beamtype="None" beam_angle="0" beam_intensity="0">"#,
    );
    for (index, channel) in desk_channels(personality).iter().enumerate() {
        channel_type(&mut line, index, channel);
    }
    line(3, "</Module>");
    line(2, "</Modules>");
    line(2, r#"<Instances index="1">"#);
    line(3, r#"<Instance index="0" module_index="0" patch="1"/>"#);
    line(2, "</Instances>");
    line(2, r#"<Wheels index="2"/>"#);
    line(2, r#"<FixtureMacroCollect index="5"/>"#);
    line(1, "</FixtureType>");
    line(0, "</MA>");
    out
}

/// One channel, its one function and the function's named values.
fn channel_type(line: &mut impl FnMut(usize, &str), index: usize, channel: &DeskChannel) {
    let names = names(channel.role);
    let wide = channel.width > 1;
    // In the channel's own resolution, as a desk writes a default.
    let native = |value: u16| {
        if wide {
            u32::from(value)
        } else {
            u32::from(value >> 8)
        }
    };

    let mut open = format!(
        r#"<ChannelType index="{index}" attribute="{}" feature="{}" preset="{}" coarse="{}""#,
        names.attribute, names.feature, names.preset, channel.offset
    );
    if wide {
        open.push_str(&format!(r#" fine="{}""#, channel.offset + 1));
    }
    if channel.default != 0 {
        open.push_str(&format!(r#" default="{}""#, native(channel.default)));
    }
    if channel.role == Role::Dimmer {
        // Highlight is full, as on every dimmer in MA's library.
        open.push_str(r#" highlight_value="100""#);
    }
    if let Some(colour) = names.colour {
        open.push_str(&format!(r#" color="{colour}""#));
    }
    open.push('>');
    line(4, &open);

    let (from, to) = channel.physical;
    let function = format!(
        r#"<ChannelFunction index="0" name="{}" from="0" to="100" min_dmx_24="0" max_dmx_24="16777215" physfrom="{}" physto="{}" subattribute="{attribute}" subattribute_user_name="{label}" attribute="{attribute}" attribute_user_name="{label}" feature="{feature}" feature_user_name="{feature_label}" preset="{preset}" preset_user_name="{preset_label}""#,
        xml(&channel.function),
        number(from),
        number(to),
        attribute = names.attribute,
        label = names.label,
        feature = names.feature,
        feature_label = names.feature_label,
        preset = names.preset,
        preset_label = names.preset_label,
    );
    if channel.sets.is_empty() {
        line(5, &format!("{function}/>"));
    } else {
        line(5, &format!("{function}>"));
        for (set, (name, from, to)) in channel.sets.iter().enumerate() {
            line(
                6,
                &format!(
                    r#"<ChannelSet index="{set}" name="{}" from_dmx="{}" to_dmx="{}"/>"#,
                    xml(name),
                    native(*from),
                    native(*to)
                ),
            );
        }
        line(5, "</ChannelFunction>");
    }
    line(4, "</ChannelType>");
}

/// What a role is called on a grandMA2.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Names {
    attribute: String,
    label: String,
    feature: &'static str,
    feature_label: &'static str,
    preset: &'static str,
    preset_label: &'static str,
    /// The emitter's colour, for the colour picker.
    colour: Option<&'static str>,
}

fn names(role: Role) -> Names {
    let named = |attribute: &str,
                 label: &str,
                 feature: &'static str,
                 feature_label: &'static str,
                 preset: &'static str,
                 preset_label: &'static str| Names {
        attribute: attribute.to_owned(),
        label: label.to_owned(),
        feature,
        feature_label,
        preset,
        preset_label,
        colour: None,
    };
    let rgb = |attribute: &str, label: &str, colour: &'static str| Names {
        colour: Some(colour),
        ..named(attribute, label, "COLORRGB", "MixColor", "COLOR", "Color")
    };
    let background = |attribute: &str, label: &str| {
        named(
            attribute,
            label,
            "OVS_RGBX_BG",
            "OVS BG RGBX",
            "COLOR",
            "Color",
        )
    };
    match role {
        Role::Dimmer => named("DIM", "Dim", "DIMMER", "Dimmer", "DIMMER", "Dimmer"),
        Role::PositionX => named("PAN", "Pan", "POSITION", "Position", "POSITION", "Position"),
        Role::PositionY => named(
            "TILT", "Tilt", "POSITION", "Position", "POSITION", "Position",
        ),
        Role::Rotation => named(
            "OVS_ROTATION",
            "Rotation",
            "POSITION",
            "Position",
            "POSITION",
            "Position",
        ),
        Role::ScaleX => named(
            "OVS_SCALE_X",
            "Scale X",
            "OVS_SCALE",
            "Scale",
            "POSITION",
            "Position",
        ),
        Role::ScaleY => named(
            "OVS_SCALE_Y",
            "Scale Y",
            "OVS_SCALE",
            "Scale",
            "POSITION",
            "Position",
        ),
        Role::Parameter(number) => named(
            &format!("PARA_{number}"),
            &format!("Parameter {number}"),
            "OVS_PARAMETER",
            "OVS Parameter",
            "VIDEO",
            "Video",
        ),
        Role::Foreground(ColourPart::Red) => rgb("COLORRGB1", "R", "ff0000"),
        Role::Foreground(ColourPart::Green) => rgb("COLORRGB2", "G", "00ff00"),
        Role::Foreground(ColourPart::Blue) => rgb("COLORRGB3", "B", "0000ff"),
        Role::Foreground(ColourPart::Alpha) => named(
            "OVS_ALPHA",
            "Alpha",
            "OVS_ALPHA",
            "OVS Alpha",
            "COLOR",
            "Color",
        ),
        Role::Background(ColourPart::Red) => background("OVS_BG_RED", "BG Red"),
        Role::Background(ColourPart::Green) => background("OVS_BG_GREEN", "BG Green"),
        Role::Background(ColourPart::Blue) => background("OVS_BG_BLUE", "BG Blue"),
        Role::Background(ColourPart::Alpha) => background("OVS_BG_ALPHA", "BG Alpha"),
    }
}

/// A physical value as MA's files write one: no more decimals than it has.
fn number(value: f64) -> String {
    let mut shown = format!("{value:.6}");
    if shown.contains('.') {
        shown = shown.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    if shown == "-0" {
        shown = "0".to_owned();
    }
    shown
}

/// Text as XML: an attribute value or an element's text.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::PersonalityTarget;

    fn of(type_id: &str) -> Personality {
        current_personalities()
            .into_iter()
            .find(|personality| personality.type_id == type_id)
            .unwrap_or_else(|| panic!("no {type_id}"))
    }

    #[test]
    fn there_is_one_for_every_fixture_the_grandma1_has() {
        assert_eq!(
            fixture_files().len(),
            crate::grandma1::fixture_files().len()
        );
    }

    #[test]
    fn the_header_is_one_a_dot2_still_takes() {
        // A dot2 ignores a file tagged newer than 1.9 without a word.
        for file in fixture_files() {
            assert!(
                file.contents
                    .contains(r#"major_vers="3" minor_vers="2" stream_vers="2""#),
                "{}",
                file.file_name
            );
        }
    }

    #[test]
    fn every_channel_is_where_the_personality_says() {
        for personality in current_personalities() {
            let text = fixture_type(&personality);
            for channel in &personality.channels {
                let at = if channel.width > 1 {
                    format!(
                        r#"coarse="{}" fine="{}""#,
                        channel.offset,
                        channel.offset + 1
                    )
                } else {
                    format!(r#"coarse="{}""#, channel.offset)
                };
                assert!(
                    text.contains(&at),
                    "{} channel {} is not at {at}",
                    personality.type_id,
                    channel.name
                );
            }
        }
    }

    #[test]
    fn every_attribute_is_used_once_in_a_fixture() {
        for personality in current_personalities() {
            let mut attributes: Vec<String> = desk_channels(&personality)
                .iter()
                .map(|channel| names(channel.role).attribute)
                .collect();
            let all = attributes.len();
            attributes.sort();
            attributes.dedup();
            assert_eq!(attributes.len(), all, "{}", personality.type_id);
        }
    }

    #[test]
    fn a_layers_slot_is_its_bands_in_eight_bit_values() {
        let text = fixture_type(&of("ovs.layer"));
        assert!(
            text.contains(r#"name="Black" from_dmx="0" to_dmx="3""#),
            "{text}"
        );
        assert!(text.contains(r#"name="Slot 1" from_dmx="4" to_dmx="6""#));
        assert!(text.contains(r#"name="Slot 84" from_dmx="253" to_dmx="255""#));
        assert!(text.contains(r#"attribute="PAN""#));
        assert!(text.contains(r#"attribute="TILT""#));
    }

    #[test]
    fn a_wide_channel_writes_its_values_in_sixteen_bits() {
        let text = fixture_type(&of("ovs.layer"));
        // Scale rests at 1 of 0 to 10, which is 6554 of 65535.
        let scale = text
            .lines()
            .find(|line| line.contains(r#"attribute="OVS_SCALE_X""#) && line.contains("coarse"))
            .expect("a scale channel");
        assert!(scale.contains(r#"fine="#), "{scale}");
        assert!(scale.contains(r#"default="6554""#), "{scale}");
    }

    #[test]
    fn a_module_amount_is_the_dimmer_and_rests_at_nought() {
        let text = fixture_type(&of("ovs.blur"));
        let dimmer = text
            .lines()
            .find(|line| line.contains(r#"<ChannelType"#) && line.contains(r#"attribute="DIM""#))
            .expect("a dimmer");
        assert!(!dimmer.contains("default="), "{dimmer}");
        assert!(dimmer.contains(r#"highlight_value="100""#));
        assert!(text.contains(r#"name="Radius""#), "{text}");
    }

    #[test]
    fn the_foreground_colour_is_the_desks_own_mix_colour() {
        let text = fixture_type(&of("ovs.solid_colour"));
        for attribute in ["COLORRGB1", "COLORRGB2", "COLORRGB3"] {
            assert!(
                text.contains(&format!(r#"attribute="{attribute}""#)),
                "{attribute}"
            );
        }
        assert!(text.contains(r#"color="ff0000""#));
    }

    #[test]
    fn short_names_are_eight_characters_and_tell_every_fixture_apart() {
        let mut shorts: Vec<String> = current_personalities().iter().map(short_name).collect();
        assert!(shorts
            .iter()
            .all(|short| !short.is_empty() && short.len() <= 8));
        let all = shorts.len();
        shorts.sort();
        shorts.dedup();
        assert_eq!(shorts.len(), all, "two fixtures share a short name");
    }

    #[test]
    fn the_file_names_are_the_librarys_form() {
        let master = current_personalities()
            .into_iter()
            .find(|personality| personality.target == PersonalityTarget::Master)
            .expect("a master");
        assert_eq!(file_name(&master), "testbild_media@ovs_master@standard.xml");
        assert!(fixture_files()
            .iter()
            .all(|file| is_fixture_file(&file.file_name)));
    }

    #[test]
    fn every_file_is_well_formed_enough_to_close_what_it_opens() {
        for file in fixture_files() {
            for element in ["FixtureType", "Modules", "Module", "Instances", "MA"] {
                let opened = file.contents.matches(&format!("<{element} ")).count()
                    + file.contents.matches(&format!("<{element}>")).count();
                let closed = file.contents.matches(&format!("</{element}>")).count();
                assert_eq!(opened, closed, "{} {element}", file.file_name);
            }
            assert_eq!(
                file.contents.matches("<ChannelType ").count(),
                file.contents.matches("</ChannelType>").count(),
                "{}",
                file.file_name
            );
        }
    }
}
