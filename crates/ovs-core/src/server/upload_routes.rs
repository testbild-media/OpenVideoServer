//! Media ingest: three requests from "here is what is coming" to a playable item.
//!
//! ```text
//! POST /media/uploads              say what is coming, or be told it is here already
//! PUT  /media/uploads/{id}?offset= send bytes, in order, resumable
//! POST /media/uploads/{id}/verify  hash it, check it, register it
//! ```
//!
//! # Nothing is trusted until it is verified
//!
//! The client declares a hash, a size and — because it has already opened the
//! file to prepare it — the dimensions. None of that is believed. The server
//! hashes what actually arrived, compares the size it actually wrote and, for a
//! still, decodes the file and compares the dimensions it actually finds. A
//! library whose metadata is wrong is a library that lies to every show built
//! on it.
//!
//! # A failed import is a visible item, not a silent nothing
//!
//! Content that does not verify leaves a [`MediaState::Failed`] item behind with
//! the reason on it. An operator who dragged in a file and saw nothing happen
//! has nothing to act on; one who sees "the hash did not match" can try again.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use ovs_proto::http::{CreateUploadRequest, CreateUploadResponse, UploadSession};
use ovs_types::media::{
    storage_extension, AssetValidation, MediaAsset, MediaItem, MediaKind, MediaState,
};
use ovs_types::time::Timestamp;
use sha2::{Digest, Sha256};

use crate::server::app_state::AppState;
use crate::server::auth_routes::authenticate;
use crate::server::error::{ApiFailure, ApiResult};

/// Largest file this server will accept, in bytes.
///
/// Not a judgement about what a show needs; a ceiling so that a client's
/// arithmetic slip cannot make the server reserve a disk it does not have.
const MAX_UPLOAD_BYTES: u64 = 256 * 1024 * 1024 * 1024;

/// Where in the file a chunk belongs.
#[derive(Debug, serde::Deserialize)]
pub struct ChunkPosition {
    /// Byte offset this chunk starts at.
    #[serde(default)]
    offset: u64,
}

