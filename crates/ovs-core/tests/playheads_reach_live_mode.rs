//! Where a clip has got to, from the renderer to the Live Mode screen.
//!
//! Each half of this is tested on its own: the scheduler records a playhead
//! where it works one out, and the status endpoint reports whatever the core was
//! told. What no unit test can show is that the two ends agree — that a position
//! measured on the render thread survives the control channel, the core's reader
//! thread and the JSON, and arrives as a number a client can draw a bar with.
//!
//! Headless throughout: nothing here needs a GPU or a decoder, because what is
//! being proved is the wire and not the arithmetic.

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{renderer_link, router, AppState};
use ovs_db::Database;
use ovs_proto::http::{CreateAccountRequest, LoginRequest, LoginResponse};
use ovs_renderer::control;
use ovs_renderer::show_state::SharedShowState;
use ovs_types::id::{LayerId, MediaItemId};
use ovs_types::telemetry::{LayerPlayhead, ServerTelemetry};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

/// Creates the first account and signs in, returning the token.
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

    let (status, body) = send(
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
    assert_eq!(status, StatusCode::OK);
    let login: LoginResponse = serde_json::from_value(body).expect("a login response");
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

fn post_json<T: serde::Serialize>(path: &str, body: &T) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).expect("serialises")))
        .expect("builds")
}

/// Asks the status endpoint what the server is doing.
async fn status(app: &axum::Router, token: &str) -> ServerTelemetry {
    let request = Request::builder()
        .uri("/api/v1/server/status")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("reads")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("a status response");
    serde_json::from_value(body["telemetry"].clone()).expect("a server telemetry")
}

/// Polls the status endpoint until the renderer's playheads arrive.
///
/// Telemetry is sent about once a second, so the first readings are of a server
/// nothing has told anything yet.
async fn wait_for_playheads(app: &axum::Router, token: &str) -> Vec<LayerPlayhead> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let telemetry = status(app, token).await;
        if !telemetry.playheads.is_empty() {
            return telemetry.playheads;
        }
        assert!(
            Instant::now() < deadline,
            "no playhead ever reached the status endpoint"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_playhead_the_renderer_measured_reaches_the_status_endpoint() {
    let state = AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test");
    let app = router(state.clone());
    let token = sign_in(&app).await;

    // The renderer listens and the core connects to it, which is the direction
    // that lets the core restart without the renderer noticing.
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
    let address = listener.local_addr().expect("has an address").to_string();

    let shared = Arc::new(SharedShowState::new());
    let running = Arc::new(AtomicBool::new(true));

    let control_state = Arc::clone(&shared);
    let control_running = Arc::clone(&running);
    let control_thread = std::thread::Builder::new()
        .name("test-control".to_owned())
        .spawn(move || {
            let _ = control::serve(&listener, &control_state, &control_running);
        })
        .expect("spawns");

    let link_state = state.clone();
    let link_address = address.clone();
    let link_running = Arc::clone(&running);
    let link_thread = std::thread::Builder::new()
        .name("test-link".to_owned())
        .spawn(move || renderer_link::run(&link_state, &link_address, &link_running))
        .expect("spawns");

    assert!(
        status(&app, &token).await.playheads.is_empty(),
        "a server nothing has reported to has no bars to draw, not bars at zero"
    );

    // What the render thread publishes once a second, in the shape the
    // scheduler records it: a four-second clip at 25 fps, a second and a half
    // in, on a layer running at half rate.
    let layer_id = LayerId::new();
    let media_item_id = MediaItemId::new();
    shared.publish_playheads(vec![LayerPlayhead {
        layer_id,
        slot: 3,
        media_item_id,
        frame: 37,
        frame_count: 100,
        // A stand-in clip: eight bits, which is what most material is.
        decode_depth: ovs_types::depth::DecodeDepth::Eight,
        position_ms: 1_480,
        duration_ms: 4_000,
        speed_ms_per_second: 500.0,
    }]);

    let playheads = wait_for_playheads(&app, &token).await;
    assert_eq!(playheads.len(), 1);
    let head = playheads[0];

    assert_eq!(head.layer_id, layer_id, "on the layer it was measured on");
    assert_eq!(head.slot, 3);
    assert_eq!(head.media_item_id, media_item_id);

    // Both units survived, which is the point of sending both: an operator
    // reads the seconds and a scheduler reads the frames.
    assert_eq!(head.frame, 37);
    assert_eq!(head.frame_count, 100);
    assert_eq!(head.position_ms, 1_480);
    assert_eq!(head.duration_ms, 4_000);
    assert!((head.fraction().expect("a length") - 0.37).abs() < 0.001);

    // And the rate, without which a bar would sit still between samples.
    assert!(
        (head.speed_ms_per_second - 500.0).abs() < f64::EPSILON,
        "got {}",
        head.speed_ms_per_second
    );

    running.store(false, Ordering::Relaxed);
    // The renderer's accept loop only looks at the flag between connections, so
    // one more connection is what lets it see it.
    let _ = std::net::TcpStream::connect(&address);
    let _ = control_thread.join();
    let _ = link_thread.join();
}
