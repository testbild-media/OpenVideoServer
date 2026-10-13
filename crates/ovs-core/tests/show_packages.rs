//! Moving a show between servers.
//!
//! What this covers is the promise a package makes: a show exported from one
//! server plays on another, the content that is already there is not sent again,
//! and every reference in the imported show points at something that exists.
//! A package that left a layer pointing at nothing would be worse than no
//! package at all, because it would look like a show until it was played.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{
    CreateAccountRequest, CreateUploadRequest, CreateUploadResponse, ImportShowResponse,
    LoginRequest, LoginResponse, SaveShowRequest, ShowExport,
};
use ovs_types::config::{Layer, Show, Slot};
use ovs_types::geometry::SizePx;
use ovs_types::id::MediaItemId;
use ovs_types::media::{MediaKind, MediaState};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

/// One server, with an operator signed in and somewhere to put media.
struct Server {
    app: axum::Router,
    token: String,
    state: AppState,
    #[expect(dead_code, reason = "deletes the media root when the test ends")]
    directory: tempfile::TempDir,
}

impl Server {
    async fn start() -> Self {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let database = Database::open(directory.path().join("ovs.db")).expect("opens");
        let state =
            AppState::with_media_root(database, "0.0.1-test", directory.path().join("media"));
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
            directory,
        }
    }

    async fn post<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        send(&self.app, post_json_auth(path, &self.token, body)).await
    }

    /// A POST with no body, for the routes that take none.
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

    async fn get(&self, path: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .uri(path)
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        send(&self.app, request).await
    }

    /// Uploads a still, optionally into an item that is already waiting.
    async fn ingest(&self, bytes: &[u8], into: Option<MediaItemId>) -> MediaItemId {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let sha256 = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        let request = CreateUploadRequest {
            display_name: "Backdrop".to_owned(),
            original_filename: "backdrop.png".to_owned(),
            folder: String::new(),
            sha256,
            byte_size: bytes.len() as u64,
            media_item_id: into,
            kind: MediaKind::Image,
            dimensions: SizePx::new(2, 2),
            duration_ms: None,
            frame_rate: None,
            has_alpha: true,
            container: "png".to_owned(),
            codec: "png".to_owned(),
        };

        let (status, body) = self.post("/api/v1/media/uploads", &request).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let answer: CreateUploadResponse = serde_json::from_value(body).expect("a response");
        let CreateUploadResponse::Accepted(session) = answer else {
            panic!("expected the server to accept the upload");
        };

        let chunk = Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/v1/media/uploads/{}?offset=0",
                session.upload_id
            ))
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::from(bytes.to_vec()))
            .expect("builds");
        let (status, _) = send(&self.app, chunk).await;
        assert_eq!(status, StatusCode::OK);

        let verify = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/v1/media/uploads/{}/verify",
                session.upload_id
            ))
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        let (status, body) = send(&self.app, verify).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        session.media_item_id
    }

    /// Creates a show whose one layer holds `clip` in slot 1.
    async fn make_show(&self, clip: MediaItemId) -> Show {
        let (status, body) = self
            .post("/api/v1/shows", &serde_json::json!({ "name": "Show" }))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let mut show: Show = serde_json::from_value(body).expect("a show");

        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, clip));
        show.layers.push(layer);

        // Loaded before it is filled in, because a show that is not loaded
        // cannot be edited (owner, 2026-09-24). This is setup rather than the
        // behaviour under test: what these tests are about is what a package
        // carries between two servers.
        let (status, body) = self
            .post_empty(&format!("/api/v1/shows/{}/open", show.id))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let (status, body) = self
            .put(
                &format!("/api/v1/shows/{}", show.id),
                &SaveShowRequest { show: show.clone() },
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        show
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

/// A two-by-two PNG of one colour.
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
async fn a_show_exported_from_one_server_plays_on_another() {
    // The whole point. Export, import, send the one clip that is missing, and
    // the show is playable on a server that had never seen it.
    let origin = Server::start().await;
    let pixels = png([10, 20, 30, 255]);
    let clip = origin.ingest(&pixels, None).await;
    let show = origin.make_show(clip).await;

    let (status, body) = origin
        .get(&format!("/api/v1/shows/{}/export", show.id))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let package: ShowExport = serde_json::from_value(body).expect("a package");
    assert_eq!(package.media.len(), 1);
    assert_eq!(package.media[0].media_item_id, clip);

    let destination = Server::start().await;
    let (status, body) = destination.post("/api/v1/shows/import", &package).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let result: ImportShowResponse = serde_json::from_value(body).expect("a result");

    assert_eq!(result.show_id, show.id, "the same show");
    assert!(result.relinked.is_empty());
    assert_eq!(result.missing.len(), 1, "it has never seen this clip");

    // The item exists, waiting for its content, and the show refers to it.
    let waiting = destination
        .state
        .with_database(|database| database.load_media_item(clip))
        .expect("the import created it");
    assert_eq!(waiting.state, MediaState::Importing);

    // Sending the content completes the show.
    destination.ingest(&pixels, Some(clip)).await;

    let ready = destination
        .state
        .with_database(|database| database.load_media_item(clip))
        .expect("still there");
    assert_eq!(ready.state, MediaState::Ready);
    assert!(destination
        .state
        .with_database(|database| database.playable_asset_for(clip))
        .expect("queries")
        .is_some());

    let imported = destination
        .state
        .with_database(|database| database.load_show(show.id))
        .expect("the show is here");
    assert_eq!(imported.referenced_media().len(), 1);
    assert!(imported.referenced_media().contains(&clip));
}

#[tokio::test]
async fn content_the_destination_already_has_is_relinked_rather_than_re_sent() {
    // The reason a package carries hashes rather than files. A festival server
    // that already holds a clip should not be sent it again because another
    // show happens to call it something else.
    let origin = Server::start().await;
    let pixels = png([40, 50, 60, 255]);
    let clip = origin.ingest(&pixels, None).await;
    let show = origin.make_show(clip).await;

    let (_, body) = origin
        .get(&format!("/api/v1/shows/{}/export", show.id))
        .await;
    let package: ShowExport = serde_json::from_value(body).expect("a package");

    // The destination already holds the same bytes, under its own identifier.
    let destination = Server::start().await;
    let local = destination.ingest(&pixels, None).await;
    assert_ne!(local, clip, "a different item on this server");

    let (status, body) = destination.post("/api/v1/shows/import", &package).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let result: ImportShowResponse = serde_json::from_value(body).expect("a result");

    assert!(result.missing.is_empty(), "nothing needs sending");
    assert_eq!(result.relinked.len(), 1);
    assert_eq!(result.relinked[0].from, clip);
    assert_eq!(result.relinked[0].to, local);

    // And the imported show points at what is actually here.
    let imported = destination
        .state
        .with_database(|database| database.load_show(show.id))
        .expect("the show is here");
    assert!(imported.referenced_media().contains(&local));
    assert!(!imported.referenced_media().contains(&clip));
}

#[tokio::test]
async fn a_picture_used_only_as_a_mask_travels_and_is_relinked_like_any_other() {
    // **The one way a show can name a picture that is not a slot.** A package
    // is built from what the show references, and an import points what it
    // references at the local copy; a mask that neither looked at would arrive
    // at a festival naming an identifier from the machine it was built on, and
    // the layer would come up unmasked with nothing to say why.
    let origin = Server::start().await;
    let clip = origin.ingest(&png([10, 20, 30, 255]), None).await;
    let stencil_pixels = png([200, 200, 200, 255]);
    let stencil = origin.ingest(&stencil_pixels, None).await;

    let mut show = origin.make_show(clip).await;
    let mut masking = ovs_types::config::EffectInstance::new("ovs.mask", 1);
    masking.parameters.insert(
        "image".to_owned(),
        ovs_types::config::slot::ParameterValue::Text(stencil.to_string()),
    );
    show.layers[0].effects.push(masking);
    let (status, body) = origin
        .put(
            &format!("/api/v1/shows/{}", show.id),
            &SaveShowRequest { show: show.clone() },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, body) = origin
        .get(&format!("/api/v1/shows/{}/export", show.id))
        .await;
    let package: ShowExport = serde_json::from_value(body).expect("a package");
    assert_eq!(
        package.media.len(),
        2,
        "the package carries the clip and the stencil"
    );

    // The destination already holds the same stencil under its own identifier.
    let destination = Server::start().await;
    let local = destination.ingest(&stencil_pixels, None).await;
    assert_ne!(local, stencil, "a different item on this server");

    let (status, body) = destination.post("/api/v1/shows/import", &package).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let imported = destination
        .state
        .with_database(|database| database.load_show(show.id))
        .expect("the show is here");
    let effect = &imported.layers[0].effects[0];
    let named = effect
        .parameters
        .get("image")
        .expect("the mask still names a picture");
    assert_eq!(
        *named,
        ovs_types::config::slot::ParameterValue::Text(local.to_string()),
        "the mask points at the copy this server holds"
    );
}

#[tokio::test]
async fn importing_the_same_package_twice_changes_nothing_the_second_time() {
    // An operator who is not sure whether the import worked will do it again.
    let origin = Server::start().await;
    let pixels = png([7, 8, 9, 255]);
    let clip = origin.ingest(&pixels, None).await;
    let show = origin.make_show(clip).await;

    let (_, body) = origin
        .get(&format!("/api/v1/shows/{}/export", show.id))
        .await;
    let package: ShowExport = serde_json::from_value(body).expect("a package");

    let destination = Server::start().await;
    destination.post("/api/v1/shows/import", &package).await;
    destination.ingest(&pixels, Some(clip)).await;

    let (status, body) = destination.post("/api/v1/shows/import", &package).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let result: ImportShowResponse = serde_json::from_value(body).expect("a result");

    assert!(result.missing.is_empty(), "the content is here now");
    assert!(
        result.relinked.is_empty(),
        "and it is here under the identifier the package used"
    );
    assert_eq!(
        destination
            .state
            .with_database(|database| database.list_media_items())
            .expect("lists")
            .len(),
        1,
        "and no second copy of anything"
    );
}

#[tokio::test]
async fn a_package_from_a_newer_build_is_refused_rather_than_half_understood() {
    // An import that dropped a field it did not know about would produce a show
    // that looks right and is missing something nobody can name.
    let origin = Server::start().await;
    let clip = origin.ingest(&png([1, 1, 1, 255]), None).await;
    let show = origin.make_show(clip).await;

    let (_, body) = origin
        .get(&format!("/api/v1/shows/{}/export", show.id))
        .await;
    let mut package: ShowExport = serde_json::from_value(body).expect("a package");
    package.package_version = 99;

    let destination = Server::start().await;
    let (status, _) = destination.post("/api/v1/shows/import", &package).await;

    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_show_whose_media_has_never_arrived_cannot_be_packaged() {
    // The package would name content nothing can supply. Saying so at export is
    // the only moment where it is still the exporting operator's problem.
    let origin = Server::start().await;
    let clip = origin.ingest(&png([2, 2, 2, 255]), None).await;
    let show = origin.make_show(clip).await;

    // A second layer pointing at something that was never uploaded.
    let orphan = MediaItemId::new();
    let mut edited = show.clone();
    let mut layer = Layer::new("Layer 2", 1);
    layer.slots.push(Slot::media(1, orphan));
    edited.layers.push(layer);
    origin
        .put(
            &format!("/api/v1/shows/{}", edited.id),
            &SaveShowRequest {
                show: edited.clone(),
            },
        )
        .await;

    let (status, body) = origin
        .get(&format!("/api/v1/shows/{}/export", edited.id))
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_package_needs_a_session() {
    let server = Server::start().await;
    let clip = server.ingest(&png([3, 3, 3, 255]), None).await;
    let show = server.make_show(clip).await;

    let request = Request::builder()
        .uri(format!("/api/v1/shows/{}/export", show.id))
        .body(Body::empty())
        .expect("builds");
    let (status, _) = send(&server.app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