/// Accepts an upload, or says the content is already here.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not internally consistent, when
/// the server has nowhere to put media, or when the database refuses the item.
pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateUploadRequest>,
) -> ApiResult<Json<CreateUploadResponse>> {
    authenticate(&state, &headers)?;
    check(&request)?;

    let store = state
        .media_store()
        .ok_or_else(|| ApiFailure::internal("this server has nowhere to store media"))?
        .clone();

    // Anything a client walked away from goes now. Swept here rather than on a
    // timer because starting an upload is the only thing that makes the table
    // grow, so it is the only moment at which the sweep can be behind.
    sweep_abandoned(&state, &store);

    // Asked before a byte crosses the network. This is what stops an operator
    // sending the same show file twice.
    let already =
        state.with_database(|database| database.media_item_for_content(&request.sha256))?;
    if let Some(media_item_id) = already {
        return Ok(Json(CreateUploadResponse::AlreadyPresent { media_item_id }));
    }

    // An import leaves items behind that are waiting for their content, and
    // the show already refers to them by identifier. Filling one is not the
    // same as creating one: a new item would leave the show pointing at
    // something that never arrives.
    let item = match request.media_item_id {
        Some(media_item_id) => {
            let existing = state
                .with_database(|database| database.load_media_item(media_item_id))
                .map_err(|_| {
                    ApiFailure::not_found(format!("there is no media item {media_item_id} here"))
                })?;

            if existing.state == MediaState::Ready {
                return Err(ApiFailure::conflict(
                    "already_ready",
                    format!("{media_item_id} already has its content"),
                ));
            }

            MediaItem {
                kind: request.kind,
                state: MediaState::Importing,
                dimensions: Some(request.dimensions),
                duration_ms: request.duration_ms,
                frame_rate: request.frame_rate,
                has_alpha: request.has_alpha,
                failure_reason: None,
                ..existing
            }
        }
        None => {
            // Made if it is not there, with everything above it, exactly as
            // `create_folder` does. An upload is a bad moment to discover that
            // somebody deleted a folder in another window, and the alternative
            // — filing it at the root and saying nothing — is the behaviour
            // being fixed.
            if !request.folder.is_empty() {
                state.with_database(|database| database.create_media_folder(&request.folder))?;
            }

            MediaItem {
                kind: request.kind,
                dimensions: Some(request.dimensions),
                duration_ms: request.duration_ms,
                frame_rate: request.frame_rate,
                has_alpha: request.has_alpha,
                folder: request.folder.clone(),
                ..MediaItem::importing(&request.display_name, &request.original_filename)
            }
        }
    };
    state.with_database(|database| database.upsert_media_item(&item))?;

    // Asked before the first byte rather than discovered at the last one. A
    // client that sends four gigabytes and is then told there was never room
    // for them has spent an ingest window finding out something the server knew
    // when it was asked. The space is read fresh here, so two clients starting
    // large uploads at the same moment are both measured against what is
    // actually left — though nothing reserves it, so the second can still fail
    // on a chunk, which is why the write path names a full disk too.
    // Space that cannot be read is treated as room. Refusing every upload on a
    // machine whose filesystems report nothing would turn a missing reading
    // into a server that cannot ingest at all, and the write path still names a
    // full disk when one turns up.
    if let Some(available) = state.media_space_available() {
        if !crate::uploads::fits(request.byte_size, available) {
            return Err(ApiFailure::out_of_space(format!(
                "this file is {}, and {} is free where media is kept; \
                 an upload is refused while less than {} would be left over",
                in_bytes(request.byte_size),
                in_bytes(available),
                in_bytes(crate::uploads::DISK_HEADROOM),
            )));
        }
    }

    let upload_id = ulid::Ulid::new().to_string();
    let path = store
        .incoming_path_for(&upload_id)
        .ok_or_else(|| ApiFailure::internal("could not name a place to stage this upload"))?;
    store
        .ensure_root()
        .and_then(|()| std::fs::write(&path, []))
        .map_err(|error| ApiFailure::from_write("could not stage this upload", &error))?;

    let session = state.with_uploads(|uploads| uploads.begin(&upload_id, request, item.id));
    tracing::info!(
        %upload_id,
        media_item_id = %item.id,
        bytes = session.byte_size,
        "accepting an upload"
    );

    Ok(Json(CreateUploadResponse::Accepted(session)))
}

/// Appends one chunk to an upload in progress.
///
/// # Errors
///
/// Returns [`ApiFailure`] when there is no such upload, when the offset is not
/// where it continues, or when the bytes cannot be written.
pub async fn append(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(upload_id): Path<String>,
    Query(position): Query<ChunkPosition>,
    body: Bytes,
) -> ApiResult<Json<UploadSession>> {
    authenticate(&state, &headers)?;

    let store = state
        .media_store()
        .ok_or_else(|| ApiFailure::internal("this server has nowhere to store media"))?
        .clone();
    let path = store
        .incoming_path_for(&upload_id)
        .ok_or_else(|| ApiFailure::not_found("no such upload"))?;

    // Written before it is counted. Counting first and then failing to write
    // would leave the server believing it holds bytes it does not, and the hash
    // check at the end would blame the client for it.
    let count = body.len() as u64;
    let expected = state
        .with_uploads(|uploads| uploads.get(&upload_id).map(|upload| upload.received_bytes))
        .ok_or_else(|| ApiFailure::not_found("no such upload"))?;
    if position.offset != expected {
        return Err(ApiFailure::conflict(
            "out_of_step",
            format!(
                "this upload continues at byte {expected}, not at {}",
                position.offset
            ),
        ));
    }

    append_at(&path, position.offset, &body)
        .map_err(|error| ApiFailure::from_write("could not write this chunk", &error))?;

    let outcome = state
        .with_uploads(|uploads| uploads.receive(&upload_id, position.offset, count))
        .ok_or_else(|| ApiFailure::not_found("no such upload"))?;

    match outcome {
        Ok(session) => Ok(Json(session)),
        Err(error) => Err(ApiFailure::conflict("out_of_step", error.to_string())),
    }
}

