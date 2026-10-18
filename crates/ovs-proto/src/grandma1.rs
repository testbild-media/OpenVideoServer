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
//!   is where MA's own `KEYSTONE BOTTOM RO` stops.
//! - **An attribute is one thing for a whole show.** 522 of the library's 527
//!   attributes carry the same label in every file that uses them, and the
//!   desk lists every group of every fixture type in a show together — the
//!   owner's screen after the second import, 2026-10-18. So an attribute here
//!   has one group, one preset and one label in every file.
//! - Nine preset types: `DIMMER`, `PAN/TILT`, `COLOR`, `GOBO`, `BEAM`,
//!   `FOCUS`, `CONTROL`, `SHAPERS` and `VIDEO`.
//!
//! # How the controls are laid out on a desk
//!
//! What the owner asked for, in the order he asked, 2026-10-18:
//!
//! - "Position X und Y könnten Pan und Tilt sein, damit man es mit dem
//!   Trackball am Pult bedienen kann. Amount könnte Dimmer sein [...] somit
//!   könnte man einen Groupmaster im Pult erstellen, der alle Amounts auf 0
//!   zieht."
//! - "Wenn du neue Attribute hinzufügst mach auch OVS Blur z.B. statt nur
//!   Blur."
//! - "Es sind zu viele einzelne Attribute Groups [...] es können max. 4
//!   Parameter angezeigt werden. Hat eine Gruppe mehr als 4 Parameter gibt es
//!   die A und B Seiten." — and of shared parameters in the manner of MA's own
//!   VPU (`Eff1 Par1` to `Par3`): "ansonsten finde ich die Param Sachen gut."
//! - "Rotation könnte mit zu Pan/Tilt in die selbe Attribute Gruppe" — "Pan
//!   Tilt und Rotation bei uns dann X Y und Z Rotation [...] Das setze ich
//!   fest." And "OVS Layer braucht keine eigene Gruppe wenn nur eine Sache drin
//!   ist."
//! - "Man könnte Global RGBX als Gruppe definieren auch für Foreground RGBA,
//!   weil hier das X mehrere Bedeutungen haben kann.. X kann Alpha, Amber oder
//!   auch White sein bei anderen Fixtures." And a background is a background:
//!   "Background Color sollte nicht Color 2 sein, das ist missverständlich."
//! - "Params bitte alle unter der Video Hauptgruppe machen, so kann man das
//!   Video Fenster in der GrandMA für Presets nutzen."
//!
//! And then the names themselves, off his desk: "ich habe hier mal ein
//! Beispiel mit Attributen und Gruppen geschrieben, die du so übernehmen
//! kannst. die kommen direkt aus dem Pult." **Every attribute and group here
//! is one of his, spelt as his desk spelt it** — underscores, leading ones
//! included — and [`DESK`] is that example, which a test holds every file to.
//! The slot and the master's own controls were not in it; on his word they are
//! parameters too, so there is no group but his.
//!
//! | Group | Preset | Attributes |
//! |---|---|---|
//! | `DIMMER` | Dimmer | `DIM`: the master's, a layer's, a module's |
//! | `PAN/TILT` | Pan/Tilt | `PAN` (X), `TILT` (Y), `OVS_ROTATION` (Z) |
//! | `SCALE` | Pan/Tilt | `OVS_SCALE_X`, `OVS_SCALE_Y` |
//! | `OVS_PARAMETER` | Video | `PARA_1` to `PARA_8`, shared by everything |
//! | `RGBX` | Color | `_RED`, `_GREEN`, `_BLUE`, `_ALPHA` |
//! | `OVS_RGBX_BG` | Color | `OVS_BG_RED`, `OVS_BG_GREEN`, `OVS_BG_BLUE`, `OVS_BG_ALPHA` |
//!
//! `DIM`, `PAN` and `TILT` are the desk's own, because a tool of the desk
//! answers to them: the masters and Highlight, the trackball and position
//! effects. `_RED`, `_GREEN` and `_BLUE` carry the colour-mix effect the
//! colour picker looks for. A gradient's far colour is the background's four.
//!
//! **Parameters are numbered.** A module's, in the order the module declares
//! them, leaving out its dimmer and its colours: Blur's radius is Parameter 1,
//! a text's cap height Parameter 1 and its alignment Parameter 4. A layer's
//! slot is its Parameter 1; the master's freeze, tap, scale and crossfade are
//! its 1 to 4. The encoder says "Parameter 1"; the named values say what it
//! is — `Radius 0`, `Radius 32`, `Radius 64`, `Slot 12`, `BPM Tap Fire` —
//! because a label belongs to the attribute and the attribute to everything.
//!
//! **Nothing goes into the desk's own `RGB`.** The first set of files put a
//! module's alpha there as a fourth attribute, and every fixture type with a
//! colour was refused (owner, 2026-10-18). His example puts `_ALPHA` beside
//! three colour-mix attributes in `RGBX`, straight off the desk, so that much
//! the desk takes.

use ovs_types::config::fixtures::{definition, ColourPart, ParameterSlot};
use ovs_types::config::{ParameterDefinition, ParameterKind};

use crate::fixtures::{
    catalogue, ChannelBand, ChannelEncodingInfo, Personality, PersonalityChannel, PersonalityTarget,
};

/// Who the desk lists these under.
pub const MANUFACTURER: &str = "testbild.media";

/// The longest attribute, feature, label or named value a grandMA1 shows.
pub const NAME_FIELD: usize = 18;

/// The owner's attributes, exactly as his desk exported them on 2026-10-18:
/// attribute, group, preset, label and the group's label. Every attribute in
/// every file here is one of these or the desk's `DIM`; a test holds them to
/// it. `_AMBER` and `_WHITE` are his for other fixtures and no OVS one has
/// either.
pub const DESK: [(&str, &str, &str, &str, &str); 24] = [
    ("PAN", "PAN/TILT", "PAN/TILT", "Pan", "Pan/Tilt"),
    ("TILT", "PAN/TILT", "PAN/TILT", "Tilt", "Pan/Tilt"),
    (
        "OVS_ROTATION",
        "PAN/TILT",
        "PAN/TILT",
        "Rotation",
        "Pan/Tilt",
    ),
    ("OVS_SCALE_X", "SCALE", "PAN/TILT", "Scale X", "Scale"),
    ("OVS_SCALE_Y", "SCALE", "PAN/TILT", "Scale Y", "Scale"),
    (
        "PARA_1",
        PARAMETERS,
        "VIDEO",
        "Parameter 1",
        "OVS Parameter",
    ),
    (
        "PARA_2",
        PARAMETERS,
        "VIDEO",
        "Parameter 2",
        "OVS Parameter",
    ),
    (
        "PARA_3",
        PARAMETERS,
        "VIDEO",
        "Parameter 3",
        "OVS Parameter",
    ),
    (
        "PARA_4",
        PARAMETERS,
        "VIDEO",
        "Parameter 4",
        "OVS Parameter",
    ),
    (
        "PARA_5",
        PARAMETERS,
        "VIDEO",
        "Parameter 5",
        "OVS Parameter",
    ),
    (
        "PARA_6",
        PARAMETERS,
        "VIDEO",
        "Parameter 6",
        "OVS Parameter",
    ),
    (
        "PARA_7",
        PARAMETERS,
        "VIDEO",
        "Parameter 7",
        "OVS Parameter",
    ),
    (
        "PARA_8",
        PARAMETERS,
        "VIDEO",
        "Parameter 8",
        "OVS Parameter",
    ),
    ("_RED", "RGBX", "COLOR", "Red", "RGBX"),
    ("_GREEN", "RGBX", "COLOR", "Green", "RGBX"),
    ("_BLUE", "RGBX", "COLOR", "Blue", "RGBX"),
    ("_ALPHA", "RGBX", "COLOR", "Alpha", "RGBX"),
    ("_AMBER", "RGBX", "COLOR", "Amber", "RGBX"),
    ("_WHITE", "RGBX", "COLOR", "White", "RGBX"),
    (
        "OVS_BG_RED",
        "OVS_RGBX_BG",
        "COLOR",
        "BG Red",
        "OVS BG RGBX",
    ),
    (
        "OVS_BG_GREEN",
        "OVS_RGBX_BG",
        "COLOR",
        "BG Green",
        "OVS BG RGBX",
    ),
    (
        "OVS_BG_BLUE",
        "OVS_RGBX_BG",
        "COLOR",
        "BG Blue",
        "OVS BG RGBX",
    ),
    (
        "OVS_BG_ALPHA",
        "OVS_RGBX_BG",
        "COLOR",
        "BG Alpha",
        "OVS BG RGBX",
    ),
    // Not in his example, and the desk's own on 490 fixtures of its library.
    ("DIM", "DIMMER", "DIMMER", "Dim", "Dimmer"),
];

