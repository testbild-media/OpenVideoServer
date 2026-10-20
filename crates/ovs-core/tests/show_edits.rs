//! The edits an operator makes with a show running: filling a slot, emptying
//! one, and stacking an effect on a slot or a layer.
//!
//! The point of the endpoints under test is the thing that must *not* happen:
//! an operator dragging a clip onto an empty pad between two tracks must not
//! restart the show. Storing a whole show does restart it — that is what
//! opening a show means — so most of what is here checks that
//! these routes are not that.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{
    AddEffectRequest, AddEffectResponse, AssignSlotRequest, AssignSlotResponse,
    CreateAccountRequest, LoginRequest, LoginResponse, MoveSlotRequest, MoveSlotResponse,
    SelectSlotRequest, SetBanksRequest, SetMappingRequest, SetSlotRequest,
};
use ovs_proto::realtime::{ServerMessage, StateChange};
use ovs_types::config::{
    DisplayMode, Layer, OutputConfig, Piece, PlaybackDirection, PlaybackMode, Show, Slot, SlotSync,
    SourceRef,
};
use ovs_types::geometry::{FrameRate, RectPx, SizePx};
use ovs_types::id::{LayerId, MediaItemId};
use ovs_types::media::{MediaItem, MediaKind, MediaState};
use tokio::sync::broadcast::Receiver;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    app: axum::Router,
    token: String,
    state: AppState,
    show_id: ovs_types::id::ShowId,
    layer_id: LayerId,
    /// A clip in the library, ready to be dragged onto something.
    clip: MediaItemId,
    /// The clip slot 1 was built with, so "it changed" is distinguishable from
    /// "it was always that".
    original: MediaItemId,
}

impl Harness {
    /// A signed-in operator with a one-layer show open **and stored**.
    ///
    /// Stored as well as open, because this endpoint edits the document: a
    /// show that was only ever loaded would have nothing to edit.
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