/// Verifies a completed upload and registers it.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the upload is incomplete, when what arrived does
/// not match what was declared, or when the item cannot be registered.
pub async fn verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(upload_id): Path<String>,
) -> ApiResult<Json<MediaItem>> {
    authenticate(&state, &headers)?;

    let store = state
        .media_store()
        .ok_or_else(|| ApiFailure::internal("this server has nowhere to store media"))?
        .clone();

    let upload = state
        .with_uploads(|uploads| uploads.get(&upload_id).cloned())
        .ok_or_else(|| ApiFailure::not_found("no such upload"))?;

    if !upload.is_complete() {
        return Err(ApiFailure::conflict(
            "incomplete",
            format!(
                "{} of {} bytes have arrived",
                upload.received_bytes, upload.declared.byte_size
            ),
        ));
    }

    let path = store
        .incoming_path_for(&upload_id)
        .ok_or_else(|| ApiFailure::not_found("no such upload"))?;

    let reading = match check_content(&path, &upload.declared) {
        Ok(reading) => reading,
        Err(reason) => {
            // The item stays, marked failed and carrying the reason. An
            // operator who sees nothing at all has nothing to act on.
            fail_item(&state, &upload.media_item_id, &reason);
            state.with_uploads(|uploads| uploads.take(&upload_id));
            store.discard(&upload_id);
            return Err(ApiFailure::bad_request("verification_failed", reason));
        }
    };

    // The extension comes from the file rather than from a profile, because
    // there is no longer a profile and the server did not produce this file
    //. The probed container wins over the declared one where there
    // was a probe, for the same reason every other reading does.
    let container = reading.as_ref().map_or_else(
        || upload.declared.container.clone(),
        |read| read.container.clone(),
    );
    let object_key = MediaAsset::object_key_for(
        &upload.declared.sha256,
        &storage_extension(&upload.declared.original_filename, &container),
    )
    .ok_or_else(|| ApiFailure::internal("could not name a place for this content"))?;

    store
        .commit(&upload_id, &object_key)
        .map_err(|error| ApiFailure::from_write("could not store this content", &error))?;
    state.with_uploads(|uploads| uploads.take(&upload_id));

    // What the item already carries, because the rest of it is rebuilt from
    // the file and the declaration below.
    //
    // **When it was first created**: an item an import left waiting for its
    // content keeps its own date, because stamping it with now would make an
    // imported library look as though it appeared today.
    //
    // **Where it is filed**: the folder was written when the upload was
    // declared, and rebuilding the item without it would file every clip at
    // the root however carefully it was dropped into a folder — which is the
    // half of this an operator sees (owner, 2026-09-23).
    let (created_at, folder) = state
        .with_database(|database| database.load_media_item(upload.media_item_id))
        .map_or_else(
            |_| (Timestamp::now(), upload.declared.folder.clone()),
            |item| (item.created_at, item.folder),
        );

    // What the library shows an operator comes from the file where the file
    // could be read, and from the request only where it could not. A length or
    // a rate that came from a client is a number nobody checked, and a cue built
    // on it ends somewhere other than where it looks like it will.
    let item = MediaItem {
        id: upload.media_item_id,
        kind: upload.declared.kind,
        dimensions: Some(
            reading
                .as_ref()
                .map_or(upload.declared.dimensions, |read| read.size),
        ),
        duration_ms: reading
            .as_ref()
            .and_then(|read| read.duration_ms)
            .or(upload.declared.duration_ms),
        frame_rate: reading
            .as_ref()
            .and_then(|read| read.rate)
            .or(upload.declared.frame_rate),
        has_alpha: reading
            .as_ref()
            .map_or(upload.declared.has_alpha, |read| read.has_alpha),
        created_at,
        folder,
        ..MediaItem::importing(
            &upload.declared.display_name,
            &upload.declared.original_filename,
        )
    };
    let asset = MediaAsset {
        id: ovs_types::id::MediaAssetId::new(),
        media_item_id: item.id,
        object_key,
        sha256: upload.declared.sha256.clone(),
        byte_size: upload.declared.byte_size,
        container,
        codec: reading
            .as_ref()
            .map_or_else(|| upload.declared.codec.clone(), |read| read.codec.clone()),
        validation: AssetValidation::Verified {
            at: Timestamp::now(),
        },
        created_at: Timestamp::now(),
    };

    // Taken from where the content now lives rather than from the staged copy,
    // which has been renamed by this point.
    if let Some(stored) = store.existing_path_for(&asset.object_key) {
        make_thumbnail(&state, item.id, &stored);
    }

    state
        .with_database(|database| database.register_media(&item, &asset))
        .map_err(|error| ApiFailure::conflict("register_failed", error.to_string()))?;

    let stored = state.with_database(|database| database.load_media_item(item.id))?;
    tracing::info!(media_item_id = %item.id, "media is ready");
    Ok(Json(stored))
}

