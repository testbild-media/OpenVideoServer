//! Media library endpoints.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use ovs_proto::http::{
    CreateFolderRequest, FileMediaRequest, FolderQuery, MediaListResponse, MediaUsageResponse,
    RenameFolderRequest,
};
use ovs_types::id::MediaItemId;
use ovs_types::media::MediaItem;

use crate::server::app_state::AppState;
use crate::server::auth_routes::authenticate;
use crate::server::error::{ApiFailure, ApiResult};

fn parse_id(raw: &str) -> ApiResult<MediaItemId> {
    raw.parse()
        .map_err(|_| ApiFailure::bad_request("invalid_id", format!("{raw:?} is not a valid id")))
}

/// Lists the media library.
///
/// A database view, not a filesystem listing: no physical path appears anywhere
/// in the response.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<MediaListResponse>> {
    authenticate(&state, &headers)?;
    let items = state.with_database(|database| {
        let items = database.list_media_items()?;
        items
            .into_iter()
            .map(|item| {
                // Zero for an item whose content has not arrived, which is what
                // an imported show's items look like until they are filled.
                let byte_size = database
                    .playable_asset_for(item.id)?
                    .map_or(0, |asset| asset.byte_size);
                Ok(ovs_proto::http::MediaListItem { item, byte_size })
            })
            .collect::<ovs_db::Result<Vec<_>>>()
    })?;
    let folders = state.with_database(|database| database.list_media_folders())?;
    Ok(Json(MediaListResponse { items, folders }))
}

/// Makes a folder in the library.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the path is not one the library allows.
pub async fn create_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateFolderRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    state.with_database(|database| database.create_media_folder(&request.path))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes a folder that has nothing in it.
///
/// Nothing in it means no item filed there and no folder under it. Deleting one
/// that still holds things would either orphan them or delete somebody's media
/// because they tidied up, so it is refused instead.
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
    let removed = state.with_database(|database| database.delete_media_folder(&query.path))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiFailure::not_found(format!(
            "there is no folder {}",
            query.path
        )))
    }
}

/// Renames a folder, which moves everything filed under it.
///
/// # Errors
///
/// Returns [`ApiFailure`] when either path is not one the library allows, when
/// something is already at the new path, or when the new path is inside the old
/// one.
pub async fn rename_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RenameFolderRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    state.with_database(|database| database.rename_media_folder(&request.from, &request.to))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Renames one item, or moves it to another folder, or both.
///
/// Neither touches the file: the asset keeps its object key and the content
/// hash it is found by, so a show that plays this item goes on playing it.
///
/// # Errors
///
/// Returns [`ApiFailure`] when there is no such item, when the name or path is
/// not one the library allows, or when the folder does not exist.
pub async fn file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(media_item_id): Path<String>,
    Json(request): Json<FileMediaRequest>,
) -> ApiResult<Json<MediaItem>> {
    authenticate(&state, &headers)?;
    let id = parse_id(&media_item_id)?;
    let item = state.with_database(|database| {
        database.file_media_item(
            id,
            request.display_name.as_deref(),
            request.folder.as_deref(),
        )
    })?;
    Ok(Json(item))
}

/// Reports which shows reference a media item.
///
/// A client calls this before offering to delete, so an operator is warned
/// rather than silently breaking an archived show.
pub async fn usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(media_item_id): Path<String>,
) -> ApiResult<Json<MediaUsageResponse>> {
    authenticate(&state, &headers)?;
    let id = parse_id(&media_item_id)?;

    let used_by_shows = state.with_database(|database| database.shows_using_media(id))?;
    Ok(Json(MediaUsageResponse { used_by_shows }))
}

/// Deletes a media item and reports the files that are now unreferenced.
///
/// Physical deletion happens after this reference check, never before
///. The keys are returned rather than acted on here, because
/// removing files is the media service's job.
/// Returns the picture of one media item, as a PNG.
///
/// Answers 404 for an item that has none, which is an item registered before
/// this existed or one nothing could be read from. A client draws a name in
/// that case, which is what it drew for everything until now.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not authenticated, the identifier
/// is not one, or there is no picture.
pub async fn thumbnail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(media_item_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    use axum::http::header;
    use axum::response::IntoResponse as _;

    authenticate(&state, &headers)?;
    let id = parse_id(&media_item_id)?;

    let path = state
        .media_store()
        .ok_or_else(|| ApiFailure::not_found("there is no picture of this item"))?
        .thumbnail_path_for(id);
    let png = std::fs::read(path)
        .map_err(|_| ApiFailure::not_found("there is no picture of this item"))?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            // A thumbnail is written once and never changes, because the item's
            // content cannot: a media item is content-addressed.
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        png,
    )
        .into_response())
}

