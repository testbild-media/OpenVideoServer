//! The fixture catalogue: the personalities, in a form something else can read.
//!
//! A console does not read a Rust constant. Somebody sits down with a desk and
//! programmes a personality by hand, channel by channel, and what they need is
//! every channel's number, what it does, and — for the stepped channels — which
//! range of values means which step. Guessing the last one is how a rig ends up
//! with a speed fader that jumps from half to quadruple with nothing in between.
//!
//! The same catalogue serves the patch editor in the client, which needs the
//! footprint and the channel list to lay a patch out.
//!
//! Published definitions never change, so this is a rendering of
//! them rather than a second source: a personality with the same type and
//! version says the same thing on every build that has it at all.

use ovs_types::config::fixtures::{
    ChannelEncoding, ParameterSlot, MASTER_SCALE_STEPS, MAX_ADDRESSABLE_SLOT, PUBLISHED,
};
use serde::{Deserialize, Serialize};

/// What kind of thing a personality can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PersonalityTarget {
    /// The one show-wide master.
    Master,
    /// One layer.
    Layer,
    /// One effect instance.
    ///
    /// A personality generated from the effect's own parameter declarations
    /// rather than published by this crate — an effect is a plugin and brings
    /// its own channels (owner decision Q19).
    Effect,
    /// One generator instance. The same, for the other kind of module.
    Generator,
}

/// One band of a stepped channel.
///
/// The part a console programmer cannot work out from a channel list, and the
/// part a client's patch editor needs to draw the fader's labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChannelBand {
    /// Lowest DMX value in this band.
    pub from: u8,
    /// Highest DMX value in this band.
    pub to: u8,
    /// What this band selects, spelled as the rest of the API spells it.
    pub value: String,
}

/// How a channel's bytes become a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "encoding", rename_all = "snake_case")]
pub enum ChannelEncodingInfo {
    /// One channel across a range.
    Ranged8 {
        /// The value at `0`.
        min: f64,
        /// The value at `255`.
        max: f64,
    },
    /// Two channels, coarse then fine, across a range.
    Ranged16 {
        /// The value at `0`.
        min: f64,
        /// The value at `65535`.
        max: f64,
    },
    /// One channel as a whole number.
    Index8,
    /// One channel as a switch.
    Switch {
        /// The lowest value that counts as on.
        threshold: u8,
    },
    /// One channel divided into bands.
    Stepped {
        /// The bands, lowest first.
        bands: Vec<ChannelBand>,
    },
}

/// One channel of a personality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PersonalityChannel {
    /// One-based offset within the fixture.
    pub offset: u16,
    /// How many channels it occupies.
    pub width: u16,
    /// What a console's channel list shows.
    pub name: String,
    /// The stable machine name of what it controls, for example `layer_opacity`.
    pub control: String,
    /// Whether it fires on a rising edge rather than holding a value.
    pub momentary: bool,
    /// How its bytes become a value.
    pub encoding: ChannelEncodingInfo,
    /// What to send to put this channel at rest.
    ///
    /// Raw, in the units the channel is sent in: the whole sixteen-bit number
    /// for a wide channel, one byte for a narrow one. A console homes a fixture
    /// by sending numbers rather than by understanding them.
    ///
    /// Zero still means zero everywhere else (owner decision Q15). This is not
    /// a neutral value the server substitutes for one; it is what a desk should
    /// put in its own home or default preset so a freshly patched layer is
    /// visible and its normal size instead of a wall of zeros.
    pub home: u16,
}

/// One published personality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Personality {
    /// Stable type identifier, for example `ovs.master`.
    pub type_id: String,
    /// Version. A new control is a new version, never a changed one.
    pub version: u32,
    /// What a console's personality list shows.
    pub name: String,
    /// How many channels an instance occupies.
    pub footprint: u16,
    /// What a patch entry using it must point at.
    pub target: PersonalityTarget,
    /// Its channels, in address order.
    pub channels: Vec<PersonalityChannel>,
}

