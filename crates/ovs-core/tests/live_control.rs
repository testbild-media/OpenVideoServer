//! Live control over the HTTP API: what an operator does during a show.
//!
//! The state machine underneath is unit-tested. What this covers is the part a
//! client actually meets: that the endpoints exist, that they refuse what they
//! should refuse with a status code worth acting on, and that what they change
//! comes back on the update stream rather than only being true inside the
//! server.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{
    CreateAccountRequest, LoginRequest, LoginResponse, SelectSlotRequest, TapTempoResponse,
    UpdateEffectRequest, UpdateLayerRequest, UpdateMasterRequest,
};
use ovs_proto::realtime::{ServerMessage, StateChange};
use ovs_types::config::{EffectInstance, Layer, ParameterMap, ParameterValue, Route, Show, Slot};
use ovs_types::id::{EffectInstanceId, LayerId, MediaItemId};
use ovs_types::live::Transport;
use tokio::sync::broadcast::Receiver;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    app: axum::Router,
    token: String,
    state: AppState,
    layer_id: LayerId,
}

impl Harness {
    /// A server with an operator signed in and a one-layer show open.
    async fn start() -> Self {
        let state = AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test");
        let app = router(state.clone());

        let (status, _) = send(
            &app,
            post_json(
                "/api/v1/auth/setup",
                &CreateAccountRequest {
                    username: "operator".to_owned(),
                    password: PASSWORD.to_owned(),
                    role: None,
                },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = send(
            &app,
            post_json(
                "/api/v1/auth/login",
                &LoginRequest {
                    username: "operator".to_owned(),
                    password: PASSWORD.to_owned(),
                },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let login: LoginResponse = serde_json::from_value(body).expect("a login response");

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        layer.slots.push(Slot::media(2, MediaItemId::new()));
        let layer_id = layer.id;
        show.layers.push(layer);
        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });

        Self {
            app,
            token: login.token,
            state,
            layer_id,
        }
    }

    /// A server with an operator signed in and nothing open.
    async fn empty() -> Self {
        let harness = Self::start().await;
        harness.state.mutate_state(|current| {
            let change = current.close_show();
            ((), change)
        });
        harness
    }

    /// Reads a path with the operator's session.
    async fn get(&self, path: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("GET")
            .uri(path)
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        send(&self.app, request).await
    }

    async fn post<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        send(&self.app, post_json_auth(path, &self.token, body)).await
    }

    /// The live state of the show's one layer, as the server holds it.
    fn layer_live(&self) -> ovs_types::live::LayerLive {
        self.state
            .with_state(|current| {
                current
                    .snapshot()
                    .layers
                    .iter()
                    .find(|live| live.layer_id == self.layer_id)
                    .copied()
            })
            .expect("the show's layer has live state")
    }

    async fn post_empty(&self, path: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        send(&self.app, request).await
    }

    async fn put<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("PUT")
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::from(serde_json::to_vec(body).expect("serialises")))
            .expect("builds");
        send(&self.app, request).await
    }

    /// Subscribes to the update stream, as a client does.
    fn watch(&self) -> Receiver<ServerMessage> {
        self.state.subscribe()
    }

    /// Opens a show whose one layer has a blur configured at a radius of eight.
    ///
    /// Eight rather than the blur's published default of zero, so that "the live
    /// value won" and "the configured value came back" are distinguishable from
    /// "nobody ever set anything".
    fn open_show_with_a_blur(&self) -> EffectInstanceId {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));

        let mut blur = EffectInstance::new("ovs.blur", 1);
        blur.parameters
            .insert("radius_px".to_owned(), ParameterValue::Float(8.0));
        let effect_instance_id = blur.id;
        layer.effects.push(blur);
        show.layers.push(layer);

        self.state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });
        effect_instance_id
    }

    /// The live value on one effect parameter, if a control source has moved it.
    fn live_effect_value(
        &self,
        effect_instance_id: EffectInstanceId,
        key: &str,
    ) -> Option<ParameterValue> {
        self.state.with_state(|current| {
            current
                .effect(effect_instance_id)
                .and_then(|live| live.get(key))
                .cloned()
        })
    }

    /// What the loaded show says one effect parameter is configured at.
    fn configured_effect_value(&self, key: &str) -> Option<ParameterValue> {
        self.state.with_state(|current| {
            current.show().expect("a show is open").layers[0].effects[0]
                .parameters
                .get(key)
                .cloned()
        })
    }

    /// Opens a show whose one slot plays a gradient, and says which one.
    ///
    /// The generator twin of [`open_show_with_a_blur`](Self::open_show_with_a_blur).
    /// The identity is read back out of the loaded show rather than minted
    /// here, because a show is given one when it is loaded — which is the
    /// half of this that a test writing its own identity would skip.
    fn open_show_with_a_gradient(&self) -> ovs_types::id::GeneratorInstanceId {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        let mut slot = Slot::media(1, MediaItemId::new());
        let mut parameters = ParameterMap::new();
        parameters.insert("angle_deg".to_owned(), ParameterValue::Float(45.0));
        slot.source = ovs_types::config::slot::SourceRef::Generator {
            instance_id: ovs_types::id::GeneratorInstanceId::nil(),
            type_id: "ovs.gradient".to_owned(),
            version: 1,
            parameters,
        };
        layer.slots.push(slot);
        show.layers.push(layer);

        self.state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });

        self.state.with_state(|current| {
            let ovs_types::config::slot::SourceRef::Generator { instance_id, .. } =
                current.show().expect("a show is open").layers[0].slots[0].source
            else {
                panic!("a generator slot");
            };
            assert!(!instance_id.is_nil(), "loading gives it an identity");
            instance_id
        })
    }

    /// The live value on one generator parameter, if anything has moved it.
    fn live_generator_value(
        &self,
        generator_instance_id: ovs_types::id::GeneratorInstanceId,
        key: &str,
    ) -> Option<ParameterValue> {
        self.state.with_state(|current| {
            current
                .generator_overrides()
                .get(&generator_instance_id)
                .and_then(|live| live.get(key))
                .cloned()
        })
    }

    /// What the loaded show says one generator parameter is configured at.
    fn configured_generator_value(&self, key: &str) -> Option<ParameterValue> {
        self.state.with_state(|current| {
            let ovs_types::config::slot::SourceRef::Generator { parameters, .. } =
                &current.show().expect("a show is open").layers[0].slots[0].source
            else {
                panic!("a generator slot");
            };
            parameters.get(key).cloned()
        })
    }
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