/// Abandons an upload in progress.
///
/// # Errors
///
/// Returns [`ApiFailure`] when there is no such upload.
pub async fn cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(upload_id): Path<String>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;

    let upload = state
        .with_uploads(|uploads| uploads.take(&upload_id))
        .ok_or_else(|| ApiFailure::not_found("no such upload"))?;

    if let Some(store) = state.media_store() {
        store.discard(&upload_id);
    }
    fail_item(&state, &upload.media_item_id, "the upload was cancelled");

    Ok(StatusCode::NO_CONTENT)
}

/// Forgets uploads nothing has arrived for in a long time.
///
/// The staged file goes, and the item it would have become is marked failed
/// with a reason. An item stuck at "importing" forever is worse than one that
/// says what happened: the first looks like the server is still working.
fn sweep_abandoned(state: &AppState, store: &crate::media_store::MediaStore) {
    let abandoned = state.with_uploads(|uploads| uploads.take_abandoned(std::time::Instant::now()));

    for (upload_id, upload) in abandoned {
        tracing::info!(%upload_id, "abandoning an upload nothing has arrived for");
        store.discard(&upload_id);
        fail_item(
            state,
            &upload.media_item_id,
            "the upload stopped and was abandoned",
        );
    }
}

/// Checks a request against itself, before anything is created.
fn check(request: &CreateUploadRequest) -> Result<(), ApiFailure> {
    if request.display_name.trim().is_empty() {
        return Err(ApiFailure::bad_request(
            "invalid_name",
            "a media item needs a name",
        ));
    }
    if !is_lowercase_hex_hash(&request.sha256) {
        return Err(ApiFailure::bad_request(
            "invalid_hash",
            "sha256 must be 64 lowercase hex characters",
        ));
    }
    if request.byte_size == 0 || request.byte_size > MAX_UPLOAD_BYTES {
        return Err(ApiFailure::bad_request(
            "invalid_size",
            format!("byte_size must be between 1 and {MAX_UPLOAD_BYTES}"),
        ));
    }
    if request.dimensions.width == 0 || request.dimensions.height == 0 {
        return Err(ApiFailure::bad_request(
            "invalid_dimensions",
            "media with no width or height cannot be shown",
        ));
    }
    if request.kind == MediaKind::Video && request.duration_ms.is_none() {
        return Err(ApiFailure::bad_request(
            "missing_duration",
            "a video must declare its duration",
        ));
    }
    if request.kind == MediaKind::Image
        && (request.duration_ms.is_some() || request.frame_rate.is_some())
    {
        return Err(ApiFailure::bad_request(
            "invalid_duration",
            "a still image has no duration and no frame rate",
        ));
    }
    // Checked here rather than at the database, so a folder that cannot exist
    // is refused before a byte is sent instead of after the whole file has
    // been uploaded.
    if let Err(problem) = ovs_types::media::check_folder(&request.folder) {
        return Err(ApiFailure::bad_request(
            "invalid_folder",
            problem.to_string(),
        ));
    }

    Ok(())
}