/// Every personality this build publishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureCatalogueResponse {
    /// The personalities, in the order this build publishes them.
    pub fixtures: Vec<Personality>,
}

/// Returns the catalogue for this build.
#[must_use]
pub fn catalogue() -> FixtureCatalogueResponse {
    // The two this crate publishes, then every module that declares channels of
    // its own. One list, because a patch editor and a console programmer do not
    // care which of the two a personality came from.
    let published = PUBLISHED.iter().chain(
        ovs_types::config::EFFECTS
            .iter()
            .filter_map(ovs_types::config::EffectManifest::personality)
            .chain(
                ovs_types::config::GENERATORS
                    .iter()
                    .filter_map(ovs_types::config::GeneratorManifest::personality),
            ),
    );

    FixtureCatalogueResponse {
        fixtures: published
            .map(|published| {
                let mut channels: Vec<PersonalityChannel> = published
                    .channels
                    .iter()
                    .map(|channel| PersonalityChannel {
                        offset: channel.offset,
                        width: channel.encoding.width(),
                        name: channel.name.to_owned(),
                        control: channel.parameter.name().to_owned(),
                        momentary: momentary(channel.parameter),
                        encoding: encoding(channel.encoding, &choices_for(published, channel)),
                        home: channel.home,
                    })
                    .collect();
                // Address order, because that is the order somebody typing a
                // personality into a desk works in.
                channels.sort_by_key(|channel| channel.offset);

                Personality {
                    type_id: published.type_id.to_owned(),
                    version: published.version,
                    name: published.name.to_owned(),
                    footprint: published.footprint,
                    target: match published.channels.first().map(|channel| channel.parameter) {
                        Some(ParameterSlot::Layer(_)) => PersonalityTarget::Layer,
                        Some(ParameterSlot::Effect(_)) => PersonalityTarget::Effect,
                        Some(ParameterSlot::Generator(_)) => PersonalityTarget::Generator,
                        _ => PersonalityTarget::Master,
                    },
                    channels,
                }
            })
            .collect(),
    }
}

