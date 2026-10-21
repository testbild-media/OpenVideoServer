//! The edits an operator makes with a show running.
//!
//! ```text
//! PUT    /shows/{id}/layers/{l}/slots/{n}              put a source in a slot
//! DELETE /shows/{id}/layers/{l}/slots/{n}              take what is in it out
//! POST   /shows/{id}/layers/{l}/slots/{n}/move         move it to another pad, any layer
//! POST   /shows/{id}/layers/{l}/effects                add an effect to a layer
//! DELETE /shows/{id}/layers/{l}/effects/{e}            take one off again
//! POST   /shows/{id}/layers/{l}/slots/{n}/effects      add an effect to a slot
//! DELETE /shows/{id}/layers/{l}/slots/{n}/effects/{e}  take one off again
//! PUT    /shows/{id}/layers/{l}/banks                  how much matrix it shows
//! PUT    /shows/{id}/layers/{l}/slots/{n}/settings     everything but its source
//! PUT    /shows/{id}/mapping                          the outputs and their pieces
//! PUT    /shows/{id}/lock                             lock or unlock, administrators only
//! ```
//!
//! # Why these are not saves
//!
//! Everything else about a show is edited by sending the whole document back
//! ([`crate::server::show_routes::save`]), and that is still the rule. These
//! are the exceptions the owner's workflow requires: a clip dragged from the
//! browser onto a pad, an effect dragged onto a slot or a layer, and a right
//! click on a pad asking for the next bank (owner, 2026-09-23). All three are
//! done with a show on.
//!
//! Storing a whole document that the server has open **reloads the show** —
//! every layer back to its opening slot, the master back to its opening dimmer,
//! the clock back to beat zero. That is correct when an operator
//! opens a show and catastrophic when they fill an empty pad between two
//! tracks. So these routes edit the document the server already holds, store
//! the result, and install it without the restart policy running
//! ([`AuthoritativeState::replace_show`]).
//!
//! # They are still whole-document writes
//!
//! The server reads the show, applies the edit, and stores and validates the
//! result as one document. Nothing arrives here half-applied, and a show
//! that would not validate is refused with the document unchanged.
//!
//! # What they are not
//!
//! They are not a general per-field API. Each one is a gesture an operator
//! makes on the live screen, and there is no route here for anything that is
//! done sitting down.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use ovs_proto::http::{
    AddEffectRequest, AddEffectResponse, AssignSlotRequest, AssignSlotResponse, LockTarget,
    MoveSlotRequest, MoveSlotResponse, OrderEffectsRequest, OrderEffectsResponse, SetBanksRequest,
    SetLockRequest, SetLockResponse, SetMappingRequest, SetSlotRequest,
};
use ovs_types::config::{EffectInstance, Layer, Show, Slot, SlotMove, SlotMoveError, SourceRef};
use ovs_types::id::{EffectInstanceId, LayerId, ShowId};
use ovs_types::time::Timestamp;

use crate::server::app_state::AppState;
use crate::server::auth_routes::{administer, authenticate};
use crate::server::error::{ApiFailure, ApiResult};
use crate::state::AuthoritativeState;

/// What the path names, parsed.
struct Target {
    show_id: ShowId,
    layer_id: LayerId,
    slot_index: u16,
}

/// Parses and range-checks what the path names.
///
/// The slot index is checked here rather than at the edit, so that a slot
/// beyond the last bank is refused before anything reads a document.
fn target(show_id: &str, layer_id: &str, slot_index: u16) -> ApiResult<Target> {
    Ok(Target {
        show_id: parse_id(show_id)?,
        layer_id: parse_id(layer_id)?,
        slot_index: {
            Layer::check_slot_index(slot_index)
                .map_err(|error| ApiFailure::bad_request("invalid_slot", error.to_string()))?;
            slot_index
        },
    })
}

fn parse_id<T: std::str::FromStr>(raw: &str) -> ApiResult<T> {
    raw.parse()
        .map_err(|_| ApiFailure::bad_request("invalid_id", format!("{raw:?} is not a valid id")))
}