/// The group every parameter shares.
const PARAMETERS: &str = "OVS_PARAMETER";

/// How many parameters the owner's desk has.
const PARAMETER_COUNT: usize = 8;

/// One fixture type, ready to be written to disk and imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureFile {
    /// The file's name, in the library's own `MANUFACTURER@NAME.TXT` form and
    /// the capitals the desk insists on; see [`file_name`].
    pub file_name: String,
    /// What the desk's fixture list calls it.
    pub name: String,
    /// The file, with CRLF line ends. MA's own files and the owner's desk
    /// write CR CR LF; the desk has taken these as they are.
    pub contents: String,
}

/// Every fixture type a desk needs for this build.
///
/// The master, the layer, and every effect and generator a client can add
/// that declares DMX: **one each, the version a client adds today**. The
/// owner, 2026-10-18: "Ich habe aber in der Software keine Versionsvergabe und
/// ist auch nicht gewünscht. TV Glitch V2 ist TV Glitch." The older versions
/// stay in the server so a show saved with one still opens; each grew only by
/// channels at its end, so the first channels of a type still drive an older
/// instance patched with it.
#[must_use]
pub fn fixture_files() -> Vec<FixtureFile> {
    let all = catalogue().fixtures;
    all.iter()
        .filter(|personality| !retired(personality) && current(personality, &all))
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

/// Whether this is the version of its type a client adds today.
///
/// For a module, the newest *manifest*, not the newest personality: a version
/// that dropped its DMX has no personality, and the one before it must not
/// stand in for it.
fn current(personality: &Personality, all: &[Personality]) -> bool {
    let type_id = personality.type_id.as_str();
    let newest = match personality.target {
        PersonalityTarget::Effect => ovs_types::config::effects::PUBLISHED
            .iter()
            .filter(|manifest| manifest.type_id == type_id)
            .map(|manifest| manifest.version)
            .max(),
        PersonalityTarget::Generator => ovs_types::config::generators::PUBLISHED
            .iter()
            .filter(|manifest| manifest.type_id == type_id)
            .map(|manifest| manifest.version)
            .max(),
        PersonalityTarget::Master | PersonalityTarget::Layer => all
            .iter()
            .filter(|other| other.type_id == personality.type_id)
            .map(|other| other.version)
            .max(),
    };
    newest == Some(personality.version)
}

/// What the desk's fixture list calls a personality: `OVS FX` and `OVS Gen`
/// in front of the module's own name, as the owner asked, and no version.
#[must_use]
pub fn fixture_name(personality: &Personality) -> String {
    match personality.target {
        // Already "OVS Master" and "OVS Layer".
        PersonalityTarget::Master | PersonalityTarget::Layer => personality.name.clone(),
        PersonalityTarget::Effect => format!("OVS FX {}", personality.name),
        PersonalityTarget::Generator => format!("OVS Gen {}", personality.name),
    }
}

/// The same, short, for the columns that have less room.
fn short_name(personality: &Personality) -> String {
    match personality.target {
        PersonalityTarget::Master | PersonalityTarget::Layer => personality.name.clone(),
        PersonalityTarget::Effect => format!("FX {}", personality.name),
        PersonalityTarget::Generator => format!("Gen {}", personality.name),
    }
}

/// The file's name: `MANUFACTURER@NAME.TXT`, in capitals, with no dot but the
/// one before the extension.
///
/// **The desk refuses anything else.** The owner, importing the first set on
/// 2026-10-18: "Die Fixturenamen sollten alle in Großbuchstaben sein und kein
/// Punkt vorkommen im Namen außer .TXT. Aus Punkte sollte man ein Leerzeichen
/// machen. Erst dann kann man die importieren, sonst werden die abgelehnt."
/// So `testbild.media@OVS FX Echo.txt` is `TESTBILD MEDIA@OVS FX ECHO.TXT`.
/// Only the file's name: the manufacturer and the fixture's name *inside* the
/// file are as they were, and the desk lists them as written there.
#[must_use]
pub fn file_name(personality: &Personality) -> String {
    format!(
        "{}.TXT",
        format!("{MANUFACTURER}@{}", fixture_name(personality))
            .replace('.', " ")
            .to_uppercase()
    )
}

/// Whether a file in the fixture folder is one of these, of any vintage.
///
/// Either spelling, so whatever writes the folder can clear out the names the
/// desk refused along with anything retired or superseded.
#[must_use]
pub fn is_fixture_file(name: &str) -> bool {
    let upper = name.to_uppercase();
    upper.ends_with(".TXT")
        && (upper.starts_with(&format!("{}@", MANUFACTURER.to_uppercase()))
            || upper.starts_with(&format!(
                "{}@",
                MANUFACTURER.replace('.', " ").to_uppercase()
            )))
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
    let parameters = Layout::of(personality)
        .attributes
        .iter()
        .filter_map(|attribute| {
            let number = attribute.attribute.strip_prefix("PARA_")?;
            Some(format!(
                "Parameter {number} {}",
                attribute.named_for.as_deref()?
            ))
        })
        .collect::<Vec<_>>();
    let parameters = if parameters.is_empty() {
        String::new()
    } else {
        format!(" {}.", parameters.join(", "))
    };
    format!(
        "OpenVideoServer {}, {} channels. Patch in OVS to {target}, at the same address.\
         {parameters} Generated from the OVS fixture catalogue; reference/FIXTURES.md lists \
         every value.",
        personality.type_id, personality.footprint
    )
}

/// Where one channel sits on a desk.
#[derive(Debug, Clone, PartialEq)]
struct Attribute {
    attribute: String,
    feature: String,
    preset: &'static str,
    attribute_label: String,
    feature_label: String,
    /// What the desk's own tools take the channel for.
    effect: &'static str,
    effect_sub: Option<&'static str>,
    /// What a shared parameter is in this module, which its named values say
    /// because its label cannot.
    named_for: Option<String>,
    /// What the channel's functional block is called, which the desk shows
    /// beside the value on the encoder — "40 Hue" rather than "40 Function".
    /// Unset, it is a parameter's own name or else the channel's.
    function: Option<&'static str>,
    /// Named values in the control's own units, in place of its bottom,
    /// middle and top: a hue's colours.
    values: Option<&'static [(f64, &'static str)]>,
}

impl Attribute {
    /// A channel no tool of the desk does anything particular with.
    fn plain(
        attribute: impl Into<String>,
        feature: impl Into<String>,
        preset: &'static str,
        attribute_label: impl Into<String>,
        feature_label: impl Into<String>,
    ) -> Self {
        Self {
            attribute: attribute.into(),
            feature: feature.into(),
            preset,
            attribute_label: attribute_label.into(),
            feature_label: feature_label.into(),
            effect: "NONE",
            effect_sub: None,
            named_for: None,
            function: None,
            values: None,
        }
    }

    /// The same channel, its functional block named.
    fn called(mut self, function: &'static str) -> Self {
        self.function = Some(function);
        self
    }

    /// The same channel, taken by one of the desk's tools.
    fn for_the_desk(mut self, effect: &'static str, effect_sub: Option<&'static str>) -> Self {
        self.effect = effect;
        self.effect_sub = effect_sub;
        self
    }

