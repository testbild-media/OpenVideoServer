//! Saving the show that is on stage.
//!
//! The owner's instruction (2026-09-23): **"A save must make no change to the
//! live show. Only loading a new or old show must reload everything and
//! possibly restart. A short outage or a reset while saving is not an
//! option."**
//!
//! This used to be exactly wrong: `save` installed the stored document with
//! `load_show`, which applies the restart policy — every layer back to its
//! opening slot, the master back to its opening dimmer, the clock back to beat
//! zero. A blackout and a restart in the middle of a night, as the price of
//! storing a change.
//!
//! So what is held here is that a save moves nothing, and that opening a
//! show still does.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{
    CreateAccountRequest, LoginRequest, LoginResponse, SaveShowRequest, SelectSlotRequest,
    UpdateLayerRequest, UpdateMasterRequest,
};
use ovs_types::config::{Layer, MasterConfig, Route, Show, Slot};
use ovs_types::id::{LayerId, MediaItemId, ShowId};
use ovs_types::live::MasterLive;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    app: axum::Router,
    token: String,
    state: AppState,
    show_id: ShowId,
    layer_id: LayerId,
}

impl Harness {
    /// A signed-in operator with a show open and stored.
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
        show.master = MasterConfig {
            default_bpm: 120.0,
            default_dimmer: 1.0,
            default_crossfader: 0.0,
            ..MasterConfig::default()
        };
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        layer.slots.push(Slot::media(2, MediaItemId::new()));
        let layer_id = layer.id;
        let show_id = show.id;
        show.layers.push(layer);

        // Stored, then loaded *as stored*. The server never does otherwise —
        // opening a show reads it back from the database — and a harness
        // that loaded the unstamped copy left the running document's
        // `updated_at` a millisecond behind the stored one, which is a test
        // that passes alone and fails in a full run.
        let updated_at = state
            .with_database(|database| database.save_show(&show))
            .expect("stores");
        let show = Show { updated_at, ..show };
        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });

        Self {
            app,
            token: login.token,
            state,
            show_id,
            layer_id,
        }
    }

    /// Moves the show off its opening state, so a reset would show.
    async fn set_a_look(&self) {
        let (status, _) = self
            .post(
                &format!("/api/v1/live/layers/{}/slot", self.layer_id),
                &SelectSlotRequest { slot_index: 2 },
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = self
            .put(
                &format!("/api/v1/live/layers/{}", self.layer_id),
                &UpdateLayerRequest {
                    opacity: Some(0.25),
                    route: Some(Route::B),
                    ..UpdateLayerRequest::default()
                },
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = self
            .put(
                "/api/v1/live/master",
                &UpdateMasterRequest {
                    dimmer: Some(0.5),
                    bpm: Some(174.0),
                    ..UpdateMasterRequest::default()
                },
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    /// The document as the server holds it, which is what a save sends back.
    fn running_document(&self) -> Show {
        self.state
            .with_state(|current| current.show().cloned())
            .expect("a show is open")
    }

    async fn save(&self, show: Show) -> StatusCode {
        let (status, body) = self
            .put(
                &format!("/api/v1/shows/{}", self.show_id),
                &SaveShowRequest { show },
            )
            .await;
        assert!(status.is_success(), "the save was refused: {status} {body}");
        status
    }

    /// Stores what is on stage, which is the live screen's Save.
    async fn capture(&self) -> StatusCode {
        self.post_empty(&format!("/api/v1/shows/{}/capture", self.show_id))
            .await
    }

    fn layer_live(&self) -> ovs_types::live::LayerLive {
        self.state
            .with_state(|current| current.layer(self.layer_id).copied())
            .expect("the show's layer has live state")
    }

    fn master_live(&self) -> MasterLive {
        self.state.with_state(|current| *current.master())
    }

    async fn post<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        self.with_body("POST", path, body).await
    }

    async fn put<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        self.with_body("PUT", path, body).await
    }

    async fn with_body<T: serde::Serialize>(
        &self,
        method: &str,
        path: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::from(serde_json::to_vec(body).expect("serialises")))
            .expect("builds");
        send(&self.app, request).await
    }

    async fn post_empty(&self, path: &str) -> StatusCode {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        send(&self.app, request).await.0
    }
}