/// Reads the document the edit is to be applied to.
///
/// The running copy when the show named is the one that is open, and the
/// stored one otherwise. The two agree today, because nothing changes a loaded
/// show without storing it as well; reading the running one is the answer
/// that stays right if that ever stops being true.
fn document(state: &AppState, show_id: ShowId) -> ApiResult<Show> {
    // **Only the loaded show can be edited** (owner, 2026-09-24: "man kann
    // keine fremde show bearbeiten wenn eine andere show geladen ist").
    //
    // Here rather than in each of the eight routes above, because this is the
    // one place every one of them reads the document from. It used to fall back
    // to the database for a show that was not open, with the reasoning that
    // "the route is about the document, and that it is usually the open one is
    // a fact about the workflow, not a precondition" — which was true until the
    // owner made it one.
    //
    // A whole-document save is refused the same way in `show_routes::save`.
    // Two doors, one rule, and no third way in.
    state
        .with_state(|current| current.show().filter(|show| show.id == show_id).cloned())
        .ok_or_else(|| {
            ApiFailure::conflict(
                "show_not_loaded",
                "only the loaded show can be edited; load this one first",
            )
        })
}

/// Stores the edited document and, if it is the open one, makes the show follow.
///
/// `settle` is what the live state has to do about the edit before the document
/// under it changes — a layer showing a slot that is being emptied has to come
/// off stage first. It runs inside the same lock as the replacement, so no peer
/// can see the document without the live state that goes with it.
fn store(
    state: &AppState,
    show: Show,
    settle: impl FnOnce(&mut AuthoritativeState),
) -> ApiResult<Timestamp> {
    // A generator dragged onto a pad with no identity of its own is given one
    // **before the row is written**.
    //
    // `show_routes::save` has always done this and says why: a patch entry
    // points at a generator instance and lives in the same document, so an
    // identity that exists only in the running state is minted again on the
    // next open, and every fixture patched against the previous one is
    // pointing at a generator that no longer exists. This is the other door
    // into the same document and it was leaving the identity out of the row,
    // while `replace_show` put one into the state — so the show on the machine
    // and the show on disk disagreed about what the generator was called, and
    // only until the next open at that.
    //
    // Found by driving the live server through its own API (2026-10-13): a
    // generator assigned this way could not be addressed through
    // `PUT /live/generators/{id}` at all, because the identity the stored
    // document held was none. Our own client never hit it — it mints one
    // before it sends — which is why it sat here unnoticed.
    let mut show = show;
    show.assign_missing_ids();
    show.adopt_one_of_each();

    // **A locked effect or generator stays where it is**, whoever asks
    // (owner, 2026-10-19): taking an effect off, putting a chain in order or
    // filling a pad has to leave everything locked where it was.
    crate::server::show_routes::hold_to_the_rules(state, &show)?;

    let show_id = show.id;
    let updated_at = state.with_database(|database| database.save_show(&show))?;

    // Stamped with what the row got, so that what goes out over the socket is
    // the document that was stored rather than the one that was edited.
    let stored = Show { updated_at, ..show };

    // It is the open one: `document` refuses anything else, so there is no
    // longer a case where the row moves and the running state does not.
    state.mutate_state(|current| {
        settle(current);
        let change = current.replace_show(stored);
        ((), Some(change))
    });
    let _ = show_id;

    Ok(updated_at)
}

/// Finds the layer the path names.
fn layer_of(show: &mut Show, layer_id: LayerId) -> ApiResult<&mut Layer> {
    show.layers
        .iter_mut()
        .find(|layer| layer.id == layer_id)
        .ok_or_else(|| ApiFailure::not_found(format!("layer {layer_id} is not in this show")))
}

