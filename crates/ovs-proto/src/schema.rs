//! Generation of the JSON Schema for the protocol, and for the domain below it.
//!
//! This crate is the highest point that can see both the domain types and the
//! protocol types, so it owns the generator binary. `ovs-types` still defines
//! its own schemas; this composes them with the protocol's.

pub use ovs_types::schema::GeneratedSchema;
use schemars::schema_for;

/// Generates the protocol schema documents.
///
/// # Panics
///
/// Panics if a schema cannot be serialised, which would mean a protocol type
/// produced a schema with a non-string map key. That is a programming error.
#[must_use]
pub fn generate_protocol() -> Vec<GeneratedSchema> {
    macro_rules! schema {
        ($file:literal, $ty:ty) => {
            GeneratedSchema {
                file_name: $file,
                contents: {
                    let mut json = serde_json::to_string_pretty(&schema_for!($ty))
                        .expect("protocol types must produce serialisable schemas");
                    json.push('\n');
                    json
                },
            }
        };
    }

    vec![
        schema!("client-message.schema.json", crate::realtime::ClientMessage),
        schema!("server-message.schema.json", crate::realtime::ServerMessage),
        schema!("client-hello.schema.json", crate::realtime::ClientHello),
        schema!("server-hello.schema.json", crate::realtime::ServerHello),
        schema!("state-snapshot.schema.json", crate::realtime::StateSnapshot),
        schema!("api-error.schema.json", crate::http::ApiError),
        schema!("login-request.schema.json", crate::http::LoginRequest),
        schema!("login-response.schema.json", crate::http::LoginResponse),
        schema!(
            "create-account-request.schema.json",
            crate::http::CreateAccountRequest
        ),
        schema!("account-summary.schema.json", crate::http::AccountSummary),
        schema!(
            "update-account-request.schema.json",
            crate::http::UpdateAccountRequest
        ),
        schema!(
            "set-password-request.schema.json",
            crate::http::SetPasswordRequest
        ),
        schema!("who-am-i.schema.json", crate::http::WhoAmIResponse),
        schema!(
            "capabilities.schema.json",
            crate::http::CapabilitiesResponse
        ),
        schema!("status.schema.json", crate::http::StatusResponse),
        schema!("dmx-protocols.schema.json", ovs_types::config::DmxProtocols),
        schema!(
            "preview-switch-request.schema.json",
            crate::http::PreviewSwitchRequest
        ),
        schema!(
            "test-pattern-request.schema.json",
            crate::http::TestPatternRequest
        ),
        schema!(
            "reset-counters-request.schema.json",
            crate::http::ResetCountersRequest
        ),
        schema!(
            "address-suggestion-request.schema.json",
            crate::http::AddressSuggestionRequest
        ),
        schema!(
            "address-suggestion-response.schema.json",
            crate::http::AddressSuggestionResponse
        ),
        schema!(
            "stream-ticket.schema.json",
            crate::http::StreamTicketResponse
        ),
        schema!("power-action.schema.json", crate::http::PowerAction),
        schema!("power-response.schema.json", crate::http::PowerResponse),
        schema!("update-status.schema.json", crate::http::UpdateStatus),
        schema!(
            "install-update-request.schema.json",
            crate::http::InstallUpdateRequest
        ),
        schema!(
            "install-update-response.schema.json",
            crate::http::InstallUpdateResponse
        ),
        schema!(
            "fixture-catalogue.schema.json",
            crate::fixtures::FixtureCatalogueResponse
        ),
        schema!(
            "generator-catalogue.schema.json",
            crate::generators::GeneratorCatalogueResponse
        ),
        schema!(
            "effect-catalogue.schema.json",
            crate::effects::EffectCatalogueResponse
        ),
        schema!("show-list-entry.schema.json", crate::http::ShowListEntry),
        schema!(
            "save-show-request.schema.json",
            crate::http::SaveShowRequest
        ),
        schema!("media-list.schema.json", crate::http::MediaListResponse),
        schema!(
            "create-folder-request.schema.json",
            crate::http::CreateFolderRequest
        ),
        schema!(
            "rename-folder-request.schema.json",
            crate::http::RenameFolderRequest
        ),
        schema!(
            "file-media-request.schema.json",
            crate::http::FileMediaRequest
        ),
        schema!("media-usage.schema.json", crate::http::MediaUsageResponse),
        schema!("show-export.schema.json", crate::http::ShowExport),
        schema!(
            "media-manifest-entry.schema.json",
            crate::http::MediaManifestEntry
        ),
        schema!(
            "import-show-response.schema.json",
            crate::http::ImportShowResponse
        ),
        schema!(
            "select-slot-request.schema.json",
            crate::http::SelectSlotRequest
        ),
        schema!(
            "assign-slot-request.schema.json",
            crate::http::AssignSlotRequest
        ),
        schema!(
            "assign-slot-response.schema.json",
            crate::http::AssignSlotResponse
        ),
        schema!(
            "add-effect-request.schema.json",
            crate::http::AddEffectRequest
        ),
        schema!(
            "order-effects-request.schema.json",
            crate::http::OrderEffectsRequest
        ),
        schema!(
            "order-effects-response.schema.json",
            crate::http::OrderEffectsResponse
        ),
        schema!(
            "set-banks-request.schema.json",
            crate::http::SetBanksRequest
        ),
        schema!("set-slot-request.schema.json", crate::http::SetSlotRequest),
        schema!(
            "set-mapping-request.schema.json",
            crate::http::SetMappingRequest
        ),
        schema!(
            "show-folders-response.schema.json",
            crate::http::ShowFoldersResponse
        ),
        schema!(
            "file-show-request.schema.json",
            crate::http::FileShowRequest
        ),
        schema!(
            "add-effect-response.schema.json",
            crate::http::AddEffectResponse
        ),
        schema!(
            "update-master-request.schema.json",
            crate::http::UpdateMasterRequest
        ),
        schema!(
            "update-layer-request.schema.json",
            crate::http::UpdateLayerRequest
        ),
        schema!(
            "update-effect-request.schema.json",
            crate::http::UpdateEffectRequest
        ),
        schema!("tap-tempo.schema.json", crate::http::TapTempoResponse),
        schema!(
            "create-upload-request.schema.json",
            crate::http::CreateUploadRequest
        ),
        schema!(
            "create-upload-response.schema.json",
            crate::http::CreateUploadResponse
        ),
        schema!("upload-session.schema.json", crate::http::UploadSession),
        schema!(
            "set-connector-role.schema.json",
            crate::http::SetConnectorRoleRequest
        ),
    ]
}

