//! Persistent configuration.
//!
//! Everything in this module is written to disk and survives restarts. Nothing
//! in this module may be driven by Art-Net or sACN. Live values belong in
//! [`crate::live`], and the persistence layer only ever accepts types from here,
//! so that leak cannot be written by accident.

pub mod effects;
pub mod fixtures;
pub mod generators;
pub mod layer;
pub mod manifest;
pub mod output;
pub mod patch;
pub mod piece;
pub mod show;
pub mod slot;

use serde::{Deserialize, Serialize};

use crate::error::{ValidationError, ValidationErrors};
use crate::id::EffectInstanceId;

pub use effects::{EffectManifest, PUBLISHED as EFFECTS};
pub use fixtures::{
    definition, ChannelDefinition, ChannelEncoding, ChannelValue, ColourPart, FixtureDefinition,
    LayerParameter, Parameter, ParameterSlot, PluginParameter, LAYER_V1, MASTER_V1, PUBLISHED,
};
pub use generators::{
    manifest, Choice, GeneratorManifest, ParameterDefinition, ParameterKind,
    PUBLISHED as GENERATORS,
};
pub use layer::{BlendMode, Layer, Route, SlotIndexError};
pub use output::{
    ConnectorRole, Destination, DisplayMode, OutputConfig, OutputIdentity, TestPatternMode,
    NETWORK_MODES,
};
pub use patch::FixtureInstance;
pub use piece::{Piece, PieceSource};
pub use show::{
    Composition, CrossfaderCurve, DmxProtocols, MasterConfig, Show, ShowSettings, StartupPolicy,
    MAX_BPM, MAX_SPEED_SCALE, MIN_BPM, MIN_SPEED_SCALE,
};
pub use slot::{
    ParameterMap, ParameterValue, PlaybackDirection, PlaybackMode, Slot, SlotSync, SourceRef,
    MIN_SLOT_BEATS,
};

/// One configured effect in a slot or layer effect chain.
///
/// Effects deliberately have no generic "effect master". Each effect
/// exposes its own parameters, and may optionally carry its own DMX fixture
/// personality defined by its manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EffectInstance {
    /// Stable identifier of this instance.
    pub id: EffectInstanceId,
    /// Stable effect type identifier, for example `ovs.colour_correct`.
    pub type_id: String,
    /// Always one, and not read: there is one effect of each type (owner,
    /// 2026-10-18: "Es gibt kein Invert V2.. nirgendwo"). Kept because shows
    /// carry it; [`crate::config::Show::adopt_one_of_each`] sets it to one in a
    /// show saved while some effects had a second version.
    pub version: u32,
    /// Whether this effect contributes to the chain.
    ///
    /// Everybody's to change, locked or not (owner, 2026-10-19).
    pub enabled: bool,
    /// Whether an administrator has locked this effect where it is.
    ///
    /// A locked effect cannot be removed, moved or put in another order by
    /// anybody, and only an administrator can lock or unlock one; see
    /// [`crate::control`]. Left out of a document when it is not locked, so a
    /// show nobody has locked anything in reads exactly as it did before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// Parameter values as declared by the manifest.
    #[serde(default, skip_serializing_if = "ParameterMap::is_empty")]
    pub parameters: ParameterMap,
}

impl EffectInstance {
    /// Creates an enabled, unlocked effect instance with no parameter overrides.
    #[must_use]
    pub fn new(type_id: impl Into<String>, version: u32) -> Self {
        Self {
            id: EffectInstanceId::new(),
            type_id: type_id.into(),
            version,
            enabled: true,
            locked: false,
            parameters: ParameterMap::new(),
        }
    }

    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        if self.type_id.trim().is_empty() {
            out.push(ValidationError::new(
                format!("{path}.type_id"),
                "must not be empty",
            ));
            return;
        }
        if self.version == 0 {
            out.push(ValidationError::new(
                format!("{path}.version"),
                "manifest versions start at 1",
            ));
            return;
        }

        // Checked against what this build publishes, exactly as a generator
        // source is. An effect nobody can run is a layer that will draw
        // something other than what the show says, and the moment to say so
        // is now rather than in a venue.
        let Some(manifest) = effects::manifest(&self.type_id) else {
            out.push(ValidationError::new(
                format!("{path}.type_id"),
                format!("this build does not publish {}", self.type_id),
            ));
            return;
        };

