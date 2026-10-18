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
//!   fest." And "Layer Scale sollte eine eigene Gruppe fern ab von den
//!   Parametern sein", but "OVS Layer braucht keine eigene Gruppe wenn nur
//!   eine Sache drin ist", so the slot joins the scale.
//! - "Man könnte Global RGBX als Gruppe definieren auch für Foreground RGBA,
//!   weil hier das X mehrere Bedeutungen haben kann.. X kann Alpha, Amber oder
//!   auch White sein bei anderen Fixtures." And a background is a background:
//!   "Background Color sollte nicht Color 2 sein, das ist missverständlich."
//! - "Params bitte alle unter der Video Hauptgruppe machen, so kann man das
//!   Video Fenster in der GrandMA für Presets nutzen."
//!
//! So there are **six groups, and a seventh only on purpose**
//! (`the_groups_are_these_six` says so when one appears):
//!
//! | Group | Preset | Attributes |
//! |---|---|---|
//! | `DIMMER` | Dimmer | `DIM`: the master's, a layer's, a module's |
//! | `PAN/TILT` | Pan/Tilt | `PAN` (X), `TILT` (Y), `OVS ROTATION` (Z) |
//! | `RGBX` | Color | `R`, `G`, `B`, `X`; then `BG R`, `BG G`, `BG B`, `BG X` |
//! | `OVS LAYER` | Video | `OVS SLOT`, `OVS SCALE X`, `OVS SCALE Y` |
//! | `OVS MASTER` | Video | freeze, BPM tap, BPM scale, crossfade |
//! | `OVS PARAM` | Video | `OVS PARAM 1` to `6`, shared by every module |
//!
//! `DIM`, `PAN` and `TILT` are the desk's own, written as the library writes
//! them, because a tool of the desk answers to them: the masters and
//! Highlight, the trackball and position effects. `RGBX` is the owner's own
//! group for any fixture, so it says nothing of OVS; its `R`, `G` and `B` keep
//! the colour-mix effect the colour picker looks for, and `X` is whatever a
//! fixture's fourth part is — alpha, here. A gradient's far colour is the
//! background's four, the one place `BG` is not literally a background.
//! Everything else is ours and says `OVS`.
//!
//! **A module's parameters are numbered**, in the order the module declares
//! them, leaving out its dimmer and its colours: Blur's radius is Param 1, a
//! text's cap height is Param 1 and its alignment Param 4. The encoder says
//! "Param 1"; the named values say what it is — `Radius 0`, `Radius 32`,
//! `Radius 64` — because a label belongs to the attribute and the attribute
//! to every module.
//!
//! **A colour-mix group is not shared with the desk's `RGB`.** The first set of
//! files put a module's alpha into `RGB` as a fourth attribute, and every
//! fixture type with a colour was refused by the desk (owner, 2026-10-18)
//! while every one without was taken; no fixture of the 25 in the library
//! with colour mixing in `RGB` has anything else in it. Whether `RGBX`, which
//! is new and has an `X`, is taken is not known until it is imported: two
//! probe files beside these ask the desk.

use ovs_types::config::fixtures::{definition, ColourPart, ParameterSlot};
use ovs_types::config::{ParameterDefinition, ParameterKind};

use crate::fixtures::{
    catalogue, ChannelBand, ChannelEncodingInfo, Personality, PersonalityChannel, PersonalityTarget,
};

/// Who the desk lists these under.
pub const MANUFACTURER: &str = "testbild.media";

/// The longest attribute, feature, label or named value a grandMA1 shows.
pub const NAME_FIELD: usize = 18;

/// The group every module's parameters share.
const PARAMETERS: &str = "OVS PARAM";

