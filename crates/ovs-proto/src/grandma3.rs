//! The personalities as GDTF fixture types, which a grandMA3 imports.
//!
//! The owner, 2026-10-21: "du kannst jetzt bitte auch komplette Fixtures für
//! GrandMA 2 und 3 bauen, sowie dot2. Bitte implementiere mit bestem Gewissen."
//!
//! # Why GDTF
//!
//! A grandMA3 imports two kinds of fixture type: its own XML and GDTF
//! (DIN SPEC 15800). Its own XML has no published description, is tied to the
//! console's version, and is imported as an empty fixture type without a word
//! when anything in it is wrong. GDTF has a specification, a schema and a set of
//! rules, all public (github.com/mvrdevelopment/spec and /tools), and every
//! grandMA3 since 1.x reads it. So GDTF 1.2, and this is written to that.
//!
//! **Not yet imported on a console.** Every rule below is the specification's
//! or MA's release notes', and `reference/grandma3/README.md` says how to
//! import; what a grandMA3 makes of these files has to be seen on one.
//!
//! # What a grandMA3 needs of the file itself
//!
//! - **Stored, not compressed.** MA's notes for 2.4: "It was not possible to
//!   import compressed GDTF zip files" — so a console before 2.4 takes only a
//!   stored archive ([`crate::zip::stored`]).
//! - **`.gdtf` in lower case.** The same notes: files named `*.GDTF` could not
//!   be imported.
//! - **One attribute once per geometry.** 2.2: a mode with conflicting
//!   attributes in one geometry blocked the whole type. Every channel here has
//!   an attribute of its own, and a test holds every file to it.
//! - **No `Display` geometry**, which crashed 1.9. One plain geometry, `Body`.
//!
//! # The same layout as the grandMA1
//!
//! Every channel goes where the owner put it on his grandMA1 (2026-10-18; see
//! `grandma1.rs`): a module's amount on the dimmer, a layer's X and Y on pan and
//! tilt so the trackball moves it, its turn and size on rotation and scale,
//! everything else a numbered parameter under Video, and two colours. In GDTF's
//! own attribute names where GDTF has one, so the desk's tools take them for
//! what they are:
//!
//! | Role | Attribute | Feature |
//! |---|---|---|
//! | dimmer | `Dimmer` | Dimmer.Dimmer |
//! | X, Y | `Pan`, `Tilt` | Position.PanTilt |
//! | rotation | `Rot_Z` | Position.Rotation |
//! | scale | `Scale_X`, `Scale_Y` | Position.Scale |
//! | parameter n | `VideoEffect1Parameter`n | Video.Video |
//! | colour | `ColorAdd_R`, `_G`, `_B` | Color.RGB |
//! | its alpha | `OVS_Alpha` | Color.Alpha |
//! | background | `OVS_BG_R`, `_G`, `_B`, `OVS_BG_Alpha` | Color.Background |
//!
//! **The parameters share their attributes across fixtures**, as the
//! grandMA1's `PARA_1` to `PARA_8` do: a grandMA3 show holds 1,024 attributes,
//! and one per control of every module would spend them. What a parameter is on
//! this fixture is its channel function's name — Blur's Parameter 1 is
//! "Radius". The alpha and the background have no GDTF attribute, so they are
//! declared here, in features of their own: an alpha in GDTF's RGB would be
//! taken for a fourth emitter by the colour picker, which would set it to nought
//! for every saturated colour and make the picture transparent.
//!
//! # Values
//!
//! A step of a stepped channel and each side of a switch are channel sets,
//! which in GDTF run from where they start to where the next begins — so a
//! step's set is exactly the band the server reads as that step, as the
//! grandMA1's are (owner, 2026-10-18: "immer von bis Wert"). A hue's colours
//! are sets too. A continuous channel has none: its physical range is the
//! control's own, so a grandMA3 shows and takes the value in its own units
//! (degrees, kelvin, pixels), which is what the grandMA1's named landmarks were
//! standing in for.

