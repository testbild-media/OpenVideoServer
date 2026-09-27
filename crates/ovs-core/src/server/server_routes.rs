//! Server capability, status and configuration endpoints.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use ovs_proto::fixtures::FixtureCatalogueResponse;
use ovs_proto::http::{
    capabilities as build_capabilities, CapabilitiesResponse, PowerAction, PowerResponse,
    StatusResponse,
};
use ovs_types::config::{DmxProtocols, StartupPolicy};
use ovs_types::telemetry::ServerTelemetry;

use crate::server::app_state::AppState;
use crate::server::auth_routes::{administer, authenticate};
use crate::server::error::ApiResult;

/// Reports what this server can do.
///
/// Unauthenticated: a client needs it before it can sign in, and it is how the
/// client learns whether first-run setup is still needed.
pub async fn capabilities(State(state): State<AppState>) -> ApiResult<Json<CapabilitiesResponse>> {
    let configured = state.with_database(|database| database.account_count())? > 0;

    // What the renderer found, with what an operator has decided about each.
    // Empty until a renderer has connected and said, which is different from
    // "this machine has no connectors" — a client offering sockets to assign has
    // to be able to tell those apart.
    let connectors = state.connector_infos();

    // And the sockets on every other card, which the renderer cannot drive and
    // which are therefore the System screen's to have.
    let connectors_elsewhere = state.connectors_elsewhere();

    // Asked of this binary every time rather than cached: it is a handful of
    // lookups in a table FFmpeg compiled in, and a cached answer is one more
    // thing that could be wrong after the libraries under the server change.
    Ok(Json(build_capabilities(
        state.server_version(),
        configured,
        connectors,
        connectors_elsewhere,
        ovs_media::playable_codecs(),
    )))
}