        let clip = library_item(&state, "The new clip");
        let original = library_item(&state, "What slot 1 already had");

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        // Slot 1 is set up the way an operator would have set it up: not at the
        // defaults, so that an assignment which quietly reset it would show.
        let mut occupied = Slot::media(1, original);
        occupied.playback_mode = PlaybackMode::Bounce;
        occupied.sync = SlotSync::BeatSync { beats: 16 };
        layer.slots.push(occupied);
        let layer_id = layer.id;
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
        let show_id = show.id;
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
            clip,
            original,
        }
    }

    fn slot_path(&self, slot_index: u16) -> String {
        format!(
            "/api/v1/shows/{}/layers/{}/slots/{slot_index}",
            self.show_id, self.layer_id
        )
    }

    async fn assign(&self, slot_index: u16, source: SourceRef) -> (StatusCode, serde_json::Value) {
        self.put(&self.slot_path(slot_index), &AssignSlotRequest { source })
            .await
    }

    async fn set_playback(
        &self,
        slot_index: u16,
        change: &SetSlotRequest,
    ) -> (StatusCode, serde_json::Value) {
        self.put(&format!("{}/settings", self.slot_path(slot_index)), change)
            .await
    }

    async fn clear(&self, slot_index: u16) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("DELETE")
            .uri(self.slot_path(slot_index))
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

    async fn post<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::from(serde_json::to_vec(body).expect("serialises")))
            .expect("builds");
        send(&self.app, request).await
    }

    /// The layer's live state, as the server holds it.
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

    /// One slot of the running document.
    fn running_slot(&self, slot_index: u16) -> Option<Slot> {
        self.state.with_state(|current| {
            current
                .show()
                .and_then(|show| show.layer(self.layer_id))
                .and_then(|layer| layer.slot(slot_index))
                .cloned()
        })
    }

    /// One slot of the document as it is on disk, which is the one that
    /// survives a restart.
    fn stored_slot(&self, slot_index: u16) -> Option<Slot> {
        self.state
            .with_database(|database| database.load_show(self.show_id))
            .expect("loads")
            .layer(self.layer_id)
            .and_then(|layer| layer.slot(slot_index))
            .cloned()
    }

    async fn add_layer_effect(
        &self,
        type_id: &str,
        version: u32,
    ) -> (StatusCode, serde_json::Value) {
        self.post(
            &format!(
                "/api/v1/shows/{}/layers/{}/effects",
                self.show_id, self.layer_id
            ),
            &AddEffectRequest {
                type_id: type_id.to_owned(),
                version,
            },
        )
        .await
    }

    async fn add_slot_effect(
        &self,
        slot_index: u16,
        type_id: &str,
        version: u32,
    ) -> (StatusCode, serde_json::Value) {
        self.post(
            &format!("{}/effects", self.slot_path(slot_index)),
            &AddEffectRequest {
                type_id: type_id.to_owned(),
                version,
            },
        )
        .await
    }

    async fn remove_layer_effect(&self, effect_id: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("DELETE")
            .uri(format!(
                "/api/v1/shows/{}/layers/{}/effects/{effect_id}",
                self.show_id, self.layer_id
            ))
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        send(&self.app, request).await
    }

    /// The effect chain on the running layer.
    fn layer_chain(&self) -> Vec<String> {
        self.state.with_state(|current| {
            current
                .show()
                .and_then(|show| show.layer(self.layer_id))
                .map(|layer| {
                    layer
                        .effects
                        .iter()
                        .map(|effect| effect.type_id.clone())
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn watch(&self) -> Receiver<ServerMessage> {
        self.state.subscribe()
    }
}

/// Puts a ready item in the library and returns its identity.
fn library_item(state: &AppState, name: &str) -> MediaItemId {
    let item = MediaItem {
        kind: MediaKind::Video,
        state: MediaState::Ready,
        // A ready video has to know its length and its size, which is the
        // library's own rule and not this test's business beyond satisfying it.
        duration_ms: Some(4_000),
        dimensions: Some(SizePx::new(1920, 1080)),
        frame_rate: Some(FrameRate::new(25, 1)),
        ..MediaItem::importing(name, "clip.mov")
    };
    let id = item.id;
    state
        .with_database(|database| database.upsert_media_item(&item))
        .expect("stores the item");
    id
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

/// The next change on the stream, ignoring anything that is not one.
fn next_change(updates: &mut Receiver<ServerMessage>) -> Option<StateChange> {
    while let Ok(message) = updates.try_recv() {
        if let ServerMessage::Update { change, .. } = message {
            return Some(change);
        }
    }
    None
}

#[tokio::test]
async fn how_a_clip_runs_is_changed_with_the_show_on_and_nothing_restarts() {
    // The owner took the slot grid out of Settings on 2026-09-23, because a
    // slot is filled by dragging onto its pad in LIVE. What the clip in it
    // *does* — loop or once, forwards or backwards, its own rate or the beat —
    // went with it, and had to land somewhere that does not stop the show.
    let harness = Harness::start().await;

    // Somewhere in the middle of the evening: not on the opening slot, and
    // with a dimmer nobody wants reset.
    harness.state.mutate_state(|current| {
        let change = current.update_master(|master| master.set_dimmer(0.42));
        ((), change)
    });
    let before = harness.state.with_state(|current| current.sequence());

    let (status, body) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: None,
                playback_mode: Some(PlaybackMode::OnceFreeze),
                direction: Some(PlaybackDirection::Reverse),
                sync: Some(SlotSync::FreeRun),
                speed: None,
                transform: None,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let slot = harness.running_slot(1).expect("slot 1 is still there");
    assert_eq!(slot.playback_mode, PlaybackMode::OnceFreeze);
    assert_eq!(slot.direction, PlaybackDirection::Reverse);
    assert_eq!(slot.sync, SlotSync::FreeRun);

    // And the show did not go back to the top. This is the whole reason the
    // endpoint exists rather than the operator pressing Save.
    let dimmer = harness.state.with_state(|current| current.master().dimmer);
    assert!(
        (dimmer - 0.42).abs() < 0.001,
        "the master is where the operator left it: {dimmer}"
    );
    assert!(
        harness.state.with_state(|current| current.sequence()) > before,
        "and every client was told"
    );

    // Stored as well as running, because it is the document.
    assert_eq!(
        harness
            .stored_slot(1)
            .expect("slot 1 is on disk")
            .playback_mode,
        PlaybackMode::OnceFreeze,
    );
}

#[tokio::test]
async fn a_field_that_was_not_sent_is_left_alone() {
    // Every field is optional, so the one control the operator moved is the
    // only thing that travels. The slot starts on Bounce at sixteen beats.
    let harness = Harness::start().await;

    let (status, body) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: None,
                playback_mode: None,
                direction: Some(PlaybackDirection::Reverse),
                sync: None,
                speed: None,
                transform: None,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let slot = harness.running_slot(1).expect("slot 1 is still there");
    assert_eq!(slot.direction, PlaybackDirection::Reverse, "what was sent");
    assert_eq!(slot.playback_mode, PlaybackMode::Bounce, "and nothing else");
    assert_eq!(slot.sync, SlotSync::BeatSync { beats: 16 });
}

#[tokio::test]
async fn a_beat_count_the_model_refuses_leaves_the_slot_as_it_was() {
    // Validated as a whole document, like every other edit here, so a refusal
    // is a refusal with nothing written rather than half an edit.
    let harness = Harness::start().await;

    let (status, _) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: None,
                playback_mode: None,
                direction: None,
                sync: Some(SlotSync::BeatSync { beats: 0 }),
                speed: None,
                transform: None,
            },
        )
        .await;
    assert_ne!(status, StatusCode::OK, "zero beats is not a pass");

    assert_eq!(
        harness.running_slot(1).expect("slot 1 is still there").sync,
        SlotSync::BeatSync { beats: 16 },
        "the slot is as it was"
    );
}

#[tokio::test]
async fn setting_the_playback_of_a_slot_with_nothing_in_it_says_so() {
    let harness = Harness::start().await;

    let (status, _) = harness
        .set_playback(
            7,
            &SetSlotRequest {
                name: None,
                playback_mode: Some(PlaybackMode::Loop),
                direction: None,
                sync: None,
                speed: None,
                transform: None,
            },
        )
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_pad_takes_a_name_of_its_own_and_gives_it_back() {
    // The owner, 2026-10-15: "Manchmal kann ja der selbe Inhalt sein, aber mit
    // anderen Effekte drauf etc. für Dauerhafte installation wäre das noch
    // gut." Two pads of one clip with different effects on them read alike,
    // and on an installation that nobody is standing at every week, reading is
    // all there is.
    let harness = Harness::start().await;

    let (status, body) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: Some("  Intro – wide  ".to_owned()),
                playback_mode: None,
                direction: None,
                sync: None,
                speed: None,
                transform: None,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Trimmed, because a field an operator typed into has whitespace at one
    // end of it as often as not.
    assert_eq!(
        harness.running_slot(1).expect("slot 1").name.as_deref(),
        Some("Intro – wide")
    );
    assert_eq!(
        harness.stored_slot(1).expect("slot 1 is on disk").name,
        harness.running_slot(1).expect("slot 1").name,
        "the running show and the document agree"
    );

    // And off again. Empty text is how a pad goes back to being named after
    // what it plays; there is no other way to say it, since an absent field
    // means "leave this alone".
    let (status, body) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: Some(String::new()),
                playback_mode: None,
                direction: None,
                sync: None,
                speed: None,
                transform: None,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(harness.running_slot(1).expect("slot 1").name, None);
}

#[tokio::test]
async fn a_name_longer_than_a_pad_may_carry_leaves_the_slot_as_it_was() {
    // Validated as a whole document, like every other edit here. Sixty-four
    // characters is the ceiling; what arrives from a console or a script is
    // not obliged to respect it.
    let harness = Harness::start().await;

    let (status, _) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: Some("x".repeat(65)),
                playback_mode: None,
                direction: None,
                sync: None,
                speed: None,
                transform: None,
            },
        )
        .await;
    assert_ne!(status, StatusCode::OK, "sixty-five characters is too many");
    assert_eq!(
        harness.running_slot(1).expect("slot 1").name,
        None,
        "the slot is as it was"
    );
}

/// A wall an output could be driving, for the mapping tests.
fn a_wall(name: &str) -> OutputConfig {
    OutputConfig::new(
        name,
        DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
    )
}

#[tokio::test]
async fn a_mapping_reaches_the_wall_without_stopping_the_show() {
    // The owner, 2026-09-23: the mapping is to be made live, so that the
    // output being changed is the output being looked at.
    //
    // Lining a wall up is the one job that cannot be done blind — whether a
    // piece sits right is seen on the wall, not worked out from four numbers —
    // and until this route the only way to get a mapping onto a wall was to
    // save the show, which reloads the show.
    let harness = Harness::start().await;

    // Somewhere in the middle of the evening: a dimmer nobody wants reset.
    harness.state.mutate_state(|current| {
        let change = current.update_master(|master| master.set_dimmer(0.42));
        ((), change)
    });
    let before = harness.state.with_state(|current| current.sequence());

    let wall = a_wall("Upstage wall");
    let output_id = wall.id;
    let left = Piece::new(
        "Left half",
        output_id,
        RectPx::new(0, 0, 960, 1080),
        RectPx::new(0, 0, 960, 1080),
    );
    let right = Piece::new(
        "Right half",
        output_id,
        RectPx::new(960, 0, 960, 1080),
        RectPx::new(960, 0, 960, 1080),
    );

    let (status, body) = harness
        .put(
            &format!("/api/v1/shows/{}/mapping", harness.show_id),
            &SetMappingRequest {
                outputs: vec![wall],
                // Both pieces in one request, because a mapping is dragged as a
                // group: two pieces moved together must land in one frame.
                pieces: vec![left, right],
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // On the wall: the running document has it, without being reopened.
    let running = harness.state.with_state(|current| {
        let show = current.show().expect("a show is open");
        (show.outputs.len(), show.pieces.len())
    });
    assert_eq!((1, 2), running, "the mapping is under the running show");

    let dimmer = harness.state.with_state(|current| current.master().dimmer);
    assert!(
        (dimmer - 0.42).abs() < 0.001,
        "the master is where the operator left it: {dimmer}"
    );
    assert_eq!(
        harness
            .running_slot(1)
            .expect("slot 1 is still there")
            .source,
        SourceRef::Media {
            media_item_id: harness.original
        },
        "and the slots are untouched"
    );
    assert!(
        harness.state.with_state(|current| current.sequence()) > before,
        "and every client was told"
    );

    // Stored as well as running. A mapping made live survives a restart, which
    // is what makes Put back a restore of what the session opened with rather
    // than an undo of a gesture.
    let stored = harness
        .state
        .with_database(|database| database.load_show(harness.show_id))
        .expect("loads");
    assert_eq!(2, stored.pieces.len(), "the mapping is on disk");
}

#[tokio::test]
async fn a_mapping_the_model_refuses_leaves_the_wall_alone() {
    // A piece taking a region larger than the composition. Refused whole, with
    // nothing written and the wall still showing what it was showing — which
    // matters more here than elsewhere, because a live mapping sends on every
    // gesture and a partly applied one would be a wall in an impossible state.
    let harness = Harness::start().await;

    let wall = a_wall("Upstage wall");
    let output_id = wall.id;
    let too_big = Piece::new(
        "Bigger than the composition",
        output_id,
        RectPx::new(0, 0, 8000, 8000),
        RectPx::new(0, 0, 1920, 1080),
    );

    let (status, _) = harness
        .put(
            &format!("/api/v1/shows/{}/mapping", harness.show_id),
            &SetMappingRequest {
                outputs: vec![wall],
                pieces: vec![too_big],
            },
        )
        .await;
    assert_eq!(
        StatusCode::BAD_REQUEST,
        status,
        "a source region the composition does not contain"
    );

    let (outputs, pieces) = harness.state.with_state(|current| {
        let show = current.show().expect("a show is open");
        (show.outputs.len(), show.pieces.len())
    });
    assert_eq!((0, 0), (outputs, pieces), "the wall was left alone");
}

#[tokio::test]
async fn a_piece_naming_an_output_the_request_does_not_carry_is_refused() {
    // The reason outputs and pieces travel together. A client that sent its
    // pieces first would, for one request, be describing a wall that is not
    // there — and the model says so rather than storing it.
    let harness = Harness::start().await;

    let orphan = Piece::new(
        "Nowhere",
        a_wall("A wall that was not sent").id,
        RectPx::new(0, 0, 1920, 1080),
        RectPx::new(0, 0, 1920, 1080),
    );

    let (status, _) = harness
        .put(
            &format!("/api/v1/shows/{}/mapping", harness.show_id),
            &SetMappingRequest {
                outputs: Vec::new(),
                pieces: vec![orphan],
            },
        )
        .await;
    assert_eq!(StatusCode::BAD_REQUEST, status);

    assert_eq!(
        0,
        harness
            .state
            .with_state(|current| current.show().expect("open").pieces.len()),
        "nothing was written"
    );
}

#[tokio::test]
async fn a_slot_is_given_a_rate_of_its_own_and_put_back_to_one() {
    // The owner, 2026-09-23: "pro Slot Scale sollt aber verfügbar sein für Free
    // Run und BPM Sync."
    //
    // This test used to set a trigger grid, which went the same day: "Trigger
    // Grid weg. Beats ist das was man braucht." What a slot owns about time now
    // is how long its pass is and how fast it runs through one.
    let harness = Harness::start().await;

    let (status, body) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: None,
                playback_mode: None,
                direction: None,
                sync: None,
                speed: Some(2.0),
                transform: None,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(harness.running_slot(1).expect("slot 1").speed, 2.0);

    // Something else about the slot, with the rate left out: it stays.
    let (status, body) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: None,
                playback_mode: Some(PlaybackMode::OnceFreeze),
                direction: None,
                sync: None,
                speed: None,
                transform: None,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        harness.running_slot(1).expect("slot 1").speed,
        2.0,
        "an absent field leaves it alone"
    );

    // And back to the clip's own rate.
    let (status, body) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: None,
                playback_mode: None,
                direction: None,
                sync: None,
                speed: Some(1.0),
                transform: None,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(harness.running_slot(1).expect("slot 1").speed, 1.0);

    // Stored as well as running, because it is the document.
    assert_eq!(harness.stored_slot(1).expect("on disk").speed, 1.0);

    // A rate off the ladder is refused, with nothing written.
    let (status, _) = harness
        .set_playback(
            1,
            &SetSlotRequest {
                name: None,
                playback_mode: None,
                direction: None,
                sync: None,
                speed: Some(40.0),
                transform: None,
            },
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        harness.running_slot(1).expect("slot 1").speed,
        1.0,
        "a refused edit changed nothing"
    );
}

#[tokio::test]
async fn a_clip_dropped_on_an_empty_pad_is_in_the_show_and_on_disk() {
    let harness = Harness::start().await;

    let (status, body) = harness
        .assign(
            5,
            SourceRef::Media {
                media_item_id: harness.clip,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let answer: AssignSlotResponse = serde_json::from_value(body).expect("an assign response");
    assert_eq!(answer.replaced, None, "the pad was empty");

    let slot = harness.running_slot(5).expect("the show has slot 5 now");
    assert_eq!(slot.media_item_id(), Some(harness.clip));
    assert_eq!(
        harness.stored_slot(5).and_then(|slot| slot.media_item_id()),
        Some(harness.clip),
        "an edit that is not on disk is lost at the next restart"
    );
}

#[tokio::test]
async fn a_generator_dropped_on_a_pad_is_given_an_identity_on_disk() {
    // **The identity has to be in the row, not only in the running state.**
    //
    // A patch entry points at a generator instance and lives in the same
    // document. `replace_show` mints one into the live state, so this worked
    // for as long as the server stayed up; the row kept none, and the next open
    // minted a different one — so a fixture patched against the first was
    // pointing at a generator that no longer existed. The whole-document save
    // had always done this. This door had not.
    //
    // Found on the live server (2026-10-13), where a generator assigned this
    // way could not be driven through `PUT /live/generators/{id}` at all. Our
    // own client never hit it: it mints an identity before it sends.
    let harness = Harness::start().await;

    let (status, body) = harness
        .assign(
            5,
            SourceRef::Generator {
                instance_id: ovs_types::id::GeneratorInstanceId::nil(),
                type_id: "ovs.solid_colour".to_owned(),
                version: 1,
                locked: false,
                parameters: ovs_types::config::slot::ParameterMap::new(),
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let on_disk = match harness.stored_slot(5).expect("the row has slot 5").source {
        SourceRef::Generator { instance_id, .. } => instance_id,
        other => panic!("the stored slot holds {other:?}"),
    };
    assert!(
        !on_disk.is_nil(),
        "the stored generator has no identity, so nothing in the document can point at it"
    );

    let running = match harness.running_slot(5).expect("the show has slot 5").source {
        SourceRef::Generator { instance_id, .. } => instance_id,
        other => panic!("the running slot holds {other:?}"),
    };
    assert_eq!(
        running, on_disk,
        "the row and the running show disagree about what the generator is called"
    );
}

// --- moving a pad (owner, 2026-10-21: "Slot 1 per Drag and Drop auf Slot 10") --

impl Harness {
    async fn move_slot(&self, from: u16, to: u16) -> (StatusCode, serde_json::Value) {
        self.post(
            &format!("{}/move", self.slot_path(from)),
            &MoveSlotRequest { to },
        )
        .await
    }

    async fn select(&self, slot_index: u16) {
        let (status, _) = self
            .post(
                &format!("/api/v1/live/layers/{}/slot", self.layer_id),
                &SelectSlotRequest { slot_index },
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
}

#[tokio::test]
async fn a_slot_dragged_to_an_empty_pad_goes_there_whole_and_on_disk() {
    let harness = Harness::start().await;

    let (status, body) = harness.move_slot(1, 10).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer: MoveSlotResponse = serde_json::from_value(body).expect("a move response");
    assert!(!answer.swapped, "pad 10 was empty");

    assert!(harness.running_slot(1).is_none(), "pad 1 is empty now");
    let moved = harness.running_slot(10).expect("slot 10 has it");
    assert_eq!(moved.media_item_id(), Some(harness.original));
    // How it was set up went with it.
    assert_eq!(moved.playback_mode, PlaybackMode::Bounce);
    assert_eq!(moved.sync, SlotSync::BeatSync { beats: 16 });
    assert_eq!(
        harness
            .stored_slot(10)
            .and_then(|slot| slot.media_item_id()),
        Some(harness.original),
        "a move that is not on disk is undone by the next restart"
    );
    assert!(harness.stored_slot(1).is_none());
}

#[tokio::test]
async fn a_slot_dragged_onto_a_taken_pad_swaps_with_it() {
    let harness = Harness::start().await;
    let (status, _) = harness
        .assign(
            10,
            SourceRef::Media {
                media_item_id: harness.clip,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = harness.move_slot(1, 10).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer: MoveSlotResponse = serde_json::from_value(body).expect("a move response");
    assert!(answer.swapped);

    assert_eq!(
        harness
            .running_slot(10)
            .and_then(|slot| slot.media_item_id()),
        Some(harness.original)
    );
    assert_eq!(
        harness
            .running_slot(1)
            .and_then(|slot| slot.media_item_id()),
        Some(harness.clip),
        "what was at 10 is at 1, not gone"
    );
}

#[tokio::test]
async fn the_layer_showing_a_moved_slot_goes_with_it() {
    // An edit does not change what is on stage. Left where it was, the layer
    // would show whatever arrived at its old number — here, nothing.
    let harness = Harness::start().await;
    harness.select(1).await;

    let (status, body) = harness.move_slot(1, 10).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(harness.layer_live().active_slot, 10);

    // And the other way round: a layer on the pad that is swapped *into*
    // follows what it was showing to the pad that one left.
    let (status, _) = harness
        .assign(
            3,
            SourceRef::Media {
                media_item_id: harness.clip,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = harness.move_slot(3, 10).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(harness.layer_live().active_slot, 3);
    assert_eq!(
        harness
            .running_slot(3)
            .and_then(|slot| slot.media_item_id()),
        Some(harness.original),
        "slot 3 now holds what the layer was showing"
    );
}

#[tokio::test]
async fn a_layer_on_another_pad_is_left_where_it_is() {
    let harness = Harness::start().await;
    harness.select(5).await;
    let before = harness.layer_live();

    let (status, _) = harness.move_slot(1, 10).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(harness.layer_live(), before);
}

#[tokio::test]
async fn moving_an_empty_pad_says_so_and_dropping_one_on_itself_stores_nothing() {
    let harness = Harness::start().await;

    let (status, body) = harness.move_slot(4, 10).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "empty_slot", "{body}");

    let mut updates = harness.watch();
    let (status, body) = harness.move_slot(1, 1).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        next_change(&mut updates).is_none(),
        "a drop where it was picked up is not a change to publish"
    );
    assert!(harness.running_slot(1).is_some());
}

#[tokio::test]
async fn a_move_to_slot_zero_or_past_the_last_bank_is_refused() {
    let harness = Harness::start().await;
    for to in [0, Layer::MAX_SLOT_INDEX + 1] {
        let (status, body) = harness.move_slot(1, to).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid_slot", "{body}");
    }
    assert!(harness.running_slot(1).is_some(), "it stayed where it was");
}

#[tokio::test]
async fn filling_a_pad_does_not_restart_the_show() {
    // The whole reason this endpoint exists. Saving the show would reload it
    // on the server, which puts every layer back to its opening slot — in front
    // of an audience, because somebody filled a pad they were not using.
    let harness = Harness::start().await;

    let (status, _) = harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            &SelectSlotRequest { slot_index: 1 },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let before = harness.layer_live();
    assert_eq!(before.active_slot, 1, "the show is playing slot 1");

    let (status, _) = harness
        .assign(
            7,
            SourceRef::Media {
                media_item_id: harness.clip,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let after = harness.layer_live();
    assert_eq!(
        after.active_slot, 1,
        "the layer came off the slot it was on"
    );
    assert_eq!(after, before, "something about the layer moved");
}

#[tokio::test]
async fn what_the_show_hears_about_carries_the_live_state_it_already_had() {
    // A peer applies the document *and* the live state that travels with it. If
    // the live state sent were the restart policy's rather than the one running,
    // every client and the renderer would jump even though the server had not.
    let harness = Harness::start().await;
    harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            &SelectSlotRequest { slot_index: 1 },
        )
        .await;

    let mut updates = harness.watch();
    harness
        .assign(
            4,
            SourceRef::Media {
                media_item_id: harness.clip,
            },
        )
        .await;

    let change = next_change(&mut updates).expect("the edit was published");
    let StateChange::ShowLoaded { show, layers, .. } = change else {
        panic!("the document changed, so the document is what goes out");
    };
    let show = show.expect("a show is open");
    assert!(
        show.layer(harness.layer_id)
            .and_then(|layer| layer.slot(4))
            .is_some(),
        "the new slot is in what was published"
    );
    assert_eq!(
        layers
            .iter()
            .find(|live| live.layer_id == harness.layer_id)
            .map(|live| live.active_slot),
        Some(1),
        "the live state sent was the restart policy's, not the one running"
    );
}

#[tokio::test]
async fn dropping_a_clip_on_an_occupied_pad_changes_the_clip_and_nothing_else() {
    // An operator who set a pad to bounce over sixteen beats and then drops a
    // different clip on it has changed the clip, not the way the pad plays.
    let harness = Harness::start().await;

    let (status, body) = harness
        .assign(
            1,
            SourceRef::Media {
                media_item_id: harness.clip,
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer: AssignSlotResponse = serde_json::from_value(body).expect("an assign response");
    assert_eq!(
        answer.replaced,
        Some(SourceRef::Media {
            media_item_id: harness.original
        }),
        "the answer has to say what was actually there"
    );

    let slot = harness.running_slot(1).expect("slot 1 is still there");
    assert_eq!(slot.media_item_id(), Some(harness.clip));
    assert_eq!(slot.playback_mode, PlaybackMode::Bounce);
    assert_eq!(slot.sync, SlotSync::BeatSync { beats: 16 });
}

#[tokio::test]
async fn a_clip_the_library_does_not_have_is_refused_with_the_pad_untouched() {
    // A slot pointing at nothing is a layer showing its placeholder on the
    // night, which reads as a fault rather than as the mistake it was.
    let harness = Harness::start().await;

    let (status, _) = harness
        .assign(
            1,
            SourceRef::Media {
                media_item_id: MediaItemId::new(),
            },
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    assert_eq!(
        harness
            .running_slot(1)
            .and_then(|slot| slot.media_item_id()),
        Some(harness.original),
        "a refused edit changed the show anyway"
    );
}

#[tokio::test]
async fn the_black_slot_and_the_end_of_the_matrix_are_both_refused() {
    let harness = Harness::start().await;
    let source = SourceRef::Media {
        media_item_id: harness.clip,
    };

    let (status, _) = harness.assign(0, source.clone()).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "slot 0 is permanently Black and is not storage"
    );

    let past_the_end = Layer::MAX_SLOT_INDEX + 1;
    let (status, _) = harness.assign(past_the_end, source).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn emptying_the_pad_that_is_on_stage_takes_the_layer_to_black() {
    // Not to the next slot along, and not to a slot that no longer exists: a
    // layer pointing at nothing draws its placeholder, and black is the one
    // answer that is never wrong.
    let harness = Harness::start().await;
    harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            &SelectSlotRequest { slot_index: 1 },
        )
        .await;
    assert_eq!(harness.layer_live().active_slot, 1);

    let (status, body) = harness.clear(1).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer: AssignSlotResponse = serde_json::from_value(body).expect("an assign response");
    assert_eq!(
        answer.replaced,
        Some(SourceRef::Media {
            media_item_id: harness.original
        })
    );

    assert_eq!(harness.running_slot(1), None, "the pad is empty now");
    assert_eq!(
        harness.layer_live().active_slot,
        Layer::BLACK_SLOT_INDEX,
        "the layer is still pointing at a slot that is gone"
    );
}

#[tokio::test]
async fn emptying_a_pad_that_is_already_empty_changes_nothing_and_says_nothing() {
    let harness = Harness::start().await;
    let before = harness
        .state
        .with_database(|database| database.load_show(harness.show_id))
        .expect("loads")
        .updated_at;

    let mut updates = harness.watch();
    let (status, body) = harness.clear(9).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer: AssignSlotResponse = serde_json::from_value(body).expect("an assign response");
    assert_eq!(answer.replaced, None);
    assert_eq!(
        answer.updated_at, before,
        "an edit that changed nothing moved the show's timestamp"
    );
    assert!(
        next_change(&mut updates).is_none(),
        "an edit that changed nothing was published to every client"
    );
}

#[tokio::test]
async fn a_show_that_is_not_loaded_cannot_be_edited_at_all() {
    // **The rule, and it used to be the opposite.** This test asserted that a
    // stored show could be edited without opening it, on the reasoning that the
    // route is about the document and that it is usually the open one is a fact
    // about the workflow rather than a precondition.
    //
    // The owner made it a precondition on 2026-09-24: "man kann keine fremde
    // show bearbeiten wenn eine andere show geladen ist." So the editor is
    // always editing what is on the wall, and there is one answer to "what am I
    // editing".
    let harness = Harness::start().await;
    harness.state.mutate_state(|current| {
        let change = current.close_show();
        ((), change)
    });

    let (status, body) = harness
        .assign(
            3,
            SourceRef::Media {
                media_item_id: harness.clip,
            },
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "show_not_loaded");

    // And nothing was written. A refusal that had already stored half the edit
    // would be worse than no refusal.
    assert_eq!(
        harness.stored_slot(3).and_then(|slot| slot.media_item_id()),
        None
    );

    // The same rule through the other door: a whole-document save.
    let stored = harness
        .state
        .with_database(|database| database.load_show(harness.show_id))
        .expect("the show is still on disc");
    let (status, body) = harness
        .put(
            &format!("/api/v1/shows/{}", stored.id),
            &ovs_proto::http::SaveShowRequest { show: stored },
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "show_not_loaded");
}

// --- effects stack ------------------------------------------------------------

#[tokio::test]
async fn an_effect_dropped_on_a_layer_goes_on_top_of_what_is_there() {
    // “They never replace anything — they stack on top” (owner, 2026-09-23).
    let harness = Harness::start().await;

    let (status, body) = harness.add_layer_effect("ovs.blur", 1).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let first: AddEffectResponse = serde_json::from_value(body).expect("an add response");

    let (status, body) = harness.add_layer_effect("ovs.colour_correct", 1).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let second: AddEffectResponse = serde_json::from_value(body).expect("an add response");

    assert_ne!(first.effect_instance_id, second.effect_instance_id);
    assert_eq!(
        harness.layer_chain(),
        vec!["ovs.blur".to_owned(), "ovs.colour_correct".to_owned()],
        "the second effect replaced the first, or went in front of it"
    );
}

#[tokio::test]
async fn stacking_an_effect_does_not_restart_the_show() {
    let harness = Harness::start().await;
    harness
        .post(
            &format!("/api/v1/live/layers/{}/slot", harness.layer_id),
            &SelectSlotRequest { slot_index: 1 },
        )
        .await;
    let before = harness.layer_live();

    let (status, _) = harness.add_layer_effect("ovs.blur", 1).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        harness.layer_live(),
        before,
        "something about the layer moved"
    );
}

#[tokio::test]
async fn an_effect_this_build_does_not_publish_is_refused() {
    // A show naming an effect nobody can run is a layer that draws something
    // other than what the document says, and the moment to say so is now.
    let harness = Harness::start().await;

    let (status, body) = harness.add_layer_effect("ovs.not_a_real_effect", 1).await;
    // A bad request rather than an unprocessable entity, which is what every
    // other refused document in this server answers with (`ApiFailure::invalid`).
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.to_string().contains("does not publish"),
        "the refusal has to say why, in the model's own words: {body}"
    );
    assert!(
        harness.layer_chain().is_empty(),
        "a refused edit changed the document"
    );
}

#[tokio::test]
async fn an_effect_can_be_taken_off_again() {
    let harness = Harness::start().await;
    let (_, body) = harness.add_layer_effect("ovs.blur", 1).await;
    let added: AddEffectResponse = serde_json::from_value(body).expect("an add response");

    let (status, _) = harness
        .remove_layer_effect(&added.effect_instance_id.to_string())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(harness.layer_chain().is_empty());

    let (status, _) = harness
        .remove_layer_effect(&added.effect_instance_id.to_string())
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "removing it twice claimed to work"
    );
}

#[tokio::test]
async fn an_effect_on_an_empty_pad_is_refused_rather_than_creating_a_slot() {
    // An effect belongs to a source. A pad with nothing in it has none, and
    // inventing a slot to hang one on would put something in the matrix that
    // the operator did not ask for.
    let harness = Harness::start().await;

    let (status, _) = harness.add_slot_effect(9, "ovs.blur", 1).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = harness.add_slot_effect(1, "ovs.blur", 1).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let chain = harness
        .running_slot(1)
        .expect("slot 1 is there")
        .effects
        .len();
    assert_eq!(chain, 1);
}

// --- how much matrix a layer shows --------------------------------------------

#[tokio::test]
async fn a_bank_an_operator_asks_for_stays_even_though_it_is_empty() {
    // The whole reason the count is stored rather than derived. An operator
    // right-clicks for the next group in order to fill it.
    let harness = Harness::start().await;

    let (status, body) = harness
        .put(
            &format!(
                "/api/v1/shows/{}/layers/{}/banks",
                harness.show_id, harness.layer_id
            ),
            &SetBanksRequest { banks: 3 },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let banks = harness.state.with_state(|current| {
        current
            .show()
            .and_then(|show| show.layer(harness.layer_id))
            .map(ovs_types::config::Layer::bank_count)
    });
    assert_eq!(banks, Some(3));
}

#[tokio::test]
async fn asking_for_fewer_banks_than_the_layer_uses_is_refused_rather_than_hiding_slots() {
    let harness = Harness::start().await;
    harness
        .assign(
            20,
            SourceRef::Media {
                media_item_id: harness.clip,
            },
        )
        .await;

    let (status, _) = harness
        .put(
            &format!(
                "/api/v1/shows/{}/layers/{}/banks",
                harness.show_id, harness.layer_id
            ),
            &SetBanksRequest { banks: 1 },
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_layer_cannot_be_given_more_banks_than_the_model_allows() {
    // Eighty-four slots, which is seven banks (owner, 2026-09-23).
    let harness = Harness::start().await;

    let (status, _) = harness
        .put(
            &format!(
                "/api/v1/shows/{}/layers/{}/banks",
                harness.show_id, harness.layer_id
            ),
            &SetBanksRequest {
                banks: ovs_types::config::Layer::MAX_BANKS + 1,
            },
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