fn post_json<T: serde::Serialize>(path: &str, body: &T) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).expect("serialises")))
        .expect("builds")
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

#[tokio::test]
async fn saving_the_open_show_does_not_move_anything_on_stage() {
    // The whole point. "A short outage or a reset while saving is not an
    // option" (owner, 2026-09-23).
    let harness = Harness::start().await;
    harness.set_a_look().await;
    let layer = harness.layer_live();
    let master = harness.master_live();

    harness.save(harness.running_document()).await;

    assert_eq!(harness.layer_live(), layer, "the layer was reset by a save");
    assert_eq!(
        harness.master_live(),
        master,
        "the master was reset by a save"
    );
    assert_eq!(
        harness.layer_live().active_slot,
        2,
        "the layer came off the slot it was playing"
    );
}

#[tokio::test]
async fn saving_an_edited_document_puts_the_edit_on_stage_without_restarting() {
    // The document has to follow: an operator who renames a layer and saves
    // wants the new name on screen. What must not follow is the live state.
    let harness = Harness::start().await;
    harness.set_a_look().await;

    let mut document = harness.running_document();
    document.layers[0].name = "Renamed".to_owned();
    harness.save(document).await;

    assert_eq!(
        harness.state.with_state(|current| current
            .show()
            .and_then(|show| show.layer(harness.layer_id))
            .map(|layer| layer.name.clone())),
        Some("Renamed".to_owned())
    );
    assert_eq!(harness.layer_live().active_slot, 2);
}

#[tokio::test]
async fn a_save_that_captures_writes_what_is_on_stage_into_the_document() {
    // "Save stores the complete current live state into the show."
    let harness = Harness::start().await;
    harness.set_a_look().await;

    assert_eq!(harness.capture().await, StatusCode::OK);

    let stored = harness
        .state
        .with_database(|database| database.load_show(harness.show_id))
        .expect("loads");
    let layer = stored.layer(harness.layer_id).expect("the layer");
    assert!((layer.default_opacity - 0.25).abs() < 1e-6, "{layer:?}");
    assert_eq!(layer.route, Route::B);
    assert!((stored.master.default_dimmer - 0.5).abs() < 1e-6);
    assert!((stored.master.default_bpm - 174.0).abs() < 1e-6);

    // And still nothing moved.
    assert_eq!(harness.layer_live().active_slot, 2);
}

#[tokio::test]
async fn a_save_that_does_not_capture_leaves_the_document_exactly_as_it_arrived() {
    // Somebody who has typed a default opacity into Settings must not have it
    // overwritten by whatever the fader happens to be at.
    let harness = Harness::start().await;
    harness.set_a_look().await;

    let mut document = harness.running_document();
    document.layers[0].default_opacity = 0.9;
    harness.save(document).await;

    let stored = harness
        .state
        .with_database(|database| database.load_show(harness.show_id))
        .expect("loads");
    let opacity = stored
        .layer(harness.layer_id)
        .expect("the layer")
        .default_opacity;
    assert!(
        (opacity - 0.9).abs() < 1e-6,
        "the editor's value was overwritten by the live one: {opacity}"
    );
}

#[tokio::test]
async fn opening_a_show_still_restarts_it() {
    // The other half of the owner's instruction: loading is the thing that
    // reloads. If this ever stopped being true, a show would never return to a
    // known state.
    let harness = Harness::start().await;
    harness.set_a_look().await;
    harness.save(harness.running_document()).await;

    let status = harness
        .post_empty(&format!("/api/v1/shows/{}/open", harness.show_id))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert_eq!(
        harness.layer_live().active_slot,
        Layer::BLACK_SLOT_INDEX,
        "opening a show did not apply the restart policy"
    );
    let master = harness.master_live();
    assert!((master.dimmer - 1.0).abs() < 1e-6, "{master:?}");
}