fn post_json<T: serde::Serialize>(path: &str, body: &T) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).expect("serialises")))
        .expect("builds")
}

fn post_json_auth<T: serde::Serialize>(path: &str, token: &str, body: &T) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(serde_json::to_vec(body).expect("serialises")))
        .expect("builds")
}

/// Signs in on a fresh server and returns the token.
async fn sign_in(app: &axum::Router) -> String {
    let (status, _) = send(
        app,
        post_json(
            "/api/v1/auth/setup",
            &CreateAccountRequest {
                username: "operator".to_owned(),
                password: PASSWORD.to_owned(),
                role: None,
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, body) = send(
        app,
        post_json(
            "/api/v1/auth/login",
            &LoginRequest {
                username: "operator".to_owned(),
                password: PASSWORD.to_owned(),
            },
        ),
    )
    .await;
    let login: LoginResponse = serde_json::from_value(body).expect("a login response");
    login.token
}

fn put_json_auth<T: serde::Serialize>(path: &str, token: &str, body: &T) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(serde_json::to_vec(body).expect("serialises")))
        .expect("builds")
}

/// Takes the next change off the stream, or fails the test.
fn next_change(updates: &mut Receiver<ServerMessage>) -> StateChange {
    loop {
        match updates.try_recv() {
            Ok(ServerMessage::Update { change, .. }) => return change,
            Ok(_) => {}
            Err(error) => panic!("expected an update: {error}"),
        }
    }
}