/// Puts a source in a slot.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show, layer or media item is not there,
/// when the slot index is not one a layer can hold, or when the edited document
/// would not validate.
pub async fn assign(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id, slot_index)): Path<(String, String, u16)>,
    Json(request): Json<AssignSlotRequest>,
) -> ApiResult<Json<AssignSlotResponse>> {
    authenticate(&state, &headers)?;
    let target = target(&show_id, &layer_id, slot_index)?;

    // A slot pointing at a media item the library does not have is a layer
    // showing its placeholder on the night for a reason nobody at the wall can
    // see. Refused here, in front of the operator who asked
    // for it, rather than discovered there.
    if let SourceRef::Media { media_item_id } = &request.source {
        state.with_database(|database| database.load_media_item(*media_item_id))?;
    }

    // A source arrives unlocked, whatever it says: a lock is the lock
    // request's to set, and an administrator's.
    let mut source = request.source;
    if let SourceRef::Generator { locked, .. } = &mut source {
        *locked = false;
    }

    let mut show = document(&state, target.show_id)?;
    let replaced = layer_of(&mut show, target.layer_id)?
        .assign_slot(target.slot_index, source)
        .map_err(|error| ApiFailure::bad_request("invalid_slot", error.to_string()))?;

    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(AssignSlotResponse {
        updated_at,
        replaced,
    }))
}

/// Empties a slot.
///
/// A slot that is already empty is not an error: the pad the operator is
/// looking at is empty either way, and nothing is stored or published.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show or layer is not there, or when the
/// slot index is not one a layer can hold.
pub async fn clear(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id, slot_index)): Path<(String, String, u16)>,
) -> ApiResult<Json<AssignSlotResponse>> {
    authenticate(&state, &headers)?;
    let target = target(&show_id, &layer_id, slot_index)?;

    let mut show = document(&state, target.show_id)?;
    let removed = layer_of(&mut show, target.layer_id)?.clear_slot(target.slot_index);

    let Some(slot) = removed else {
        // Nothing was there. Answering with the timestamp the document already
        // has, rather than storing it again: an edit that changed nothing must
        // not read to every other client as a show that has just moved.
        return Ok(Json(AssignSlotResponse {
            updated_at: show.updated_at,
            replaced: None,
        }));
    };

    let updated_at = store(&state, show, |current| {
        current.black_out_slot(target.layer_id, target.slot_index);
    })?;

    Ok(Json(AssignSlotResponse {
        updated_at,
        replaced: Some(slot.source),
    }))
}

/// Moves a slot to another pad, of the same layer or another.
///
/// A pad dragged onto another on the live matrix (owner, 2026-10-21). The whole
/// slot goes, and a pad that is taken swaps with it ([`Show::move_slot`]).
///
/// **What is on stage.** Within a layer, a layer showing either of the two
/// goes with what it was showing, so the picture does not change because
/// somebody tidied the matrix. Across two layers that cannot be kept: the clip
/// has left the layer that was showing it. A layer showing either slot is
/// taken to black, as one showing a slot that is emptied is — black is the
/// one answer that is never wrong, and the other choice would be a layer
/// starting a clip nobody pressed.
///
/// A locked effect or generator in either slot refuses the move, as it
/// refuses being moved any other way: its place is its layer and its slot.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show or either layer is not there, when
/// either index is not one a layer can hold, when there is nothing at the pad
/// named, or when something locked would move.
pub async fn move_slot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id, slot_index)): Path<(String, String, u16)>,
    Json(request): Json<MoveSlotRequest>,
) -> ApiResult<Json<MoveSlotResponse>> {
    authenticate(&state, &headers)?;
    let target = target(&show_id, &layer_id, slot_index)?;
    let to = request.to;
    let to_layer = request.to_layer.unwrap_or(target.layer_id);
    Layer::check_slot_index(to)
        .map_err(|error| ApiFailure::bad_request("invalid_slot", error.to_string()))?;

    let mut show = document(&state, target.show_id)?;
    let moved = show
        .move_slot((target.layer_id, target.slot_index), (to_layer, to))
        .map_err(|error| match error {
            SlotMoveError::UnknownLayer(_) => ApiFailure::not_found(error.to_string()),
            SlotMoveError::Index(_) => ApiFailure::bad_request("invalid_slot", error.to_string()),
        })?;

    let across = to_layer != target.layer_id;
    let swapped = match moved {
        SlotMove::Nothing if target.slot_index == to && !across => {
            // Dropped where it was picked up. Nothing is stored, as for a clear
            // of an empty pad: an edit that changed nothing must not read to
            // every other client as a show that has just moved.
            return Ok(Json(MoveSlotResponse {
                updated_at: show.updated_at,
                swapped: false,
            }));
        }
        SlotMove::Nothing => {
            return Err(ApiFailure::bad_request(
                "empty_slot",
                format!("slot {} has nothing in it to move", target.slot_index),
            ));
        }
        SlotMove::Moved => false,
        SlotMove::Swapped => true,
    };

    let updated_at = store(&state, show, |current| {
        if across {
            current.black_out_slot(target.layer_id, target.slot_index);
            current.black_out_slot(to_layer, to);
        } else {
            current.follow_moved_slot(target.layer_id, target.slot_index, to);
        }
    })?;
    Ok(Json(MoveSlotResponse {
        updated_at,
        swapped,
    }))
}

