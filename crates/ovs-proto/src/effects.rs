//! The effect catalogue: what a client can add to a slot or a layer.
//!
//! The same shape as the generator catalogue, and for the same reason: a client
//! cannot read a Rust constant, and an operator adding an effect needs its
//! parameters, their types, their ranges and where they start.
//!
//! A rendering of the manifests rather than a second source of truth, and one
//! entry a type: there is one of each.

use ovs_types::config::effects::{PUBLISHED, RETIRED};
use serde::{Deserialize, Serialize};

use crate::parameters::ModuleParameter;

/// One published effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Effect {
    /// Stable type identifier, for example `ovs.colour_correct`.
    pub type_id: String,
    /// Always one: there is one effect of each type. Still served because a
    /// show carries the number.
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
            .filter(|manifest| !RETIRED.contains(&manifest.type_id))
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
    use ovs_types::config::effects::manifest;

    #[test]
    fn the_catalogue_offers_one_of_every_effect_still_published() {
        // One entry a type, and nothing that has been retired. There is
        // only one of each to offer (owner, 2026-10-18: "Es gibt kein
        // Invert V2.. nirgendwo").
        let catalogue = catalogue();
        let offered: Vec<&str> = PUBLISHED
            .iter()
            .map(|manifest| manifest.type_id)
            .filter(|type_id| !RETIRED.contains(type_id))
            .collect();
        assert_eq!(
            catalogue
                .effects
                .iter()
                .map(|effect| effect.type_id.as_str())
                .collect::<Vec<_>>(),
            offered
        );
        assert!(catalogue.effects.iter().all(|effect| effect.version == 1));
    }

    #[test]
    fn a_retired_effect_is_still_served_to_a_show_that_has_one() {
        // Retired, not deleted: a show with a Tint in a chain opens with it.
        for type_id in RETIRED {
            assert!(manifest(type_id).is_some(), "{type_id} is still served");
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
