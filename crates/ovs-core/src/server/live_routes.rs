//! Live control: what an operator does during a show.
//!
//! ```text
//! POST /live/layers/{layer_id}/slot   select a slot
//! PUT  /live/layers/{layer_id}        opacity, transform, bus, transport, enable
//! PUT  /live/effects/{effect_id}      effect parameters, live over configured
//! PUT  /live/generators/{id}         generator parameters, the same way
//! PUT  /live/master                   dimmer, crossfader, BPM, speed, freeze
//! POST /live/tap                      tap tempo
//! POST /live/phase/reset              put the beat back on the downbeat
//! ```
//!
//! # Why these are HTTP and not the realtime socket
//!
//! The socket carries state outwards. It is not a second command channel
//!: a command needs a bearer token, an answer, and a status
//! code the client can act on, and a fire-and-forget message over a socket has
//! none of those. What a command *changes* comes back over the socket, the same
//! as any other change, so every client sees it and not just the one that asked.
//!
//! # A command that changes nothing answers the same as one that does
//!
//! A console re-sending the same DMX frame forty times a second, an operator
//! re-selecting the slot that is already running: neither is an error and
//! neither puts anything on the update stream. The status code says "your
//! instruction was carried out", not "something moved".

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use ovs_proto::http::{
    SelectSlotRequest, TapTempoResponse, UpdateEffectRequest, UpdateLayerRequest,
    UpdateMasterRequest,
};
use ovs_types::id::{EffectInstanceId, GeneratorInstanceId, LayerId};

use crate::server::app_state::AppState;
use crate::server::auth_routes::authenticate;
use crate::server::error::{ApiFailure, ApiResult};

fn parse_id<T: std::str::FromStr>(raw: &str) -> ApiResult<T> {
    raw.parse()
        .map_err(|_| ApiFailure::bad_request("invalid_id", format!("{raw:?} is not a valid id")))
}