        for (key, reason) in manifest.problems(&self.parameters) {
            out.push(ValidationError::new(
                format!("{path}.parameters.{key}"),
                reason,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_effect_this_build_does_not_publish_is_refused() {
        // The alternative is a layer quietly drawing something other than what
        // the show says, which is the kind of thing that is discovered in
        // front of an audience.
        let mut errors = ValidationErrors::new();
        EffectInstance::new("ovs.kaleidoscope", 1).validate("effects[0]", &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors.to_string().contains("ovs.kaleidoscope"), "{errors}");
    }

    #[test]
    fn an_effect_parameter_is_checked_against_its_manifest() {
        let mut errors = ValidationErrors::new();
        let mut instance = EffectInstance::new("ovs.blur", 1);
        instance.parameters.insert(
            "radius_px".to_owned(),
            crate::config::ParameterValue::Float(4_000.0),
        );
        instance.validate("effects[0]", &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(
            errors
                .to_string()
                .contains("effects[0].parameters.radius_px"),
            "{errors}"
        );
    }

    #[test]
    fn effect_instances_require_a_type_and_version() {
        let mut errors = ValidationErrors::new();
        EffectInstance::new("  ", 1).validate("effects[0]", &mut errors);
        assert!(
            errors.to_string().contains("effects[0].type_id"),
            "{errors}"
        );

        let mut errors = ValidationErrors::new();
        EffectInstance {
            version: 0,
            ..EffectInstance::new("ovs.blur", 1)
        }
        .validate("effects[0]", &mut errors);
        assert!(
            errors.to_string().contains("effects[0].version"),
            "{errors}"
        );
    }

    #[test]
    fn a_valid_effect_instance_reports_nothing() {
        let mut errors = ValidationErrors::new();
        EffectInstance::new("ovs.colour_correct", 1).validate("effects[0]", &mut errors);
        assert!(errors.is_empty(), "{errors}");
    }
}

#[cfg(test)]
mod catalogue {
    //! What every published manifest has to be true of, whoever publishes it.
    //!
    //! A manifest is code (`manifest.rs`), so a mistake in one is a mistake in
    //! this build rather than in somebody's show — which means it can be a
    //! build failure instead of a show that opens wrong.

    use super::manifest::{ParameterDefinition, ParameterKind};
    use super::{EFFECTS, GENERATORS};

    /// Every parameter this build publishes, with the module it came from.
    fn published() -> Vec<(&'static str, &'static ParameterDefinition)> {
        let mut out = Vec::new();
        for manifest in GENERATORS.iter() {
            out.extend(
                manifest
                    .parameters
                    .iter()
                    .map(|parameter| (manifest.type_id, parameter)),
            );
        }
        for manifest in EFFECTS.iter() {
            out.extend(
                manifest
                    .parameters
                    .iter()
                    .map(|parameter| (manifest.type_id, parameter)),
            );
        }
        out
    }

    #[test]
    fn a_choice_parameter_starts_at_one_of_its_own_choices() {
        // A default naming something not in the list is a control that opens
        // showing nothing and a show that is refused the moment it is saved.
        for (module, parameter) in published() {
            let ParameterKind::Choice { choices, default } = parameter.kind else {
                continue;
            };
            assert!(
                choices.iter().any(|choice| choice.id == default),
                "{module}'s {} starts at {default:?}, which it does not offer",
                parameter.key
            );
        }
    }

    #[test]
    fn a_choice_parameter_offers_more_than_one_thing() {
        // A list of one is not a choice, and a client would draw a menu nobody
        // can change. Whatever it was meant to be, it is not this kind.
        for (module, parameter) in published() {
            let ParameterKind::Choice { choices, .. } = parameter.kind else {
                continue;
            };
            assert!(
                choices.len() >= 2,
                "{module}'s {} offers {} choice(s)",
                parameter.key,
                choices.len()
            );
        }
    }

    #[test]
    fn no_two_choices_share_an_identifier_or_a_name() {
        // Two entries with one identifier is a show that cannot say which it
        // meant; two with one name is a menu an operator cannot read.
        for (module, parameter) in published() {
            let ParameterKind::Choice { choices, .. } = parameter.kind else {
                continue;
            };
            let mut ids = std::collections::BTreeSet::new();
            let mut names = std::collections::BTreeSet::new();
            for choice in choices {
                assert!(
                    ids.insert(choice.id),
                    "{module}'s {} offers {:?} twice",
                    parameter.key,
                    choice.id
                );
                assert!(
                    names.insert(choice.name),
                    "{module}'s {} shows {:?} twice",
                    parameter.key,
                    choice.name
                );
            }
        }
    }

    #[test]
    fn a_choice_identifier_is_something_a_show_can_hold() {
        // Lowercase, and nothing in it that would have to be escaped: an
        // identifier travels through JSON, a schema and a client's markup, and
        // one that needed quoting somewhere would eventually not be quoted.
        for (module, parameter) in published() {
            let ParameterKind::Choice { choices, .. } = parameter.kind else {
                continue;
            };
            for choice in choices {
                assert!(
                    !choice.id.is_empty()
                        && choice
                            .id
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                    "{module}'s {} offers {:?}, which is not a plain identifier",
                    parameter.key,
                    choice.id
                );
                assert!(
                    !choice.name.trim().is_empty(),
                    "{module}'s {} offers a choice with no name",
                    parameter.key
                );
            }
        }
    }
}
