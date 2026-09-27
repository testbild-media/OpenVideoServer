//! Previews: what each output is actually showing.
//!
//! ```text
//! GET /preview            what there are pictures of
//! GET /preview/{output}   the picture, as a PNG
//! ```
//!
//! # Why this is not part of the state stream
//!
//! A preview is what an output looked like a moment ago, which is only ever
//! interesting now. Putting it on the update stream would mean every client
//! receiving every output's picture whether it is looking at one or not, and
//! sequencing something whose whole value is that it is the latest.
//!
//! # Why the server never makes one up
//!
//! A preview comes from the renderer or it does not exist. An output with no
//! picture answers 404 rather than a black frame, because "the renderer has not
//! sent one" and "the wall is black" are different things, and the second is the
//! one an operator needs to be able to see.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ovs_proto::http::PreviewSwitchRequest;
use ovs_types::id::OutputId;

use crate::server::app_state::AppState;
use crate::server::auth_routes::authenticate;
use crate::server::error::{ApiFailure, ApiResult};

/// One output there is a picture of.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreviewEntry {
    /// Which output.
    pub output_id: OutputId,
    /// Its operator-visible name.
    pub name: String,
    /// When the picture arrived.
    pub received_at: ovs_types::time::Timestamp,
    /// Size of the picture in bytes.
    pub byte_size: u64,
}

/// What there are pictures of.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreviewListResponse {
    /// One entry per output with a picture, oldest identifier first.
    pub outputs: Vec<PreviewEntry>,
}

/// Lists the outputs there are pictures of.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not authenticated.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<PreviewListResponse>> {
    authenticate(&state, &headers)?;

    let outputs = state
        .previews()
        .into_iter()
        .map(|preview| PreviewEntry {
            output_id: preview.output_id,
            name: preview.name,
            received_at: preview.received_at,
            byte_size: preview.png.len() as u64,
        })
        .collect();

    Ok(Json(PreviewListResponse { outputs }))
}

/// Turns previews on or off.
///
/// A preview costs a readback from the GPU and an encode. Small, but not
/// nothing, and on a machine at its limit an operator needs to be able to take
/// it back. Remembered across restarts: somebody who
/// switched them off to get a show through the night should not find them back
/// on in the morning.
pub async fn set_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PreviewSwitchRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;

    state.set_previews_enabled(request.enabled)?;
    tracing::info!(enabled = request.enabled, "previews switched");
    Ok(StatusCode::NO_CONTENT)
}

/// Returns the latest picture of the whole composition, as a PNG.
///
/// What the show is, rather than what one wall has. On a rig with six walls the
/// per-output previews are six different answers and this is the one they were
/// all cut from.
///
/// # Errors
///
/// Returns 404 until a renderer has sent one. That is the honest answer while
/// nothing is rendering, and it is not a fault.
pub async fn composition(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    authenticate(&state, &headers)?;

    let png = state.composition_preview().ok_or_else(|| {
        ApiFailure::not_found(
            "there is no picture of the composition; the renderer has not sent one",
        )
    })?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        png,
    )
        .into_response())
}

/// Returns the latest picture of one output, as a PNG.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not authenticated, the identifier
/// is not one, or there is no picture of that output.
pub async fn show(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(output_id): Path<String>,
) -> ApiResult<Response> {
    authenticate(&state, &headers)?;

    let output_id: OutputId = output_id.parse().map_err(|_| {
        ApiFailure::bad_request("invalid_id", format!("{output_id:?} is not a valid id"))
    })?;

    let preview = state.preview(output_id).ok_or_else(|| {
        ApiFailure::not_found("there is no picture of that output; the renderer has not sent one")
    })?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            // A preview is superseded several times a second, so a cached one is
            // a stale one, and a stale picture of a wall is worse than none.
            (header::CACHE_CONTROL, "no-store"),
        ],
        preview.png,
    )
        .into_response())
}
