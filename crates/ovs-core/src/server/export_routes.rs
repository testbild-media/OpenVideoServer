//! Moving a show between servers.
//!
//! ```text
//! GET  /shows/{id}/export   the show and an inventory of what it needs
//! POST /shows/import        put that package on this server
//! ```
//!
//! # A package is a document, not a bundle
//!
//! The media is not in it. A package is something an operator can read and a
//! server can validate before a byte of video moves, and the files come
//! afterwards through the ordinary upload path — only the ones the receiving
//! server does not already hold, and resumable like any other upload.
//!
//! Putting the files inside would mean a single transfer that is tens of
//! gigabytes, fails as a unit, and re-sends clips the destination already has.
//!
//! # Content decides identity
//!
//! An import reuses what this server already holds, by hash. If
//! the same bytes are here under a different item, the imported show's
//! references are pointed at the local one and the change is reported. If they
//! are not here, the item is created empty under the identifier the package gave
//! it, so that uploading the content later completes the show rather than
//! producing a second item nothing refers to.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use ovs_proto::http::{ImportShowResponse, MediaManifestEntry, Relinked, ShowExport};
use ovs_types::config::{Show, SourceRef};
use ovs_types::id::{MediaItemId, ShowId};
use ovs_types::media::{MediaItem, MediaState};
use ovs_types::time::Timestamp;
use ovs_types::version::{check_show_schema_version, PACKAGE_VERSION};

use crate::server::app_state::AppState;
use crate::server::auth_routes::authenticate;
use crate::server::error::{ApiFailure, ApiResult};

fn parse_show(raw: &str) -> ApiResult<ShowId> {
    raw.parse()
        .map_err(|_| ApiFailure::bad_request("invalid_id", format!("{raw:?} is not a valid id")))
}

/// Writes a package for one show.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show is not here, or when one of its media
/// items is not in a state that can be described — an item still importing has
/// no content to name, and a package that pointed at one would be a package that
/// cannot be imported.
pub async fn export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
) -> ApiResult<Json<ShowExport>> {
    authenticate(&state, &headers)?;
    let id = parse_show(&show_id)?;

    let show = state.with_database(|database| database.load_show(id))?;

    let mut media = Vec::new();
    for media_item_id in show.referenced_media() {
        let entry = state.with_database(|database| {
            let item = database.load_media_item(media_item_id)?;
            let asset = database.playable_asset_for(media_item_id)?;
            Ok::<_, ovs_db::DbError>((item, asset))
        })?;

        let (item, Some(asset)) = entry else {
            // A layer pointing at something that has never finished importing.
            // Exporting it would produce a package whose inventory cannot be
            // satisfied, which is worse than saying so now.
            return Err(ApiFailure::conflict(
                "media_not_ready",
                format!(
                    "{} has no playable content yet, so this show cannot be packaged",
                    media_item_id
                ),
            ));
        };

        // A ready item always has dimensions — `MediaItem::validate` refuses one
        // without — so this is the case where the database has been edited by
        // something that should not have.
        let Some(dimensions) = item.dimensions else {
            return Err(ApiFailure::conflict(
                "media_not_ready",
                format!("{media_item_id} has no known size, so this show cannot be packaged"),
            ));
        };

        media.push(MediaManifestEntry {
            media_item_id,
            display_name: item.display_name,
            original_filename: item.original_filename,
            kind: item.kind,
            sha256: asset.sha256,
            byte_size: asset.byte_size,
            dimensions,
            duration_ms: item.duration_ms,
            frame_rate: item.frame_rate,
            has_alpha: item.has_alpha,
            container: asset.container,
            codec: asset.codec,
        });
    }

    Ok(Json(ShowExport {
        package_version: PACKAGE_VERSION,
        exported_by: state.server_version().to_owned(),
        exported_at: Timestamp::now(),
        show,
        media,
    }))
}

