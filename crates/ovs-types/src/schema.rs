//! Generation of the language-neutral JSON Schema for every shared contract.
//!
//! The schemas in `schemas/` are generated from the Rust types in this crate and
//! committed. They are the contract the desktop client is built against, and the
//! golden test in `tests/golden_schema.rs` fails whenever the generated output
//! stops matching what is committed.
//!
//! That failure is the point: it turns "somebody changed the show model" into
//! a build error that has to be answered with either a version bump and a
//! migration, or an admission that the change was accidental.

use schemars::schema_for;

/// One generated schema document.
pub struct GeneratedSchema {
    /// File name to write, relative to the schema directory.
    pub file_name: &'static str,
    /// Pretty-printed JSON Schema.
    pub contents: String,
}

/// Generates every committed schema document.
///
/// # Panics
///
/// Panics if a schema cannot be serialised, which would mean a type in this
/// crate produced a schema containing a non-string map key. That is a programming
/// error, not a runtime condition.
#[must_use]
pub fn generate_all() -> Vec<GeneratedSchema> {
    macro_rules! schema {
        ($file:literal, $ty:ty) => {
            GeneratedSchema {
                file_name: $file,
                contents: {
                    let mut json = serde_json::to_string_pretty(&schema_for!($ty))
                        .expect("shared types must produce serialisable schemas");
                    json.push('\n');
                    json
                },
            }
        };
    }

    vec![
        schema!("show.schema.json", crate::config::Show),
        schema!("media-item.schema.json", crate::media::MediaItem),
        schema!("media-asset.schema.json", crate::media::MediaAsset),
        schema!("master-live.schema.json", crate::live::MasterLive),
        schema!("layer-live.schema.json", crate::live::LayerLive),
        schema!(
            "server-telemetry.schema.json",
            crate::telemetry::ServerTelemetry
        ),
        schema!(
            "stream-encoding.schema.json",
            crate::telemetry::StreamEncoding
        ),
        schema!("connector-role.schema.json", crate::config::ConnectorRole),
        schema!("startup-policy.schema.json", crate::config::StartupPolicy),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_schema_is_valid_json_with_a_unique_name() {
        let generated = generate_all();
        assert!(!generated.is_empty());

        let mut seen = std::collections::BTreeSet::new();
        for schema in &generated {
            assert!(
                seen.insert(schema.file_name),
                "duplicate schema {}",
                schema.file_name
            );
            let parsed: serde_json::Value =
                serde_json::from_str(&schema.contents).expect("valid JSON");
            assert!(
                parsed.get("title").is_some(),
                "{} has no title",
                schema.file_name
            );
        }
    }

    #[test]
    fn the_show_schema_documents_its_version_field() {
        let show = generate_all()
            .into_iter()
            .find(|schema| schema.file_name == "show.schema.json")
            .expect("show schema is generated");
        let parsed: serde_json::Value = serde_json::from_str(&show.contents).expect("valid JSON");
        assert!(
            parsed["properties"]["schema_version"].is_object(),
            "a client must be able to read the schema version before anything else"
        );
        assert!(
            parsed["required"]
                .as_array()
                .expect("required list")
                .iter()
                .any(|value| value == "schema_version"),
            "schema_version must be required"
        );
    }
}