    /// One of the owner's attributes, exactly as his desk has it.
    ///
    /// Only ever asked for by a name written in this file, so a name that is
    /// not in [`DESK`] is a mistake here and every test says so at once.
    fn desk(name: &str) -> Self {
        let &(attribute, feature, preset, label, feature_label) = DESK
            .iter()
            .find(|row| row.0 == name)
            .unwrap_or_else(|| panic!("{name} is not an attribute of the owner's desk"));
        Self::plain(attribute, feature, preset, label, feature_label)
    }

    /// The desk's dimmer, written as 490 fixtures of the library write it.
    fn dimmer() -> Self {
        Self::desk("DIM").for_the_desk("DIMMER", None)
    }

    /// The `number`th parameter, which is `control` on this fixture.
    ///
    /// Past the owner's eight it is a name his desk does not have, and the
    /// test that holds every attribute to [`DESK`] says so rather than this
    /// stopping the generator.
    fn parameter(number: usize, control: &str) -> Self {
        let mut attribute = if number <= PARAMETER_COUNT {
            Self::desk(&format!("PARA_{number}"))
        } else {
            Self::plain(
                format!("PARA_{number}"),
                PARAMETERS,
                "VIDEO",
                format!("Parameter {number}"),
                "OVS Parameter",
            )
        };
        attribute.named_for = Some(ascii(control));
        attribute
    }
}