/// Returns true when `hash` is 64 lowercase hex characters.
fn is_lowercase_hex_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Checks what arrived against what was declared.
///
/// Returns what the file turned out to be, when this build could read it, or
/// the reason it failed in the words an operator should see.
///
/// A video is opened and read rather than taken on the client's word. What is
/// refused is a file this build cannot decode, and a size that disagrees with
/// what was declared — the one that would make the library lie about the show.
/// The codec is no longer checked against a declaration, because the server
/// accepts whatever it can play: what the probe reads is simply what
/// is recorded. A length or a frame rate that disagrees is not refused either,
/// because a client computing a duration slightly differently is not a reason to
/// reject an upload an operator has been waiting for.
fn check_content(
    path: &std::path::Path,
    declared: &CreateUploadRequest,
) -> Result<Option<ovs_media::Probe>, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("the staged file could not be read: {error}"))?;

    let byte_size = bytes.len() as u64;
    if byte_size != declared.byte_size {
        return Err(format!(
            "{byte_size} bytes arrived, but {} were declared",
            declared.byte_size
        ));
    }

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex(&hasher.finalize());
    if actual != declared.sha256 {
        return Err("the content does not match the hash that was declared".to_owned());
    }

    if declared.kind == MediaKind::Video {
        return check_video(path, declared);
    }

    // A still is opened as an image, which is both the check and the reading.
    let decoded = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|error| format!("this file could not be read as an image: {error}"))?
        .into_dimensions()
        .map_err(|error| format!("this file could not be read as an image: {error}"))?;

    if decoded != (declared.dimensions.width, declared.dimensions.height) {
        return Err(format!(
            "the image is {}x{}, not the {}x{} that was declared",
            decoded.0, decoded.1, declared.dimensions.width, declared.dimensions.height
        ));
    }

    Ok(None)
}

/// Says why a file could not be read, in the operator's own words for it.
///
/// `FFmpeg`'s message names the staged file, which is a path in this server's
/// spool directory under a generated upload identifier. It is exactly right and
/// completely useless to the person who chose the file: what they can act on is
/// the name they saw in the picker.
fn unreadable(error: ovs_media::ProbeError, filename: &str) -> String {
    use ovs_media::ProbeError;

    match error {
        ProbeError::NoDecoder { codec, .. } => {
            format!("{filename} holds {codec}, and this server has no decoder for it")
        }
        ProbeError::NoVideo { .. } => format!("there is no video in {filename}"),
        ProbeError::TooLarge { width, height, .. } => format!(
            "{filename} is {width}x{height}, which is larger than anything this server can show"
        ),
        ProbeError::Unreadable { reason, .. } => {
            format!("{filename} could not be read: {reason}")
        }
        // Handled by the caller, which returns before reaching here.
        ProbeError::Unavailable => format!("{filename} could not be read"),
    }
}