// --- how much matrix a layer shows -------------------------------------------

/// Sets how many banks of twelve a layer's matrix shows.
///
/// Here rather than in the editor because it is done on the matrix, with a
/// show on: an operator right-clicks a pad and asks for the next group rather
/// than waiting for this one to fill (owner, 2026-09-23).
///
/// The count is stored on the layer rather than derived from its slots,
/// because a bank is added in order to put something in it: a derived count
/// would collapse back to what is filled on the next redraw.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show or layer is not there, or when the
/// count is not between one and [`Layer::MAX_BANKS`].
pub async fn set_banks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id)): Path<(String, String)>,
    Json(request): Json<SetBanksRequest>,
) -> ApiResult<Json<AssignSlotResponse>> {
    authenticate(&state, &headers)?;
    let show_id: ShowId = parse_id(&show_id)?;
    let layer_id: LayerId = parse_id(&layer_id)?;

    let mut show = document(&state, show_id)?;
    let layer = layer_of(&mut show, layer_id)?;

    // Refused rather than truncated. Asking for fewer banks than the layer has
    // slots in would hide work an operator did, and quietly moving the number
    // up to fit would answer a question they did not ask.
    let needed = layer.bank_count();
    if request.banks < needed {
        return Err(ApiFailure::bad_request(
            "banks_in_use",
            format!("this layer has slots in {needed} banks"),
        ));
    }
    layer.banks = request.banks;

    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(AssignSlotResponse {
        updated_at,
        replaced: None,
    }))
}

// --- effect chains -----------------------------------------------------------

/// Adds an effect to the end of a layer's chain.
///
/// **Appended, never replacing.** An effect dropped on a layer stacks on top of
/// what is there (owner, 2026-09-23); a chain is an order and dropping a second
/// effect is not a request to undo the first.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show or layer is not there, or when this
/// build does not publish that effect.
pub async fn add_layer_effect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id)): Path<(String, String)>,
    Json(request): Json<AddEffectRequest>,
) -> ApiResult<Json<AddEffectResponse>> {
    authenticate(&state, &headers)?;
    let show_id: ShowId = parse_id(&show_id)?;
    let layer_id: LayerId = parse_id(&layer_id)?;

    let mut show = document(&state, show_id)?;
    let effect = EffectInstance::new(request.type_id, request.version);
    let effect_instance_id = effect.id;
    layer_of(&mut show, layer_id)?.effects.push(effect);

    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(AddEffectResponse {
        updated_at,
        effect_instance_id,
    }))
}

