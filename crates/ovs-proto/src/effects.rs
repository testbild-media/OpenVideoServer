//! The effect catalogue: what a client can add to a slot or a layer.
//!
//! The same shape as the generator catalogue, and for the same reason: a client
//! cannot read a Rust constant, and an operator adding an effect needs its
//! parameters, their types, their ranges and where they start.
//!
//! Manifests are immutable once published, so this is a rendering of them rather
//! than a second source of truth.

use ovs_types::config::effects::PUBLISHED;
use serde::{Deserialize, Serialize};

use crate::parameters::ModuleParameter;

/// One published effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Effect {
    /// Stable type identifier, for example `ovs.colour_correct`.
    pub type_id: String,
    /// Version. A new parameter is a new version, never a changed one.
    pub version: u32,
    /// What an effect list shows.
    pub name: String,
    /// One line describing what it does.
    pub description: String,
    /// Its parameters, in the order the manifest declares them.
    pub parameters: Vec<ModuleParameter>,
}

/// Every effect this build publishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EffectCatalogueResponse {
    /// The effects.
    pub effects: Vec<Effect>,
}

/// Returns the catalogue for this build.
#[must_use]
pub fn catalogue() -> EffectCatalogueResponse {
    EffectCatalogueResponse {
        effects: PUBLISHED
            .iter()
            .map(|manifest| Effect {
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
    fn the_catalogue_describes_every_published_effect() {
        let catalogue = catalogue();
        assert_eq!(catalogue.effects.len(), PUBLISHED.len());

        for (rendered, manifest) in catalogue.effects.iter().zip(PUBLISHED.iter()) {
            assert_eq!(rendered.type_id, manifest.type_id);
            assert_eq!(rendered.version, manifest.version);
            assert_eq!(rendered.parameters.len(), manifest.parameters.len());
        }
    }

    #[test]
    fn an_effect_a_show_names_can_be_found_in_the_catalogue() {
        // What a client does with this: an operator opens a show, sees
        // `ovs.blur` in a chain, and the client has to draw its controls.
        let catalogue = catalogue();
        let blur = catalogue
            .effects
            .iter()
            .find(|effect| effect.type_id == "ovs.blur" && effect.version == 1)
            .expect("this build publishes the blur");

        assert!(blur
            .parameters
            .iter()
            .any(|parameter| parameter.key == "radius_px"));
    }
}