/// One fixture type, ready to be written to disk and imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureFile {
    /// The file's name, in the library's own `MANUFACTURER@NAME.TXT` form and
    /// the capitals the desk insists on; see [`file_name`].
    pub file_name: String,
    /// What the desk's fixture list calls it.
    pub name: String,
    /// The file, with the CRLF line ends MA's own files have.
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
            let number = attribute.attribute.strip_prefix("OVS PARAM ")?;
            Some(format!(
                "Param {number} {}",
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
    /// What a shared parameter is in this module, which its named values say
    /// because its label cannot.
    named_for: Option<String>,
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
        }
    }

    /// The same channel, taken by one of the desk's tools.
    fn for_the_desk(mut self, effect: &'static str, effect_sub: Option<&'static str>) -> Self {
        self.effect = effect;
        self.effect_sub = effect_sub;
        self
    }

    /// The desk's dimmer, written as 490 fixtures of the library write it.
    fn dimmer() -> Self {
        Self::plain("DIM", "DIMMER", "DIMMER", "Dim", "Dimmer").for_the_desk("DIMMER", None)
    }

    /// One of the master's own controls.
    fn master(attribute: impl Into<String>, label: impl Into<String>) -> Self {
        Self::plain(attribute, "OVS MASTER", "VIDEO", label, "OVS Master")
    }

    /// One of a layer's own controls that is not position or turn.
    fn layer(attribute: impl Into<String>, label: impl Into<String>) -> Self {
        Self::plain(attribute, "OVS LAYER", "VIDEO", label, "OVS Layer")
    }

    /// One part of a colour, in the owner's RGBX: `R`, `G`, `B` and `X`, or
    /// the same with `BG` in front for a background. The attribute is its own
    /// label, because both are what the encoder shows.
    fn rgbx(attribute: String) -> Self {
        Self::plain(attribute.clone(), "RGBX", "COLOR", attribute, "RGBX")
    }

    /// A module's `number`th parameter, which is `control` in this module.
    fn parameter(number: usize, control: &str) -> Self {
        let mut attribute = Self::plain(
            format!("{PARAMETERS} {number}"),
            PARAMETERS,
            "VIDEO",
            format!("Param {number}"),
            "OVS Param",
        );
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
    let pan_tilt =
        |attribute, label| Attribute::plain(attribute, "PAN/TILT", "PAN/TILT", label, "Pan/Tilt");
    let attribute = match control {
        "master_dimmer" | "layer_opacity" => Attribute::dimmer(),
        "master_freeze" => Attribute::master("OVS FREEZE", "Freeze"),
        "master_tap_tempo" => Attribute::master("OVS BPM TAP", "BPM Tap"),
        "master_speed_scale" => Attribute::master("OVS BPM SCALE", "BPM Scale"),
        "master_crossfader" => Attribute::master("OVS CROSSFADE", "Crossfade A/B"),
        "layer_slot" => Attribute::layer("OVS SLOT", "Slot"),
        // The trackball's. Up is up: a larger value moves the picture up the
        // wall, as a larger tilt lifts a head.
        "layer_position_x" => pan_tilt("PAN", "Pan").for_the_desk("PAN", None),
        "layer_position_y" => pan_tilt("TILT", "Tilt").for_the_desk("TILT", None),
        // Ours, in the desk's group: X, Y and the turn about Z on one page.
        "layer_rotation" => pan_tilt("OVS ROTATION", "Rotation"),
        // With the slot, away from the parameters.
        "layer_scale_x" => Attribute::layer("OVS SCALE X", "Scale X"),
        "layer_scale_y" => Attribute::layer("OVS SCALE Y", "Scale Y"),
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
        return attribute;
    }

    let Some(parameter) = (match slot {
        Some(ParameterSlot::Effect(parameter) | ParameterSlot::Generator(parameter)) => {
            Some(parameter)
        }
        _ => None,
    }) else {
        // A control of the server's own that `fixed` does not know yet, in
        // the group of whichever it belongs to.
        let attribute = format!("OVS {}", upper(&channel.name));
        return match personality.target {
            PersonalityTarget::Layer => Attribute::layer(attribute, field(&channel.name)),
            _ => Attribute::master(attribute, field(&channel.name)),
        };
    };

    if is_dimmer(&personality.type_id, parameter.key) {
        return Attribute::dimmer();
    }

    if let Some(part) = parameter.component {
        let letter = match part {
            ColourPart::Red => "R",
            ColourPart::Green => "G",
            ColourPart::Blue => "B",
            ColourPart::Alpha => "X",
        };
        // The first colour is the foreground, and its red, green and blue
        // carry the colour-mix effect, which is what the colour picker looks
        // for. The background — or a gradient's far colour — has no picker:
        // one per fixture, as in every fixture of the library.
        if first_colour == Some(parameter.key) {
            let attribute = Attribute::rgbx(letter.to_owned());
            return match part {
                ColourPart::Red => attribute.for_the_desk("COLOR_MIX", Some("RED")),
                ColourPart::Green => attribute.for_the_desk("COLOR_MIX", Some("GREEN")),
                ColourPart::Blue => attribute.for_the_desk("COLOR_MIX", Some("BLUE")),
                ColourPart::Alpha => attribute,
            };
        }
        return Attribute::rgbx(format!("BG {letter}"));
    }

    let name = parameters
        .iter()
        .find(|defined| defined.key == parameter.key)
        .map_or(parameter.key, |defined| defined.name);
    *numbered += 1;
    Attribute::parameter(*numbered, name)
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
    let named_for = attribute.named_for.as_deref();
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
            let (off, on) = match named_for {
                Some(control) => (format!("{control} {off}"), format!("{control} {on}")),
                None => (off.to_owned(), on.to_owned()),
            };
            let below = threshold.saturating_sub(1);
            let (low_from, low_to) = byte_band(0, below);
            let (high_from, high_to) = byte_band(*threshold, 255);
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
            landmarks(*min, *max, 255, channel.home, named_for)
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
            landmarks(*min, *max, 65_535, channel.home, named_for)
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

/// The bottom, the middle, the top and home of a continuous channel, as named
/// values: the numbers somebody at a desk asked for (owner, 2026-10-16: "wo
/// Mitte ist, welche Scale wo liegt"). Home only where it is none of the
/// other three.
///
/// Each is named after what the server makes of the number sent, not after
/// the round figure it is near: the middle of an eight-bit channel is 128 of
/// 255, which is not quite half way. A shared parameter's are named after what
/// it is in this module instead — `Radius 32` rather than `Center 32` — since
/// its encoder says only "Param 1".
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

/// A name in the capitals an attribute or a group is written in.
///
/// Not cut to the field: an attribute cut short could become another one, and
/// a test says when a name no longer fits instead.
fn upper(name: &str) -> String {
    ascii(name).to_uppercase()
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
    fn the_groups_are_these_six() {
        // "Es sind zu viele einzelne Attribute Groups." A seventh is a
        // decision, not something a new module does by being added.
        let mut groups: BTreeMap<String, (&str, BTreeSet<String>)> = BTreeMap::new();
        for (_, attribute) in every_attribute() {
            groups
                .entry(attribute.feature)
                .or_insert((attribute.preset, BTreeSet::new()))
                .1
                .insert(attribute.attribute);
        }
        let names: Vec<(&str, &str)> = groups
            .iter()
            .map(|(group, (preset, _))| (group.as_str(), *preset))
            .collect();
        // "Params bitte alle unter der Video Hauptgruppe", "RGBX unter Color!"
        assert_eq!(
            names,
            [
                ("DIMMER", "DIMMER"),
                ("OVS LAYER", "VIDEO"),
                ("OVS MASTER", "VIDEO"),
                ("OVS PARAM", "VIDEO"),
                ("PAN/TILT", "PAN/TILT"),
                ("RGBX", "COLOR"),
            ]
        );
        // "Es können max. 4 Parameter angezeigt werden. Hat eine Gruppe mehr
        // als 4 Parameter gibt es die A und B Seiten." Two pages at most.
        for (group, (_, attributes)) in &groups {
            assert!(
                attributes.len() <= 8,
                "{group} needs a third page: {attributes:?}"
            );
        }
        let members = |group: &str| groups[group].1.clone();
        let set = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
        // X, Y and the turn on one page.
        assert_eq!(members("PAN/TILT"), set(&["PAN", "TILT", "OVS ROTATION"]));
        // The layer's size with its slot, away from the parameters; and no
        // group with one thing in it.
        assert_eq!(
            members("OVS LAYER"),
            set(&["OVS SLOT", "OVS SCALE X", "OVS SCALE Y"])
        );
        // "BPM Sync entfernen wir komplett aus DMX": four, one page.
        assert_eq!(
            members("OVS MASTER"),
            set(&[
                "OVS FREEZE",
                "OVS BPM TAP",
                "OVS BPM SCALE",
                "OVS CROSSFADE"
            ])
        );
        // A foreground on one page, a background on the other.
        assert_eq!(
            members("RGBX"),
            set(&["R", "G", "B", "X", "BG R", "BG G", "BG B", "BG X"])
        );
    }

    #[test]
    fn a_colour_is_rgbx_and_only_the_foreground_is_under_the_picker() {
        // "Man könnte Global RGBX als Gruppe definieren auch für Foreground
        // RGBA, weil hier das X mehrere Bedeutungen haben kann." And nothing in
        // the desk's own RGB: every fixture type that put a fourth attribute
        // there was refused.
        for (fixture, attribute) in every_attribute() {
            assert_ne!(attribute.feature, "RGB", "{fixture}");
            if attribute.feature == "RGBX" {
                let mixed = ["R", "G", "B"].contains(&attribute.attribute.as_str());
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
                "_ATTRIBUT \"R\" _FEATURE \"RGBX\" _PRESET \"COLOR\" _ATT_LABEL \"R\" \
                 _FEA_LABEL \"RGBX\""
            ),
            "{shape}"
        );
        assert!(shape.contains("_EFF COLOR_MIX _EFFSUB RED"));
        assert!(shape.contains("_ATTRIBUT \"X\" _FEATURE \"RGBX\""));
        // "Background Color sollte nicht Color 2 sein."
        assert!(shape.contains("_ATTRIBUT \"BG R\" _FEATURE \"RGBX\""));
        assert!(shape.contains("_ATTRIBUT \"BG X\" _FEATURE \"RGBX\""));
        assert_eq!(shape.matches("COLOR_MIX").count(), 3, "one picker");
        // A gradient runs from the foreground to the background's four.
        let gradient = named("OVS Gen Gradient");
        assert!(gradient.contains("_ATTRIBUT \"BG B\""));
    }

    #[test]
    fn a_parameter_is_numbered_and_its_values_say_what_it_is() {
        // "ansonsten finde ich die Param Sachen gut": the encoder says Param 1,
        // so the named values have to say Radius.
        let blur = named("OVS FX Blur");
        assert!(
            blur.contains(
                "_ATTRIBUT \"OVS PARAM 1\" _FEATURE \"OVS PARAM\" _PRESET \"VIDEO\" \
                 _ATT_LABEL \"Param 1\" _FEA_LABEL \"OVS Param\""
            ),
            "{blur}"
        );
        for value in ["\"Radius 0\"", "\"Radius 32\"", "\"Radius 64\""] {
            assert!(blur.contains(value), "{value} in {blur}");
        }
        assert!(blur.contains("Param 1 Radius"), "the comment says it too");

        // Numbered past the dimmer and the colours: a text's alignment is its
        // fourth parameter although it is its seventh channel.
        let text = named("OVS Gen Text");
        assert!(text.contains("_ATTRIBUT \"OVS PARAM 4\""), "{text}");
        assert!(text.contains("Param 4 Alignment"), "{text}");
        assert!(!text.contains("OVS PARAM 5"));
        // Colour Correct uses all six.
        let correct = named("OVS FX Colour Correct");
        assert!(correct.contains("_ATTRIBUT \"OVS PARAM 6\""));
        assert!(correct.contains("\"Temperature 10000\""), "{correct}");
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
        assert!(layer.contains("_ATTRIBUT \"OVS SLOT\" _FEATURE \"OVS LAYER\" _PRESET \"VIDEO\""));
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
            "_ATTRIBUT \"OVS ROTATION\" _FEATURE \"PAN/TILT\" _PRESET \"PAN/TILT\" \
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
        // sein."
        assert!(layer.contains(
            "_ATTRIBUT \"OVS SCALE X\" _FEATURE \"OVS LAYER\" _PRESET \"VIDEO\" \
             _ATT_LABEL \"Scale X\" _FEA_LABEL \"OVS Layer\" _DEFAULT 10.0008%"
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
    fn everything_but_the_desks_and_rgbx_says_ovs() {
        // "Wenn du neue Attribute hinzufügst mach auch OVS Blur z.B. statt nur
        // Blur." RGBX is the owner's group for every fixture, not ours.
        const DESK_GROUPS: [&str; 3] = ["DIMMER", "PAN/TILT", "RGBX"];
        const DESK: [&str; 3] = ["DIM", "PAN", "TILT"];
        for (fixture, attribute) in every_attribute() {
            if !DESK.contains(&attribute.attribute.as_str()) && attribute.feature != "RGBX" {
                assert!(
                    attribute.attribute.starts_with("OVS "),
                    "{fixture}: {}",
                    attribute.attribute
                );
            }
            if !DESK_GROUPS.contains(&attribute.feature.as_str()) {
                assert!(
                    attribute.feature.starts_with("OVS ")
                        && attribute.feature_label.starts_with("OVS "),
                    "{fixture}: {} / {}",
                    attribute.feature,
                    attribute.feature_label
                );
            }
        }
    }

    #[test]
    fn an_attribute_means_one_thing_in_every_file() {
        // The desk keeps one label and one group for an attribute, for the
        // whole show: whatever a second file said would not be shown.
        let mut seen: BTreeMap<String, (String, &str, String, String, &str)> = BTreeMap::new();
        let mut groups: BTreeMap<String, (&str, String)> = BTreeMap::new();
        for (fixture, attribute) in every_attribute() {
            let meaning = (
                attribute.feature.clone(),
                attribute.preset,
                attribute.attribute_label.clone(),
                attribute.feature_label.clone(),
                attribute.effect,
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
        assert!(seen.contains_key("OVS PARAM 6"));
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
        assert!(master.contains("_ATTRIBUT \"OVS BPM TAP\" _FEATURE \"OVS MASTER\""));
        assert!(!master.contains("SYNC"), "{master}");
        assert!(master.contains("ovs.master, 5 channels"), "{master}");
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
