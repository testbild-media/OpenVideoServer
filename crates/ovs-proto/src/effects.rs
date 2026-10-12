//! The effect catalogue: what a client can add to a slot or a layer.
//!
//! The same shape as the generator catalogue, and for the same reason: a client
//! cannot read a Rust constant, and an operator adding an effect needs its
//! parameters, their types, their ranges and where they start.
//!
//! Manifests are immutable once published, so this is a rendering of them rather
//! than a second source of truth.

use ovs_types::config::effects::{PUBLISHED, RETIRED};
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
pub fn catalogue() -> EffectCatalogueResponse {
    EffectCatalogueResponse {
        effects: PUBLISHED
            .iter()
            .filter(|manifest| !RETIRED.contains(&manifest.type_id))
            .filter(|manifest| is_newest(manifest.type_id, manifest.version))
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
    fn the_catalogue_offers_the_newest_of_every_effect_still_published() {
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
        assert_eq!(catalogue.effects.len(), types.len());

        for rendered in &catalogue.effects {
            assert!(
                !RETIRED.contains(&rendered.type_id.as_str()),
                "{} is offered after being retired",
                rendered.type_id
            );
            let newest = PUBLISHED
                .iter()
                .filter(|manifest| manifest.type_id == rendered.type_id)
                .map(|manifest| manifest.version)
                .max()
                .expect("a version of a type that is in the catalogue");
            assert_eq!(rendered.version, newest, "{}", rendered.type_id);
        }
    }

    #[test]
    fn a_retired_effect_is_still_served_to_a_show_that_has_one() {
        // Retired, not deleted: a show with a Tint in a chain opens with it.
        for type_id in RETIRED {
            assert!(manifest(type_id, 1).is_some(), "{type_id} is still served");
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