/// Generates every schema document the repository commits.
///
/// Domain first, then protocol.
#[must_use]
pub fn generate_all() -> Vec<GeneratedSchema> {
    let mut all = ovs_types::schema::generate_all();
    all.extend(generate_protocol());
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_schema_has_a_unique_file_name() {
        let mut seen = std::collections::BTreeSet::new();
        for schema in generate_all() {
            assert!(
                seen.insert(schema.file_name),
                "duplicate schema {}",
                schema.file_name
            );
        }
    }

    #[test]
    fn the_combined_set_contains_both_layers() {
        let names: Vec<&str> = generate_all()
            .iter()
            .map(|schema| schema.file_name)
            .collect();
        assert!(
            names.contains(&"show.schema.json"),
            "domain schemas must be included"
        );
        assert!(
            names.contains(&"server-message.schema.json"),
            "protocol schemas must be included"
        );
    }

    #[test]
    fn every_schema_is_valid_json_with_a_title() {
        for schema in generate_all() {
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
    fn the_protocol_schema_exposes_the_message_tag() {
        // A generated TypeScript client discriminates on this, so it has to be
        // in the schema rather than only in the Rust attribute.
        let message = generate_protocol()
            .into_iter()
            .find(|schema| schema.file_name == "server-message.schema.json")
            .expect("generated");
        assert!(message.contents.contains("\"type\""));
        assert!(message.contents.contains("snapshot"));
        assert!(message.contents.contains("telemetry"));
    }
}