/// Renders the catalogue as the document somebody programmes a desk from.
///
/// Committed to `reference/FIXTURES.md` and checked by a golden test, so the
/// document cannot drift away from the personalities the server actually
/// publishes — a printed channel list that disagrees with the machine is worse
/// than none, because it is believed.
#[must_use]
pub fn markdown() -> String {
    let mut out = String::new();
    out.push_str(
        "# DMX fixture personalities\n\
         \n\
         Generated from the personalities this build publishes. Do not edit by hand:\n\
         run `cargo run -p ovs-proto --bin ovs-fixture-doc` and commit the result.\n\
         \n\
         A published personality never changes. A console has the channel layout\n\
         physically programmed into its show file, so a new control is a new version\n\
         rather than a changed one, and a server refuses a patch naming a version it\n\
         does not have rather than serving the nearest one.\n\
         \n\
         The server receives only. It answers no ArtPoll, appears in no console's node\n\
         list, and transmits nothing.\n\
         \n\
         A patched fixture owns every channel of its personality, including the ones\n\
         nobody moved: faders left at zero are values, not silence. Park the patch entry\n\
         to hand a parameter back to the client.\n\n\
         The **Home** column is what to put in a desk's home or default preset. It is\n\
         not something this server substitutes for a zero it received — zero is a real\n\
         value and stays one — it is what a freshly patched fixture should be sent so\n\
         that a layer is visible, its normal size, upright and on black rather than a\n\
         wall of zeros.\n\n\
         ## Reading a continuous channel\n\n\
         Each personality below carries a **Where the values sit** table for its\n\
         continuous channels: the DMX number for the bottom of the range, the middle,\n\
         the top, and whatever home is, with what each one means.\n\n\
         For anything between them the channel is linear end to end, so\n\n\
         ```\n\
         DMX   = (value - min) / (max - min) * full\n\
         value = min + (max - min) * DMX / full\n\
         ```\n\n\
         where `full` is **255** for a one-channel control and **65535** for a\n\
         two-channel one, and `min` and `max` are the two ends in the Values column.\n\
         A scale running 0 to 10 therefore sits at 1 when the channel is at a tenth of\n\
         its travel, which is 6554 of 65535 — not at the middle, which is 5.\n\n\
         A two-channel control is programmed as two bytes, coarse then fine, and the\n\
         landmark tables print both: `32768 (128/0)` means coarse 128, fine 0. Coarse\n\
         alone moves in steps of 256, which is a quarter of a per cent of the travel —\n\
         visible on a slow position move, which is why these are sixteen bits.\n\n",
    );

    for personality in catalogue().fixtures {
        out.push_str(&format!(
            "## {} — `{}` v{}\n\n",
            personality.name, personality.type_id, personality.version
        ));
        out.push_str(&format!(
            "Footprint: **{} channels**. Patch to: **{}**.\n\n",
            personality.footprint,
            match personality.target {
                PersonalityTarget::Master => "the show master (one per show)",
                PersonalityTarget::Layer => "one layer",
                PersonalityTarget::Effect => "one effect instance in a chain",
                PersonalityTarget::Generator => "one generator instance in a slot",
            }
        ));

        out.push_str("| Channel | Name | Control | Values | Home |\n");
        out.push_str("|---|---|---|---|---|\n");
        for channel in &personality.channels {
            let range = if channel.width > 1 {
                format!("{}–{}", channel.offset, channel.offset + channel.width - 1)
            } else {
                channel.offset.to_string()
            };
            out.push_str(&format!(
                "| {range} | {} | `{}` | {} | {} |\n",
                channel.name,
                channel.control,
                summary(channel),
                channel.home
            ));
        }
        out.push('\n');

        // Where the numbers actually sit on a continuous channel.
        //
        // The owner, 2026-10-16: "außerdem brauche ich von jedem DMX Kabel die
        // Werte für wo Mitte ist, welche Scale wo liegt etc." The table above
        // gives the two ends, which is enough to describe a channel and not
        // enough to programme one: somebody at a desk needs the number to type
        // for the middle, and the number that gives a scale of one.
        //
        // Endpoints, the midpoint and home, and nothing invented between them.
        // Which values are "round" depends on the parameter, and guessing would
        // put numbers in this document that no part of this build agrees with.
        // The formula is in the preamble for everything else.
        let landmarks: Vec<&PersonalityChannel> = personality
            .channels
            .iter()
            .filter(|channel| {
                matches!(
                    channel.encoding,
                    ChannelEncodingInfo::Ranged8 { .. } | ChannelEncodingInfo::Ranged16 { .. }
                )
            })
            .collect();
        if !landmarks.is_empty() {
            out.push_str("### Where the values sit\n\n");
            out.push_str("| Channel | Name | Bottom | Middle | Top | Home |\n");
            out.push_str("|---|---|---|---|---|---|\n");
            for channel in landmarks {
                let (min, max) = match channel.encoding {
                    ChannelEncodingInfo::Ranged8 { min, max }
                    | ChannelEncodingInfo::Ranged16 { min, max } => (min, max),
                    _ => continue,
                };
                let top = if channel.width > 1 { 65_535_u32 } else { 255 };
                let range = if channel.width > 1 {
                    format!("{}–{}", channel.offset, channel.offset + channel.width - 1)
                } else {
                    channel.offset.to_string()
                };
                out.push_str(&format!(
                    "| {range} | {} | {} | {} | {} | {} |\n",
                    channel.name,
                    landmark(0, top, min, max, channel.width),
                    // `(top + 1) / 2`, not `top / 2`. A channel runs 0 to 255
                    // or 0 to 65535, so its exact middle is a half and cannot
                    // be sent. Rounding up is the one that matters: on the
                    // sixteen-bit transforms it is 32768, which lands a
                    // symmetric range exactly on zero — the number somebody is
                    // looking for when they ask where the middle is. Rounding
                    // down gives 32767 and a position of minus nothing.
                    landmark(top.div_ceil(2), top, min, max, channel.width),
                    landmark(top, top, min, max, channel.width),
                    landmark(u32::from(channel.home), top, min, max, channel.width),
                ));
            }
            out.push('\n');
        }

        for channel in &personality.channels {
            let ChannelEncodingInfo::Stepped { bands } = &channel.encoding else {
                continue;
            };
            out.push_str(&format!(
                "### {} (channel {})\n\n| DMX | Step |\n|---|---|\n",
                channel.name, channel.offset
            ));
            for band in bands {
                out.push_str(&format!(
                    "| {}–{} | `{}` |\n",
                    band.from, band.to, band.value
                ));
            }
            out.push('\n');
        }
    }

    out
}

