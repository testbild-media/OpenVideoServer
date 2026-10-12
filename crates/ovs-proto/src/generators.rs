//! The generator catalogue: what a client can add to a slot.
//!
//! The same shape of thing as the fixture catalogue and for the same reason: a
//! client cannot read a Rust constant, and an operator picking a generator needs
//! its parameters, their types, their ranges and where they start.
//!
//! Manifests are immutable once published, so this is a rendering of them rather
//! than a second source of truth.

use ovs_types::config::generators::{PUBLISHED, RETIRED};
use serde::{Deserialize, Serialize};

use crate::parameters::ModuleParameter;

pub use crate::parameters::ParameterType;

/// One published generator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Generator {
    /// Stable type identifier, for example `ovs.solid_colour`.
    pub type_id: String,
    /// Version. A new parameter is a new version, never a changed one.
    pub version: u32,
    /// What a source list shows.
    pub name: String,
    /// One line describing what it draws.
    pub description: String,
    /// Its parameters, in the order the manifest declares them.
    pub parameters: Vec<ModuleParameter>,
}

/// Every generator this build publishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GeneratorCatalogueResponse {
    /// The generators.
    pub generators: Vec<Generator>,
}

/// Whether this is the newest published version of its type.
///
/// A retired type is left out before this is asked; see `RETIRED`.
///
/// What a catalogue answers is "what can I put in a slot now", and the answer
/// is one entry a type. An older version is still served to a show that names
/// it: that is `manifest(type_id, version)`, and it is why both are published.
fn is_newest(type_id: &str, version: u32) -> bool {
    !PUBLISHED
        .iter()
        .any(|other| other.type_id == type_id && other.version > version)
}

/// Returns the catalogue for this build.
#[must_use]
pub fn catalogue() -> GeneratorCatalogueResponse {
    GeneratorCatalogueResponse {
        generators: PUBLISHED
            .iter()
            .filter(|manifest| !RETIRED.contains(&manifest.type_id))
            .filter(|manifest| is_newest(manifest.type_id, manifest.version))
            .map(|manifest| Generator {
                type_id: manifest.type_id.to_owned(),
                version: manifest.version,
                name: manifest.name.to_owned(),
                description: manifest.description.to_owned(),
                parameters: manifest
                    .parameters
                    .iter()
                    .map(ModuleParameter::from)
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovs_types::config::generators::manifest;

    #[test]
    fn the_catalogue_offers_the_newest_of_every_published_type() {
        let catalogue = catalogue();

        // One entry a type that is still offered: the newest version of it,
        // and nothing that has been retired.
        let mut types: Vec<&str> = PUBLISHED
            .iter()
            .map(|manifest| manifest.type_id)
            .filter(|type_id| !RETIRED.contains(type_id))
            .collect();
        types.sort_unstable();
        types.dedup();
        assert_eq!(catalogue.generators.len(), types.len());

        for rendered in &catalogue.generators {
            let newest = PUBLISHED
                .iter()
                .filter(|manifest| manifest.type_id == rendered.type_id)
                .map(|manifest| manifest.version)
                .max()
                .expect("a version of a type that is in the catalogue");
            assert_eq!(rendered.version, newest, "{}", rendered.type_id);

            let manifest = manifest(&rendered.type_id, rendered.version).expect("published");
            assert_eq!(rendered.parameters.len(), manifest.parameters.len());
        }
    }

    #[test]
    fn a_retired_generator_is_still_served_to_a_show_that_has_one() {
        // The whole of why these are retired rather than deleted: a show saved
        // with a clock on a layer opens with the clock it was saved with, and
        // nobody finds a hole where a layer used to be.
        for type_id in RETIRED {
            assert!(
                manifest(type_id, 1).is_some(),
                "{type_id} is retired, which is not the same as gone"
            );
            assert!(
                !catalogue()
                    .generators
                    .iter()
                    .any(|generator| generator.type_id == type_id),
                "{type_id} is offered after being retired"
            );
        }
    }

    #[test]
    fn an_older_version_is_still_served_to_a_show_that_names_it() {
        // The other half of offering only the newest: a show saved against v1
        // opens drawing what it was left drawing, rather than losing its layer
        // to a build that no longer admits the version it names.
        assert!(manifest("ovs.test_pattern", 1).is_some());
        assert!(manifest("ovs.test_pattern", 2).is_some());
        assert!(
            !catalogue()
                .generators
                .iter()
                .any(|generator| generator.type_id == "ovs.test_pattern" && generator.version == 1),
            "v1 is served but not offered"
        );
    }

    #[test]
    fn a_parameter_carries_what_a_client_needs_to_draw_a_control() {
        // A number without its range is a text box; with one it is a fader.
        let catalogue = catalogue();
        let strobe = catalogue
            .generators
            .iter()
            .find(|generator| generator.type_id == "ovs.strobe")
            .expect("the strobe");

        let rate = strobe
            .parameters
            .iter()
            .find(|parameter| parameter.key == "hz")
            .expect("a rate");

        match rate.parameter_type {
            ParameterType::Number { min, max, default } => {
                assert!(min < default && default < max);
            }
            ref other => panic!("expected a number, got {other:?}"),
        }
    }

    #[test]
    fn a_colour_and_a_switch_reach_a_client_with_where_they_start() {
        // A colour well with no colour in it, or a switch with no position, is
        // the same problem as a fader with no ends: the client has nothing to
        // draw until the operator touches it, and an untouched show would
        // then look nothing like what the wall is showing.
        let catalogue = catalogue();
        let parameter = |type_id: &str, key: &str| {
            catalogue
                .generators
                .iter()
                .find(|generator| generator.type_id == type_id)
                .unwrap_or_else(|| panic!("{type_id} is published"))
                .parameters
                .iter()
                .find(|parameter| parameter.key == key)
                .unwrap_or_else(|| panic!("{type_id} declares {key}"))
                .parameter_type
                .clone()
        };

        assert_eq!(
            parameter("ovs.gradient", "colour_a"),
            ParameterType::Colour {
                default: [1.0, 1.0, 1.0, 1.0]
            }
        );
        assert_eq!(
            parameter("ovs.noise", "coloured"),
            ParameterType::Toggle { default: false }
        );
    }

    #[test]
    fn the_generators_named_in_the_specification_are_the_ones_offered() {
        // A client's source list is this catalogue and nothing else, so a
        // generator missing here is a generator no operator can reach even
        // though the renderer can draw it.
        let offered: std::collections::BTreeSet<String> = catalogue()
            .generators
            .into_iter()
            .map(|generator| generator.type_id)
            .collect();

        for type_id in [
            "ovs.solid_colour",
            "ovs.test_pattern",
            "ovs.gradient",
            "ovs.grid",
            "ovs.noise",
        ] {
            assert!(offered.contains(type_id), "{type_id} is not offered");
        }
    }
}