/// Adds an effect to the end of one slot's chain.
///
/// # Errors
///
/// As [`add_layer_effect`], and additionally when the slot is not one the layer
/// has anything in: an effect belongs to a source, and an empty pad has none.
pub async fn add_slot_effect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id, slot_index)): Path<(String, String, u16)>,
    Json(request): Json<AddEffectRequest>,
) -> ApiResult<Json<AddEffectResponse>> {
    authenticate(&state, &headers)?;
    let target = target(&show_id, &layer_id, slot_index)?;

    let mut show = document(&state, target.show_id)?;
    let effect = EffectInstance::new(request.type_id, request.version);
    let effect_instance_id = effect.id;
    slot_of(&mut show, target.layer_id, target.slot_index)?
        .effects
        .push(effect);

    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(AddEffectResponse {
        updated_at,
        effect_instance_id,
    }))
}

/// Takes one effect off a layer's chain.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show, layer or instance is not there.
pub async fn remove_layer_effect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id, effect_id)): Path<(String, String, String)>,
) -> ApiResult<Json<AddEffectResponse>> {
    authenticate(&state, &headers)?;
    let show_id: ShowId = parse_id(&show_id)?;
    let layer_id: LayerId = parse_id(&layer_id)?;
    let effect_instance_id: EffectInstanceId = parse_id(&effect_id)?;

    let mut show = document(&state, show_id)?;
    let chain = &mut layer_of(&mut show, layer_id)?.effects;
    take_effect(chain, effect_instance_id)?;

    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(AddEffectResponse {
        updated_at,
        effect_instance_id,
    }))
}

/// Takes one effect off a slot's chain.
///
/// # Errors
///
/// As [`remove_layer_effect`], and additionally when the slot is empty.
pub async fn remove_slot_effect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id, slot_index, effect_id)): Path<(String, String, u16, String)>,
) -> ApiResult<Json<AddEffectResponse>> {
    authenticate(&state, &headers)?;
    let target = target(&show_id, &layer_id, slot_index)?;
    let effect_instance_id: EffectInstanceId = parse_id(&effect_id)?;

    let mut show = document(&state, target.show_id)?;
    let chain = &mut slot_of(&mut show, target.layer_id, target.slot_index)?.effects;
    take_effect(chain, effect_instance_id)?;

    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(AddEffectResponse {
        updated_at,
        effect_instance_id,
    }))
}

/// Sets the order of a layer's chain.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show or layer is not there, or when the order
/// is not exactly what the chain holds.
pub async fn order_layer_effects(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id)): Path<(String, String)>,
    Json(request): Json<OrderEffectsRequest>,
) -> ApiResult<Json<OrderEffectsResponse>> {
    authenticate(&state, &headers)?;
    let show_id: ShowId = parse_id(&show_id)?;
    let layer_id: LayerId = parse_id(&layer_id)?;

    let mut show = document(&state, show_id)?;
    let chain = &mut layer_of(&mut show, layer_id)?.effects;
    put_in_order(chain, &request.order)?;

    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(OrderEffectsResponse { updated_at }))
}

/// Sets the order of a slot's chain.
///
/// # Errors
///
/// As [`order_layer_effects`], and additionally when the slot is empty.
pub async fn order_slot_effects(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id, slot_index)): Path<(String, String, u16)>,
    Json(request): Json<OrderEffectsRequest>,
) -> ApiResult<Json<OrderEffectsResponse>> {
    authenticate(&state, &headers)?;
    let target = target(&show_id, &layer_id, slot_index)?;

    let mut show = document(&state, target.show_id)?;
    let chain = &mut slot_of(&mut show, target.layer_id, target.slot_index)?.effects;
    put_in_order(chain, &request.order)?;

    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(OrderEffectsResponse { updated_at }))
}

