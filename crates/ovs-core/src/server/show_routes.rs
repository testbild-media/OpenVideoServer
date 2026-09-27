//! Show endpoints.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use ovs_proto::http::{
    AddressSuggestionRequest, AddressSuggestionResponse, CaptureLiveResponse, CreateFolderRequest,
    CreateShowRequest, FileShowRequest, FolderQuery, RenameFolderRequest, SaveShowRequest,
    SaveShowResponse, ShowFoldersResponse, ShowListEntry,
};
use ovs_types::config::Show;
use ovs_types::id::ShowId;

use crate::server::app_state::AppState;
use crate::server::auth_routes::authenticate;
use crate::server::error::{ApiFailure, ApiResult};

/// Parses a show identifier from the path.
///
/// A malformed identifier is a bad request, not a missing show and not a
/// server error.
fn parse_id(raw: &str) -> ApiResult<ShowId> {
    raw.parse()
        .map_err(|_| ApiFailure::bad_request("invalid_id", format!("{raw:?} is not a valid id")))
}

/// Lists every show, including ones this build cannot open.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<ShowListEntry>>> {
    authenticate(&state, &headers)?;

    let summaries = state.with_database(|database| database.list_shows())?;
    Ok(Json(
        summaries
            .into_iter()
            .map(|summary| ShowListEntry {
                loadable: summary.is_loadable(),
                id: summary.id,
                name: summary.name,
                schema_version: summary.schema_version,
                created_at: summary.created_at,
                updated_at: summary.updated_at,
                folder: summary.folder,
            })
            .collect(),
    ))
}

/// Every folder the show list is organised into.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the list cannot be read.
pub async fn folders(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<ShowFoldersResponse>> {
    authenticate(&state, &headers)?;
    let folders = state.with_database(|database| database.list_show_folders())?;
    Ok(Json(ShowFoldersResponse { folders }))
}

/// Makes a folder in the show list, and every folder above it.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the path is not one the list allows.
pub async fn create_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateFolderRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    state.with_database(|database| database.create_show_folder(&request.path))?;
    Ok(StatusCode::CREATED)
}

/// Removes a show folder that has nothing in it.
///
/// Nothing in it means no show filed there and no folder under it: deleting
/// one that still holds shows would either orphan them or take somebody's work
/// with it, and neither is a thing to do because an operator tidied up.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the folder is not there or still holds
/// something.
pub async fn delete_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FolderQuery>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    let removed = state.with_database(|database| database.delete_show_folder(&query.path))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiFailure::not_found(format!(
            "there is no folder {}",
            query.path
        )))
    }
}

/// Renames a show folder, which moves everything under it.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the folder is not there, when the path is not
/// one the list allows, or when the move would put the folder inside itself.
pub async fn rename_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RenameFolderRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    state.with_database(|database| database.rename_show_folder(&request.from, &request.to))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Files one show in a folder.
///
/// **The document is untouched**, and so is what the show plays: filing is
/// a label on this server's list (owner, 2026-09-23). A show that is open
/// stays open, because nothing about the show has changed.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show or the folder is not there, or when
/// the path is not one the list allows.
pub async fn file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
    Json(request): Json<FileShowRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    let show_id: ShowId = parse_id(&show_id)?;
    state.with_database(|database| database.file_show(show_id, &request.folder))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Creates an empty show.
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateShowRequest>,
) -> ApiResult<(StatusCode, Json<Show>)> {
    authenticate(&state, &headers)?;

    let mut show = Show::new(request.name);
    // The time the row got rather than the one `Show::new` minted a moment
    // earlier, so the document handed back is the document that was stored.
    let updated_at = state.with_database(|database| database.save_show(&show))?;
    show.updated_at = updated_at;
    Ok((StatusCode::CREATED, Json(show)))
}

/// Returns one show.
pub async fn load(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
) -> ApiResult<Json<Show>> {
    authenticate(&state, &headers)?;
    let id = parse_id(&show_id)?;

    let show = state.with_database(|database| database.load_show(id))?;
    Ok(Json(show))
}

/// Suggests where a fixture fits in a universe.
///
/// What a patch editor offers when an operator adds a fixture, and what stops
/// them counting channels by hand (V0.2: "collision detection and address
/// suggestions").
///
/// Answered from the show on disk rather than the one that happens to be
/// open, because a patch is edited in a show that may not be the live one.
pub async fn suggest_address(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
    Query(request): Query<AddressSuggestionRequest>,
) -> ApiResult<Json<AddressSuggestionResponse>> {
    authenticate(&state, &headers)?;
    let id = parse_id(&show_id)?;

    let show = state.with_database(|database| database.load_show(id))?;
    let start_address = ovs_types::config::patch::suggest_address(
        &show.fixture_patch,
        request.universe,
        request.footprint,
        request.moving,
    );

    Ok(Json(AddressSuggestionResponse {
        universe: request.universe,
        start_address,
    }))
}