use ovs_types::config::fixtures::ColourPart;

use crate::fixtures::{ChannelEncodingInfo, Personality};
use crate::grandma1::{
    comment, current_personalities, desk_channels, full_name, short_full_name, DeskChannel, Role,
    MANUFACTURER,
};
use crate::zip;

/// One fixture type, ready to be written to disk and imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GdtfFile {
    /// `testbild media@OVS FX Blur.gdtf`; see [`file_name`].
    pub file_name: String,
    /// What the desk's fixture list calls it.
    pub name: String,
    /// The `description.xml` inside, for reading and for tests.
    pub description: String,
    /// The `.gdtf` itself: a stored ZIP with the description in it.
    pub contents: Vec<u8>,
}

/// The one geometry every fixture here has, and every channel is on.
const GEOMETRY: &str = "Body";

/// The one DMX mode every fixture here has.
const MODE: &str = "Standard";

/// Every fixture type a grandMA3 needs for this build: the master, the layer,
/// and every effect and generator a client can add that declares DMX.
#[must_use]
pub fn fixture_files() -> Vec<GdtfFile> {
    current_personalities()
        .iter()
        .map(|personality| {
            let description = description(personality);
            let contents = zip::stored(&[(
                "description.xml".to_owned(),
                description.clone().into_bytes(),
            )])
            .unwrap_or_default();
            GdtfFile {
                file_name: file_name(personality),
                name: name(&full_name(personality)),
                description,
                contents,
            }
        })
        .collect()
}

/// `MANUFACTURER@NAME.gdtf`, the form GDTF recommends, with the manufacturer's
/// dot a space as in the grandMA1's file names, and the extension in lower
/// case: a grandMA3 before 2.4 does not import `.GDTF`.
#[must_use]
pub fn file_name(personality: &Personality) -> String {
    format!(
        "{}@{}.gdtf",
        MANUFACTURER.replace('.', " "),
        name(&full_name(personality))
    )
}

/// Whether a file in the fixture folder is one of these, of any vintage.
#[must_use]
pub fn is_fixture_file(name: &str) -> bool {
    name.ends_with(".gdtf")
        && (name.starts_with(&format!("{}@", MANUFACTURER.replace('.', " ")))
            || name.starts_with(&format!("{MANUFACTURER}@")))
}