/// One cell of the landmark table: the DMX number, and what it means.
///
/// A sixteen-bit channel is shown as its whole number and as the two bytes a
/// desk is actually programmed with, because that is the pair somebody types
/// and getting the fine byte wrong is a fault nobody sees until the value is
/// nearly right and will not sit still.
///
/// The value is printed to three decimals and then trimmed, so a scale of one
/// reads `1` and a lift of minus a third does not pretend to be exact.
fn landmark(dmx: u32, top: u32, min: f64, max: f64, width: u16) -> String {
    let at = if top == 0 {
        min
    } else {
        min + (max - min) * (f64::from(dmx) / f64::from(top))
    };
    let mut shown = format!("{at:.3}");
    if shown.contains('.') {
        shown = shown.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    // A value that rounded to nothing from below prints as `-0`, which reads
    // like a direction and is not one.
    if shown == "-0" {
        shown = "0".to_owned();
    }
    if width > 1 {
        let coarse = dmx / 256;
        let fine = dmx % 256;
        format!("`{dmx}` ({coarse}/{fine}) → {shown}")
    } else {
        format!("`{dmx}` → {shown}")
    }
}

/// Describes one channel's values in a table cell.
fn summary(channel: &PersonalityChannel) -> String {
    match &channel.encoding {
        ChannelEncodingInfo::Ranged8 { min, max } | ChannelEncodingInfo::Ranged16 { min, max } => {
            let top = if channel.width > 1 { 65_535 } else { 255 };
            format!("{min} at 0, {max} at {top}")
        }
        ChannelEncodingInfo::Index8 => "the number itself; 0 is black".to_owned(),
        ChannelEncodingInfo::Switch { threshold } => {
            if channel.momentary {
                format!("fires once when it reaches {threshold}; held is not repeated")
            } else {
                format!("on at {threshold} and above")
            }
        }
        ChannelEncodingInfo::Stepped { bands } => {
            format!("{} steps, listed below", bands.len())
        }
    }
}

/// Returns whether a channel fires rather than holds.
fn momentary(slot: ParameterSlot) -> bool {
    match slot {
        ParameterSlot::Master(parameter) => parameter.is_momentary(),
        // A layer control's name is the same bound or unbound, so the one that
        // fires can be recognised without inventing a layer to bind it to.
        ParameterSlot::Layer(parameter) => parameter.name() == "layer_trigger",
        // A plugin's parameter is a value. Nothing in a manifest fires: a
        // `Toggle` is a switch somebody leaves where they put it, and a
        // momentary control would be a kind of parameter that does not exist.
        ParameterSlot::Effect(_) | ParameterSlot::Generator(_) => false,
    }
}

/// The choice list a channel divides itself into, when it is a choice channel
/// of a module this build knows.
///
/// Empty for everything else. The vocabulary lives in the manifest, and this is
/// where a personality is put back beside the manifest it came from.
fn choices_for(
    personality: &'static ovs_types::config::FixtureDefinition,
    channel: &ovs_types::config::ChannelDefinition,
) -> Vec<&'static str> {
    let key = match channel.parameter {
        ParameterSlot::Effect(parameter) | ParameterSlot::Generator(parameter) => parameter.key,
        _ => return Vec::new(),
    };
    let parameters = ovs_types::config::effects::manifest(personality.type_id, personality.version)
        .map(|manifest| manifest.parameters)
        .or_else(|| {
            ovs_types::config::generators::manifest(personality.type_id, personality.version)
                .map(|manifest| manifest.parameters)
        });
    let Some(parameters) = parameters else {
        return Vec::new();
    };
    parameters
        .iter()
        .find(|parameter| parameter.key == key)
        .and_then(|parameter| match parameter.kind {
            ovs_types::config::ParameterKind::Choice { choices, .. } => {
                Some(choices.iter().map(|choice| choice.id).collect())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// Renders one encoding, working out the bands where there are any.
///
/// `choices` is the vocabulary a [`ChannelEncoding::Choices`] channel divides
/// itself into, in band order, and is empty for every other encoding. It comes
/// from the caller because a fixture definition holds no plugin's words: the
/// encoding knows there are three bands and only the manifest knows that the
/// third one is a triangle.
fn encoding(encoding: ChannelEncoding, choices: &[&str]) -> ChannelEncodingInfo {
    match encoding {
        ChannelEncoding::Ranged8 { min, max } => ChannelEncodingInfo::Ranged8 { min, max },
        ChannelEncoding::Ranged16 { min, max } => ChannelEncodingInfo::Ranged16 { min, max },
        ChannelEncoding::Index8 => ChannelEncodingInfo::Index8,
        ChannelEncoding::Switch { threshold } => ChannelEncodingInfo::Switch { threshold },
        ChannelEncoding::ScaleLadder => ChannelEncodingInfo::Stepped {
            bands: bands(&MASTER_SCALE_STEPS),
        },
        // Not `bands`: the slots do not divide the channel evenly. Black owns
        // the bottom four values and every slot after that owns three, so the
        // table is built from the same arithmetic the decoder uses rather than
        // from an even division that would disagree with it.
        ChannelEncoding::SlotBands => ChannelEncodingInfo::Stepped {
            bands: slot_bands(),
        },
        ChannelEncoding::Choices { count } => ChannelEncodingInfo::Stepped {
            // The names where the caller has them, and the band numbers where
            // it has not — which is a personality that was built without its
            // manifest and should not be possible, but is better read as "0, 1,
            // 2" than as an empty table.
            bands: if choices.len() == usize::from(count) {
                bands(choices)
            } else {
                bands(&(0..count).collect::<Vec<_>>())
            },
        },
    }
}

/// The DMX range each slot occupies, Black first.
///
/// Eighty-five rows, which is a long table and the right one: a console
/// programming a slot needs the numbers, and working them out from a sentence
/// is how a desk ends up one value out on slot 40.
fn slot_bands() -> Vec<ChannelBand> {
    let mut out = vec![ChannelBand {
        from: 0,
        to: 3,
        value: "black".to_owned(),
    }];
    for slot in 1..=MAX_ADDRESSABLE_SLOT {
        let from = 4 + (slot as u32 - 1) * 3;
        out.push(ChannelBand {
            from: from as u8,
            to: (from + 2).min(255) as u8,
            value: slot.to_string(),
        });
    }
    out
}

/// Returns the DMX range each step occupies.
///
/// The steps divide the channel evenly, so this is arithmetic rather than a
/// table — and doing it here rather than by hand is what stops the catalogue
/// disagreeing with the decoder about where a band starts.
fn bands<T: Serialize>(steps: &[T]) -> Vec<ChannelBand> {
    let count = steps.len();
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let from = edge(index, count);
            let to = edge(index + 1, count).saturating_sub(1);
            ChannelBand {
                from: from as u8,
                to: to.min(255) as u8,
                value: label(step),
            }
        })
        .collect()
}

/// Returns the first DMX value belonging to band `index`.
///
/// The decoder picks a band with `value * count / 256`, so a band starts at the
/// smallest value for which that quotient reaches it.
fn edge(index: usize, count: usize) -> usize {
    index.saturating_mul(256).div_ceil(count)
}

/// Returns how the rest of the API spells a step.
fn label<T: Serialize>(step: &T) -> String {
    match serde_json::to_value(step) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        // A step that cannot be serialised would be a programming error, and an
        // empty label is a less useful lie than saying so.
        Err(error) => format!("<unserialisable: {error}>"),
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::fixtures::{definition, LAYER_V1, MASTER_V1};

    use super::*;

    #[test]
    fn the_document_lists_every_personality_and_every_channel() {
        // What somebody types into a desk. A channel missing from it is a
        // control an operator never finds.
        let document = markdown();
        for personality in catalogue().fixtures {
            assert!(
                document.contains(&personality.type_id),
                "{} is missing",
                personality.type_id
            );
            for channel in personality.channels {
                assert!(
                    document.contains(&channel.name),
                    "{} is missing",
                    channel.name
                );
            }
        }
    }

    #[test]
    fn the_document_gives_the_bands_of_every_stepped_channel() {
        // The thing that cannot be worked out from a channel list, and the
        // reason the document exists at all.
        let document = markdown();

        for step in MASTER_SCALE_STEPS {
            assert!(
                document.contains(&format!("| `{}` |", label(&step))),
                "the master scale ladder is missing {step}"
            );
        }

        // And the slots, which are the longest table of the two and the one a
        // console programmer most needs the numbers for.
        for expected in ["| `black` |", "| `1` |", "| `84` |"] {
            assert!(
                document.contains(expected),
                "the slot table is missing {expected}"
            );
        }
    }

    #[test]
    fn the_document_says_how_it_is_regenerated() {
        // A generated file somebody edits by hand is a file that silently stops
        // matching the server.
        assert!(markdown().contains("ovs-fixture-doc"));
    }

    #[test]
    fn the_catalogue_describes_every_personality_this_build_has() {
        let catalogue = catalogue();
        // The two this crate publishes, and one per module that declares
        // channels of its own.
        assert!(catalogue.fixtures.len() > PUBLISHED.len());

        for personality in &catalogue.fixtures {
            let published = definition(&personality.type_id, personality.version)
                .expect("the catalogue only describes what this build has");
            assert_eq!(personality.footprint, published.footprint);
            assert_eq!(personality.channels.len(), published.channels.len());
        }
    }

    #[test]
    fn channels_come_in_the_order_somebody_types_them_into_a_desk() {
        for personality in catalogue().fixtures {
            let offsets: Vec<u16> = personality
                .channels
                .iter()
                .map(|channel| channel.offset)
                .collect();
            let mut sorted = offsets.clone();
            sorted.sort_unstable();
            assert_eq!(offsets, sorted, "{}", personality.type_id);
        }
    }

    #[test]
    fn the_channels_of_a_personality_exactly_fill_its_footprint() {
        // A gap would be a channel nobody can use; an overlap would be two
        // controls on one fader. Both are the kind of thing that is obvious
        // here and invisible on a desk.
        for personality in catalogue().fixtures {
            let mut covered = vec![false; personality.footprint as usize];
            for channel in &personality.channels {
                for offset in channel.offset..channel.offset + channel.width {
                    let slot = &mut covered[usize::from(offset) - 1];
                    assert!(
                        !*slot,
                        "{} channel {offset} is used twice",
                        personality.type_id
                    );
                    *slot = true;
                }
            }
            assert!(
                covered.iter().all(|used| *used),
                "{} leaves a channel unused",
                personality.type_id
            );
        }
    }

    #[test]
    fn a_stepped_channel_says_which_values_mean_which_step() {
        // The part a console programmer cannot work out from a channel list.
        let layer = catalogue()
            .fixtures
            .into_iter()
            .find(|personality| personality.type_id == LAYER_V1.type_id)
            .expect("the layer personality");
        let slot = layer
            .channels
            .iter()
            .find(|channel| channel.control == "layer_slot")
            .expect("a slot channel");

        let ChannelEncodingInfo::Stepped { bands } = &slot.encoding else {
            panic!("the slot channel is stepped");
        };

        // Black plus eighty-four slots, and the numbers rather than a sentence:
        // working them out by hand is how a desk ends up one value out.
        assert_eq!(bands.len(), 1 + MAX_ADDRESSABLE_SLOT as usize);
        assert_eq!((bands[0].from, bands[0].to), (0, 3), "Black");
        assert_eq!((bands[1].from, bands[1].to), (4, 6), "slot 1");
        assert_eq!((bands[2].from, bands[2].to), (7, 9), "slot 2");
        assert_eq!(
            (bands[bands.len() - 1].from, bands[bands.len() - 1].to),
            (253, 255),
            "slot 84"
        );
        assert_eq!(bands[bands.len() - 1].value, "84");
    }

    #[test]
    fn the_bands_are_the_ones_the_decoder_actually_uses() {
        // Worked out from the same arithmetic rather than written down twice: a
        // catalogue that disagreed with the decoder would send somebody to
        // programme a desk with the wrong boundaries.
        use ovs_types::config::ChannelValue;

        // The master's scale ladder, which since the layer's own ladder was
        // removed is the only stepped speed channel there is.
        let ChannelEncodingInfo::Stepped { bands } = encoding(ChannelEncoding::ScaleLadder, &[])
        else {
            panic!("stepped");
        };

        for band in &bands {
            for value in [band.from, band.to] {
                let decoded = ChannelEncoding::ScaleLadder
                    .decode(&[value], 1)
                    .expect("one channel is enough");
                let ChannelValue::Number(scale) = decoded else {
                    panic!("a scale step");
                };
                assert_eq!(label(&scale), band.value, "DMX {value}");
            }
        }
    }

    #[test]
    fn bands_cover_every_value_exactly_once() {
        let ChannelEncodingInfo::Stepped { bands } = encoding(ChannelEncoding::ScaleLadder, &[])
        else {
            panic!("stepped");
        };
        assert_eq!(bands[0].from, 0);
        assert_eq!(bands[bands.len() - 1].to, 255);
        for pair in bands.windows(2) {
            assert_eq!(
                u16::from(pair[0].to) + 1,
                u16::from(pair[1].from),
                "a gap or an overlap between bands"
            );
        }
    }

    #[test]
    fn a_momentary_channel_is_marked_as_one() {
        // A patch editor that drew the trigger as a fader would be showing
        // something an operator cannot use.
        let catalogue = catalogue();
        let master = catalogue
            .fixtures
            .iter()
            .find(|personality| personality.type_id == MASTER_V1.type_id)
            .expect("the master personality");

        let tap = master
            .channels
            .iter()
            .find(|channel| channel.control == "master_tap_tempo")
            .expect("a tap channel");
        assert!(tap.momentary);

        let dimmer = master
            .channels
            .iter()
            .find(|channel| channel.control == "master_dimmer")
            .expect("a dimmer channel");
        assert!(!dimmer.momentary);
    }

    #[test]
    fn a_personality_says_what_it_can_be_patched_to() {
        // And a generated one says so too, which is what lets a patch editor
        // offer an effect's personality against that effect rather than against
        // a layer.
        let catalogue = catalogue();
        for personality in &catalogue.fixtures {
            let expected = if personality.type_id == MASTER_V1.type_id {
                PersonalityTarget::Master
            } else if personality.type_id == LAYER_V1.type_id {
                PersonalityTarget::Layer
            } else if ovs_types::config::effects::manifest(
                &personality.type_id,
                personality.version,
            )
            .is_some()
            {
                PersonalityTarget::Effect
            } else {
                PersonalityTarget::Generator
            };
            assert_eq!(personality.target, expected, "{}", personality.type_id);
        }
    }

    #[test]
    fn a_module_that_declares_nothing_is_not_in_the_catalogue() {
        // A personality with no channels would be patchable, would occupy an
        // address, and would do nothing. Our clock declares only colours, so
        // something with no DMX at all is the honest check: the countdown's
        // duration and the text generator's words are `None`, and neither
        // module disappears because it has other parameters that are not.
        let catalogue = catalogue();
        assert!(catalogue
            .fixtures
            .iter()
            .all(|personality| personality.footprint > 0));
        assert!(catalogue
            .fixtures
            .iter()
            .all(|personality| !personality.channels.is_empty()));
    }
}