/// Saves a show whole.
///
/// # What a save does to the show: nothing
///
/// **Saving never interrupts what is running** (owner, 2026-09-23). This used
/// to call `load_show`, which applies the restart policy — every layer back
/// to its opening slot, the master back to its opening dimmer, the clock back
/// to beat zero. That is what *opening* a show means, and it was
/// happening every time somebody pressed Save on the show that was on
/// stage: a blackout and a restart in the middle of a night, as the price of
/// storing a change.
///
/// Now the stored document is installed with
/// [`AuthoritativeState::replace_show`], which carries the live state and
/// the clock across untouched. Only opening a show reloads one.
///
/// # What a save does to the document
///
/// Stores it exactly as it arrived. Somebody who has just typed a default
/// opacity of 50% into the editor must not have it overwritten by whatever
/// the fader happens to be at, so the live state is not folded in here.
/// [`capture`] is the route that does that, and it is a different gesture:
/// "store what I am looking at" rather than "store what I have edited".
///
/// [`capture`]: crate::server::show_routes::capture
pub async fn save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
    Json(request): Json<SaveShowRequest>,
) -> ApiResult<Json<SaveShowResponse>> {
    authenticate(&state, &headers)?;
    let id = parse_id(&show_id)?;

    if request.show.id != id {
        return Err(ApiFailure::bad_request(
            "id_mismatch",
            "the show in the body does not match the one in the path",
        ));
    }

    // A generator arriving without an identity is given one before it is
    // stored, not when it is next opened.
    //
    // This is the only place it can be done safely. A patch entry points at a
    // generator instance and lives in the same document, so an identity that
    // existed only in the *loaded* state would be minted again on the next open
    // — and every fixture patched against the previous one would be pointing at
    // a generator that no longer exists. Minting here means the document that
    // holds the patch holds the identity the patch names.
    //
    // A document written by an older build still gains them on open
    // (`assign_missing_ids` in `load_show`), because there is no version of
    // this that can go back and rewrite what is already on disk.
    let mut show = request.show;
    show.assign_missing_ids();

    // **A show that is not loaded cannot be edited** (owner, 2026-09-24: "man
    // kann keine fremde show bearbeiten wenn eine andere show geladen ist").
    //
    // So the editor is always editing what is on the wall, and there is exactly
    // one answer to "what am I editing". The cost is deliberate: putting a
    // different show in the editor means loading it, and loading takes the
    // current one off stage. What it prevents is the other shape of
    // that cost — saving changes into a show and then wondering why the wall
    // did not follow.
    //
    // Enforced here rather than in the client, because a rule a client keeps is
    // a rule the next client breaks.
    let is_open = state.with_state(|current| current.show_id() == Some(id));
    if !is_open {
        return Err(ApiFailure::conflict(
            "show_not_loaded",
            "only the loaded show can be edited; load this one first",
        ));
    }

    let updated_at = state.with_database(|database| database.save_show(&show))?;

    // The running state follows the document.
    {
        // Stamped here too, so that what goes out over the socket is the
        // document that was stored rather than the one that arrived. A client
        // whose mirror held a different timestamp from the show it had just
        // saved would read as having unsaved work the moment it compared them.
        let saved = Show { updated_at, ..show };
        state.mutate_state(|current| {
            let change = current.replace_show(saved);
            ((), Some(change))
        });
    }

    Ok(Json(SaveShowResponse { updated_at }))
}

/// Stores what is on stage into the open show.
///
/// The owner's Save, from the live screen (2026-09-23): "Save stores the
/// complete current live state into the show", and "a save must make no
/// change to the live show".
///
/// No body. The server already holds both halves — the document it has open
/// and the live state running on top of it — so a client sending a document
/// back would be sending a copy that could only be staler than the original.
///
/// What is captured is on [`AuthoritativeState::capture_live`]. What is not
/// is the slot each layer is playing and the transport it is in: those belong
/// to the restart snapshot, which already survives a restart
/// and is deliberately not part of the document.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show named is not the one that is
/// open, because then there is no live state belonging to it to capture.
pub async fn capture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
) -> ApiResult<Json<CaptureLiveResponse>> {
    authenticate(&state, &headers)?;
    let id = parse_id(&show_id)?;

    let mut show = state
        .with_state(|current| {
            current
                .show()
                .filter(|show| show.id == id)
                .cloned()
                .map(|mut show| {
                    current.capture_live(&mut show);
                    show
                })
        })
        .ok_or_else(|| {
            ApiFailure::conflict(
                "not_the_open_show",
                "only the show that is open has a live state to capture",
            )
        })?;
    show.assign_missing_ids();

    let updated_at = state.with_database(|database| database.save_show(&show))?;

    // The running document gains the captured values too, so that what a
    // layer says it opens at and what it is doing agree from here on. Through
    // `replace_show`, so the show does not stop — which is the whole
    // instruction.
    let saved = Show { updated_at, ..show };
    state.mutate_state(|current| {
        let change = current.replace_show(saved);
        ((), Some(change))
    });

    Ok(Json(CaptureLiveResponse { updated_at }))
}

/// Deletes a show.
pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    let id = parse_id(&show_id)?;

    // Closing first means the renderer is never left holding a show that has
    // been deleted out from under it.
    let is_open = state.with_state(|current| current.show_id() == Some(id));
    if is_open {
        state.mutate_state(|current| {
            let change = current.close_show();
            ((), change)
        });
    }

    let removed = state.with_database(|database| database.delete_show(id))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiFailure::not_found(format!("no show with id {id}")))
    }
}

/// Opens a show, making it the running show.
pub async fn open(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    let id = parse_id(&show_id)?;

    let show = state.with_database(|database| database.load_show(id))?;
    let snapshot = state.with_database(|database| database.load_live_snapshot(id))?;

    // Where the outgoing show had got to, before it stops being the open one.
    // Without this, closing a show by opening another would lose everything
    // a restart is supposed to bring back.
    state.persist_live_snapshot();

    state.mutate_state(|current| {
        let change = current.load_show(show, snapshot.as_ref());
        ((), Some(change))
    });

    Ok(StatusCode::NO_CONTENT)
}