/// Reads a video and checks it is what the upload said it was.
fn check_video(
    path: &std::path::Path,
    declared: &CreateUploadRequest,
) -> Result<Option<ovs_media::Probe>, String> {
    let read = match ovs_media::probe(path) {
        Ok(read) => read,
        Err(ovs_media::ProbeError::Unavailable) => {
            // A development build with no FFmpeg. Said out loud rather than
            // passed over: an upload that was never checked should not look
            // like one that was.
            tracing::warn!(
                media_item = %declared.display_name,
                "this build cannot read media files, so this upload is taken on the client's word"
            );
            return Ok(None);
        }
        Err(error) => return Err(unreadable(error, &declared.original_filename)),
    };

    // What the client thought it had, against what is actually there. Not a
    // refusal: the server records its own reading either way, and the two
    // disagreeing is worth a line in the log rather than a failed ingest.
    if !declared.codec.is_empty() && declared.codec != read.codec {
        tracing::info!(
            media_item = %declared.display_name,
            declared = %declared.codec,
            found = %read.codec,
            "the client read this file as one codec and the server reads it as another"
        );
    }

    if read.size != declared.dimensions {
        return Err(format!(
            "the video is {}x{}, not the {}x{} that was declared",
            read.size.width,
            read.size.height,
            declared.dimensions.width,
            declared.dimensions.height
        ));
    }

    check_profile(path, &declared.original_filename)?;

    Ok(Some(read))
}

