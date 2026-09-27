//! Previews, from the control channel to a PNG a client can fetch.
//!
//! What this covers is the promise the endpoint makes: the picture a client gets
//! is the one the renderer sent, and an output nobody has sent a picture of says
//! so rather than inventing a black frame. "No preview" and "the wall is black"
//! are different things, and only one of them means something is wrong.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::server::Preview;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{CreateAccountRequest, LoginRequest, LoginResponse};
use ovs_types::id::OutputId;
use ovs_types::time::Timestamp;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    app: axum::Router,
    token: String,
    state: AppState,
}

impl Harness {
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

        let (_, body) = send(
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
        let login: LoginResponse = serde_json::from_value(body).expect("a login response");

        Self {
            app,
            token: login.token,
            state,
        }
    }

    async fn get(&self, path: &str) -> (StatusCode, Vec<u8>, Option<String>) {
        let request = Request::builder()
            .uri(path)
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        let response = self.app.clone().oneshot(request).await.expect("responds");
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("reads")
            .to_bytes()
            .to_vec();
        (status, bytes, content_type)
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

/// A two-by-two PNG of one colour, as the renderer would send.
fn png(colour: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(2, 2, image::Rgba(colour))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encodes");
    bytes
}

#[tokio::test]
async fn a_preview_the_renderer_sent_is_the_one_a_client_gets() {
    let harness = Harness::start().await;
    let output_id = OutputId::new();
    let sent = png([10, 20, 30, 255]);

    harness.state.set_preview(Preview {
        output_id,
        name: "LED Wall".to_owned(),
        png: sent.clone(),
        received_at: Timestamp::now(),
    });

    let (status, bytes, content_type) = harness.get(&format!("/api/v1/preview/{output_id}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/png"));
    assert_eq!(bytes, sent);

    // And it really is an image, not just bytes that were handed back.
    let decoded = image::load_from_memory(&bytes).expect("decodes");
    assert_eq!(decoded.width(), 2);
    assert_eq!(decoded.height(), 2);
}

#[tokio::test]
async fn an_output_nobody_has_sent_a_picture_of_says_so() {
    // Rather than a black frame. "The renderer has not sent one" and "the wall
    // is black" are different things, and the second is the one that means
    // something is wrong.
    let harness = Harness::start().await;

    let (status, _, _) = harness
        .get(&format!("/api/v1/preview/{}", OutputId::new()))
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_latest_picture_replaces_the_one_before_it() {
    // A preview is only ever interesting now. Keeping a history would be
    // keeping frames nobody will look at.
    let harness = Harness::start().await;
    let output_id = OutputId::new();

    harness.state.set_preview(Preview {
        output_id,
        name: "LED Wall".to_owned(),
        png: png([255, 0, 0, 255]),
        received_at: Timestamp::now(),
    });
    let newer = png([0, 0, 255, 255]);
    harness.state.set_preview(Preview {
        output_id,
        name: "LED Wall".to_owned(),
        png: newer.clone(),
        received_at: Timestamp::now(),
    });

    let (status, bytes, _) = harness.get(&format!("/api/v1/preview/{output_id}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, newer);
    assert_eq!(harness.state.previews().len(), 1);
}

#[tokio::test]
async fn the_list_says_what_there_are_pictures_of() {
    let harness = Harness::start().await;
    let first = OutputId::new();
    let second = OutputId::new();

    for (output_id, name) in [(first, "Left"), (second, "Right")] {
        harness.state.set_preview(Preview {
            output_id,
            name: name.to_owned(),
            png: png([1, 2, 3, 255]),
            received_at: Timestamp::now(),
        });
    }

    let (status, bytes, _) = harness.get("/api/v1/preview").await;

    assert_eq!(status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let outputs = body["outputs"].as_array().expect("an array");
    assert_eq!(outputs.len(), 2);
    assert!(outputs
        .iter()
        .all(|entry| entry["byte_size"].as_u64().unwrap_or(0) > 0));
}

#[tokio::test]
async fn a_malformed_identifier_is_refused_rather_than_searched_for() {
    let harness = Harness::start().await;

    let (status, _, _) = harness.get("/api/v1/preview/not-an-id").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_permission_to_watch_is_narrower_than_a_session() {
    // The reason this exists: the window that decodes a preview should not be
    // holding something that can also stop the machine.
    let harness = Harness::start().await;

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/preview/stream/composition/ticket")
        .header("authorization", format!("Bearer {}", harness.token))
        .body(Body::empty())
        .expect("builds");
    let (status, body) = send(&harness.app, request).await;

    assert_eq!(status, StatusCode::OK);
    let ticket = body["ticket"].as_str().expect("a ticket").to_owned();
    assert_ne!(ticket, harness.token, "a ticket is not a session");
    assert!(
        body["expires_at"].as_i64().expect("an expiry") > Timestamp::now().as_millis(),
        "and it is good for a little while yet"
    );

    // It opens the surface it was minted for, once.
    assert!(harness
        .state
        .spend_stream_ticket(&ticket, ovs_types::telemetry::StreamSurface::Composition));
    assert!(
        !harness
            .state
            .spend_stream_ticket(&ticket, ovs_types::telemetry::StreamSurface::Composition),
        "and only once"
    );
}

#[tokio::test]
async fn minting_a_permission_to_watch_needs_a_session_of_its_own() {
    // Otherwise the narrow credential would be available to anybody, which
    // would make it no narrower than none at all.
    let harness = Harness::start().await;

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/preview/stream/composition/ticket")
        .body(Body::empty())
        .expect("builds");
    let (status, _) = send(&harness.app, request).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_permission_cannot_be_minted_for_a_surface_that_is_not_one() {
    let harness = Harness::start().await;

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/preview/stream/wall-2/ticket")
        .header("authorization", format!("Bearer {}", harness.token))
        .body(Body::empty())
        .expect("builds");
    let (status, _) = send(&harness.app, request).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_permission_for_one_wall_does_not_open_another() {
    let harness = Harness::start().await;
    let one = OutputId::new();
    let other = OutputId::new();

    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/preview/stream/{one}/ticket"))
        .header("authorization", format!("Bearer {}", harness.token))
        .body(Body::empty())
        .expect("builds");
    let (status, body) = send(&harness.app, request).await;
    assert_eq!(status, StatusCode::OK);

    let ticket = body["ticket"].as_str().expect("a ticket").to_owned();
    assert!(!harness.state.spend_stream_ticket(
        &ticket,
        ovs_types::telemetry::StreamSurface::Output { output_id: other }
    ));
}

#[tokio::test]
async fn a_preview_needs_a_session() {
    // A picture of the stage is as much of a disclosure as anything else on
    // this server.
    let harness = Harness::start().await;
    let output_id = OutputId::new();
    harness.state.set_preview(Preview {
        output_id,
        name: "LED Wall".to_owned(),
        png: png([1, 2, 3, 255]),
        received_at: Timestamp::now(),
    });

    for path in [
        "/api/v1/preview".to_owned(),
        format!("/api/v1/preview/{output_id}"),
    ] {
        let request = Request::builder()
            .uri(&path)
            .body(Body::empty())
            .expect("builds");
        let response = harness
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("responds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn the_system_screen_is_served_to_the_machine_itself_without_a_session() {
    // It is the touchscreen in the rack. There is nobody to type a password at
    // it and nowhere to keep one.
    let harness = Harness::start().await;

    let mut request = Request::builder()
        .uri("/console")
        .body(Body::empty())
        .expect("builds");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            51234,
        ))));

    let response = harness
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("responds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );

    let body = response
        .into_body()
        .collect()
        .await
        .expect("reads")
        .to_bytes();
    let html = String::from_utf8(body.to_vec()).expect("utf-8");
    assert!(html.contains("OpenVideoServer"), "it is the System screen");
    assert!(
        html.contains("/api/v1"),
        "and it knows where the API is, without a build step to tell it"
    );
    assert!(
        !html.contains("password"),
        "and it asks for nothing: a screen with a login is a screen nobody uses"
    );
}

#[tokio::test]
async fn the_system_screen_does_not_exist_from_the_network() {
    // Serving it anywhere else would hand the show — and the power button — to
    // whoever could reach the port.
    let harness = Harness::start().await;

    let mut request = Request::builder()
        .uri("/console")
        .body(Body::empty())
        .expect("builds");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [10, 0, 0, 12],
            51234,
        ))));

    let response = harness
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("responds");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_machine_itself_needs_no_session_and_the_network_still_does() {
    // The other half of the same rule, on an endpoint that matters: the screen
    // reads the machine's state, and so could anything else on the box.
    let harness = Harness::start().await;

    let mut local = Request::builder()
        .uri("/api/v1/server/status")
        .body(Body::empty())
        .expect("builds");
    local
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            51234,
        ))));
    let response = harness.app.clone().oneshot(local).await.expect("responds");
    assert_eq!(response.status(), StatusCode::OK);

    let mut remote = Request::builder()
        .uri("/api/v1/server/status")
        .body(Body::empty())
        .expect("builds");
    remote
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [10, 0, 0, 12],
            51234,
        ))));
    let response = harness.app.clone().oneshot(remote).await.expect("responds");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn saying_you_are_local_does_not_make_you_local() {
    // The header is this server's own mark. Anything that arrives wearing it is
    // stripped before a handler ever sees it.
    let harness = Harness::start().await;

    let mut request = Request::builder()
        .uri("/api/v1/server/status")
        .header("x-ovs-local", "1")
        .body(Body::empty())
        .expect("builds");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [10, 0, 0, 12],
            51234,
        ))));

    let response = harness
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("responds");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