#[tokio::test]
async fn the_status_says_where_the_beat_is() {
    // The drawing has a beat strip and the specification asks for bar and phase
    // information. It is reported with the measurements rather than with the
    // state, because a beat moves sixty times a second and nobody edits it: a
    // state stream that published it would be a metronome.
    let empty = Harness::empty().await;
    let (status, body) = empty.get("/api/v1/server/status").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["telemetry"]["clock"].is_null(),
        "with no show open there is no show for a beat to belong to"
    );

    let harness = Harness::start().await;
    let (status, body) = harness.get("/api/v1/server/status").await;
    assert_eq!(status, StatusCode::OK);
    let clock = &body["telemetry"]["clock"];

    let per_bar = clock["beats_per_bar"].as_u64().expect("beats in a bar");
    assert!(per_bar > 0, "{clock}");
    assert!(clock["bar"].is_u64(), "{clock}");
    assert!(
        clock["beat_in_bar"].as_u64().expect("a beat") < per_bar,
        "the beat is inside the bar: {clock}"
    );

    let phase = clock["phase"].as_f64().expect("a phase");
    assert!(
        (0.0..1.0).contains(&phase),
        "the phase is inside a beat: {phase}"
    );
    assert!(
        clock["beats_per_second"].as_f64().expect("a rate") > 0.0,
        "and it says how fast to animate between samples: {clock}"
    );
}

#[tokio::test]
async fn selecting_a_slot_changes_the_layer_and_says_so_on_the_stream() {
    let harness = Harness::start().await;
    let mut updates = harness.watch();

    let (status, _) = harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            &SelectSlotRequest { slot_index: 2 },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let StateChange::LayerChanged { layer } = next_change(&mut updates) else {
        panic!("expected a layer change");
    };
    assert_eq!(layer.layer_id, harness.layer_id);
    assert_eq!(layer.active_slot, 2);
}

#[tokio::test]
async fn selecting_the_slot_that_is_already_running_is_a_retrigger() {
    // An operator hitting the same pad twice means "start it again", which
    // happens on stage and has to reach the renderer. The slot index does not
    // move, so the trigger counter is the only thing that can carry it.
    let harness = Harness::start().await;
    let path = format!("/api/v1/live/layers/{}/slot", harness.layer_id);

    let (status, _) = harness
        .post(&path, &SelectSlotRequest { slot_index: 1 })
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let first = harness
        .state
        .with_state(|current| current.layer(harness.layer_id).map(|live| live.trigger))
        .expect("the layer is loaded");

    let mut updates = harness.watch();
    let (status, _) = harness
        .post(&path, &SelectSlotRequest { slot_index: 1 })
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let StateChange::LayerChanged { layer } = next_change(&mut updates) else {
        panic!("expected a layer change");
    };
    assert_eq!(layer.active_slot, 1, "still the same slot");
    assert_eq!(layer.trigger, first + 1, "and it was triggered again");
}

#[tokio::test]
async fn the_black_slot_is_always_selectable() {
    // Slot 0 is permanently black on every layer, configured nowhere. It is how
    // an operator takes a layer off stage.
    let harness = Harness::start().await;
    let path = format!("/api/v1/live/layers/{}/slot", harness.layer_id);
    harness
        .post(&path, &SelectSlotRequest { slot_index: 1 })
        .await;

    let (status, _) = harness
        .post(&path, &SelectSlotRequest { slot_index: 0 })
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        harness
            .state
            .with_state(|current| current.layer(harness.layer_id).map(|live| live.active_slot)),
        Some(0)
    );
}

#[tokio::test]
async fn an_empty_pad_is_taken_and_shows_nothing() {
    // The owner, 2026-09-23: "jeder nicht belegte Slot ist ein Black/Full
    // Tranzparent für den Layer." Slot 7 of this layer holds nothing, and
    // pressing it is how a layer is taken off stage now that Black has gone
    // from the matrix.
    let harness = Harness::start().await;

    let (status, body) = harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            &SelectSlotRequest { slot_index: 7 },
        )
        .await;

    // 204 rather than 200: the route answers with no body, as every live
    // control does.
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        harness
            .state
            .with_state(|current| current.layer(harness.layer_id).map(|live| live.active_slot)),
        Some(7)
    );
}