/// Refuses a video that is not one of the three things this server plays.
///
/// The owner's decision, 2026-09-23: **HEVC 8-bit, HEVC 10-bit
/// and HAP, and nothing else.** Not a preference — a file outside them either
/// costs a decoder this machine cannot spare seven times over, or costs a
/// per-frame conversion, or seeks in seconds rather than in frames.
///
/// The client is expected to transcode before it uploads, so an operator
/// should never meet this. It is here because *should never* is not a
/// guarantee: an older client, a script, or a client whose own probe read the
/// file differently all reach this endpoint, and the library must not end up
/// holding something the wall cannot show.
///
/// # What it costs
///
/// One pass over the staged file without decoding it, because the keyframe
/// interval is in the packets and nowhere else — see [`ovs_media::profile_of`].
/// The file has just been written and hashed, so it is warm.
fn check_profile(path: &std::path::Path, filename: &str) -> Result<(), String> {
    use ovs_types::media::{faults, PlaybackProfile};

    let video = match ovs_media::profile_of(path) {
        Ok(video) => video,
        Err(ovs_media::ProbeError::Unavailable) => return Ok(()),
        Err(error) => return Err(unreadable(error, filename)),
    };

    let profile = PlaybackProfile::of(&video);
    let problems = faults(&video, profile);
    if problems.is_empty() {
        return Ok(());
    }

    // Every fault, not the first. Somebody fixing a file wants the whole list
    // in one go rather than one refusal per re-encode.
    let listed = problems
        .iter()
        .map(|fault| {
            format!(
                "{}: wants {}, has {}",
                fault.what, fault.wanted, fault.found
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!(
        "{filename} is not something this server plays. It needs to be HEVC \
         8-bit, HEVC 10-bit or HAP, and this one is {} \u{2014} {listed}",
        video.codec
    ))
}

/// How large a thumbnail is, on its longest edge.
///
/// A library shows a grid of these and an operator reads them at a glance, so
/// what matters is telling one clip from another rather than seeing detail. At
/// this size a picture is a few kilobytes and a library of two hundred is one
/// short scroll rather than a download.
const THUMBNAIL_SIDE: u32 = 320;

/// How large one frame of a moving preview is, on its longest edge.
///
/// Half the still's, because there are ten of them and they are drawn at the
/// size of a slot pad — "Es sollte 160x90px reichen als Vorschau" (owner,
/// 2026-09-23). Ten frames at this size come to about the weight of one still
/// at the other.
const STRIP_FRAME_SIDE: u32 = 160;

/// How many frames a moving preview is made of.
///
/// Ten, across the whole clip, which is what the owner asked for: a twenty
/// second clip gives one every two seconds and a ten second clip one every
/// second. Enough to say what happens in a clip, and few enough that the strip
/// stays a strip.
const STRIP_FRAMES: u32 = 10;

/// Takes a picture of what was just registered, if one can be taken.
///
/// Never a reason to fail an upload. A library entry with no picture is a
/// library entry with a name on it, which is what every one of them was until
/// now; an upload refused because a thumbnail could not be made would be a
/// working file an operator cannot use.
fn make_thumbnail(
    state: &AppState,
    media_item_id: ovs_types::id::MediaItemId,
    path: &std::path::Path,
) {
    let Some(store) = state.media_store() else {
        return;
    };

    let png = match std::fs::read(path)
        .ok()
        .and_then(|bytes| still_thumbnail(&bytes))
    {
        Some(png) => Some(png),
        None => moving_thumbnail(path),
    };

    let Some(png) = png else {
        tracing::debug!(%media_item_id, "no picture could be taken of this item");
        return;
    };

    if let Err(error) = store.put_thumbnail(media_item_id, &png) {
        tracing::warn!(%media_item_id, "the thumbnail could not be written: {error}");
    }

    // And the moving one, for anything that moves. A still needs none — "Bilder
    // die Hochgeladen werden brauchen kein Gif" — and a clip that could not be
    // read for one keeps its still, which is what every item had until now.
    if let Some(strip) = moving_preview(path) {
        if let Err(error) = store.put_strip(media_item_id, &strip) {
            tracing::warn!(%media_item_id, "the moving preview could not be written: {error}");
        }
    }
}

/// Ten frames of a clip in one picture, side by side and in order.
///
/// **One picture rather than ten**, and rather than an animated image. Ten
/// files would be ten requests per pad in a matrix of eighty-four; an animated
/// image would play whenever it is on screen, and what the owner asked for is
/// a pad that shows its first frame until the pointer is over it (2026-09-23).
/// A strip does both: the client shows one frame of it, and steps through the
/// rest when it should be moving.
fn moving_preview(path: &std::path::Path) -> Option<Vec<u8>> {
    let frames = ovs_media::frames(path, STRIP_FRAMES, STRIP_FRAME_SIDE).ok()?;
    let first = frames.first()?;
    let (width, height) = (first.size.width, first.size.height);
    // Every frame at the size of the first. A clip whose size changes part way
    // through is not something this has to handle, but a frame that came back
    // at another size must not shift the ones after it along the strip.
    let usable: Vec<&ovs_media::Thumbnail> = frames
        .iter()
        .filter(|frame| frame.size.width == width && frame.size.height == height)
        .collect();
    // Two frames is the least that is worth calling a preview. One means the
    // clip did not say how long it is and could not be divided, and ten copies
    // of one picture is a still with a cost.
    if usable.len() < 2 {
        return None;
    }

    // **Always exactly `STRIP_FRAMES` wide**, with the last frame repeated to
    // fill it. The window shows one tenth of the strip at a time and steps
    // through the tenths; a strip of seven would have it showing seven tenths
    // of a frame. A clip too short to give ten simply holds on its last one,
    // which is what it does on the wall too.
    let mut strip = image::RgbaImage::new(width.checked_mul(STRIP_FRAMES)?, height);
    for column in 0..STRIP_FRAMES {
        let frame = usable
            .get(column as usize)
            .or_else(|| usable.last())
            .copied()?;
        let buffer: image::RgbaImage =
            image::ImageBuffer::from_raw(width, height, frame.pixels.clone())?;
        let at = column.checked_mul(width)?;
        image::imageops::replace(&mut strip, &buffer, i64::from(at), 0);
    }

    // **JPEG rather than PNG** (owner, 2026-09-23): "Es reicht auch eine JPG
    // Sequenz, die der Client bei Hover selber wiedergibt." Ten frames of
    // photographic content compress to a fraction of what a lossless picture
    // of them costs, and a library of two hundred clips is two hundred of
    // these. Nothing reads a pixel of it — it is looked at.
    //
    // Opaque first, because JPEG has no alpha and the encoder would refuse the
    // RGBA buffer rather than flatten it.
    encode_jpeg(image::DynamicImage::ImageRgb8(
        image::DynamicImage::ImageRgba8(strip).to_rgb8(),
    ))
}

/// JPEG, at a quality that is plenty for a picture this size.
fn encode_jpeg(image: image::DynamicImage) -> Option<Vec<u8>> {
    let mut jpeg = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut jpeg), 80);
    encoder.encode_image(&image).ok()?;
    Some(jpeg)
}