/// Returns the moving preview of one media item, as a JPEG strip.
///
/// Ten frames of the clip side by side, earliest first, at the size a slot pad
/// draws one. The client shows the first of them and steps through the rest
/// while the pointer is over a pad (owner, 2026-09-23).
///
/// Answers 404 for a still, for an item registered before this existed, and for
/// one nothing could be read from. A client draws the ordinary picture in that
/// case, which is what it drew for everything until now.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not authenticated, the identifier
/// is not one, or there is no moving preview.
pub async fn strip(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(media_item_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    use axum::http::header;
    use axum::response::IntoResponse as _;

    authenticate(&state, &headers)?;
    let id = parse_id(&media_item_id)?;

    let path = state
        .media_store()
        .ok_or_else(|| ApiFailure::not_found("there is no moving preview of this item"))?
        .strip_path_for(id);
    let jpeg = std::fs::read(path)
        .map_err(|_| ApiFailure::not_found("there is no moving preview of this item"))?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            // Written once and never changed, for the reason the still gives:
            // a media item is content-addressed.
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        jpeg,
    )
        .into_response())
}

/// Streams one media item's content back to a client.
///
/// **Media went up and never came down**, and that made a show something that
/// could only ever live on the server it was uploaded to (owner, 2026-09-23):
/// "Export alles müsste, um auf dem server nicht zu viel Platz zu brauchen,
/// einzeln an den Clienten schicken und der muss daraus eine Zip oder ähnlich
/// machen." This is the "einzeln" half — one item per request, streamed from
/// disk — and the client does the packing.
///
/// One at a time and streamed on purpose: a server that built an archive of a
/// show's media would need room for a second copy of it, which is exactly the
/// cost the owner asked to avoid.
///
/// The physical name never leaves the server. What a client is told
/// is the item's own name and its container, which is enough to write a file.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not authenticated, the identifier
/// is not one, the item has no content, or it cannot be read.
pub async fn content(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(media_item_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    use axum::http::header;
    use axum::response::IntoResponse as _;

    authenticate(&state, &headers)?;
    let id = parse_id(&media_item_id)?;

    let asset = state
        .with_database(|database| database.playable_asset_for(id))?
        .ok_or_else(|| ApiFailure::not_found("this item has no content on this server"))?;

    let path = state
        .media_store()
        .and_then(|store| store.existing_path_for(&asset.object_key))
        .ok_or_else(|| ApiFailure::not_found("this item has no content on this server"))?;

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|error| ApiFailure::internal(format!("this item could not be read: {error}")))?;

    // Streamed in chunks rather than read whole: a show clip is gigabytes, and
    // a server that held one in memory to answer a request would be a server
    // one export could take down.
    let body = axum::body::Body::from_stream(futures_util::stream::try_unfold(
        file,
        |mut file| async move {
            use tokio::io::AsyncReadExt as _;

            let mut chunk = vec![0_u8; 256 * 1024];
            let read = file.read(&mut chunk).await?;
            if read == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            chunk.truncate(read);
            Ok(Some((axum::body::Bytes::from(chunk), file)))
        },
    ));

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (header::CONTENT_LENGTH, asset.byte_size.to_string()),
            // Content-addressed, so the bytes under one item never change.
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_owned(),
            ),
        ],
        body,
    )
        .into_response())
}

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(media_item_id): Path<String>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    let id = parse_id(&media_item_id)?;

    let in_use = state.with_database(|database| database.shows_using_media(id))?;
    if !in_use.is_empty() {
        return Err(ApiFailure::conflict(
            "media_in_use",
            format!("this media is used by: {}", in_use.join(", ")),
        ));
    }

    let unreferenced = state.with_database(|database| database.delete_media_item(id))?;
    // **Deleted, not noted.** This used to log the count and drop the list, so
    // an operator who cleared their library got the rows back and none of the
    // disc — seventeen files on the reference machine for four items in the
    // library (owner, 2026-09-25).
    let mut freed = 0;
    if let Some(store) = state.media_store() {
        store.drop_thumbnail(id);
        for object_key in &unreferenced {
            freed += store.remove(object_key);
        }
    }
    tracing::info!(
        media_item_id = %id,
        objects = unreferenced.len(),
        freed_bytes = freed,
        "media deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}