/// Puts a package on this server.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the package is from a newer build, when the
/// show does not validate, or when the database refuses it.
pub async fn import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(package): Json<ShowExport>,
) -> ApiResult<(StatusCode, Json<ImportShowResponse>)> {
    authenticate(&state, &headers)?;

    if package.package_version > PACKAGE_VERSION {
        return Err(ApiFailure::conflict(
            "unknown_package_version",
            format!(
                "this server reads package version {PACKAGE_VERSION}, and this one is {}",
                package.package_version
            ),
        ));
    }
    check_show_schema_version(package.show.schema_version)
        .map_err(|error| ApiFailure::conflict("unsupported_schema", error.to_string()))?;

    let mut show = package.show;
    // Before it is checked: a package exported while a module still had a
    // second version names that number, and a patch entry the footprint
    // it had then.
    show.adopt_one_of_each();
    show.validate().map_err(ApiFailure::invalid)?;

    // A package of a show this server already has replaces it, which makes
    // this a door into the document like a save is. It carries the locks the
    // stored show has and is held to them, so that a locked effect cannot be
    // taken out by exporting the show, editing the file and bringing it back.
    // A show this server has never had keeps the locks its package carries:
    // they are part of what was exported.
    if let Ok(stored) = state.with_database(|database| database.load_show(show.id)) {
        ovs_types::control::carry_locks(&stored, &mut show);
        ovs_types::control::check_edit(&stored, &show, None)?;
    }

    let mut relinked = Vec::new();
    let mut missing = Vec::new();

    for entry in package.media {
        let existing =
            state.with_database(|database| database.media_item_for_content(&entry.sha256))?;

        match existing {
            Some(local) if local == entry.media_item_id => {}
            Some(local) => {
                // The same bytes, under a different identifier. Pointing the
                // show at what is here is the whole reason a package carries
                // hashes rather than files.
                relink(&mut show, entry.media_item_id, local);
                relinked.push(Relinked {
                    from: entry.media_item_id,
                    to: local,
                });
            }
            None => {
                // Created empty, under the identifier the package gave it, so
                // that uploading the content later completes this show
                // rather than producing a second item nothing refers to.
                let placeholder = MediaItem {
                    id: entry.media_item_id,
                    kind: entry.kind,
                    state: MediaState::Importing,
                    dimensions: Some(entry.dimensions),
                    duration_ms: entry.duration_ms,
                    frame_rate: entry.frame_rate,
                    has_alpha: entry.has_alpha,
                    ..MediaItem::importing(&entry.display_name, &entry.original_filename)
                };
                state.with_database(|database| database.upsert_media_item(&placeholder))?;
                missing.push(entry);
            }
        }
    }

    let show_id = show.id;
    state.with_database(|database| database.save_show(&show))?;

    tracing::info!(
        %show_id,
        relinked = relinked.len(),
        missing = missing.len(),
        "imported a show package"
    );

    Ok((
        StatusCode::CREATED,
        Json(ImportShowResponse {
            show_id,
            relinked,
            missing,
        }),
    ))
}

/// Points every reference to `from` at `to`.
///
/// Every way a show can name a picture, which is two: what a slot plays, and
/// what an effect was given. A show imported with only the first relinked would
/// carry a mask pointing at the identifier it had on the machine it came from.
fn relink(show: &mut Show, from: MediaItemId, to: MediaItemId) {
    for layer in &mut show.layers {
        relink_chain(&mut layer.effects, from, to);
        for slot in &mut layer.slots {
            if let SourceRef::Media { media_item_id } = &mut slot.source {
                if *media_item_id == from {
                    *media_item_id = to;
                }
            }
            relink_chain(&mut slot.effects, from, to);
        }
    }
}

/// Points a chain's picture parameters at the local copy.
fn relink_chain(
    chain: &mut [ovs_types::config::EffectInstance],
    from: MediaItemId,
    to: MediaItemId,
) {
    for instance in chain {
        let Some(manifest) = ovs_types::config::effects::manifest(&instance.type_id) else {
            // An effect this build does not publish keeps whatever it was
            // saved with: its parameters are not ours to read, and rewriting
            // one by guessing which of them names a picture would be worse
            // than leaving it.
            continue;
        };
        for parameter in manifest.parameters {
            if !matches!(parameter.kind, ovs_types::config::ParameterKind::Image) {
                continue;
            }
            if manifest.media(&instance.parameters, parameter.key) == Some(from) {
                instance.parameters.insert(
                    parameter.key.to_owned(),
                    ovs_types::config::slot::ParameterValue::Text(to.to_string()),
                );
            }
        }
    }
}