/// Every channel's attribute, worked out together: the first colour of a
/// module is told apart from the others by being first, and a parameter's
/// number is how many came before it.
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

        let mut numbered = 0;
        let attributes = personality
            .channels
            .iter()
            .zip(&slots)
            .map(|(channel, slot)| {
                attribute_of(
                    personality,
                    channel,
                    *slot,
                    first_colour,
                    parameters,
                    &mut numbered,
                )
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

/// The attributes of the two personalities the server publishes itself.
///
/// By control name, so a control added to a later version of either finds no
/// row here and is laid out like any other control rather than taking another
/// one's attribute.
fn fixed(control: &str) -> Option<Attribute> {
    let attribute = match control {
        "master_dimmer" | "layer_opacity" => Attribute::dimmer().called("Dimmer"),
        // Parameters, so the master and the layer bring no group of their own
        // (owner, 2026-10-18: "ja passt und baue es").
        "master_freeze" => Attribute::parameter(1, "Freeze"),
        "master_tap_tempo" => Attribute::parameter(2, "BPM Tap"),
        "master_speed_scale" => Attribute::parameter(3, "BPM Scale"),
        "master_crossfader" => Attribute::parameter(4, "Crossfade"),
        "layer_slot" => Attribute::parameter(1, "Slot"),
        // The trackball's. Up is up: a larger value moves the picture up the
        // wall, as a larger tilt lifts a head.
        "layer_position_x" => Attribute::desk("PAN")
            .for_the_desk("PAN", None)
            .called("Position X"),
        "layer_position_y" => Attribute::desk("TILT")
            .for_the_desk("TILT", None)
            .called("Position Y"),
        // X, Y and the turn about Z on one page, the turn in degrees.
        "layer_rotation" => Attribute::desk("OVS_ROTATION")
            .for_the_desk("VIDEO_ROTATION", Some("Z"))
            .called("Rotation"),
        // "Visualize Scale Subfunction X ist auch vorhanden [...] auch Y".
        "layer_scale_x" => Attribute::desk("OVS_SCALE_X")
            .for_the_desk("VIDEO_SCALE", Some("X"))
            .called("Scale X"),
        "layer_scale_y" => Attribute::desk("OVS_SCALE_Y")
            .for_the_desk("VIDEO_SCALE", Some("Y"))
            .called("Scale Y"),
        _ => return None,
    };
    Some(attribute)
}

/// The controls other than an `Amount` that are their module's dimmer.
///
/// Each is how much of its module there is, and at its least the module does
/// nothing at all, so a group master pulled to nought takes the module out of
/// the picture and never puts something into it: blocks of one pixel, channels
/// split by none, a trail no frames long, a picture not inverted.
/// `pulling_a_dimmer_down_never_adds_an_effect` holds every one of them, and
/// every `Amount`, to that.
///
/// Not here, because no single control of theirs is "none at zero": Colour
/// Correct and RGB Correct (nought brightness is black), Crop (four edges) and
/// Mirror (two axes).
const DIMMED: [(&str, &str); 4] = [
    ("ovs.pixelate", "block_px"),
    ("ovs.rgb_split", "offset_px"),
    ("ovs.echo", "length_s"),
    ("ovs.invert", "on"),
];

fn is_dimmer(type_id: &str, key: &str) -> bool {
    key == "amount" || DIMMED.contains(&(type_id, key))
}

/// Works out where one channel sits.
fn attribute_of(
    personality: &Personality,
    channel: &PersonalityChannel,
    slot: Option<ParameterSlot>,
    first_colour: Option<&'static str>,
    parameters: &'static [ParameterDefinition],
    numbered: &mut usize,
) -> Attribute {
    if let Some(attribute) = fixed(&channel.control) {
        // A control the server publishes itself may be numbered already; one
        // after it is not given the same number.
        if let Some(number) = attribute
            .attribute
            .strip_prefix("PARA_")
            .and_then(|number| number.parse::<usize>().ok())
        {
            *numbered = (*numbered).max(number);
        }
        return attribute;
    }

    let Some(parameter) = (match slot {
        Some(ParameterSlot::Effect(parameter) | ParameterSlot::Generator(parameter)) => {
            Some(parameter)
        }
        _ => None,
    }) else {
        // A control of the server's own that `fixed` does not know yet: the
        // next parameter, named after itself.
        *numbered += 1;
        return Attribute::parameter(*numbered, &field(&channel.name));
    };

    if is_dimmer(&personality.type_id, parameter.key) {
        return Attribute::dimmer();
    }

    if let Some(part) = parameter.component {
        // The first colour is the foreground, and its red, green and blue
        // carry the colour-mix effect, which is what the colour picker looks
        // for. The background — or a gradient's far colour — has no picker:
        // one per fixture, as in every fixture of the library.
        if first_colour == Some(parameter.key) {
            return match part {
                ColourPart::Red => Attribute::desk("_RED").for_the_desk("COLOR_MIX", Some("RED")),
                ColourPart::Green => {
                    Attribute::desk("_GREEN").for_the_desk("COLOR_MIX", Some("GREEN"))
                }
                ColourPart::Blue => {
                    Attribute::desk("_BLUE").for_the_desk("COLOR_MIX", Some("BLUE"))
                }
                ColourPart::Alpha => Attribute::desk("_ALPHA"),
            };
        }
        return Attribute::desk(match part {
            ColourPart::Red => "OVS_BG_RED",
            ColourPart::Green => "OVS_BG_GREEN",
            ColourPart::Blue => "OVS_BG_BLUE",
            ColourPart::Alpha => "OVS_BG_ALPHA",
        });
    }

    let name = parameters
        .iter()
        .find(|defined| defined.key == parameter.key)
        .map_or(parameter.key, |defined| defined.name);
    *numbered += 1;
    let mut attribute = Attribute::parameter(*numbered, name);
    if let Some(&(_, _, effect, effect_sub)) = VISUALISED
        .iter()
        .find(|row| row.0 == personality.type_id && row.1 == parameter.key)
    {
        attribute = attribute.for_the_desk(effect, effect_sub);
    }
    if parameter.key == "hue" {
        // Its named values are its colours.
        attribute.values = Some(&HUES);
    }
    attribute
}

/// How a module's control is visualised, where the desk has a kind for it.
///
/// **Visualised is what lets a value be typed in its own units.** The owner,
/// 2026-10-18: "Ohne den Visualize Color kann man nicht die Werte von 0 bis
/// 360 angeben als Degree" — and of the kinds there are: Brightness and
/// Contrast take 0.0 to 4.0, "es gäbe sogar Visualize Hue direkt", CTC on
/// Wheel 1 takes 2000 to 10000, Scale on X and Y takes 0.0 to 4.0. The
/// physical range written beside it is the control's own, so a hue is typed in
/// degrees and a temperature in kelvin.
///
/// The names are the program's own, read off the table in the grandMA1 image
/// in the order its menu shows them — `VIDEO_HUE` is "Hue", `VIDEO_COLORINTENSITY`
/// is "Color", `COLOR_CORRECTION` is "CTC", `COLOR` is "Color Wheel" — and
/// `the_desk_knows_every_visualisation` holds every one written to that table.
///
/// Not here, because nothing on the desk means them: gamma, tint, the
/// thresholds and amounts, widths of lines, the choices. Nor Crop, whose
/// edges would be the shutter's blades, which side being which blade not
/// being anything the library says.
const VISUALISED: [(&str, &str, &str, Option<&str>); 15] = [
    ("ovs.colorize", "hue", "VIDEO_HUE", None),
    ("ovs.colorize", "saturation", "VIDEO_COLORINTENSITY", None),
    ("ovs.colour_correct", "brightness", "VIDEO_BRIGHTNESS", None),
    ("ovs.colour_correct", "contrast", "VIDEO_CONTRAST", None),
    (
        "ovs.colour_correct",
        "saturation",
        "VIDEO_COLORINTENSITY",
        None,
    ),
    (
        "ovs.colour_correct",
        "temperature_k",
        "COLOR_CORRECTION",
        Some("WHEEL_1"),
    ),
    // A blur is what a frost does to a beam.
    ("ovs.blur", "radius_px", "FROST", None),
    // Angles and turns in degrees, about the axis out of the wall.
    ("ovs.rgb_split", "angle_deg", "VIDEO_ROTATION", Some("Z")),
    ("ovs.gradient", "angle_deg", "VIDEO_ROTATION", Some("Z")),
    ("ovs.shape", "rotation_deg", "VIDEO_ROTATION", Some("Z")),
    // Sizes, in the composition's pixels.
    ("ovs.shape", "width_px", "VIDEO_SCALE", Some("X")),
    ("ovs.shape", "height_px", "VIDEO_SCALE", Some("Y")),
    ("ovs.text", "cap_px", "VIDEO_SCALE", Some("ALL")),
    // Flashes a second, as MA's own strobes are.
    ("ovs.strobe", "hz", "STROBE", None),
    ("ovs.invert", "hz", "STROBE", None),
];

/// A hue's colours, in degrees. The owner, 2026-10-18: "Bei Hue kannst du noch
/// bei 0 Red, 30 Orange, 60 Yellow, 120 Green, 180 Cyan, 240 Blue, 300 Magenta
/// als Werte eintragen? Dann können Leute das einfacher machen und schneller
/// zu den richtigen Farben kommen."
const HUES: [(f64, &str); 7] = [
    (0.0, "Red"),
    (30.0, "Orange"),
    (60.0, "Yellow"),
    (120.0, "Green"),
    (180.0, "Cyan"),
    (240.0, "Blue"),
    (300.0, "Magenta"),
];

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
    // What the control is on this fixture, which the encoder shows beside its
    // value: the owner's screen read "40 Function" when every block was called
    // that, and a shared parameter's label cannot say it.
    let called = attribute
        .function
        .map(str::to_owned)
        .or_else(|| attribute.named_for.clone())
        .unwrap_or_else(|| field(&channel.name));
    let mut function = format!(
        "\t\t{{ _NAME {} _RANGE [  0,255] _EFF {}",
        padded(&quoted(&called)),
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
///
/// **Each is one value, never a band.** The owner's sheet showed "25" for an
/// echo resting on Behind, "16" for a mirror on Off and "-" for a layer on
/// Black, and names wherever a named value was a single value — a radius of
/// nought, a centred pan. So a step is named at one value inside its band,
/// which the server reads as that step like any other in it: the channel's
/// home where home is in the band, so a fixture at rest shows a name, and the
/// band's middle elsewhere, so a nudge does not leave it. A switch is named at
/// nought and full, as MA's own switches are.
fn channel_sets(
    personality: &Personality,
    channel: &PersonalityChannel,
    attribute: &Attribute,
) -> Vec<(String, f64, f64)> {
    let named_for = attribute.named_for.as_deref();
    let home = channel.home;
    match &channel.encoding {
        ChannelEncodingInfo::Stepped { bands } => bands
            .iter()
            .map(|band| {
                let at = if (u16::from(band.from)..=u16::from(band.to)).contains(&home) {
                    home as u8
                } else {
                    ((u16::from(band.from) + u16::from(band.to)) / 2) as u8
                };
                let (from, to) = byte_band(at, at);
                (step_name(personality, channel, band), from, to)
            })
            .collect(),
        ChannelEncodingInfo::Switch { threshold } => {
            let (off, on) = if channel.momentary {
                ("Idle", "Fire")
            } else {
                ("Off", "On")
            };
            let (off, on) = match named_for {
                Some(control) => (format!("{control} {off}"), format!("{control} {on}")),
                None => (off.to_owned(), on.to_owned()),
            };
            let low = if home < u16::from(*threshold) {
                home as u8
            } else {
                0
            };
            let high = if home >= u16::from(*threshold) {
                home.min(255) as u8
            } else {
                255
            };
            let (low_from, low_to) = byte_band(low, low);
            let (high_from, high_to) = byte_band(high, high);
            vec![(off, low_from, low_to), (on, high_from, high_to)]
        }
        // A colour and a dimmer are faders and nothing else, as they are on
        // every fixture in the library.
        ChannelEncodingInfo::Ranged8 { .. } | ChannelEncodingInfo::Ranged16 { .. }
            if attribute.preset == "COLOR" || attribute.effect == "DIMMER" =>
        {
            Vec::new()
        }
        ChannelEncodingInfo::Ranged8 { min, max } => {
            named_values(attribute, *min, *max, 255, home, named_for)
                .into_iter()
                .map(|(name, dmx)| {
                    let (from, to) = byte_band(dmx as u8, dmx as u8);
                    (name, from, to)
                })
                .collect()
        }
        // Exactly the value, not a coarse step's worth of them: a desk sends
        // the start of a set, and a home of a scale of one has to be one.
        ChannelEncodingInfo::Ranged16 { min, max } => {
            named_values(attribute, *min, *max, 65_535, home, named_for)
                .into_iter()
                .map(|(name, dmx)| {
                    let at = f64::from(dmx) * 100.0 / 65_535.0;
                    (name, at, at)
                })
                .collect()
        }
        ChannelEncodingInfo::Index8 => Vec::new(),
    }
}

/// A continuous channel's named values, as a name and the DMX value: the ones
/// the control has of its own — a hue's colours — or else its landmarks.
fn named_values(
    attribute: &Attribute,
    min: f64,
    max: f64,
    top: u32,
    home: u16,
    named_for: Option<&str>,
) -> Vec<(String, u32)> {
    match attribute.values {
        Some(values) => values
            .iter()
            .map(|&(value, name)| {
                let dmx = ((value - min) / (max - min) * f64::from(top))
                    .round()
                    .clamp(0.0, f64::from(top)) as u32;
                (name.to_owned(), dmx)
            })
            .collect(),
        None => landmarks(min, max, top, home, named_for),
    }
}

/// The bottom, the middle, the top and home of a continuous channel, as named
/// values: the numbers somebody at a desk asked for (owner, 2026-10-16: "wo
/// Mitte ist, welche Scale wo liegt"). Home only where it is none of the
/// other three.
///
/// Each is named after what the server makes of the number sent, not after
/// the round figure it is near: the middle of an eight-bit channel is 128 of
/// 255, which is not quite half way. A shared parameter's are named after what
/// it is in this module instead — `Radius 32` rather than `Center 32` — since
/// its encoder says only "Parameter 1".
fn landmarks(
    min: f64,
    max: f64,
    top: u32,
    home: u16,
    named_for: Option<&str>,
) -> Vec<(String, u32)> {
    let middle = top.div_ceil(2);
    let value = |dmx: u32| number(min + (max - min) * f64::from(dmx) / f64::from(top));
    let name = |landmark: &str, dmx: u32| match named_for {
        Some(control) => format!("{control} {}", value(dmx)),
        None => format!("{landmark} {}", value(dmx)),
    };
    let mut sets = vec![
        (name("Min", 0), 0),
        (name("Center", middle), middle),
        (name("Max", top), top),
    ];
    let home = u32::from(home);
    if home != 0 && home != middle && home != top {
        sets.push((name("Home", home), home));
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
            // client shows beside the same channel and says what it is on its
            // own — "Keep left", "Ellipse", "Lighten".
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

/// A number as a set's name shows it: two decimals at most, trimmed, and none
/// from a thousand up.
///
/// Two, because a name is read on a desk's screen: the home of a shape's width
/// is sent as 2047.97 and reads as 2048, which is what it is for. None above a
/// thousand, because a colour temperature of 6000.06 is 6000 to anybody and
/// "Temperature 6000.06" does not fit the field.
/// `reference/FIXTURES.md` has the third decimal for anybody who needs it.
fn number(value: f64) -> String {
    let decimals = if value.abs() >= 1000.0 { 0 } else { 2 };
    let mut shown = format!("{value:.decimals$}");
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
    use std::collections::{BTreeMap, BTreeSet};

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

    /// The personalities a file is written for.
    fn generated() -> Vec<Personality> {
        let all = catalogue().fixtures;
        all.iter()
            .filter(|personality| !retired(personality) && current(personality, &all))
            .cloned()
            .collect()
    }

    /// Every attribute of every file, with the fixture it came from.
    fn every_attribute() -> Vec<(String, Attribute)> {
        generated()
            .iter()
            .flat_map(|personality| {
                Layout::of(personality)
                    .attributes
                    .into_iter()
                    .map(|attribute| (fixture_name(personality), attribute))
            })
            .collect()
    }

    #[test]
    fn every_module_a_client_can_add_has_one_file_and_no_version() {
        // "TV Glitch V2 ist TV Glitch. Invert V2 ist Invert."
        let files = fixture_files();
        let names: BTreeSet<String> = files.iter().map(|file| file.name.clone()).collect();
        for expected in [
            "OVS Master",
            "OVS Layer",
            "OVS FX Blur",
            "OVS FX Echo",
            "OVS FX Invert",
            "OVS FX TV Glitch",
            "OVS FX Colour Correct",
            "OVS Gen Solid Colour",
            "OVS Gen Strobe",
        ] {
            assert!(names.contains(expected), "{expected} is missing: {names:?}");
        }
        assert_eq!(names.len(), files.len(), "a name is written twice");
        for name in &names {
            assert!(!name.contains(" v"), "{name} carries a version");
        }
        // The newest of each: TV Glitch with Out of sync, Invert with a rate.
        assert!(named("OVS FX TV Glitch").contains("ovs.tv_glitch, 10 channels"));
        assert!(named("OVS FX Invert").contains("ovs.invert, 2 channels"));
        assert!(named("OVS Gen Strobe").contains("ovs.strobe, 6 channels"));
        // Retired, or with nothing for a desk to patch.
        for gone in [
            "OVS FX Tint",
            "OVS Gen Clock",
            "OVS Gen Countdown",
            "OVS Gen Grid",
            "OVS Gen Noise",
        ] {
            assert!(!names.contains(gone), "{gone} is still generated");
        }
    }

    #[test]
    fn a_file_has_one_block_per_channel_and_a_fine_block_per_wide_one() {
        // What a desk counts to work out the footprint. A fine half left out
        // shifts every channel after it by one, and nothing says so until a
        // fader moves the wrong thing.
        for personality in generated() {
            let text = fixture_type(&personality);
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
        for personality in generated() {
            let layout = Layout::of(&personality);
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
            for (channel, attribute) in personality.channels.iter().zip(&layout.attributes) {
                for (name, _, _) in channel_sets(&personality, channel, attribute) {
                    assert!(
                        name.len() <= NAME_FIELD,
                        "{}: the value {name:?} does not fit",
                        personality.type_id
                    );
                }
            }
        }
    }

    #[test]
    fn every_attribute_is_one_the_owners_desk_has() {
        // "ich habe hier mal ein Beispiel mit Attributen und Gruppen
        // geschrieben, die du so übernehmen kannst. die kommen direkt aus dem
        // Pult." Name, group, preset, label and the group's label, each as his
        // desk wrote it.
        for (fixture, attribute) in every_attribute() {
            let row = DESK
                .iter()
                .find(|row| row.0 == attribute.attribute)
                .unwrap_or_else(|| panic!("{fixture}: {} is not on the desk", attribute.attribute));
            assert_eq!(
                (
                    attribute.feature.as_str(),
                    attribute.preset,
                    attribute.attribute_label.as_str(),
                    attribute.feature_label.as_str()
                ),
                (row.1, row.2, row.3, row.4),
                "{fixture}: {}",
                attribute.attribute
            );
        }
    }

    #[test]
    fn the_desk_table_is_the_owners_example_line_for_line() {
        // Typed out from the file his desk exported, which is committed beside
        // the fixture types so this can be wrong: a label mistyped here is
        // a label his desk does not have.
        let example = include_str!("../../../reference/grandma1/desk/EXAMPLE.TXT");
        let exported: BTreeSet<(String, String, String, String, String)> = example
            .lines()
            .filter_map(|line| {
                let mut quoted = line.split('"').skip(1).step_by(2);
                if !line.trim_start().starts_with("{ _ATTRIBUT") || line.contains("_TYPE FINE") {
                    return None;
                }
                Some((
                    quoted.next()?.to_owned(),
                    quoted.next()?.to_owned(),
                    quoted.next()?.to_owned(),
                    quoted.next()?.to_owned(),
                    quoted.next()?.to_owned(),
                ))
            })
            .collect();
        assert_eq!(exported.len(), 23, "{exported:?}");
        let typed: BTreeSet<(String, String, String, String, String)> = DESK
            .iter()
            .filter(|row| row.0 != "DIM")
            .map(|row| {
                (
                    row.0.to_owned(),
                    row.1.to_owned(),
                    row.2.to_owned(),
                    row.3.to_owned(),
                    row.4.to_owned(),
                )
            })
            .collect();
        assert_eq!(typed, exported);
    }

    #[test]
    fn the_groups_are_the_owners_six() {
        // "Es sind zu viele einzelne Attribute Groups." No group but his, and
        // none needs a third page: "Es können max. 4 Parameter angezeigt
        // werden. Hat eine Gruppe mehr als 4 Parameter gibt es die A und B
        // Seiten."
        let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (_, attribute) in every_attribute() {
            groups
                .entry(attribute.feature)
                .or_default()
                .insert(attribute.attribute);
        }
        let names: Vec<&str> = groups.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "DIMMER",
                "OVS_PARAMETER",
                "OVS_RGBX_BG",
                "PAN/TILT",
                "RGBX",
                "SCALE"
            ]
        );
        let set = |names: &[&str]| -> BTreeSet<String> {
            names.iter().map(|name| (*name).to_owned()).collect()
        };
        // X, Y and the turn about Z on one page.
        assert_eq!(groups["PAN/TILT"], set(&["PAN", "TILT", "OVS_ROTATION"]));
        assert_eq!(groups["SCALE"], set(&["OVS_SCALE_X", "OVS_SCALE_Y"]));
        // The foreground, and the background beside it.
        assert_eq!(groups["RGBX"], set(&["_RED", "_GREEN", "_BLUE", "_ALPHA"]));
        assert_eq!(
            groups["OVS_RGBX_BG"],
            set(&["OVS_BG_RED", "OVS_BG_GREEN", "OVS_BG_BLUE", "OVS_BG_ALPHA"])
        );
        assert!(groups["OVS_PARAMETER"].len() <= PARAMETER_COUNT);
    }

    #[test]
    fn only_the_foreground_is_under_the_picker() {
        // "Man könnte Global RGBX als Gruppe definieren auch für Foreground
        // RGBA." And nothing in the desk's own RGB: every fixture type that put
        // a fourth attribute there was refused.
        for (fixture, attribute) in every_attribute() {
            assert_ne!(attribute.feature, "RGB", "{fixture}");
            let mixed = ["_RED", "_GREEN", "_BLUE"].contains(&attribute.attribute.as_str());
            if attribute.preset == "COLOR" {
                assert_eq!(
                    attribute.effect,
                    if mixed { "COLOR_MIX" } else { "NONE" },
                    "{fixture}: {}",
                    attribute.attribute
                );
            }
        }
        let shape = named("OVS Gen Shape");
        assert!(
            shape.contains(
                "_ATTRIBUT \"_RED\" _FEATURE \"RGBX\" _PRESET \"COLOR\" _ATT_LABEL \"Red\" \
                 _FEA_LABEL \"RGBX\""
            ),
            "{shape}"
        );
        assert!(shape.contains("_EFF COLOR_MIX _EFFSUB RED"));
        assert!(shape.contains("_ATTRIBUT \"_ALPHA\" _FEATURE \"RGBX\""));
        // "Background Color sollte nicht Color 2 sein."
        assert!(shape.contains(
            "_ATTRIBUT \"OVS_BG_RED\" _FEATURE \"OVS_RGBX_BG\" _PRESET \"COLOR\" \
             _ATT_LABEL \"BG Red\" _FEA_LABEL \"OVS BG RGBX\""
        ));
        assert_eq!(shape.matches("COLOR_MIX").count(), 3, "one picker");
        // A gradient runs from the foreground to the background's four.
        let gradient = named("OVS Gen Gradient");
        assert!(gradient.contains("_ATTRIBUT \"OVS_BG_BLUE\""));
    }

    #[test]
    fn a_parameter_is_numbered_and_its_values_say_what_it_is() {
        // "ansonsten finde ich die Param Sachen gut": the encoder says
        // Parameter 1, so the named values have to say Radius.
        let blur = named("OVS FX Blur");
        assert!(
            blur.contains(
                "_ATTRIBUT \"PARA_1\" _FEATURE \"OVS_PARAMETER\" _PRESET \"VIDEO\" \
                 _ATT_LABEL \"Parameter 1\" _FEA_LABEL \"OVS Parameter\""
            ),
            "{blur}"
        );
        for value in ["\"Radius 0\"", "\"Radius 32\"", "\"Radius 64\""] {
            assert!(blur.contains(value), "{value} in {blur}");
        }
        assert!(
            blur.contains("Parameter 1 Radius"),
            "the comment says it too"
        );

        // Numbered past the dimmer and the colours: a text's alignment is its
        // fourth parameter although it is its seventh channel.
        let text = named("OVS Gen Text");
        assert!(text.contains("_ATTRIBUT \"PARA_4\""), "{text}");
        assert!(text.contains("Parameter 4 Alignment"), "{text}");
        assert!(!text.contains("PARA_5"));
        // Colour Correct uses six.
        let correct = named("OVS FX Colour Correct");
        assert!(correct.contains("_ATTRIBUT \"PARA_6\""));
        assert!(correct.contains("\"Temperature 10000\""), "{correct}");

        // The master's and the layer's own controls are parameters too, so
        // neither brings a group (owner, 2026-10-18: "ja passt und baue es").
        let master = named("OVS Master");
        assert!(master.contains(
            "Parameter 1 Freeze, Parameter 2 BPM Tap, Parameter 3 BPM Scale, \
             Parameter 4 Crossfade"
        ));
        assert!(master.contains("{ _NAME \"Freeze On\" _RANGE [99.6109%,100.0000%] }"));
        assert!(named("OVS Layer").contains("Parameter 1 Slot"));
    }

    #[test]
    fn the_layer_names_every_slot_where_the_server_reads_it() {
        // "inkl. aller Slots": Black and eighty-four slots, each at one value
        // the decoder reads as it. Black is home, DMX 0; slot 1 is the middle
        // of 4 to 6, DMX 5, which is 1280 of 65535 on the desk; slot 84 the
        // middle of 253 to 255.
        let layer = named("OVS Layer");
        assert!(
            layer.contains("{ _NAME \"Black\" _RANGE [0.0000%,0.3891%] }"),
            "{layer}"
        );
        assert!(
            layer.contains("{ _NAME \"Slot 1\" _RANGE [1.9532%,2.3423%] }"),
            "{layer}"
        );
        assert!(
            layer.contains("{ _NAME \"Slot 84\" _RANGE [99.2203%,99.6094%] }"),
            "{layer}"
        );
        assert_eq!(layer.matches("\"Slot ").count(), 84);
        assert!(layer.contains("_ATTRIBUT \"PARA_1\" _FEATURE \"OVS_PARAMETER\" _PRESET \"VIDEO\""));
    }

    #[test]
    fn the_layer_moves_on_the_trackball_and_turns_on_the_same_page() {
        // "Position X und Y könnten Pan und Tilt sein, damit man es mit dem
        // Trackball am Pult bedienen kann." "Rotation könnte mit zu Pan/Tilt
        // in die selbe Attribute Gruppe."
        let layer = named("OVS Layer");
        for (attribute, effect) in [("PAN", "PAN"), ("TILT", "TILT")] {
            assert!(
                layer.contains(&format!(
                    "_ATTRIBUT \"{attribute}\" _FEATURE \"PAN/TILT\" _PRESET \"PAN/TILT\""
                )),
                "{layer}"
            );
            assert!(layer.contains(&format!(
                "_ATTRIBUT \"{attribute}\" _FEATURE \"PAN/TILT\" _PRESET \"PAN/TILT\" _TYPE FINE"
            )));
            assert!(layer.contains(&format!("_EFF {effect} ")), "{layer}");
        }
        assert!(layer.contains(
            "_ATTRIBUT \"OVS_ROTATION\" _FEATURE \"PAN/TILT\" _PRESET \"PAN/TILT\" \
             _ATT_LABEL \"Rotation\""
        ));
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
        // "Layer Scale sollte eine eigene Gruppe fern ab von den Parametern
        // sein": his desk's SCALE, beside pan and tilt.
        assert!(layer.contains(
            "_ATTRIBUT \"OVS_SCALE_X\" _FEATURE \"SCALE\" _PRESET \"PAN/TILT\" \
             _ATT_LABEL \"Scale X\" _FEA_LABEL \"Scale\" _DEFAULT 10.0008%"
        ));
        // And the centre of a position is the middle the server reads as nought.
        assert!(layer.contains("{ _NAME \"Center 0\" _RANGE [50.0008%,50.0008%] }"));
    }

    #[test]
    fn an_amount_is_the_dimmer_so_a_group_master_takes_effects_out() {
        // "... somit könnte man einen Groupmaster im Pult erstellen, der alle
        // Amounts auf 0 zieht, für komplett ohne Effekte."
        for personality in generated() {
            let has_amount = parameters_of(&personality)
                .iter()
                .any(|parameter| parameter.key == "amount" && parameter.dmx.is_some());
            if has_amount {
                let file = fixture_type(&personality);
                assert!(
                    file.contains("_ATTRIBUT \"DIM\" _FEATURE \"DIMMER\" _PRESET \"DIMMER\""),
                    "{} has an Amount and no dimmer",
                    personality.type_id
                );
                assert!(file.contains("_EFF DIMMER"), "{}", personality.type_id);
            }
        }
        for name in [
            "OVS FX Blur",
            "OVS FX Pixelate",
            "OVS FX RGB Split",
            "OVS FX Echo",
            "OVS FX Invert",
        ] {
            assert!(named(name).contains("_ATTRIBUT \"DIM\""), "{name}");
        }
        for name in [
            "OVS FX Colour Correct",
            "OVS FX Crop",
            "OVS FX Mirror",
            "OVS FX RGB Correct",
        ] {
            assert!(!named(name).contains("_ATTRIBUT \"DIM\""), "{name}");
        }
    }

    #[test]
    fn pulling_a_dimmer_down_never_adds_an_effect() {
        // A group master at nought must take a module out of the picture. A
        // dimmer whose least were a look — nought brightness is black — would
        // put one in.
        for personality in generated() {
            let layout = Layout::of(&personality);
            for (channel, attribute) in personality.channels.iter().zip(&layout.attributes) {
                if attribute.attribute != "DIM" {
                    continue;
                }
                let Some(ParameterSlot::Effect(slot) | ParameterSlot::Generator(slot)) =
                    slot_of(&personality, channel)
                else {
                    continue;
                };
                let parameter = parameters_of(&personality)
                    .iter()
                    .find(|parameter| parameter.key == slot.key)
                    .expect("a dimmer is a parameter of its module");
                match parameter.kind {
                    ParameterKind::Number { min, default, .. } => {
                        let at_zero = match channel.encoding {
                            ChannelEncodingInfo::Ranged8 { min, .. }
                            | ChannelEncodingInfo::Ranged16 { min, .. } => min,
                            _ => panic!("{}: a numeric dimmer that is not a fader", slot.key),
                        };
                        assert!((at_zero - min).abs() < 1e-9, "{}", slot.key);
                        // An amount of nought is none of the module; any other
                        // dimmer's least is the default every module is
                        // neutral at.
                        assert!(
                            if slot.key == "amount" {
                                min == 0.0
                            } else {
                                (min - default).abs() < 1e-9
                            },
                            "{} {}: nought on the dimmer is {min}, not nothing",
                            personality.type_id,
                            slot.key
                        );
                    }
                    ParameterKind::Toggle { default } => {
                        assert!(!default, "{} {}", personality.type_id, slot.key);
                    }
                    _ => panic!(
                        "{} {}: a dimmer that is a choice",
                        personality.type_id, slot.key
                    ),
                }
            }
        }
    }

    #[test]
    fn an_attribute_means_one_thing_in_every_file() {
        // The desk keeps one label and one group for an attribute, for the
        // whole show: whatever a second file said would not be shown. How a
        // channel is visualised belongs to its functional block, which is the
        // fixture type's own — a hue is shown as a colour, Parameter 1 of
        // another module is not.
        let mut seen: BTreeMap<String, (String, &str, String, String)> = BTreeMap::new();
        let mut groups: BTreeMap<String, (&str, String)> = BTreeMap::new();
        for (fixture, attribute) in every_attribute() {
            let meaning = (
                attribute.feature.clone(),
                attribute.preset,
                attribute.attribute_label.clone(),
                attribute.feature_label.clone(),
            );
            let first = seen
                .entry(attribute.attribute.clone())
                .or_insert_with(|| meaning.clone());
            assert_eq!(
                *first, meaning,
                "{} in {fixture} means something else elsewhere",
                attribute.attribute
            );
            let group = groups
                .entry(attribute.feature.clone())
                .or_insert((attribute.preset, attribute.feature_label.clone()));
            assert_eq!(
                *group,
                (attribute.preset, attribute.feature_label.clone()),
                "{}",
                attribute.feature
            );
        }
        assert!(seen.contains_key("PARA_6"));
    }

    #[test]
    fn every_functional_block_is_named_for_what_it_does() {
        // The owner's encoder read "40 Function" for a hue. The block's name is
        // the fixture type's own, so it can say what Parameter 1 cannot.
        for file in fixture_files() {
            assert!(
                !file.contents.contains("_NAME \"Function\""),
                "{}",
                file.file_name
            );
        }
        assert!(named("OVS FX Colorize").contains("{ _NAME \"Hue\" _RANGE [ 0,255]"));
        assert!(named("OVS FX Colorize").contains("{ _NAME \"Amount\" _RANGE [ 0,255] _EFF DIMMER"));
        assert!(named("OVS FX Pixelate").contains("{ _NAME \"Block size\" _RANGE"));
        let layer = named("OVS Layer");
        for block in [
            "Dimmer",
            "Slot",
            "Position X",
            "Position Y",
            "Rotation",
            "Scale X",
        ] {
            assert!(
                layer.contains(&format!("{{ _NAME \"{block}\" _RANGE [ 0,255]")),
                "{block} in {layer}"
            );
        }
        assert!(named("OVS Gen Text").contains("{ _NAME \"Background Red\" _RANGE [ 0,255]"));
    }

    #[test]
    fn a_hue_is_in_degrees_with_its_colours_named() {
        // "Ohne den Visualize Color kann man nicht die Werte von 0 bis 360
        // angeben als Degree", and "Bei Hue kannst du noch bei 0 Red, 30
        // Orange, 60 Yellow, 120 Green, 180 Cyan, 240 Blue, 300 Magenta als
        // Werte eintragen?"
        // "... es gäbe sogar Visualize Hue direkt."
        let colorize = named("OVS FX Colorize");
        assert!(
            colorize.contains(
                "{ _NAME \"Hue\" _RANGE [ 0,255] _EFF VIDEO_HUE _PHYS [ 0.00, 360.00] \t\t\t_CHANSET"
            ),
            "{colorize}"
        );
        // 30 degrees of 360 is 5461 of 65535, and 180 is the middle, 32768.
        for set in [
            "{ _NAME \"Red\" _RANGE [0.0000%,0.0000%] }",
            "{ _NAME \"Orange\" _RANGE [8.3330%,8.3330%] }",
            "{ _NAME \"Yellow\" _RANGE [16.6674%,16.6674%] }",
            "{ _NAME \"Green\" _RANGE [33.3333%,33.3333%] }",
            "{ _NAME \"Cyan\" _RANGE [50.0008%,50.0008%] }",
            "{ _NAME \"Blue\" _RANGE [66.6667%,66.6667%] }",
            "{ _NAME \"Magenta\" _RANGE [83.3341%,83.3341%] }",
        ] {
            assert!(colorize.contains(set), "{set} in {colorize}");
        }
        assert!(
            !colorize.contains("\"Hue 180\""),
            "the colours replace the landmarks"
        );
    }

    #[test]
    fn a_control_is_typed_in_its_own_units() {
        // The owner, 2026-10-18: Brightness and Contrast take 0.0 to 4.0, CTC
        // on Wheel 1 takes 2000 to 10000, Scale on X and Y takes 0.0 to 4.0.
        let correct = named("OVS FX Colour Correct");
        for line in [
            "{ _NAME \"Brightness\" _RANGE [ 0,255] _EFF VIDEO_BRIGHTNESS _PHYS [ 0.00, 4.00]",
            "{ _NAME \"Contrast\" _RANGE [ 0,255] _EFF VIDEO_CONTRAST _PHYS [ 0.00, 4.00]",
            "{ _NAME \"Saturation\" _RANGE [ 0,255] _EFF VIDEO_COLORINTENSITY _PHYS [ 0.00, 4.00]",
            "{ _NAME \"Temperature\" _RANGE [ 0,255] _EFF COLOR_CORRECTION _EFFSUB WHEEL_1 \
             _PHYS [ 2000.00,10000.00]",
        ] {
            assert!(correct.contains(line), "{line} in {correct}");
        }
        let layer = named("OVS Layer");
        for line in [
            "{ _NAME \"Rotation\" _RANGE [ 0,255] _EFF VIDEO_ROTATION _EFFSUB Z _PHYS [ -180.00, 180.00]",
            "{ _NAME \"Scale X\" _RANGE [ 0,255] _EFF VIDEO_SCALE _EFFSUB X _PHYS [ 0.00, 10.00]",
            "{ _NAME \"Scale Y\" _RANGE [ 0,255] _EFF VIDEO_SCALE _EFFSUB Y _PHYS [ 0.00, 10.00]",
        ] {
            assert!(layer.contains(line), "{line} in {layer}");
        }
        assert!(named("OVS Gen Strobe")
            .contains("{ _NAME \"Rate\" _RANGE [ 0,255] _EFF STROBE _PHYS [ 0.50, 30.00]"));
        // Nothing is left visualised as the colour wheel by mistake.
        for file in fixture_files() {
            assert!(!file.contents.contains("_EFF COLOR "), "{}", file.file_name);
        }
    }

    /// Every visualisation a grandMA1 has, as its program names them, in the
    /// order of its menu: read off the table in the onPC 6.808 image.
    const DESK_VISUALISATIONS: [&str; 70] = [
        "NONE",
        "PAN",
        "TILT",
        "GOBO",
        "GOBO_ANGLE",
        "GOBO_ROTATE",
        "COLOR",
        "COLOR_ROTATE",
        "COLOR_MIX",
        "DIMMER",
        "STROBE",
        "ZOOM",
        "IRIS",
        "FOCUS",
        "VIDEO_POSITION",
        "VIDEO_ROTATION",
        "VIDEO_IMAGE_SPLIT",
        "VIDEO_IMAGE_OFFSET",
        "VIDEO_SCALE",
        "VIDEO_OBJECT_TYPE",
        "VIDEO_CAMERA_INDEX",
        "VIDEO_FRAME_INDEX",
        "VIDEO_IMAGE_ANIMATION",
        "VIDEO_ROTATE_ANIMATION",
        "VIDEO_POSITION_ANIMATION",
        "VIDEO_SPEED",
        "VIDEO_STARTFRAME",
        "VIDEO_ENDFRAME",
        "VIDEO_DARKNESS",
        "VIDEO_CAMERA_POS",
        "VIDEO_DARK_IS_TRANSPARENT",
        "VIDEO_SHUTTER",
        "VIDEO_PLAY_MODE",
        "VIDEO_GOBO_POOL",
        "VE_VIDEO_POSITION_SCALE",
        "VE_VIDEO_CONTROL",
        "VIDEO_CAMERA_ROT",
        "VIDEO_EFFECT_1",
        "VIDEO_EFFECT_2",
        "VIDEO_EFFECT_3",
        "VIDEO_EFFECT_4",
        "VIDEO_COLOR_BOOSTER",
        "BLADES_ROTATION",
        "BARN_DOOR",
        "GOBO_WHEEL_ROTATE",
        "PRISM",
        "PRISM_POS",
        "PRISM_ROT",
        "FROST",
        "ROLL",
        "CLAMP",
        "COLOR_CORRECTION",
        "GOBO_SHAKE",
        "MOVE_SPEED",
        "EFFECT_SPEED",
        "COLOR_RANDOM",
        "VIDEO_SHOW_LAYER_ON_HEAD",
        "VIDEO_COLORINTENSITY",
        "VIDEO_BRIGHTNESS",
        "VIDEO_CONTRAST",
        "VIDEO_COLOR_KEY",
        "VIDEO_DUMMY",
        "VIDEO_SOUND_VOLUMEN_1",
        "VIDEO_SOUND_VOLUMEN_2",
        "VIDEO_3D_KEYSTONE",
        "VIDEO_MASTER_SHAPER_ON",
        "VIDEO_KEYSTONE",
        "VIDEO_SPLITSCREEN",
        "VIDEO_HUE",
        "VIDEO_SHOW_LAYER_ON_CAMERA",
    ];

    #[test]
    fn the_desk_knows_every_visualisation() {
        // A misspelt one would arrive as nothing, or be refused, and the
        // file would still look right here.
        for file in fixture_files() {
            for line in file.contents.lines() {
                let Some((_, rest)) = line.split_once("_EFF ") else {
                    continue;
                };
                let effect = rest.split_whitespace().next().unwrap_or_default();
                assert!(
                    DESK_VISUALISATIONS.contains(&effect),
                    "{}: {effect}",
                    file.file_name
                );
                if let Some((_, sub)) = rest.split_once("_EFFSUB ") {
                    let sub = sub.split_whitespace().next().unwrap_or_default();
                    // The subfunctions MA's own files write for these.
                    assert!(
                        ["RED", "GREEN", "BLUE", "X", "Y", "Z", "ALL", "WHEEL_1"].contains(&sub),
                        "{}: {effect} {sub}",
                        file.file_name
                    );
                }
            }
        }
    }

    #[test]
    fn every_named_step_is_one_value_the_server_reads_as_that_step() {
        // The sheet showed a number for anything resting inside a band-wide
        // named value, so each is one DMX value — and it must still be one the
        // server reads as the step it is named for.
        let step = 256.0 * 100.0 / 65_535.0;
        for personality in generated() {
            let layout = Layout::of(&personality);
            for (channel, attribute) in personality.channels.iter().zip(&layout.attributes) {
                let sets = channel_sets(&personality, channel, attribute);
                match &channel.encoding {
                    ChannelEncodingInfo::Stepped { bands } => {
                        assert_eq!(sets.len(), bands.len(), "{}", channel.name);
                        for ((name, from, to), band) in sets.iter().zip(bands) {
                            assert!(
                                (to - from - (step - 100.0 / 65_535.0)).abs() < 1e-3,
                                "{name}"
                            );
                            let dmx = (from * 65_535.0 / 100.0 / 256.0).round() as u8;
                            assert!(
                                (band.from..=band.to).contains(&dmx),
                                "{} {name} at {dmx}, outside {}..={}",
                                personality.type_id,
                                band.from,
                                band.to
                            );
                        }
                    }
                    ChannelEncodingInfo::Switch { threshold } => {
                        let at = |percent: f64| (percent * 65_535.0 / 100.0 / 256.0).round() as u8;
                        assert!(at(sets[0].1) < *threshold, "{}", channel.name);
                        assert!(at(sets[1].1) >= *threshold, "{}", channel.name);
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn every_file_name_is_capitals_with_one_dot_before_txt() {
        // The owner, 2026-10-18: the desk refused the first set until the
        // names were in capitals with no dot but the extension's.
        for file in fixture_files() {
            let name = &file.file_name;
            assert!(name.ends_with(".TXT"), "{name}");
            assert_eq!(name.matches('.').count(), 1, "{name}");
            assert_eq!(&name.to_uppercase(), name, "{name}");
            assert!(name.starts_with("TESTBILD MEDIA@OVS "), "{name}");
            assert!(is_fixture_file(name), "{name}");
        }
        assert!(fixture_files()
            .iter()
            .any(|file| file.file_name == "TESTBILD MEDIA@OVS FX ECHO.TXT"));
        assert!(fixture_files()
            .iter()
            .any(|file| file.file_name == "TESTBILD MEDIA@OVS FX COLOUR CORRECT.TXT"));
        // The old spellings are still recognised as ours, so they can be
        // cleared out.
        assert!(is_fixture_file("testbild.media@OVS FX Echo.txt"));
        assert!(is_fixture_file("TESTBILD MEDIA@OVS FX TV GLITCH V2.TXT"));
        assert!(!is_fixture_file("README.md"));
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
    fn a_choice_is_named_as_the_client_names_it() {
        // Behind at the echo's home, 64; Lighten in the middle of 128 to 255.
        let echo = named("OVS FX Echo");
        assert!(
            echo.contains("{ _NAME \"Behind\" _RANGE [25.0004%,25.3895%] }"),
            "{echo}"
        );
        assert!(
            echo.contains("{ _NAME \"Lighten\" _RANGE [74.6105%,74.9996%] }"),
            "{echo}"
        );
    }

    #[test]
    fn the_master_names_its_speed_steps_and_its_triggers() {
        let master = named("OVS Master");
        assert!(master.contains("\"0.125x\""), "{master}");
        // 1x at home, 128, so a master at rest reads 1x.
        assert!(
            master.contains("{ _NAME \"1x\" _RANGE [50.0008%,50.3899%] }"),
            "{master}"
        );
        assert!(master.contains("_ATTRIBUT \"PARA_2\" _FEATURE \"OVS_PARAMETER\""));
        assert!(!master.contains("Sync"), "{master}");
        assert!(master.contains("ovs.master, 5 channels"), "{master}");
        assert!(
            master.contains("{ _NAME \"BPM Tap Fire\" _RANGE [99.6109%,100.0000%] }"),
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
        assert!(raw.contains("\t\t{ _NAME \"Dimmer\"     _RANGE [  0,255] _EFF DIMMER _PHYS [    0.00,    1.00] }\r\n"), "{raw}");
        assert!(
            raw.contains("\t\t\t{ _NAME \"Black\"      _RANGE [0.0000%,0.3891%] }\r\n"),
            "{raw}"
        );
    }
}
