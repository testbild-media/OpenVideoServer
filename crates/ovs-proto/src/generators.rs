//! The generator catalogue: what a client can add to a slot.
//!
//! The same shape of thing as the fixture catalogue and for the same reason: a
//! client cannot read a Rust constant, and an operator picking a generator needs
//! its parameters, their types, their ranges and where they start.
//!
//! Manifests are immutable once published, so this is a rendering of them rather
//! than a second source of truth.

use ovs_types::config::generators::PUBLISHED;
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

/// Returns the catalogue for this build.
#[must_use]
pub fn catalogue() -> GeneratorCatalogueResponse {
    GeneratorCatalogueResponse {
        generators: PUBLISHED
            .iter()
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

    #[test]
    fn the_catalogue_describes_every_published_generator() {
        let catalogue = catalogue();
        assert_eq!(catalogue.generators.len(), PUBLISHED.len());

        for (rendered, manifest) in catalogue.generators.iter().zip(PUBLISHED.iter()) {
            assert_eq!(rendered.type_id, manifest.type_id);
            assert_eq!(rendered.version, manifest.version);
            assert_eq!(rendered.parameters.len(), manifest.parameters.len());
        }
    }

    #[test]
    fn a_parameter_carries_what_a_client_needs_to_draw_a_control() {
        // A number without its range is a text box; with one it is a fader.
        let catalogue = catalogue();
        let pattern = catalogue
            .generators
            .iter()
            .find(|generator| generator.type_id == "ovs.test_pattern")
            .expect("the test pattern");

        let grid = pattern
            .parameters
            .iter()
            .find(|parameter| parameter.key == "grid_px")
            .expect("a grid size");

        match grid.parameter_type {
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