/// The `description.xml` of one personality.
#[must_use]
pub fn description(personality: &Personality) -> String {
    let channels = desk_channels(personality);
    let mut out = String::new();
    let mut line = |depth: usize, text: &str| {
        out.push_str(&"  ".repeat(depth));
        out.push_str(text);
        out.push('\n');
    };

    line(
        0,
        r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>"#,
    );
    line(0, r#"<GDTF DataVersion="1.2">"#);
    line(
        1,
        &format!(
            r#"<FixtureType CanHaveChildren="No" Description="{}" FixtureTypeID="{}" LongName="{}" Manufacturer="{}" Name="{}" RefFT="" ShortName="{}" Thumbnail="">"#,
            xml(&comment(personality)),
            fixture_type_id(personality),
            xml(&name(&full_name(personality))),
            xml(MANUFACTURER),
            xml(&name(&full_name(personality))),
            xml(&name(&short_full_name(personality))),
        ),
    );

    // What attributes this fixture uses, each declared once, in the order a
    // desk's attribute list shows them.
    let mut used: Vec<&'static Definition> = channels
        .iter()
        .map(|channel| definition(channel.role))
        .collect();
    used.sort_by_key(|definition| (group_rank(definition.group), definition.order));
    used.dedup_by_key(|definition| definition.name);

    line(2, "<AttributeDefinitions>");
    let activation: Vec<&str> = {
        let mut groups: Vec<&str> = used.iter().filter_map(|d| d.activation).collect();
        groups.dedup();
        groups
    };
    if activation.is_empty() {
        line(3, "<ActivationGroups/>");
    } else {
        line(3, "<ActivationGroups>");
        for group in &activation {
            line(4, &format!(r#"<ActivationGroup Name="{group}"/>"#));
        }
        line(3, "</ActivationGroups>");
    }
    line(3, "<FeatureGroups>");
    let mut groups: Vec<&str> = used.iter().map(|d| d.group).collect();
    groups.dedup();
    for group in groups {
        line(
            4,
            &format!(r#"<FeatureGroup Name="{group}" Pretty="{group}">"#),
        );
        let mut features: Vec<&str> = used
            .iter()
            .filter(|d| d.group == group)
            .map(|d| d.feature)
            .collect();
        features.dedup();
        for feature in features {
            line(5, &format!(r#"<Feature Name="{feature}"/>"#));
        }
        line(4, "</FeatureGroup>");
    }
    line(3, "</FeatureGroups>");
    line(3, "<Attributes>");
    for definition in &used {
        let activation = definition
            .activation
            .map(|group| format!(r#" ActivationGroup="{group}""#))
            .unwrap_or_default();
        line(
            4,
            &format!(
                r#"<Attribute{activation} Feature="{}.{}" Name="{}" PhysicalUnit="{}" Pretty="{}"/>"#,
                definition.group,
                definition.feature,
                definition.name,
                definition.unit,
                definition.pretty,
            ),
        );
    }
    line(3, "</Attributes>");
    line(2, "</AttributeDefinitions>");

    line(2, "<Wheels/>");
    line(2, "<PhysicalDescriptions/>");
    line(2, "<Models/>");
    line(2, "<Geometries>");
    line(3, &format!(r#"<Geometry Name="{GEOMETRY}"/>"#));
    line(2, "</Geometries>");

    line(2, "<DMXModes>");
    line(
        3,
        &format!(
            r#"<DMXMode Description="{} channels" Geometry="{GEOMETRY}" Name="{MODE}">"#,
            personality.footprint
        ),
    );
    line(4, "<DMXChannels>");
    for channel in &channels {
        dmx_channel(&mut line, channel);
    }
    line(4, "</DMXChannels>");
    line(4, "<Relations/>");
    line(4, "<FTMacros/>");
    line(3, "</DMXMode>");
    line(2, "</DMXModes>");
    line(2, "<Revisions/>");
    line(2, "<FTPresets/>");
    line(2, "<Protocols/>");
    line(1, "</FixtureType>");
    line(0, "</GDTF>");
    out
}

/// One DMX channel: its logical channel, its one channel function, and the
/// function's sets.
fn dmx_channel(line: &mut impl FnMut(usize, &str), channel: &DeskChannel) {
    let definition = definition(channel.role);
    let attribute = definition.name;
    let wide = channel.width > 1;
    let offset = if wide {
        format!("{},{}", channel.offset, channel.offset + 1)
    } else {
        channel.offset.to_string()
    };
    let function = name(&channel.function);
    let highlight = if channel.role == Role::Dimmer {
        dmx(u16::MAX, wide)
    } else {
        "None".to_owned()
    };

    line(
        5,
        &format!(
            r#"<DMXChannel DMXBreak="1" Geometry="{GEOMETRY}" Highlight="{highlight}" InitialFunction="{GEOMETRY}_{attribute}.{attribute}.{}" Offset="{offset}">"#,
            xml(&function)
        ),
    );
    line(
        6,
        &format!(
            r#"<LogicalChannel Attribute="{attribute}" DMXChangeTimeLimit="0.000000" Master="{}" MibFade="0.000000" Snap="{}">"#,
            // A dimmer answers to the grand master, as the grandMA1's do: a
            // show faded out takes the effects' amounts down on the way.
            if channel.role == Role::Dimmer {
                "Grand"
            } else {
                "None"
            },
            if channel.snaps { "Yes" } else { "No" },
        ),
    );

    let (from, to) = channel.physical;
    let sets = sets(channel);
    let open = format!(
        r#"<ChannelFunction Attribute="{attribute}" CustomName="" DMXFrom="{}" Default="{}" Max="{to:.6}" Min="{from:.6}" Name="{}" OriginalAttribute="" PhysicalFrom="{from:.6}" PhysicalTo="{to:.6}" RealAcceleration="0.000000" RealFade="0.000000""#,
        dmx(0, wide),
        dmx(channel.default, wide),
        xml(&function),
    );
    if sets.is_empty() {
        line(7, &format!("{open}/>"));
    } else {
        line(7, &format!("{open}>"));
        for (set, start) in &sets {
            line(
                8,
                &format!(
                    r#"<ChannelSet DMXFrom="{}" Name="{}"/>"#,
                    dmx(*start, wide),
                    xml(set)
                ),
            );
        }
        line(7, "</ChannelFunction>");
    }
    line(6, "</LogicalChannel>");
    line(5, "</DMXChannel>");
}

/// A channel's sets as a name and where it starts, as a sixteen-bit value.
///
/// A step and each side of a switch start where the server starts reading it
/// as that, and run to the next — so the bands are the server's own, not the
/// grandMA1's dimmer-switch shorthand of nought and full. A hue's colours start
/// at the eight-bit step they are in. Names are made unique, because a
/// grandMA3 2.3 could freeze on a duplicated set.
fn sets(channel: &DeskChannel) -> Vec<(String, u16)> {
    let names: Vec<String> = channel.sets.iter().map(|(set, _, _)| name(set)).collect();
    let mut starts: Vec<(String, u16)> = match &channel.encoding {
        ChannelEncodingInfo::Stepped { bands } => bands
            .iter()
            .zip(&names)
            .map(|(band, set)| (set.clone(), u16::from(band.from) << 8))
            .collect(),
        ChannelEncodingInfo::Switch { threshold } => {
            let mut sides = names.iter();
            match (sides.next(), sides.next()) {
                (Some(off), Some(on)) => {
                    vec![(off.clone(), 0), (on.clone(), u16::from(*threshold) << 8)]
                }
                _ => Vec::new(),
            }
        }
        _ if channel.own_values => channel
            .sets
            .iter()
            .zip(&names)
            .map(|((_, from, _), set)| (set.clone(), *from))
            .collect(),
        _ => Vec::new(),
    };
    starts.sort_by_key(|&(_, start)| start);
    starts.dedup_by_key(|&mut (_, start)| start);

    // Unique names, keeping the first as it is.
    let mut seen: Vec<String> = Vec::new();
    for (set, _) in &mut starts {
        let mut candidate = set.clone();
        let mut count = 2;
        while seen.contains(&candidate) {
            candidate = format!("{set} {count}");
            count += 1;
        }
        seen.push(candidate.clone());
        *set = candidate;
    }
    starts
}

/// A DMX value in GDTF's own notation, at the channel's width.
///
/// A sixteen-bit channel is written as the sixteen-bit value over two bytes,
/// `32768/2`; an eight-bit one as its byte over one, the top byte of the
/// sixteen-bit value. Not `128/1` on a wide channel, which GDTF mirrors to
/// 32896 rather than 32768.
fn dmx(value: u16, wide: bool) -> String {
    if wide {
        format!("{value}/2")
    } else {
        format!("{}/1", value >> 8)
    }
}

/// One attribute this generator writes, and what it is declared as.
#[derive(Debug, PartialEq, Eq)]
struct Definition {
    name: &'static str,
    pretty: &'static str,
    group: &'static str,
    feature: &'static str,
    activation: Option<&'static str>,
    unit: &'static str,
    /// Where it sits among its group's attributes.
    order: u16,
}

const fn define(
    name: &'static str,
    pretty: &'static str,
    group: &'static str,
    feature: &'static str,
    activation: Option<&'static str>,
    unit: &'static str,
    order: u16,
) -> Definition {
    Definition {
        name,
        pretty,
        group,
        feature,
        activation,
        unit,
        order,
    }
}

const DIMMER: Definition = define("Dimmer", "Dim", "Dimmer", "Dimmer", None, "None", 0);
const PAN: Definition = define(
    "Pan",
    "Pan",
    "Position",
    "PanTilt",
    Some("PanTilt"),
    "None",
    0,
);
const TILT: Definition = define(
    "Tilt",
    "Tilt",
    "Position",
    "PanTilt",
    Some("PanTilt"),
    "None",
    1,
);
const ROTATION: Definition = define("Rot_Z", "Rot Z", "Position", "Rotation", None, "Angle", 2);
const SCALE_X: Definition = define("Scale_X", "Scale X", "Position", "Scale", None, "None", 3);
const SCALE_Y: Definition = define("Scale_Y", "Scale Y", "Position", "Scale", None, "None", 4);
const RED: Definition = define(
    "ColorAdd_R",
    "R",
    "Color",
    "RGB",
    Some("ColorRGB"),
    "ColorComponent",
    0,
);
const GREEN: Definition = define(
    "ColorAdd_G",
    "G",
    "Color",
    "RGB",
    Some("ColorRGB"),
    "ColorComponent",
    1,
);
const BLUE: Definition = define(
    "ColorAdd_B",
    "B",
    "Color",
    "RGB",
    Some("ColorRGB"),
    "ColorComponent",
    2,
);
const ALPHA: Definition = define("OVS_Alpha", "Alpha", "Color", "Alpha", None, "None", 3);
const BG_RED: Definition = define("OVS_BG_R", "BG R", "Color", "Background", None, "None", 4);
const BG_GREEN: Definition = define("OVS_BG_G", "BG G", "Color", "Background", None, "None", 5);
const BG_BLUE: Definition = define("OVS_BG_B", "BG B", "Color", "Background", None, "None", 6);
const BG_ALPHA: Definition = define(
    "OVS_BG_Alpha",
    "BG Alpha",
    "Color",
    "Background",
    None,
    "None",
    7,
);

/// The shared parameters, as many as any module has. GDTF counts them from one.
const PARAMETERS: [Definition; 12] = [
    define(
        "VideoEffect1Parameter1",
        "Parameter 1",
        "Video",
        "Video",
        None,
        "None",
        1,
    ),
    define(
        "VideoEffect1Parameter2",
        "Parameter 2",
        "Video",
        "Video",
        None,
        "None",
        2,
    ),
    define(
        "VideoEffect1Parameter3",
        "Parameter 3",
        "Video",
        "Video",
        None,
        "None",
        3,
    ),
    define(
        "VideoEffect1Parameter4",
        "Parameter 4",
        "Video",
        "Video",
        None,
        "None",
        4,
    ),
    define(
        "VideoEffect1Parameter5",
        "Parameter 5",
        "Video",
        "Video",
        None,
        "None",
        5,
    ),
    define(
        "VideoEffect1Parameter6",
        "Parameter 6",
        "Video",
        "Video",
        None,
        "None",
        6,
    ),
    define(
        "VideoEffect1Parameter7",
        "Parameter 7",
        "Video",
        "Video",
        None,
        "None",
        7,
    ),
    define(
        "VideoEffect1Parameter8",
        "Parameter 8",
        "Video",
        "Video",
        None,
        "None",
        8,
    ),
    define(
        "VideoEffect1Parameter9",
        "Parameter 9",
        "Video",
        "Video",
        None,
        "None",
        9,
    ),
    define(
        "VideoEffect1Parameter10",
        "Parameter 10",
        "Video",
        "Video",
        None,
        "None",
        10,
    ),
    define(
        "VideoEffect1Parameter11",
        "Parameter 11",
        "Video",
        "Video",
        None,
        "None",
        11,
    ),
    define(
        "VideoEffect1Parameter12",
        "Parameter 12",
        "Video",
        "Video",
        None,
        "None",
        12,
    ),
];

/// The attribute a role is written as.
///
/// # Panics
///
/// For a thirteenth parameter, which no module has; the test that builds every
/// file says so before a desk could.
fn definition(role: Role) -> &'static Definition {
    match role {
        Role::Dimmer => &DIMMER,
        Role::PositionX => &PAN,
        Role::PositionY => &TILT,
        Role::Rotation => &ROTATION,
        Role::ScaleX => &SCALE_X,
        Role::ScaleY => &SCALE_Y,
        Role::Foreground(ColourPart::Red) => &RED,
        Role::Foreground(ColourPart::Green) => &GREEN,
        Role::Foreground(ColourPart::Blue) => &BLUE,
        Role::Foreground(ColourPart::Alpha) => &ALPHA,
        Role::Background(ColourPart::Red) => &BG_RED,
        Role::Background(ColourPart::Green) => &BG_GREEN,
        Role::Background(ColourPart::Blue) => &BG_BLUE,
        Role::Background(ColourPart::Alpha) => &BG_ALPHA,
        Role::Parameter(number) => PARAMETERS
            .get(number.wrapping_sub(1))
            .unwrap_or_else(|| panic!("parameter {number} has no GDTF attribute here")),
    }
}

/// Where a feature group sits in a desk's attribute list.
fn group_rank(group: &str) -> u8 {
    match group {
        "Dimmer" => 0,
        "Position" => 1,
        "Color" => 2,
        _ => 3,
    }
}

/// A fixture type's identity: the same for the same module in every build, so
/// a newer file is the same fixture type to a desk rather than a second one.
///
/// Two FNV-1a hashes of the type, marked as an RFC 4122 name-based identity
/// (version 5, variant 1) in the GUID form GDTF asks for.
#[must_use]
pub fn fixture_type_id(personality: &Personality) -> String {
    let hash = |seed: &str| {
        let mut value: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in format!("{seed}:{}", personality.type_id).bytes() {
            value ^= u64::from(byte);
            value = value.wrapping_mul(0x0000_0100_0000_01b3);
        }
        value
    };
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&hash("openvideoserver gdtf").to_be_bytes());
    bytes[8..].copy_from_slice(&hash("openvideoserver gdtf fixture type").to_be_bytes());
    bytes[6] = (bytes[6] & 0x0F) | 0x50;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A name as GDTF allows one: below code point 128, only letters, digits, the
/// space and ``"#%'()*+-/:;<=>@_` `` — no dot and no comma, which are what its
/// links are made of (Annex C). Anything else is a space, and runs of spaces
/// one.
fn name(text: &str) -> String {
    let allowed = |character: char| {
        character.is_ascii_alphanumeric()
            || " \"#%'()*+-/:;<=>@_`".contains(character)
            || !character.is_ascii()
    };
    let replaced: String = text
        .chars()
        .map(|character| if allowed(character) { character } else { ' ' })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text as an XML attribute value.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every() -> Vec<GdtfFile> {
        fixture_files()
    }

    #[test]
    fn there_is_one_for_every_fixture_the_grandma1_has() {
        let gdtf: Vec<String> = every().into_iter().map(|file| file.name).collect();
        assert_eq!(gdtf.len(), crate::grandma1::fixture_files().len());
        assert!(gdtf.contains(&"OVS Master".to_owned()));
        assert!(gdtf.contains(&"OVS Layer".to_owned()));
        assert!(
            gdtf.contains(&"OVS FX Colour Correct".to_owned()),
            "{gdtf:?}"
        );
    }

    #[test]
    fn each_is_a_stored_zip_with_its_description_at_the_root() {
        for file in every() {
            let inside = zip::read_stored(&file.contents);
            assert_eq!(inside.len(), 1, "{}", file.file_name);
            assert_eq!(inside[0].0, "description.xml");
            assert_eq!(inside[0].1, file.description.as_bytes());
            assert!(file.file_name.ends_with(".gdtf"), "{}", file.file_name);
        }
    }

    #[test]
    fn every_attribute_is_used_once_in_a_mode() {
        // A grandMA3 2.2 refused a whole type over two of one in a geometry.
        for personality in current_personalities() {
            let mut names: Vec<&str> = desk_channels(&personality)
                .iter()
                .map(|channel| definition(channel.role).name)
                .collect();
            let all = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), all, "{}", personality.type_id);
        }
    }

    #[test]
    fn every_channel_is_where_the_personality_says_at_its_width() {
        for personality in current_personalities() {
            let description = description(&personality);
            for channel in &personality.channels {
                let offset = if channel.width > 1 {
                    format!(r#"Offset="{},{}""#, channel.offset, channel.offset + 1)
                } else {
                    format!(r#"Offset="{}""#, channel.offset)
                };
                assert!(
                    description.contains(&offset),
                    "{} channel {} is not at {offset}",
                    personality.type_id,
                    channel.name
                );
            }
        }
    }

    #[test]
    fn the_sections_are_in_the_order_the_schema_demands() {
        let description = description(&current_personalities()[0]);
        let order = [
            "<AttributeDefinitions>",
            "<Wheels/>",
            "<PhysicalDescriptions/>",
            "<Models/>",
            "<Geometries>",
            "<DMXModes>",
            "<Revisions/>",
            "<FTPresets/>",
            "<Protocols/>",
        ];
        let at: Vec<usize> = order
            .iter()
            .map(|section| description.find(section).unwrap_or(usize::MAX))
            .collect();
        assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{at:?}");
    }

    #[test]
    fn a_layers_slot_is_a_set_for_every_band_the_server_reads() {
        let layer = current_personalities()
            .into_iter()
            .find(|personality| personality.type_id == "ovs.layer")
            .expect("a layer");
        let description = description(&layer);
        assert!(description.contains(r#"<ChannelSet DMXFrom="0/1" Name="Black"/>"#));
        assert!(description.contains(r#"<ChannelSet DMXFrom="4/1" Name="Slot 1"/>"#));
        assert!(description.contains(r#"Name="Slot 84"/>"#));
        assert!(description.contains(r#"Attribute="Pan""#));
        assert!(description.contains(r#"Attribute="Tilt""#));
    }

    #[test]
    fn a_module_amount_is_the_dimmer_at_rest_at_nought_on_the_grand_master() {
        let blur = current_personalities()
            .into_iter()
            .find(|personality| personality.type_id == "ovs.blur")
            .expect("blur");
        let description = description(&blur);
        assert!(description.contains(r#"<LogicalChannel Attribute="Dimmer""#));
        assert!(description.contains(r#"Master="Grand""#));
        assert!(description.contains(r#"Name="Radius""#), "{description}");
    }

    #[test]
    fn a_wide_value_is_written_over_two_bytes_and_a_narrow_one_over_one() {
        assert_eq!(dmx(32_768, true), "32768/2");
        assert_eq!(dmx(u16::MAX, false), "255/1");
        assert_eq!(dmx(128 << 8, false), "128/1");
    }

    #[test]
    fn a_name_has_no_dot_or_comma_in_it() {
        assert_eq!(name("Colour Corr. 2, left"), "Colour Corr 2 left");
        for file in every() {
            assert!(!file.name.contains(['.', ',']), "{}", file.name);
        }
    }

    #[test]
    fn the_identity_is_a_guid_and_the_same_every_time() {
        let master = &current_personalities()[0];
        let id = fixture_type_id(master);
        assert_eq!(id, fixture_type_id(master));
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
        let others: Vec<String> = current_personalities()
            .iter()
            .map(fixture_type_id)
            .collect();
        let mut unique = others.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), others.len(), "two types share an identity");
    }
}