/// Selects a slot on a layer.
///
/// # Errors
///
/// Returns [`ApiFailure`] when no show is open, when the layer or slot is not
/// part of the one that is, or when the layer takes its slot from a console
/// and not from here.
pub async fn select_slot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(layer_id): Path<String>,
    Json(request): Json<SelectSlotRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    let layer_id: LayerId = parse_id(&layer_id)?;

    state.mutate_state(|current| {
        // **The other half of the owner's switch** (2026-10-16). A layer set to
        // take its slot from a console is one somebody is cueing from a desk,
        // and the reason that setting exists at all is that a picture on a
        // screen is easy to press by accident. Refused with a reason rather
        // than ignored, because a client can say what happened and a console
        // cannot.
        let from_the_desk = current
            .show()
            .and_then(|show| show.layers.iter().find(|layer| layer.id == layer_id))
            .is_some_and(|layer| !layer.slot_control.client_may_select());
        if from_the_desk {
            return (
                Err(ApiFailure::bad_request(
                    "slot_is_the_desks",
                    "this layer takes its slot from a lighting console; \
                     change that in the layer's settings to select here",
                )),
                None,
            );
        }
        match current.select_slot(layer_id, request.slot_index) {
            Ok(change) => (Ok(()), change),
            Err(error) => (Err(ApiFailure::from(error)), None),
        }
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Changes part of one layer's live state.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the layer is not part of the open show.
pub async fn update_layer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(layer_id): Path<String>,
    Json(request): Json<UpdateLayerRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    let layer_id: LayerId = parse_id(&layer_id)?;

    state.mutate_state(|current| {
        let applied = current.update_layer(layer_id, |live| {
            if let Some(opacity) = request.opacity {
                live.set_opacity(opacity);
            }
            if let Some(transform) = request.transform {
                live.transform = transform;
            }
            if let Some(route) = request.route {
                live.route = route;
            }
            if let Some(blend_mode) = request.blend_mode {
                live.blend_mode = blend_mode;
            }
            if let Some(transport) = request.transport {
                live.transport = transport;
            }
            if let Some(enabled) = request.enabled {
                live.enabled = enabled;
            }
        });

        match applied {
            Ok(change) => (Ok(()), change),
            Err(error) => (Err(ApiFailure::from(error)), None),
        }
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Changes the live values on one effect instance.
///
/// The live counterpart of editing the effect in the show, and deliberately
/// not the same thing. What an operator does here sits on top of what the effect
/// was configured with and is never written back, so a blur pushed during a
/// number leaves the show opening tomorrow at the value it was built with
///. Editing the show is a show endpoint, and this is not a
/// shortcut to it.
///
/// Every value is checked against the effect's manifest, exactly as a value
/// being saved into a show is: a control that accepted a blur radius of four
/// thousand would be a way to stop a show from a client.
///
/// # Errors
///
/// Returns [`ApiFailure`] when no show is open, when the effect instance is
/// not part of the one that is, when this build does not publish that effect, or
/// when a value is not one the effect declares. A refused request changes
/// nothing.
pub async fn update_effect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(effect_instance_id): Path<String>,
    Json(request): Json<UpdateEffectRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    let effect_instance_id: EffectInstanceId = parse_id(&effect_instance_id)?;

    state.mutate_state(|current| {
        let applied =
            current.set_effect_parameters(effect_instance_id, &request.overrides, request.reset);

        match applied {
            Ok(change) => (Ok(()), change),
            Err(error) => (Err(ApiFailure::from(error)), None),
        }
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Changes the live values on one generator instance.
///
/// Everything [`update_effect`] is, for the other kind of module. There is one
/// route each rather than one route for both because the identities are
/// different types, and one parameter model behind them because an effect and a
/// generator are both plugins with declared parameters (owner, 2026-09-23).
///
/// This is also the half that makes the DMX side honest: a console moving a
/// generator's colour and a client moving it go through
/// `set_generator_parameters` and arrive at the renderer the same way. A client
/// that had its own path would be a second definition of what the value is.
///
/// # Errors
///
/// Returns [`ApiFailure`] when no show is open, when the generator instance
/// is not part of the one that is, when this build does not publish that
/// generator, or when a value is not one it declares. A refused request changes
/// nothing.
pub async fn update_generator(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(generator_instance_id): Path<String>,
    Json(request): Json<UpdateEffectRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;
    let generator_instance_id: GeneratorInstanceId = parse_id(&generator_instance_id)?;

    state.mutate_state(|current| {
        let applied = current.set_generator_parameters(
            generator_instance_id,
            &request.overrides,
            request.reset,
        );

        match applied {
            Ok(change) => (Ok(()), change),
            Err(error) => (Err(ApiFailure::from(error)), None),
        }
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Changes part of the live master state.
pub async fn update_master(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateMasterRequest>,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;

    state.mutate_state(|current| {
        let change = current.update_master(|master| {
            if let Some(dimmer) = request.dimmer {
                master.set_dimmer(dimmer);
            }
            if let Some(crossfader) = request.crossfader {
                master.set_crossfader(crossfader);
            }
            if let Some(bpm) = request.bpm {
                master.set_bpm(bpm);
            }
            if let Some(speed_scale) = request.speed_scale {
                master.set_speed_scale(speed_scale);
            }
            if let Some(freeze) = request.freeze {
                master.freeze = freeze;
            }
            if let Some(enabled) = request.dmx_enabled {
                master.dmx_enabled = enabled;
            }
        });
        ((), change)
    });

    Ok(StatusCode::NO_CONTENT)
}

/// Taps the tempo.
///
/// The first tap of a run never changes anything: one tap is not an interval.
/// The answer says so, so a client can show "tapping…" until it is true.
pub async fn tap(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<TapTempoResponse>> {
    authenticate(&state, &headers)?;

    let changed = state.mutate_state(|current| {
        let change = current.tap_tempo(std::time::Instant::now());
        (change.is_some(), change)
    });

    let bpm = state.with_state(|current| current.snapshot().master.bpm);
    Ok(Json(TapTempoResponse { bpm, changed }))
}

/// Puts the beat back on the downbeat, without changing the tempo.
pub async fn reset_phase(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    authenticate(&state, &headers)?;

    // Not a state change: the phase is the clock's, and nothing in the
    // authoritative state describes it. What an operator sees is the next beat
    // landing where they asked for it.
    state.mutate_state(|current| {
        current.reset_phase(std::time::Instant::now());
        ((), None)
    });

    Ok(StatusCode::NO_CONTENT)
}
