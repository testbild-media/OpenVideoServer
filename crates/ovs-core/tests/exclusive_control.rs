//! Who may change what, over the HTTP API: the desk, the client, an administrator.
//!
//! The owner's rule of 2026-10-19, as a client meets it. With DMX on, whatever
//! a patched fixture drives is refused here with `409 held_by_desk`; with it
//! off, the client has the whole show. A layer's slot set to GUI is the one
//! exception, and the tempo is the one thing both may do. An effect or a
//! generator an administrator has locked cannot be removed, moved or reordered
//! by anybody, and only the lock's own request changes a lock.
//!
//! The rules themselves are unit-tested in `ovs_types::control`; this is the
//! half that says every door of the server asks them.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{
    AssignSlotRequest, CreateAccountRequest, LockTarget, LoginRequest, LoginResponse,
    MoveSlotRequest, OrderEffectsRequest, SaveShowRequest, SelectSlotRequest, SetLockRequest,
    UpdateEffectRequest, UpdateLayerRequest, UpdateMasterRequest,
};
use ovs_types::config::layer::SlotControl;
use ovs_types::config::patch::FixtureTarget;
use ovs_types::config::slot::SourceRef;
use ovs_types::config::{
    definition, BlendMode, EffectInstance, FixtureInstance, Layer, ParameterMap, ParameterValue,
    Show, Slot, LAYER_V1, MASTER_V1,
};
use ovs_types::geometry::Transform2D;
use ovs_types::id::{
    EffectInstanceId, FixtureInstanceId, GeneratorInstanceId, LayerId, MediaItemId,
};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

/// A server with an administrator and an operator signed in, and nothing open.
struct Harness {
    app: axum::Router,
    state: AppState,
    admin: String,
    operator: String,
}

impl Harness {
    async fn start() -> Self {
        let state = AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test");
        let app = router(state.clone());

        // The first account is the server's administrator, whatever it asks for.
        let (status, _) = send(
            &app,
            json(
                "POST",
                "/api/v1/auth/setup",
                None,
                &CreateAccountRequest {
                    username: "chief".to_owned(),
                    password: PASSWORD.to_owned(),
                    role: None,
                },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let admin = log_in(&app, "chief").await;

        // An operator, which is what an account made by an administrator is.
        let (status, body) = send(
            &app,
            json(
                "POST",
                "/api/v1/accounts",
                Some(&admin),
                &CreateAccountRequest {
                    username: "crew".to_owned(),
                    password: PASSWORD.to_owned(),
                    role: None,
                },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let operator = log_in(&app, "crew").await;

        Self {
            app,
            state,
            admin,
            operator,
        }
    }

    /// Puts a show on stage, the way opening one does.
    fn open(&self, show: Show) {
        self.state
            .with_database(|database| database.save_show(&show))
            .expect("stores");
        self.state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });
    }

    /// The open show, as the server now holds it.
    fn show(&self) -> Show {
        self.state
            .with_state(|current| current.show().cloned())
            .expect("a show is open")
    }

    /// Sends as the operator.
    async fn put<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        send(&self.app, json("PUT", path, Some(&self.operator), body)).await
    }

    /// Sends as the operator.
    async fn post<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        send(&self.app, json("POST", path, Some(&self.operator), body)).await
    }

    /// Sends as the operator, with no body.
    async fn delete(&self, path: &str, token: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("DELETE")
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .expect("builds");
        send(&self.app, request).await
    }