/// Rearranges a chain into the order given, or refuses and leaves it alone.
///
/// **Every instance, exactly once.** The length is checked first so that a short
/// order cannot quietly drop effects, and each named instance is then taken out
/// of the chain — so an instance named twice fails on its second appearance,
/// when it is no longer there to find. What is left over at the end would be an
/// instance the order did not mention, which the length check has already made
/// impossible; it is asserted rather than assumed, because the cost is one
/// comparison and the alternative is a silently shortened chain.
///
/// Nothing is written until it has all worked: the rearranging happens in a
/// vector of its own and replaces the chain only at the end.
fn put_in_order(chain: &mut Vec<EffectInstance>, order: &[EffectInstanceId]) -> ApiResult<()> {
    if order.len() != chain.len() {
        return Err(ApiFailure::bad_request(
            "chain_order_mismatch",
            format!(
                "this chain has {} effects and the order names {}; an order must name every \
                 one of them exactly once",
                chain.len(),
                order.len()
            ),
        ));
    }

    let mut taken = chain.clone();
    let mut ordered: Vec<EffectInstance> = Vec::with_capacity(order.len());
    for id in order {
        let at = taken
            .iter()
            .position(|effect| effect.id == *id)
            .ok_or_else(|| {
                ApiFailure::not_found(format!(
                    "this chain has no effect instance {id} left to put in order; an order \
                     names each of them once"
                ))
            })?;
        ordered.push(taken.remove(at));
    }
    if !taken.is_empty() {
        return Err(ApiFailure::bad_request(
            "chain_order_mismatch",
            "that order does not name every effect in this chain",
        ));
    }

    *chain = ordered;
    Ok(())
}

/// Finds the slot the path names, for something that attaches to a source.
fn slot_of(show: &mut Show, layer_id: LayerId, slot_index: u16) -> ApiResult<&mut Slot> {
    layer_of(show, layer_id)?
        .slots
        .iter_mut()
        .find(|slot| slot.index == slot_index)
        .ok_or_else(|| {
            ApiFailure::not_found(format!(
                "slot {slot_index} has nothing in it, so there is nothing to put an effect on"
            ))
        })
}

/// Changes one slot's settings: everything about it but its source.
///
/// Loop or play once, forwards or backwards, its own rate or the show's beat,
/// where it sits, and what the pad is called. All of it is the document rather
/// than live state — it describes the clip, not what the show is doing with
/// it — and all of it is decided while watching the wall, which is why it is
/// here and not behind a save (owner, 2026-09-23).
///
/// A name is sent as text, and empty text takes the name off rather than
/// setting an empty one: see [`SetSlotRequest::name`].
///
/// Nothing is restarted. A clip that is playing goes on playing; what changes
/// is what it does at the end of this pass, which is where a change of mode
/// takes effect anyway, and where it is drawn, which is the next frame.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show, layer or slot is not there, when
/// the slot index is not one a layer can hold, or when the edited document
/// would not validate — a beat count below the minimum, for instance.
pub async fn set_slot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((show_id, layer_id, slot_index)): Path<(String, String, u16)>,
    Json(request): Json<SetSlotRequest>,
) -> ApiResult<Json<AssignSlotResponse>> {
    authenticate(&state, &headers)?;
    let target = target(&show_id, &layer_id, slot_index)?;

    let mut show = document(&state, target.show_id)?;
    {
        let slot = slot_of(&mut show, target.layer_id, target.slot_index)?;
        if let Some(name) = request.name {
            // Trimmed, and nothing left means no name: an operator clearing a
            // field leaves whitespace behind as often as not, and a pad whose
            // name is three spaces is a pad with no name on it that the
            // document says has one.
            let name = name.trim();
            slot.name = (!name.is_empty()).then(|| name.to_owned());
        }
        if let Some(playback_mode) = request.playback_mode {
            slot.playback_mode = playback_mode;
        }
        if let Some(direction) = request.direction {
            slot.direction = direction;
        }
        if let Some(sync) = request.sync {
            slot.sync = sync;
        }
        if let Some(speed) = request.speed {
            slot.speed = speed;
        }
        if let Some(transform) = request.transform {
            slot.transform = transform;
        }
    }

    // Validated as a whole document by the store, like every other edit here,
    // so a beat count the model refuses is refused with nothing written.
    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(AssignSlotResponse {
        updated_at,
        replaced: None,
    }))
}