/// A picture of a still, which is the still itself made small.
fn still_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let decoded = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;

    let small = decoded.thumbnail(THUMBNAIL_SIDE, THUMBNAIL_SIDE);
    encode_png(small)
}

/// A picture of a clip, which is one frame of it.
fn moving_thumbnail(path: &std::path::Path) -> Option<Vec<u8>> {
    let picture = ovs_media::thumbnail(path, THUMBNAIL_SIDE).ok()?;
    let buffer: image::RgbaImage =
        image::ImageBuffer::from_raw(picture.size.width, picture.size.height, picture.pixels)?;
    encode_png(image::DynamicImage::ImageRgba8(buffer))
}

/// PNG, because that is what a browser draws without being told anything.
fn encode_png(image: image::DynamicImage) -> Option<Vec<u8>> {
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// Says where one upload stands.
///
/// A client that was interrupted knows what it sent; it does not know what
/// arrived. Without this the only recoveries are to trust its own count or to
/// start again from nothing, and a venue's network is exactly where that
/// difference is a twenty-minute wait.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not authenticated, or when there
/// is no such upload — which includes one the server has swept, so a client
/// that gets this should declare again rather than retry.
pub async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(upload_id): Path<String>,
) -> ApiResult<Json<UploadSession>> {
    authenticate(&state, &headers)?;

    let upload = state
        .with_uploads(|uploads| uploads.get(&upload_id).cloned())
        .ok_or_else(|| ApiFailure::not_found("no such upload"))?;

    Ok(Json(UploadSession {
        upload_id,
        media_item_id: upload.media_item_id,
        byte_size: upload.declared.byte_size,
        received_bytes: upload.received_bytes,
        chunk_size: crate::uploads::CHUNK_SIZE,
    }))
}

/// Marks an item failed, with the reason an operator should see.
fn fail_item(state: &AppState, media_item_id: &ovs_types::id::MediaItemId, reason: &str) {
    let failed = state.with_database(|database| {
        let mut item = database.load_media_item(*media_item_id)?;
        item.state = ovs_types::media::MediaState::Failed;
        item.failure_reason = Some(reason.to_owned());
        database.upsert_media_item(&item)
    });

    if let Err(error) = failed {
        tracing::warn!(%media_item_id, "could not record why an import failed: {error}");
    }
}

/// Writes a byte count the way an operator would say it.
///
/// Rounded to one decimal and in powers of two, because the numbers this
/// appears in are a file an operator can see the size of in their own file
/// manager, and a figure that disagrees with the one on their screen reads as a
/// different fact rather than as a different convention.
fn in_bytes(count: u64) -> String {
    const UNITS: [(u64, &str); 3] = [
        (1024 * 1024 * 1024, "GB"),
        (1024 * 1024, "MB"),
        (1024, "kB"),
    ];

    for (size, name) in UNITS {
        if count >= size {
            return format!("{:.1} {name}", count as f64 / size as f64);
        }
    }
    format!("{count} bytes")
}

/// Writes `bytes` at `offset`, extending the file.
fn append_at(path: &std::path::Path, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};

    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)?;
    file.sync_data()
}

/// Lowercase hex, which is the only form a storage key accepts.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::in_bytes;

    #[test]
    fn a_size_is_written_the_way_an_operator_would_say_it() {
        assert_eq!(in_bytes(0), "0 bytes");
        assert_eq!(in_bytes(512), "512 bytes");
        assert_eq!(in_bytes(1024), "1.0 kB");
        assert_eq!(in_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(in_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        assert_eq!(in_bytes(256 * 1024 * 1024 * 1024), "256.0 GB");
    }
}