/// Returns every DMX personality this build publishes.
///
/// What somebody sits down with to programme a console by hand, and what the
/// patch editor lays a patch out from. Served rather than documented only,
/// because the operator programming the desk is usually standing in front of
/// the machine and the version that matters is the one it is running.
pub async fn fixtures(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<FixtureCatalogueResponse>> {
    authenticate(&state, &headers)?;
    Ok(Json(ovs_proto::fixtures::catalogue()))
}

/// Reports measured server state.
pub async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<StatusResponse>> {
    authenticate(&state, &headers)?;

    let startup_policy = state.with_database(|database| database.startup_policy())?;
    let dmx_protocols = state.with_database(|database| database.dmx_protocols())?;
    let (show, renderer) =
        state.with_state(|current| (current.show().cloned(), current.renderer().clone()));

    // What the machine is doing comes from the background sample, so this
    // reads it rather than measuring it. GPU figures are still absent rather
    // than zero: they come from the renderer, which is the process that has the
    // device open, and nothing here can invent them (house rule §16).
    let host = state.host();
    let gpu = state.gpu_telemetry();
    let playback = state.playback_telemetry();
    let clock = state.clock_telemetry();
    let telemetry = ServerTelemetry {
        version: state.server_version().to_owned(),
        uptime_s: state.uptime_s(),
        ip_address: host.ip_address,
        current_show: show.as_ref().map(|show| show.id),
        current_show_name: show.map(|show| show.name),
        renderer,
        cpu_percent: host.cpu_percent,
        cpu_temperature_c: host.cpu_temperature_c,
        ram_percent: host.ram_percent,
        ram_used_bytes: host.ram_used_bytes,
        ram_total_bytes: host.ram_total_bytes,
        // From the driver's own management library, which the renderer opens by
        // name at runtime where a machine has one. Absent rather than zero on a
        // machine that does not: a card at 0% and a card nobody asked must not
        // look the same on a console.
        gpu_percent: gpu
            .as_ref()
            .and_then(|gpu| gpu.utilisation_percent)
            .map(|percent| percent as f32),
        gpu_temperature_c: gpu
            .as_ref()
            .and_then(|gpu| gpu.temperature_c)
            .map(|degrees| degrees as f32),
        vram_used_bytes: gpu.as_ref().and_then(|gpu| gpu.vram_used_bytes),
        vram_total_bytes: gpu.as_ref().and_then(|gpu| gpu.vram_total_bytes),
        disk_used_bytes: host.disk_used_bytes,
        disk_total_bytes: host.disk_total_bytes,
        active_decoders: playback.map_or(0, |playback| playback.playing),
        decode_errors: playback.map_or(0, |playback| playback.errors),
        late_frames: playback.map_or(0, |playback| playback.late_frames),
        test_pattern: state.test_pattern(),
        outputs: state.output_telemetry(),
        missing_outputs: state.missing_outputs(),
        // Where the renderer's last frame went. From the renderer, because it is
        // the only process that draws one.
        frame: state.frame_timings(),
        dmx: state.dmx_telemetry(),
        clock,
        // What the renderer said, not what the core worked out. Only the
        // process that decodes knows where a clip has got to, and a layer it
        // said nothing about is absent rather than at zero (house rule §16).
        playheads: state.playheads(),
    };

    Ok(Json(StatusResponse {
        telemetry,
        startup_policy,
        dmx_protocols,
        previews_enabled: state.previews_enabled(),
    }))
}

/// Returns every generator this build publishes.
///
/// What a client offers when an operator adds a source that is not a file, and
/// what tells it how to draw the controls beside it.
/// Every effect this build publishes.
///
/// What a client draws an effect list and its controls from. Separate from the
/// generators because they are different things — one makes a picture, the other
/// changes one — and an operator picks from one list or the other, never both at
/// once.
pub async fn effects(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<ovs_proto::effects::EffectCatalogueResponse>> {
    authenticate(&state, &headers)?;
    Ok(Json(ovs_proto::effects::catalogue()))
}

pub async fn generators(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<ovs_proto::generators::GeneratorCatalogueResponse>> {
    authenticate(&state, &headers)?;
    Ok(Json(ovs_proto::generators::catalogue()))
}

/// Restarts the renderer.
///
/// Answers as soon as the request is recorded rather than waiting for the
/// renderer to come back: the process that would report success is the one
/// being stopped, and an operator watching the console sees the status go to
/// `unreachable` and then to `running` on its own.
pub async fn restart_renderer(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    // It takes the wall down for a moment, which nobody should be able to do to
    // somebody else's show by accident.
    administer(&state, &headers)?;
    state.request_renderer_restart();
    Ok(StatusCode::ACCEPTED)
}

/// Says what one connector on this machine is for.
///
/// Machine configuration, not show data: which socket carries a show and
/// which carries the Service Display describes the rack, and moving a show
/// between servers must not move the wiring with it.
///
/// The renderer is told immediately, but only rebuilds its outputs when it
/// restarts — changing what a socket is for is a commissioning action, not a
/// live one, and doing it under a running show would mean a wall going dark
/// between one frame and the next.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not authenticated, or when the
/// setting cannot be written.
pub async fn set_connector_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ovs_proto::http::SetConnectorRoleRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;

    state.set_connector_role(&request.gpu_pci_address, &request.connector, request.role)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Puts the operator-visible counters back to zero.
///
/// Dropped frames, late frames and decoder errors — the three numbers on the
/// System screen that only mean anything relative to when somebody last looked.
/// The lifetime counts underneath are untouched: "has this machine ever dropped
/// a frame" is a different question, and the answer to it is not an operator's
/// to erase.
///
/// Accepted rather than done: the counters live in the renderer, and this is a
/// request on its way there. The next telemetry says what they are.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not authenticated.
pub async fn reset_counters(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Option<Json<ovs_proto::http::ResetCountersRequest>>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    // No body means all of them: the simplest thing a client can send, and the
    // thing a client that knows nothing about groups means.
    let group = request
        .map(|Json(request)| request.group)
        .unwrap_or_default();
    state.request_counter_reset(group);
    Ok(StatusCode::ACCEPTED)
}

/// Puts the commissioning test pattern on every output, or takes it off.
///
/// A service action, not a show one: it replaces what the composition holds
/// while leaving the piece mapping alone, so each output shows its own region
/// of the pattern and an engineer can tell a mis-mapped wall from a mis-cabled
/// one. It touches no show and is never persisted.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the request is not authenticated.
pub async fn set_test_pattern(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ovs_proto::http::TestPatternRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;

    state.set_test_pattern(request.mode, request.highlighted);
    Ok(StatusCode::NO_CONTENT)
}

/// Reboots or shuts the machine down.
///
/// The console confirms before calling this. The server does not second-guess
/// it: an operator who has confirmed a reboot in front of the machine has said
/// what they mean.
pub async fn power(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(action): Json<PowerAction>,
) -> ApiResult<Json<PowerResponse>> {
    // **Turning the machine off is not driving the show.** The clearest case
    // for the split the owner asked for on 2026-09-24: a guest at the desk can
    // reach every pad on it and cannot reach this.
    administer(&state, &headers)?;

    tracing::warn!(?action, "an operator asked the machine to stop");
    let outcome = crate::power::request(action);
    if !outcome.accepted {
        tracing::error!(?action, "{}", outcome.detail);
    }

    Ok(Json(PowerResponse {
        accepted: outcome.accepted,
        detail: outcome.detail,
    }))
}

/// Sets what this server opens on start.
/// Sets which DMX-over-IP protocols reach the show.
///
/// Two switches rather than one, because a rig is not one thing: a venue's
/// house desk may speak sACN while a visiting console brings Art-Net, and an
/// operator who wants one of them to stop wants the other to keep going
/// (owner, 2026-09-23).
///
/// **Switched off means it does not drive anything, not that it is not
/// listened for.** The sockets stay open and the status keeps saying what is on
/// the wire, exactly as the master's own DMX switch does for both at once: an
/// operator who has taken a protocol off the show still has to be able to tell
/// that from a cable that has come out.
///
/// Takes effect at once. The link reads this every pass rather than at start,
/// so nothing has to be restarted for a switch to mean something.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the setting cannot be stored.
pub async fn set_dmx_protocols(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(protocols): Json<DmxProtocols>,
) -> ApiResult<StatusCode> {
    // Which protocols this machine listens to is how it is wired into a venue,
    // not a thing about tonight's show.
    administer(&state, &headers)?;
    state.with_database(|database| database.set_dmx_protocols(protocols))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn set_startup_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(policy): Json<StartupPolicy>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    state.with_database(|database| database.set_startup_policy(policy))?;
    Ok(StatusCode::NO_CONTENT)
}