/// Replaces the mapping — the outputs and the pieces — under the running show.
///
/// **Lining a wall up is the one job that cannot be done blind.** A piece is a
/// rectangle of the composition put on a rectangle of a wall, and whether it
/// is right is a thing an operator sees on the wall rather than works out from
/// four numbers. Until now the only way to get a mapping onto the wall was to
/// save the show, and saving a show the server has open used to reload
/// it — which is why the mapping was edited with the wall showing the old one
/// (owner, 2026-09-23).
///
/// So this is the same kind of targeted edit a slot assignment is: the
/// document the server already holds is edited, stored, and installed without
/// the restart policy running. Nothing goes back to its opening slot and the
/// clock does not move.
///
/// Both halves together, because they refer to each other: sending the pieces
/// alone would leave a moment in which one names an output that is not there.
///
/// # Errors
///
/// Returns [`ApiFailure`] when the show is not there, or when the edited
/// document would not validate — a piece taking a region outside the
/// composition, or naming an output this request does not carry.
pub async fn set_mapping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
    Json(request): Json<SetMappingRequest>,
) -> ApiResult<Json<AssignSlotResponse>> {
    authenticate(&state, &headers)?;
    let show_id: ShowId = parse_id(&show_id)?;

    let mut show = document(&state, show_id)?;
    show.outputs = request.outputs;
    show.pieces = request.pieces;

    // Validated as a whole document by the store, like every other edit here,
    // so a mapping the model refuses is refused with nothing written and the
    // wall still showing what it was showing.
    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(AssignSlotResponse {
        updated_at,
        replaced: None,
    }))
}

// --- the lock ------------------------------------------------------------------

/// Locks one effect or generator where it is, or unlocks it.
///
/// **An administrator's, and the only way a lock changes** (owner,
/// 2026-10-19). Everything else that writes a show carries the locks it finds
/// ([`ovs_types::control::carry_locks`]), so this is where one is put on and
/// the one place it comes off. Done with a show running, from where the effect
/// is seen, like every other edit here: nothing restarts.
///
/// # Errors
///
/// Forbidden for an account that may not administer, and not found when the
/// open show has no such effect or generator.
pub async fn set_lock(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(show_id): Path<String>,
    Json(request): Json<SetLockRequest>,
) -> ApiResult<Json<SetLockResponse>> {
    administer(&state, &headers)?;
    let show_id: ShowId = parse_id(&show_id)?;

    let mut show = document(&state, show_id)?;
    let lock: Option<&mut bool> = match request.target {
        LockTarget::Effect { effect_instance_id } => show
            .layers
            .iter_mut()
            .flat_map(|layer| {
                layer.effects.iter_mut().chain(
                    layer
                        .slots
                        .iter_mut()
                        .flat_map(|slot| slot.effects.iter_mut()),
                )
            })
            .find(|effect| effect.id == effect_instance_id)
            .map(|effect| &mut effect.locked),
        LockTarget::Generator {
            generator_instance_id,
        } => show
            .layers
            .iter_mut()
            .flat_map(|layer| layer.slots.iter_mut())
            .find_map(|slot| match &mut slot.source {
                SourceRef::Generator {
                    instance_id,
                    locked,
                    ..
                } if *instance_id == generator_instance_id => Some(locked),
                _ => None,
            }),
    };
    let Some(lock) = lock else {
        return Err(ApiFailure::not_found(
            "the open show has no such effect or generator to lock",
        ));
    };
    *lock = request.locked;

    let updated_at = store(&state, show, |_| {})?;
    Ok(Json(SetLockResponse { updated_at }))
}

/// Removes one instance from a chain, or says it was not in it.
fn take_effect(chain: &mut Vec<EffectInstance>, id: EffectInstanceId) -> ApiResult<()> {
    let before = chain.len();
    chain.retain(|effect| effect.id != id);
    if chain.len() == before {
        return Err(ApiFailure::not_found(format!(
            "this chain has no effect instance {id}"
        )));
    }
    Ok(())
}