#[tokio::test]
async fn a_pad_the_layer_does_not_have_is_refused() {
    // Past the banks the layer shows is not an empty pad: it is a pad that is
    // not there, and a console patched against another show is where that
    // comes from.
    let harness = Harness::start().await;

    let (status, body) = harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            &SelectSlotRequest { slot_index: 40 },
        )
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_layer_from_another_show_is_refused() {
    let harness = Harness::start().await;

    let (status, _) = harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", LayerId::new()),
            &SelectSlotRequest { slot_index: 1 },
        )
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn live_control_with_nothing_open_says_so_rather_than_pretending() {
    // A 409 rather than a 404: the request is well formed and the server is
    // simply not in a state to carry it out, which is a different thing for a
    // client to show an operator.
    let harness = Harness::empty().await;

    let (status, _) = harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            &SelectSlotRequest { slot_index: 1 },
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_layer_can_be_paused_stopped_and_played_without_touching_its_slot() {
    // The four things owner decision Q16 says must not be conflated. Selecting
    // slot 0 is Black and a different gesture; these are about the source the
    // layer already has, and none of them changes which slot is selected.
    let harness = Harness::start().await;
    let path = format!("/api/v1/live/layers/{}", harness.layer_id);

    harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            &SelectSlotRequest { slot_index: 1 },
        )
        .await;

    for transport in [Transport::Paused, Transport::Stopped, Transport::Playing] {
        let (status, _) = harness
            .put(
                &path,
                &UpdateLayerRequest {
                    transport: Some(transport),
                    ..UpdateLayerRequest::default()
                },
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{transport:?}");

        let live = harness.layer_live();
        assert_eq!(live.transport, transport);
        assert_eq!(live.active_slot, 1, "the slot is not the transport");
        assert!(live.enabled, "and neither is being in the composition");
    }
}

#[tokio::test]
async fn switching_a_layer_off_live_leaves_the_show_alone() {
    // The whole of the second half of Q16: a layer taken out of the
    // composition during a show keeps its slots, its transform and its chain,
    // and opens tomorrow the way the show says.
    let harness = Harness::start().await;
    let path = format!("/api/v1/live/layers/{}", harness.layer_id);

    let (status, _) = harness
        .put(
            &path,
            &UpdateLayerRequest {
                enabled: Some(false),
                ..UpdateLayerRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!harness.layer_live().enabled);

    // The show still says the layer is on, because nothing wrote to it.
    let configured = harness.state.with_state(|current| {
        current
            .snapshot()
            .show
            .as_ref()
            .and_then(|show| show.layers.first().map(|layer| layer.enabled))
    });
    assert_eq!(configured, Some(true), "the document was never touched");

    let (status, _) = harness
        .put(
            &path,
            &UpdateLayerRequest {
                enabled: Some(true),
                ..UpdateLayerRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(harness.layer_live().enabled);
}

#[tokio::test]
async fn a_layer_opens_playing_and_in_the_composition() {
    // A show opens in a defined state. Nothing carries a stopped
    // layer across a restart, and nothing should: a wall that came back frozen
    // is an operator looking for a fault there is not.
    let harness = Harness::start().await;
    let live = harness.layer_live();

    assert_eq!(live.transport, Transport::Playing);
    assert!(live.enabled);
}

#[tokio::test]
async fn the_master_moves_one_field_at_a_time() {
    // A console sending one fader must not have to send the others, and a
    // client that sent a default for a field it was not thinking about would
    // move something on stage.
    let harness = Harness::start().await;

    let (status, _) = harness
        .put(
            "/api/v1/live/master",
            &UpdateMasterRequest {
                crossfader: Some(1.0),
                ..UpdateMasterRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let master = harness
        .state
        .with_state(|current| current.snapshot().master);
    assert!((master.crossfader - 1.0).abs() < f32::EPSILON);
    assert!((master.dimmer - 1.0).abs() < f32::EPSILON, "untouched");
    assert!((master.bpm - 120.0).abs() < f64::EPSILON, "untouched");
}

#[tokio::test]
async fn a_master_value_out_of_range_is_clamped_rather_than_refused() {
    // A console with a badly scaled fader must not be able to brighten the show
    // past full, and must not be able to stop it working either.
    let harness = Harness::start().await;

    let (status, _) = harness
        .put(
            "/api/v1/live/master",
            &UpdateMasterRequest {
                dimmer: Some(5.0),
                crossfader: Some(-2.0),
                ..UpdateMasterRequest::default()
            },
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    let master = harness
        .state
        .with_state(|current| current.snapshot().master);
    assert!((master.dimmer - 1.0).abs() < f32::EPSILON);
    assert!(master.crossfader.abs() < f32::EPSILON);
}

#[tokio::test]
async fn freeze_is_a_master_field_like_any_other() {
    let harness = Harness::start().await;
    let mut updates = harness.watch();

    let (status, _) = harness
        .put(
            "/api/v1/live/master",
            &UpdateMasterRequest {
                freeze: Some(true),
                ..UpdateMasterRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let StateChange::MasterChanged { master } = next_change(&mut updates) else {
        panic!("expected a master change");
    };
    assert!(master.freeze);
}

#[tokio::test]
async fn a_layers_opacity_and_bus_can_be_set_live() {
    let harness = Harness::start().await;

    let (status, _) = harness
        .put(
            &format!("/api/v1/live/layers/{}", harness.layer_id),
            &UpdateLayerRequest {
                opacity: Some(0.25),
                route: Some(Route::B),
                ..UpdateLayerRequest::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let live = harness
        .state
        .with_state(|current| current.layer(harness.layer_id).copied())
        .expect("the layer is loaded");
    assert!((live.opacity - 0.25).abs() < f32::EPSILON);
    assert_eq!(live.route, Route::B);
    assert_eq!(live.active_slot, 0, "untouched");
}

#[tokio::test]
async fn the_first_tap_of_a_run_changes_nothing_and_says_so() {
    // One tap is not an interval. A client showing "tapping…" until this turns
    // true is showing the truth.
    let harness = Harness::start().await;

    let (status, body) = harness.post_empty("/api/v1/live/tap").await;
    assert_eq!(status, StatusCode::OK);
    let answer: TapTempoResponse = serde_json::from_value(body).expect("a tap response");

    assert!(!answer.changed);
    assert!((answer.bpm - 120.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn tapping_twice_sets_a_tempo() {
    let harness = Harness::start().await;

    harness.post_empty("/api/v1/live/tap").await;
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let (status, body) = harness.post_empty("/api/v1/live/tap").await;

    assert_eq!(status, StatusCode::OK);
    let answer: TapTempoResponse = serde_json::from_value(body).expect("a tap response");
    assert!(answer.changed, "two taps make an interval");
    // A quarter of a second between taps is 240 BPM, give or take the scheduler.
    assert!(
        (150.0..=400.0).contains(&answer.bpm),
        "got {} BPM",
        answer.bpm
    );
}

#[tokio::test]
async fn the_phase_can_be_reset_without_changing_the_tempo() {
    let harness = Harness::start().await;
    harness
        .put(
            "/api/v1/live/master",
            &UpdateMasterRequest {
                bpm: Some(140.0),
                ..UpdateMasterRequest::default()
            },
        )
        .await;

    let (status, _) = harness.post_empty("/api/v1/live/phase/reset").await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    let master = harness
        .state
        .with_state(|current| current.snapshot().master);
    assert!((master.bpm - 140.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn live_control_needs_a_session() {
    // This is the surface that changes what an audience sees. Every part of it
    // is behind a token.
    let harness = Harness::start().await;

    // Bodies a signed-in client would send. A malformed one is refused by the
    // extractor before the handler runs, which answers 422 rather than 401 —
    // true of every endpoint on this server, and not what is being tested here.
    let paths = [
        (
            "POST",
            format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            r#"{"slot_index":1}"#,
        ),
        (
            "PUT",
            format!("/api/v1/live/layers/{}", harness.layer_id),
            "{}",
        ),
        ("PUT", "/api/v1/live/master".to_owned(), "{}"),
        ("POST", "/api/v1/live/tap".to_owned(), ""),
        ("POST", "/api/v1/live/phase/reset".to_owned(), ""),
    ];

    for (method, path, body) in paths {
        let request = Request::builder()
            .method(method)
            .uri(&path)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("builds");
        let (status, _) = send(&harness.app, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

#[tokio::test]
async fn what_is_on_stage_survives_a_restart() {
    // A restart brings back the show, the BPM value and which
    // slot each layer was showing. None of that happens unless somebody writes
    // it down, and for a long time nobody did.
    use ovs_db::Database;
    use ovs_types::config::StartupPolicy;

    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("ovs.db");

    let show_id;
    let layer_id;
    {
        let mut database = Database::open(&path).expect("opens");
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        layer.slots.push(Slot::media(4, MediaItemId::new()));
        layer_id = layer.id;
        show.layers.push(layer);
        show_id = show.id;
        database.save_show(&show).expect("saves");
        database
            .set_startup_policy(StartupPolicy::Pinned { show_id })
            .expect("pins");

        let state = AppState::new(database, "0.0.1-test");
        let app = router(state.clone());
        let token = sign_in(&app).await;

        let (status, _) = send(
            &app,
            post_json_auth(
                &format!("/api/v1/shows/{show_id}/open"),
                &token,
                &serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = send(
            &app,
            post_json_auth(
                &format!("/api/v1/live/layers/{layer_id}/slot"),
                &token,
                &SelectSlotRequest { slot_index: 4 },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = send(
            &app,
            put_json_auth(
                "/api/v1/live/master",
                &token,
                &UpdateMasterRequest {
                    bpm: Some(174.0),
                    ..UpdateMasterRequest::default()
                },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert!(state.persist_live_snapshot(), "something to write");
        assert!(
            !state.persist_live_snapshot(),
            "and nothing to write the second time"
        );
    }

    // A new process, on the same database.
    let database = Database::open(&path).expect("reopens");
    let snapshot = database
        .load_live_snapshot(show_id)
        .expect("loads")
        .expect("there is one");

    assert_eq!(snapshot.selected_slot(layer_id), Some(4));
    assert_eq!(snapshot.bpm, Some(174.0));
}

#[tokio::test]
async fn opening_another_show_saves_where_this_one_had_got_to() {
    // Closing a show by opening another must not lose what a restart is
    // supposed to bring back.
    use ovs_db::Database;

    let directory = tempfile::tempdir().expect("a temporary directory");
    let mut database = Database::open(directory.path().join("ovs.db")).expect("opens");

    let mut first = Show::new("First");
    let mut layer = Layer::new("Layer 1", 0);
    layer.slots.push(Slot::media(2, MediaItemId::new()));
    let layer_id = layer.id;
    first.layers.push(layer);
    let first_id = first.id;
    database.save_show(&first).expect("saves");

    let second = Show::new("Second");
    let second_id = second.id;
    database.save_show(&second).expect("saves");

    let state = AppState::new(database, "0.0.1-test");
    let app = router(state.clone());
    let token = sign_in(&app).await;

    send(
        &app,
        post_json_auth(
            &format!("/api/v1/shows/{first_id}/open"),
            &token,
            &serde_json::json!({}),
        ),
    )
    .await;
    send(
        &app,
        post_json_auth(
            &format!("/api/v1/live/layers/{layer_id}/slot"),
            &token,
            &SelectSlotRequest { slot_index: 2 },
        ),
    )
    .await;

    // Opening the second show is what closes the first.
    let (status, _) = send(
        &app,
        post_json_auth(
            &format!("/api/v1/shows/{second_id}/open"),
            &token,
            &serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let snapshot = state
        .with_database(|database| database.load_live_snapshot(first_id))
        .expect("loads")
        .expect("the first show was written down on its way out");
    assert_eq!(snapshot.selected_slot(layer_id), Some(2));
}

#[tokio::test]
async fn a_server_with_nothing_open_writes_nothing() {
    let harness = Harness::empty().await;
    assert!(!harness.state.persist_live_snapshot());
}

/// One parameter, as a live update request body.
fn one(key: &str, value: ParameterValue) -> UpdateEffectRequest {
    UpdateEffectRequest {
        overrides: ParameterMap::from([(key.to_owned(), value)]),
        reset: false,
    }
}

#[tokio::test]
async fn an_effects_parameter_can_be_moved_during_a_show_without_editing_the_show() {
    // An effect whose parameters cannot be moved during a show is half an
    // effect. This is the other half, and the assertion that matters as much as
    // the status code is the second one: the show still says what the show
    // was built with.
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();

    let (status, _) = harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("radius_px", ParameterValue::Float(40.0)),
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        harness.live_effect_value(blur, "radius_px"),
        Some(ParameterValue::Float(40.0))
    );
    assert_eq!(
        harness.configured_effect_value("radius_px"),
        Some(ParameterValue::Float(8.0)),
        "a live value is never written back to the show"
    );
}

#[tokio::test]
async fn a_generators_parameter_can_be_moved_during_a_show_without_editing_the_show() {
    // The other half of the owner's instruction of 2026-09-23: a client and a
    // console reach a generator the same way they reach an effect, through one
    // parameter model. This is the client's half of that, and the assertion
    // that matters as much as the status code is the last one.
    let harness = Harness::start().await;
    let gradient = harness.open_show_with_a_gradient();

    let (status, _) = harness
        .put(
            &format!("/api/v1/live/generators/{gradient}"),
            &one("angle_deg", ParameterValue::Float(180.0)),
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        harness.live_generator_value(gradient, "angle_deg"),
        Some(ParameterValue::Float(180.0))
    );
    assert_eq!(
        harness.configured_generator_value("angle_deg"),
        Some(ParameterValue::Float(45.0)),
        "a live value is never written back to the show"
    );
}

#[tokio::test]
async fn a_live_generator_value_outside_the_manifests_range_is_refused() {
    // The same guard the effects have, because it is the same code: a manifest
    // says what a parameter may be and nothing gets past it.
    let harness = Harness::start().await;
    let gradient = harness.open_show_with_a_gradient();

    let (status, body) = harness
        .put(
            &format!("/api/v1/live/generators/{gradient}"),
            &one("angle_deg", ParameterValue::Float(4_000.0)),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_parameter");
    assert_eq!(
        harness.live_generator_value(gradient, "angle_deg"),
        None,
        "a refused request changes nothing"
    );
}

#[tokio::test]
async fn a_live_value_for_a_generator_the_show_does_not_have_is_refused() {
    let harness = Harness::start().await;
    harness.open_show_with_a_gradient();

    let stranger = ovs_types::id::GeneratorInstanceId::new();
    let (status, _) = harness
        .put(
            &format!("/api/v1/live/generators/{stranger}"),
            &one("angle_deg", ParameterValue::Float(90.0)),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = harness
        .put(
            "/api/v1/live/generators/not-an-identifier",
            &one("angle_deg", ParameterValue::Float(90.0)),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn putting_a_generator_back_clears_every_live_value_on_it() {
    // A reset rather than the configured values, for the reason the effects
    // give: a live value equal to the configured one is still a live value, and
    // the two would part company the moment somebody edited the show.
    let harness = Harness::start().await;
    let gradient = harness.open_show_with_a_gradient();

    harness
        .put(
            &format!("/api/v1/live/generators/{gradient}"),
            &one("angle_deg", ParameterValue::Float(180.0)),
        )
        .await;
    assert!(harness
        .live_generator_value(gradient, "angle_deg")
        .is_some());

    let (status, _) = harness
        .put(
            &format!("/api/v1/live/generators/{gradient}"),
            &UpdateEffectRequest {
                overrides: ParameterMap::new(),
                reset: true,
            },
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(harness.live_generator_value(gradient, "angle_deg"), None);
    assert_eq!(
        harness.configured_generator_value("angle_deg"),
        Some(ParameterValue::Float(45.0)),
        "and the show is where it always was"
    );
}

#[tokio::test]
async fn a_live_effect_value_outside_the_manifests_range_is_refused() {
    // A live control that accepted a blur radius of four thousand would be a
    // way to stop a show from a client.
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();

    let (status, body) = harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("radius_px", ParameterValue::Float(4_000.0)),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_parameter");
    assert!(
        body["message"]
            .as_str()
            .expect("a message")
            .contains("between 0 and 64"),
        "the operator is told the range rather than only that they missed it: {body}"
    );
    assert_eq!(
        harness.live_effect_value(blur, "radius_px"),
        None,
        "a refused request changes nothing"
    );
}

#[tokio::test]
async fn a_live_value_for_a_parameter_the_effect_never_declared_is_refused() {
    // Nearly always a typo or a client written against another version.
    // Accepting it silently means an operator moving a control and seeing
    // nothing happen.
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();

    let (status, body) = harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("brightness", ParameterValue::Float(1.0)),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .expect("a message")
            .contains("ovs.blur has no such parameter"),
        "the answer says which effect was asked: {body}"
    );
}

#[tokio::test]
async fn a_live_effect_value_of_the_wrong_type_is_refused() {
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();

    let (status, _) = harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("radius_px", ParameterValue::Bool(true)),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_live_value_for_an_effect_the_show_does_not_have_is_refused() {
    let harness = Harness::start().await;
    harness.open_show_with_a_blur();

    let stranger = EffectInstanceId::new();
    let (status, _) = harness
        .put(
            &format!("/api/v1/live/effects/{stranger}"),
            &one("radius_px", ParameterValue::Float(4.0)),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = harness
        .put(
            "/api/v1/live/effects/not-an-identifier",
            &one("radius_px", ParameterValue::Float(4.0)),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_operator_can_put_an_effect_back_the_way_the_show_was_built() {
    // "No override" is an absence, so there is no value a client could send
    // that means it.
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();
    harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("radius_px", ParameterValue::Float(40.0)),
        )
        .await;

    let (status, _) = harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &UpdateEffectRequest {
                overrides: ParameterMap::new(),
                reset: true,
            },
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(harness.live_effect_value(blur, "radius_px"), None);
    assert_eq!(
        harness.configured_effect_value("radius_px"),
        Some(ParameterValue::Float(8.0)),
        "which is where the effect is running again"
    );
}

#[tokio::test]
async fn reopening_the_show_gives_the_configured_effect_value_back() {
    // The promise a live control makes. Nothing that happened during the show
    // reaches the show, so the next time it opens it is the value it was set
    // up with.
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();
    harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("radius_px", ParameterValue::Float(40.0)),
        )
        .await;

    // Exactly what a restart does: the show as it stands, plus the small
    // snapshot that survives one.
    harness.state.mutate_state(|current| {
        let show = current.show().expect("open").clone();
        let snapshot = current.live_snapshot();
        let change = current.load_show(show, Some(&snapshot));
        ((), Some(change))
    });

    assert_eq!(harness.live_effect_value(blur, "radius_px"), None);
    assert_eq!(
        harness.configured_effect_value("radius_px"),
        Some(ParameterValue::Float(8.0))
    );
}

#[tokio::test]
async fn what_landed_on_an_effect_comes_back_on_the_stream() {
    // A client shows a control as pending until the server reports what landed,
    // and never retries. An override that changed the show and said nothing
    // would leave that control pending for ever.
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();
    let mut updates = harness.watch();

    let (status, _) = harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("radius_px", ParameterValue::Float(40.0)),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let StateChange::EffectChanged {
        effect_instance_id,
        overrides,
    } = next_change(&mut updates)
    else {
        panic!("expected an effect change");
    };
    assert_eq!(effect_instance_id, blur);
    assert_eq!(
        overrides.get("radius_px"),
        Some(&ParameterValue::Float(40.0)),
        "what landed, not what was asked for"
    );
}

#[tokio::test]
async fn a_refused_override_is_distinguishable_from_one_that_has_not_landed() {
    // The same rule that makes a control pending makes this matter: a client
    // that could not tell the two apart would either clear a control the server
    // rejected or leave one pending that it accepted. The answer says which,
    // the same way every other live route says it.
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();
    let mut updates = harness.watch();

    let (status, body) = harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("radius_px", ParameterValue::Float(4_000.0)),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "refused, and it says so");
    assert_eq!(body["code"], "invalid_parameter");
    assert!(
        updates.try_recv().is_err(),
        "and nothing went out that a client could mistake for it landing"
    );
}

#[tokio::test]
async fn a_client_joining_mid_show_is_told_what_the_effects_are_running_at() {
    // A snapshot is what a client gets on connect and after a resynchronisation.
    // One that carried only the changes from then on would draw every effect
    // control at its configured value while the wall showed something else.
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();
    harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("radius_px", ParameterValue::Float(40.0)),
        )
        .await;

    let snapshot = harness.state.with_state(|current| current.snapshot());

    assert_eq!(
        snapshot
            .effects
            .get(&blur)
            .and_then(|live| live.get("radius_px")),
        Some(&ParameterValue::Float(40.0))
    );
}

#[tokio::test]
async fn putting_an_effect_back_says_so_on_the_stream_too() {
    // An empty set is how "this effect is at what the show configured" is
    // said, and a client waiting for an acknowledgement needs to hear it.
    let harness = Harness::start().await;
    let blur = harness.open_show_with_a_blur();
    harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &one("radius_px", ParameterValue::Float(40.0)),
        )
        .await;
    let mut updates = harness.watch();

    harness
        .put(
            &format!("/api/v1/live/effects/{blur}"),
            &UpdateEffectRequest {
                overrides: ParameterMap::new(),
                reset: true,
            },
        )
        .await;

    let StateChange::EffectChanged { overrides, .. } = next_change(&mut updates) else {
        panic!("expected an effect change");
    };
    assert!(overrides.is_empty(), "nothing is overridden on it any more");
}