    async fn switch_dmx(&self, on: bool) {
        let (status, body) = self
            .put(
                "/api/v1/live/master",
                &UpdateMasterRequest {
                    dmx_enabled: Some(on),
                    ..UpdateMasterRequest::default()
                },
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }

    fn layer_live(&self, layer_id: LayerId) -> ovs_types::live::LayerLive {
        self.state
            .with_state(|current| current.layer(layer_id).copied())
            .expect("the layer has live state")
    }
}

async fn log_in(app: &axum::Router, username: &str) -> String {
    let (status, body) = send(
        app,
        json(
            "POST",
            "/api/v1/auth/login",
            None,
            &LoginRequest {
                username: username.to_owned(),
                password: PASSWORD.to_owned(),
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let login: LoginResponse = serde_json::from_value(body).expect("a login");
    login.token
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = app.clone().oneshot(request).await.expect("responds");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("reads")
        .to_bytes();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, value)
}

fn json<T: serde::Serialize>(
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &T,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder
        .body(Body::from(serde_json::to_vec(body).expect("serialises")))
        .expect("builds")
}

fn fixture(type_id: &str, footprint: u16, target: FixtureTarget) -> FixtureInstance {
    FixtureInstance {
        id: FixtureInstanceId::new(),
        type_id: type_id.to_owned(),
        version: 1,
        target,
        universe: 1,
        start_address: 1,
        footprint,
        enabled: true,
    }
}

/// A show with one layer of two slots, and the layer's identity.
fn a_show() -> (Show, LayerId) {
    let mut show = Show::new("Show");
    let mut layer = Layer::new("FX", 0);
    layer.slots.push(Slot::media(1, MediaItemId::new()));
    layer.slots.push(Slot::media(2, MediaItemId::new()));
    let layer_id = layer.id;
    show.layers.push(layer);
    (show, layer_id)
}

/// The same show with a layer fixture patched to its layer.
fn a_patched_layer() -> (Show, LayerId) {
    let (mut show, layer_id) = a_show();
    show.fixture_patch.push(fixture(
        LAYER_V1.type_id,
        LAYER_V1.footprint,
        FixtureTarget::Layer { layer_id },
    ));
    (show, layer_id)
}

fn code(body: &serde_json::Value) -> &str {
    body["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn a_patched_layer_is_the_desks_while_dmx_is_on_and_the_clients_when_it_is_off() {
    let harness = Harness::start().await;
    let (show, layer_id) = a_patched_layer();
    harness.open(show);

    let slot = format!("/api/v1/live/layers/{layer_id}/slot");
    let layer = format!("/api/v1/live/layers/{layer_id}");

    let (status, body) = harness
        .post(&slot, &SelectSlotRequest { slot_index: 2 })
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "held_by_desk");

    for request in [
        UpdateLayerRequest {
            opacity: Some(0.5),
            ..UpdateLayerRequest::default()
        },
        UpdateLayerRequest {
            transform: Some(Transform2D {
                x: 10.0,
                ..Transform2D::IDENTITY
            }),
            ..UpdateLayerRequest::default()
        },
    ] {
        let (status, body) = harness.put(&layer, &request).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(code(&body), "held_by_desk");
    }

    // What no desk has a channel for stays the client's.
    let (status, _) = harness
        .put(
            &layer,
            &UpdateLayerRequest {
                blend_mode: Some(BlendMode::Add),
                ..UpdateLayerRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(harness.layer_live(layer_id).blend_mode, BlendMode::Add);

    // And a request naming one of each changes nothing at all.
    let (status, _) = harness
        .put(
            &layer,
            &UpdateLayerRequest {
                opacity: Some(0.5),
                blend_mode: Some(BlendMode::Screen),
                ..UpdateLayerRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        harness.layer_live(layer_id).blend_mode,
        BlendMode::Add,
        "the free half of a refused request did not land either"
    );

    // The one switch. Off, and the client has it all.
    harness.switch_dmx(false).await;
    let (status, body) = harness
        .post(&slot, &SelectSlotRequest { slot_index: 2 })
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, _) = harness
        .put(
            &layer,
            &UpdateLayerRequest {
                opacity: Some(0.5),
                ..UpdateLayerRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(harness.layer_live(layer_id).active_slot, 2);
    assert!((harness.layer_live(layer_id).opacity - 0.5).abs() < 0.001);

    // On again, and it is the desk's again.
    harness.switch_dmx(true).await;
    let (status, _) = harness
        .post(&slot, &SelectSlotRequest { slot_index: 1 })
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_layer_whose_slots_are_the_guis_is_chosen_here_even_when_patched() {
    // The owner's one exception to his own rule.
    let harness = Harness::start().await;
    let (mut show, layer_id) = a_patched_layer();
    show.layers[0].slot_control = SlotControl::Gui;
    harness.open(show);

    let (status, body) = harness
        .post(
            &format!("/api/v1/live/layers/{layer_id}/slot"),
            &SelectSlotRequest { slot_index: 2 },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Everything else on the layer is still the desk's.
    let (status, _) = harness
        .put(
            &format!("/api/v1/live/layers/{layer_id}"),
            &UpdateLayerRequest {
                opacity: Some(0.5),
                ..UpdateLayerRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn an_unpatched_or_parked_layer_is_the_clients() {
    let harness = Harness::start().await;
    let (mut show, layer_id) = a_patched_layer();
    show.fixture_patch[0].enabled = false;
    harness.open(show);

    let (status, body) = harness
        .post(
            &format!("/api/v1/live/layers/{layer_id}/slot"),
            &SelectSlotRequest { slot_index: 2 },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

#[tokio::test]
async fn a_patched_master_holds_its_faders_but_never_the_tempo() {
    let harness = Harness::start().await;
    let (mut show, _) = a_show();
    show.fixture_patch.push(fixture(
        MASTER_V1.type_id,
        MASTER_V1.footprint,
        FixtureTarget::Master,
    ));
    harness.open(show);

    for request in [
        UpdateMasterRequest {
            dimmer: Some(0.5),
            ..UpdateMasterRequest::default()
        },
        UpdateMasterRequest {
            crossfader: Some(0.5),
            ..UpdateMasterRequest::default()
        },
        UpdateMasterRequest {
            speed_scale: Some(2.0),
            ..UpdateMasterRequest::default()
        },
        UpdateMasterRequest {
            freeze: Some(true),
            ..UpdateMasterRequest::default()
        },
    ] {
        let (status, body) = harness.put("/api/v1/live/master", &request).await;
        assert_eq!(status, StatusCode::CONFLICT, "{request:?}: {body}");
    }

    // "Tap BPM ist das einzige was ich über beides machen würde."
    let (status, _) = harness
        .put(
            "/api/v1/live/master",
            &UpdateMasterRequest {
                bpm: Some(140.0),
                ..UpdateMasterRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = harness
        .post("/api/v1/live/tap", &serde_json::json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_dmx_switch_is_kept_for_the_next_start() {
    let harness = Harness::start().await;
    let (show, _) = a_show();
    harness.open(show);

    harness.switch_dmx(false).await;
    assert!(!harness
        .state
        .with_database(|database| database.dmx_enabled())
        .expect("reads"));

    // And opening another show leaves it where it is.
    let (other, _) = a_show();
    harness.open(other);
    assert!(!harness
        .state
        .with_state(|current| current.master().dmx_enabled));
}

/// A show whose layer carries a mask: one parameter a desk can drive, and some
/// it cannot.
fn a_masked_layer() -> (Show, LayerId, EffectInstanceId) {
    let (mut show, layer_id) = a_show();
    let mask = EffectInstance::new("ovs.mask", 1);
    let mask_id = mask.id;
    show.layers[0].effects.push(mask);
    let personality = definition("ovs.mask").expect("the mask offers its amount to a desk");
    show.fixture_patch.push(fixture(
        personality.type_id,
        personality.footprint,
        FixtureTarget::Effect {
            effect_instance_id: mask_id,
        },
    ));
    (show, layer_id, mask_id)
}

fn one(key: &str, value: ParameterValue) -> UpdateEffectRequest {
    UpdateEffectRequest {
        overrides: ParameterMap::from([(key.to_owned(), value)]),
        ..UpdateEffectRequest::default()
    }
}

#[tokio::test]
async fn a_patched_effect_keeps_its_desk_parameters_and_a_reset_leaves_them_alone() {
    let harness = Harness::start().await;
    let (show, _, mask) = a_masked_layer();
    harness.open(show);

    // The desk puts the amount somewhere, as the receiver would.
    harness.state.mutate_state(|current| {
        let change = current
            .set_effect_parameters(
                mask,
                &ParameterMap::from([("amount".to_owned(), ParameterValue::Float(0.5))]),
                false,
            )
            .expect("a value the mask takes");
        ((), change)
    });

    let path = format!("/api/v1/live/effects/{mask}");
    let (status, body) = harness
        .put(&path, &one("amount", ParameterValue::Float(0.9)))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "held_by_desk");

    // A parameter no desk drives is the client's.
    let (status, body) = harness
        .put(&path, &one("invert", ParameterValue::Bool(true)))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // A reset puts back only what was the client's.
    let (status, _) = harness
        .put(
            &path,
            &UpdateEffectRequest {
                reset: true,
                ..UpdateEffectRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let live = harness
        .state
        .with_state(|current| current.effect(mask).cloned())
        .expect("the desk's value is still on it");
    assert_eq!(live.get("amount"), Some(&ParameterValue::Float(0.5)));
    assert_eq!(live.get("invert"), None);

    // With DMX off it is the client's, all of it.
    harness.switch_dmx(false).await;
    let (status, _) = harness
        .put(&path, &one("amount", ParameterValue::Float(0.9)))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn the_editor_cannot_change_a_value_the_desk_holds() {
    let harness = Harness::start().await;
    let (show, _, _) = a_masked_layer();
    harness.open(show);

    let mut edited = harness.show();
    edited.layers[0].effects[0]
        .parameters
        .insert("amount".to_owned(), ParameterValue::Float(0.25));
    let path = format!("/api/v1/shows/{}", edited.id);
    let request = SaveShowRequest {
        show: edited.clone(),
    };

    let (status, body) = harness.put(&path, &request).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "held_by_desk");

    harness.switch_dmx(false).await;
    let (status, body) = harness.put(&path, &request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A show whose layer carries a blur and an invert, the blur first.
fn two_effects() -> (Show, LayerId, EffectInstanceId, EffectInstanceId) {
    let (mut show, layer_id) = a_show();
    let blur = EffectInstance::new("ovs.blur", 1);
    let invert = EffectInstance::new("ovs.invert", 1);
    let (blur_id, invert_id) = (blur.id, invert.id);
    show.layers[0].effects.push(blur);
    show.layers[0].effects.push(invert);
    (show, layer_id, blur_id, invert_id)
}

fn lock_effect(effect_instance_id: EffectInstanceId, locked: bool) -> SetLockRequest {
    SetLockRequest {
        target: LockTarget::Effect { effect_instance_id },
        locked,
    }
}

#[tokio::test]
async fn only_an_administrator_locks_and_then_nobody_moves_it() {
    let harness = Harness::start().await;
    let (show, layer_id, blur, invert) = two_effects();
    let show_id = show.id;
    harness.open(show);
    let lock = format!("/api/v1/shows/{show_id}/lock");

    let (status, _) = harness.put(&lock, &lock_effect(blur, true)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "an operator may not lock");

    let (status, body) = send(
        &harness.app,
        json("PUT", &lock, Some(&harness.admin), &lock_effect(blur, true)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(harness.show().layers[0].effects[0].locked);

    // Not removable, not reorderable — by the administrator either.
    let remove = format!("/api/v1/shows/{show_id}/layers/{layer_id}/effects/{blur}");
    for token in [&harness.operator, &harness.admin] {
        let (status, body) = harness.delete(&remove, token).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(code(&body), "locked");
    }
    let (status, body) = harness
        .put(
            &format!("/api/v1/shows/{show_id}/layers/{layer_id}/effects/order"),
            &OrderEffectsRequest {
                order: vec![invert, blur],
            },
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Nor taken out by sending the show whole without it.
    let mut without = harness.show();
    without.layers[0].effects.remove(0);
    let save = format!("/api/v1/shows/{show_id}");
    let (status, _) = send(
        &harness.app,
        json(
            "PUT",
            &save,
            Some(&harness.admin),
            &SaveShowRequest { show: without },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Switching it off is everybody's, and a draft from before the lock does
    // not undo it by being saved.
    let mut stale = harness.show();
    stale.layers[0].effects[0].locked = false;
    stale.layers[0].effects[0].enabled = false;
    let (status, body) = harness.put(&save, &SaveShowRequest { show: stale }).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let effect = harness.show().layers[0].effects[0].clone();
    assert!(!effect.enabled, "switched off");
    assert!(effect.locked, "and still locked");

    // Unlocked by an administrator, it goes like any other.
    let (status, _) = send(
        &harness.app,
        json(
            "PUT",
            &lock,
            Some(&harness.admin),
            &lock_effect(blur, false),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = harness.delete(&remove, &harness.operator).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_locked_generator_stays_in_its_slot() {
    let harness = Harness::start().await;
    let (mut show, layer_id) = a_show();
    show.layers[0].slots[0].source = SourceRef::Generator {
        instance_id: GeneratorInstanceId::new(),
        type_id: "ovs.solid_colour".to_owned(),
        version: 1,
        locked: false,
        parameters: ParameterMap::new(),
    };
    let show_id = show.id;
    let SourceRef::Generator { instance_id, .. } = show.layers[0].slots[0].source else {
        unreachable!("set above");
    };
    harness.open(show);

    let (status, body) = send(
        &harness.app,
        json(
            "PUT",
            &format!("/api/v1/shows/{show_id}/lock"),
            Some(&harness.admin),
            &SetLockRequest {
                target: LockTarget::Generator {
                    generator_instance_id: instance_id,
                },
                locked: true,
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let slot = format!("/api/v1/shows/{show_id}/layers/{layer_id}/slots/1");
    let (status, body) = harness.delete(&slot, &harness.admin).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // Another generator dropped on the pad, which needs nothing from the
    // library and so reaches the lock.
    let (status, body) = harness
        .put(
            &slot,
            &AssignSlotRequest {
                source: SourceRef::Generator {
                    instance_id: GeneratorInstanceId::nil(),
                    type_id: "ovs.gradient".to_owned(),
                    version: 1,
                    locked: false,
                    parameters: ParameterMap::new(),
                },
            },
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "locked");
    assert!(matches!(
        &harness.show().layers[0].slots[0].source,
        SourceRef::Generator { locked: true, type_id, .. } if type_id == "ovs.solid_colour"
    ));

    // Nor is it dragged to another pad, by itself or by a slot swapping
    // into its place (2026-10-21): its place is its slot's number.
    let (status, body) = harness
        .post(&format!("{slot}/move"), &MoveSlotRequest { to: 10 })
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "locked");
    // Slot 2, the clip beside it, dragged onto it.
    let neighbour = format!("/api/v1/shows/{show_id}/layers/{layer_id}/slots/2");
    let (status, body) = harness
        .post(&format!("{neighbour}/move"), &MoveSlotRequest { to: 1 })
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "locked");
    assert_eq!(
        harness.show().layers[0].slots[0].index,
        1,
        "still at slot 1"
    );
}
