//! Media ingest over the HTTP API, from "here is a file" to a playable item.
//!
//! The unit tests either side of this one cover the parts: the store refuses a
//! key that would escape it, the upload table refuses a chunk from the wrong
//! place. What they cannot show is that the three requests fit together — that
//! what the first one promises, the second one delivers and the third one
//! verifies — which is the whole of what ingest is.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{
    CreateAccountRequest, CreateUploadRequest, CreateUploadResponse, LoginRequest, LoginResponse,
};
use ovs_types::geometry::SizePx;
use ovs_types::media::{MediaItem, MediaKind, MediaState};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

/// A server with somewhere to put media, and an operator logged in.
struct Harness {
    app: axum::Router,
    token: String,
    state: AppState,
    #[expect(dead_code, reason = "deletes the media root when the test ends")]
    directory: tempfile::TempDir,
}

impl Harness {
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

        Self {
            app,
            token: login.token,
            state,
            directory,
        }
    }

    /// Declares an upload and returns what the server answered.
    async fn declare(&self, request: &CreateUploadRequest) -> (StatusCode, serde_json::Value) {
        send(
            &self.app,
            post_json_auth("/api/v1/media/uploads", &self.token, request),
        )
        .await
    }

    /// Sends one chunk at `offset`.
    async fn send_chunk(
        &self,
        upload_id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("PUT")
            .uri(format!("/api/v1/media/uploads/{upload_id}?offset={offset}"))
            .header("authorization", format!("Bearer {}", self.token))
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes.to_vec()))
            .expect("builds");
        send(&self.app, request).await
    }

    /// Asks the server to verify and register an upload.
    /// Fetches a path and returns what came back, as JSON.
    async fn fetch_json(&self, path: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("GET")
            .uri(path)
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        send(&self.app, request).await
    }

    /// Fetches a path and returns what came back, as bytes.
    async fn fetch(&self, path: &str) -> (StatusCode, Vec<u8>) {
        let request = Request::builder()
            .method("GET")
            .uri(path)
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        let response = self.app.clone().oneshot(request).await.expect("responds");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("reads")
            .to_bytes()
            .to_vec();
        (status, body)
    }

    async fn verify(&self, upload_id: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/media/uploads/{upload_id}/verify"))
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        send(&self.app, request).await
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

/// A two-by-two PNG and everything a client would say about it.
fn a_still() -> (Vec<u8>, CreateUploadRequest) {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encodes");

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
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
        media_item_id: None,
        kind: MediaKind::Image,
        dimensions: SizePx::new(2, 2),
        duration_ms: None,
        frame_rate: None,
        has_alpha: true,
        container: "png".to_owned(),
        codec: "png".to_owned(),
    };

    (bytes, request)
}

/// Runs the whole flow and returns the registered item.
async fn ingest(harness: &Harness, bytes: &[u8], request: &CreateUploadRequest) -> MediaItem {
    let (status, body) = harness.declare(request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let CreateUploadResponse::Accepted(session) =
        serde_json::from_value(body).expect("an upload response")
    else {
        panic!("expected the server to accept the upload");
    };

    let (status, _) = harness.send_chunk(&session.upload_id, 0, bytes).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = harness.verify(&session.upload_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_value(body).expect("a media item")
}

#[tokio::test]
async fn a_clip_imported_from_inside_a_folder_is_filed_there() {
    // The owner's report, 2026-09-23: "Wenn ich in einem Ordner bin und Medien
    // hochlade, dann tauchen Sie immer erst im All Media auf statt in dem
    // Ordner indem ich bin."
    //
    // The folder travels with the declaration rather than being a move made
    // afterwards, because a large file finishes uploading long after the
    // operator has walked off to another folder.
    let harness = Harness::start().await;
    let (bytes, base) = a_still();
    let request = CreateUploadRequest {
        folder: "Intros/2026".to_owned(),
        ..base
    };

    let item = ingest(&harness, &bytes, &request).await;

    assert_eq!(item.folder, "Intros/2026");

    // And the folder is there to draw. Made with everything above it, exactly
    // as making one by hand does: a tree with a hole in it cannot be drawn.
    let folders = harness
        .state
        .with_database(|database| database.list_media_folders())
        .expect("lists the folders");
    assert!(
        folders.contains(&"Intros/2026".to_owned()),
        "the folder it was filed in: {folders:?}"
    );
    assert!(
        folders.contains(&"Intros".to_owned()),
        "and the one above it: {folders:?}"
    );
}

#[tokio::test]
async fn an_import_with_no_folder_is_filed_at_the_root() {
    // What every import did before there were folders, and what dropping into
    // All media means.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();

    let item = ingest(&harness, &bytes, &request).await;

    assert_eq!(item.folder, "", "the root");
}

#[tokio::test]
async fn a_folder_name_the_library_cannot_hold_is_refused_before_a_byte_is_sent() {
    // Before, not after: the alternative is an operator who has spent an
    // ingest window sending four gigabytes and is then told about a space at
    // the end of a folder name.
    let harness = Harness::start().await;
    let (_, base) = a_still();

    let (status, body) = harness
        .declare(&CreateUploadRequest {
            folder: "Intros/ 2026".to_owned(),
            ..base
        })
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn a_still_uploaded_becomes_a_playable_item() {
    let harness = Harness::start().await;
    let (bytes, request) = a_still();

    let item = ingest(&harness, &bytes, &request).await;

    assert_eq!(item.state, MediaState::Ready);
    assert_eq!(item.display_name, "Backdrop");
    assert_eq!(item.dimensions, Some(SizePx::new(2, 2)));
    assert_eq!(item.duration_ms, None);

    // And the file is where the renderer will look for it.
    let store = harness.state.media_store().expect("configured").clone();
    let asset = harness
        .state
        .with_database(|database| database.playable_asset_for(item.id))
        .expect("queries")
        .expect("a playable asset");
    let path = store.existing_path_for(&asset.object_key).expect("stored");
    assert_eq!(std::fs::read(path).expect("reads"), bytes);
}

#[tokio::test]
async fn deleting_an_item_takes_its_file_off_the_disc() {
    // **Deleted means gone** (owner, 2026-09-25: "Wenn ich etwas lösche, dann soll
    // das weg sein und nicht weiter platz belegen").
    //
    // Everything either side of the route was already held down — the database
    // returns the keys, the store can remove a file — and the route in between
    // logged the count and dropped the list, so a library that had had everything
    // deleted from it still held every byte. Seventeen files for four items on the
    // reference machine.
    //
    // Which is why this test goes all the way to the filesystem. Asserting that
    // the item is gone from the library would have passed all along.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();
    let item = ingest(&harness, &bytes, &request).await;

    let store = harness.state.media_store().expect("configured").clone();
    let asset = harness
        .state
        .with_database(|database| database.playable_asset_for(item.id))
        .expect("queries")
        .expect("a playable asset");
    let path = store.existing_path_for(&asset.object_key).expect("stored");
    let thumbnail = store.thumbnail_path_for(item.id);

    let request = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/media/{}", item.id))
        .header("authorization", format!("Bearer {}", harness.token))
        .body(Body::empty())
        .expect("builds");
    let (status, _) = send(&harness.app, request).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(!path.exists(), "the file is still taking up the disc");
    assert!(!thumbnail.exists(), "the thumbnail is still there");
    assert!(
        !path.parent().expect("has a parent").exists(),
        "the shard directory was left behind empty"
    );
    assert_eq!(
        store.existing_path_for(&asset.object_key),
        None,
        "the store still resolves a key to something"
    );

    // And the library agrees, which is the half that already worked. The list
    // rather than the item: there is no route that fetches one by identifier, so
    // asking for it would answer 405 whether it existed or not.
    let (status, body) = harness.fetch_json("/api/v1/media").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.to_string().contains(&item.id.to_string()),
        "the library still lists it: {body}"
    );
}

#[tokio::test]
async fn the_same_content_is_never_stored_twice() {
    // What stops an operator sending the same show file every night of a tour.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();

    let first = ingest(&harness, &bytes, &request).await;

    let (status, body) = harness.declare(&request).await;
    assert_eq!(status, StatusCode::OK);
    let answer: CreateUploadResponse = serde_json::from_value(body).expect("a response");
    assert_eq!(
        answer,
        CreateUploadResponse::AlreadyPresent {
            media_item_id: first.id
        }
    );
}

#[tokio::test]
async fn content_that_does_not_match_its_hash_is_refused_and_says_why() {
    // The one check that matters. Everything else about an upload is a
    // convenience; this is what stops a truncated transfer becoming a clip that
    // fails in front of an audience.
    let harness = Harness::start().await;
    let (bytes, mut request) = a_still();
    request.sha256 = "b".repeat(64);

    let (status, body) = harness.declare(&request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let CreateUploadResponse::Accepted(session) =
        serde_json::from_value(body).expect("an upload response")
    else {
        panic!("expected the server to accept the upload");
    };

    harness.send_chunk(&session.upload_id, 0, &bytes).await;
    let (status, body) = harness.verify(&session.upload_id).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("hash")),
        "{body}"
    );

    // The item is still in the library, marked failed and carrying the reason.
    let item = harness
        .state
        .with_database(|database| database.load_media_item(session.media_item_id))
        .expect("the item is still there");
    assert_eq!(item.state, MediaState::Failed);
    assert!(item.failure_reason.is_some());
}

#[tokio::test]
async fn a_still_that_is_not_the_size_it_claims_is_refused() {
    // The client has already opened the file. Taking its word anyway would put
    // a wrong size in the library and in every show built on it.
    let harness = Harness::start().await;
    let (bytes, mut request) = a_still();
    request.dimensions = SizePx::new(1920, 1080);

    let (status, body) = harness.declare(&request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let CreateUploadResponse::Accepted(session) =
        serde_json::from_value(body).expect("an upload response")
    else {
        panic!("expected the server to accept the upload");
    };

    harness.send_chunk(&session.upload_id, 0, &bytes).await;
    let (status, body) = harness.verify(&session.upload_id).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("2x2")),
        "{body}"
    );
}

#[tokio::test]
async fn a_chunk_from_the_wrong_place_is_refused_and_the_upload_survives() {
    // A client whose answer got lost retries. It must be able to carry on.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();

    let (_, body) = harness.declare(&request).await;
    let CreateUploadResponse::Accepted(session) =
        serde_json::from_value(body).expect("an upload response")
    else {
        panic!("expected the server to accept the upload");
    };

    let half = bytes.len() / 2;
    let (status, _) = harness
        .send_chunk(&session.upload_id, 0, &bytes[..half])
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = harness
        .send_chunk(&session.upload_id, 0, &bytes[half..])
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Resuming from where the server actually is works.
    let (status, _) = harness
        .send_chunk(&session.upload_id, half as u64, &bytes[half..])
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = harness.verify(&session.upload_id).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_upload_that_is_not_finished_is_not_verified() {
    let harness = Harness::start().await;
    let (bytes, request) = a_still();

    let (_, body) = harness.declare(&request).await;
    let CreateUploadResponse::Accepted(session) =
        serde_json::from_value(body).expect("an upload response")
    else {
        panic!("expected the server to accept the upload");
    };

    harness
        .send_chunk(&session.upload_id, 0, &bytes[..bytes.len() / 2])
        .await;
    let (status, _) = harness.verify(&session.upload_id).await;

    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_cancelled_upload_leaves_nothing_staged() {
    let harness = Harness::start().await;
    let (bytes, request) = a_still();

    let (_, body) = harness.declare(&request).await;
    let CreateUploadResponse::Accepted(session) =
        serde_json::from_value(body).expect("an upload response")
    else {
        panic!("expected the server to accept the upload");
    };
    harness.send_chunk(&session.upload_id, 0, &bytes).await;

    let request = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/media/uploads/{}", session.upload_id))
        .header("authorization", format!("Bearer {}", harness.token))
        .body(Body::empty())
        .expect("builds");
    let (status, _) = send(&harness.app, request).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let store = harness.state.media_store().expect("configured").clone();
    assert!(
        !store
            .incoming_path_for(&session.upload_id)
            .expect("resolves")
            .exists(),
        "the staged file should be gone"
    );

    // And verifying it afterwards finds nothing.
    let (status, _) = harness.verify(&session.upload_id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_upload_needs_a_session() {
    // Media is the one thing on this server that costs disk, so an unauthorised
    // client must not be able to start one.
    let harness = Harness::start().await;
    let (_, request) = a_still();

    let (status, _) = send(&harness.app, post_json("/api/v1/media/uploads", &request)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_request_that_contradicts_itself_is_refused_before_anything_is_created() {
    let harness = Harness::start().await;
    let (_, base) = a_still();

    let cases = [
        (
            CreateUploadRequest {
                sha256: "NOT A HASH".to_owned(),
                ..base.clone()
            },
            "a hash that is not one",
        ),
        (
            CreateUploadRequest {
                byte_size: 0,
                ..base.clone()
            },
            "an empty file",
        ),
        (
            CreateUploadRequest {
                dimensions: SizePx::new(0, 0),
                ..base.clone()
            },
            "no dimensions",
        ),
        (
            CreateUploadRequest {
                duration_ms: Some(1),
                ..base.clone()
            },
            "a still with a duration",
        ),
        (
            CreateUploadRequest {
                kind: MediaKind::Video,
                duration_ms: None,
                ..base.clone()
            },
            "a video with no duration",
        ),
        (
            CreateUploadRequest {
                display_name: "   ".to_owned(),
                ..base.clone()
            },
            "no name",
        ),
    ];

    for (request, what) in cases {
        let (status, _) = harness.declare(&request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{what}");
    }

    // Nothing was created for any of them.
    let items = harness
        .state
        .with_database(|database| database.list_media_items())
        .expect("lists");
    assert!(items.is_empty(), "got {items:?}");
}

/// The HEVC clip the renderer's tests decode, and what it actually is.
///
/// The same file rather than a copy: what the library tells an operator and
/// what the wall shows have to agree, and two fixtures of one clip drift.
///
/// It is the HEVC one and not the ProRes one because ingest now refuses
/// anything that is not HEVC or HAP (owner, 2026-09-23). The ProRes fixture is
/// still committed, still decoded by the renderer's own tests, and is what
/// `a_clip_outside_the_profile_is_refused` uploads to prove the gate shuts.
#[cfg(unix)]
fn a_real_clip() -> (Vec<u8>, CreateUploadRequest) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ovs-renderer/tests/media/colours-hevc.mp4");
    let bytes = std::fs::read(&path).expect("the fixture is committed");

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let request = CreateUploadRequest {
        display_name: "Colours".to_owned(),
        original_filename: "colours-hevc.mp4".to_owned(),
        folder: String::new(),
        sha256,
        byte_size: bytes.len() as u64,
        media_item_id: None,
        kind: MediaKind::Video,
        dimensions: SizePx::new(64, 64),
        duration_ms: Some(1200),
        frame_rate: Some(ovs_types::geometry::FrameRate::new(25, 1)),
        has_alpha: false,
        container: "mov,mp4,m4a,3gp,3g2,mj2".to_owned(),
        codec: "hevc".to_owned(),
    };

    (bytes, request)
}

/// The HAP Alpha fixture and an honest declaration of it.
///
/// The other half of what ingest accepts, and a different shape of file
/// entirely: a texture rather than a coded picture, every frame a keyframe, no
/// pixel format and no colour tags to get wrong. A test that only used the
/// HEVC clip would be testing half the policy.
#[cfg(unix)]
fn a_long_gop_clip() -> (Vec<u8>, CreateUploadRequest) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ovs-renderer/tests/media/colours-hap.mov");
    let bytes = std::fs::read(&path).expect("the fixture is committed");

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let request = CreateUploadRequest {
        display_name: "Colours, HAP".to_owned(),
        original_filename: "colours-hap.mov".to_owned(),
        folder: String::new(),
        sha256,
        byte_size: bytes.len() as u64,
        media_item_id: None,
        kind: MediaKind::Video,
        dimensions: SizePx::new(64, 64),
        duration_ms: Some(1200),
        frame_rate: Some(ovs_types::geometry::FrameRate::new(25, 1)),
        has_alpha: false,
        container: "mov,mp4,m4a,3gp,3g2,mj2".to_owned(),
        codec: "hap".to_owned(),
    };

    (bytes, request)
}

#[cfg(unix)]
#[tokio::test]
async fn what_the_library_says_about_a_clip_comes_from_the_clip() {
    // Until now every one of these numbers was whatever a client typed into a
    // request. A duration nobody checked is a cue that ends somewhere other
    // than where the operator can see it will.
    let harness = Harness::start().await;
    let (bytes, request) = a_real_clip();

    let item = ingest(&harness, &bytes, &request).await;

    assert_eq!(item.dimensions, Some(SizePx::new(64, 64)));
    assert_eq!(item.duration_ms, Some(1200), "30 frames at 25 fps");
    assert_eq!(
        item.frame_rate,
        Some(ovs_types::geometry::FrameRate::new(25, 1))
    );
    assert!(!item.has_alpha, "422 Proxy carries no alpha");
}

#[cfg(unix)]
#[tokio::test]
async fn a_clip_that_is_not_what_it_says_it_is_never_becomes_playable() {
    // The declaration is a client's word about a file the server now holds.
    // Believing it is how a library ends up describing a show that is not the
    // one on disc.
    let harness = Harness::start().await;
    let (bytes, honest) = a_real_clip();

    let cases = [
        (
            CreateUploadRequest {
                dimensions: SizePx::new(1920, 1080),
                ..honest.clone()
            },
            "a size it is not",
        ),
        // A codec that is not what the declaration said is no longer here.
        // The server takes what it is given and records what it read,
        // so there is nothing left to disagree about.
    ];

    for (request, what) in cases {
        let (status, body) = harness.declare(&request).await;
        assert_eq!(status, StatusCode::OK, "declaring is fine: {what}");
        let upload_id = body["upload_id"].as_str().expect("an upload id").to_owned();

        let (status, _) = harness.send_chunk(&upload_id, 0, &bytes).await;
        assert_eq!(status, StatusCode::OK, "the bytes arrive: {what}");

        let (status, _) = harness.verify(&upload_id).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{what}");
    }

    // Nothing became playable, and what is there says why.
    let items = harness
        .state
        .with_database(|database| database.list_media_items())
        .expect("lists");
    for item in items {
        assert_eq!(item.state, MediaState::Failed, "{item:?}");
        assert!(item.failure_reason.is_some(), "{item:?}");
    }
}

/// The eight bytes every PNG starts with.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

#[tokio::test]
async fn a_still_gets_a_picture_of_itself() {
    // A library of names tells an operator which file they uploaded. A library
    // of pictures tells them which clip they are about to put on a wall.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();
    let item = ingest(&harness, &bytes, &request).await;

    let (status, png) = harness
        .fetch(&format!("/api/v1/media/{}/thumbnail", item.id))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(&png[..8], &PNG_SIGNATURE, "and it is a picture");
}

#[cfg(unix)]
#[tokio::test]
async fn a_clip_gets_a_picture_of_one_of_its_frames() {
    let harness = Harness::start().await;
    let (bytes, request) = a_real_clip();
    let item = ingest(&harness, &bytes, &request).await;

    let (status, png) = harness
        .fetch(&format!("/api/v1/media/{}/thumbnail", item.id))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(&png[..8], &PNG_SIGNATURE);
}

#[cfg(unix)]
#[tokio::test]
async fn a_clip_gets_ten_frames_of_itself_in_one_picture() {
    // The owner, 2026-09-23: ten frames across the whole clip, so an operator
    // has an idea what happens in it before putting it on a wall.
    //
    // Ten frames *wide*, always, with the last repeated where a clip was too
    // short to give ten: the window shows one tenth of the strip at a time, and
    // a strip of seven would have it showing seven tenths of a frame.
    let harness = Harness::start().await;
    let (bytes, request) = a_real_clip();
    let item = ingest(&harness, &bytes, &request).await;

    let (status, strip) = harness
        .fetch(&format!("/api/v1/media/{}/strip", item.id))
        .await;
    assert_eq!(status, StatusCode::OK);
    // JPEG rather than PNG, which is what the owner said was enough: ten frames
    // of photographic content compress to a fraction of a lossless picture of
    // them, and nothing reads a pixel of this.
    assert_eq!(&strip[..3], &[0xFF, 0xD8, 0xFF], "and it is a JPEG");

    let (status, still) = harness
        .fetch(&format!("/api/v1/media/{}/thumbnail", item.id))
        .await;
    assert_eq!(status, StatusCode::OK);

    // Ten times as wide as it is tall for a square clip, which this fixture is,
    // and measured rather than assumed.
    let strip_size = image::ImageReader::new(std::io::Cursor::new(&strip))
        .with_guessed_format()
        .expect("a readable picture")
        .into_dimensions()
        .expect("a size");
    let still_size = image::ImageReader::new(std::io::Cursor::new(&still))
        .with_guessed_format()
        .expect("a readable picture")
        .into_dimensions()
        .expect("a size");

    assert_eq!(strip_size.0, strip_size.1 * 10, "ten frames side by side");
    assert!(
        still_size.0 >= strip_size.0 / 10,
        "the still is the larger picture: {} against a frame of {}",
        still_size.0,
        strip_size.0 / 10
    );
}

#[tokio::test]
async fn a_still_gets_no_moving_preview() {
    // "Bilder die Hochgeladen werden brauchen kein Gif, da reicht ein jpg."
    // A photograph does not move, and a strip of ten copies of it would be a
    // still that costs ten times as much to fetch.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();
    let item = ingest(&harness, &bytes, &request).await;

    let (status, _) = harness
        .fetch(&format!("/api/v1/media/{}/strip", item.id))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn what_was_uploaded_can_be_read_back_byte_for_byte() {
    // The owner, 2026-09-23: "Export alles müsste, um auf dem server nicht zu
    // viel Platz zu brauchen, einzeln an den Clienten schicken und der muss
    // daraus eine Zip oder ähnlich machen."
    //
    // Media went up and never came down, which made a show something that could
    // only ever live on the server it was uploaded to. One item per request,
    // streamed from disk: the server never holds a second copy of anything.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();
    let item = ingest(&harness, &bytes, &request).await;

    let (status, back) = harness
        .fetch(&format!("/api/v1/media/{}/content", item.id))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(back, bytes, "the same bytes, in the same order");
}

#[tokio::test]
async fn an_item_with_no_content_says_so_rather_than_sending_nothing() {
    // A client that got zero bytes would write a zero-byte file and call it a
    // clip. A 404 is something it can report.
    let harness = Harness::start().await;
    let missing = ovs_types::id::MediaItemId::new();

    let (status, _) = harness
        .fetch(&format!("/api/v1/media/{missing}/content"))
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_item_with_no_picture_says_so_rather_than_sending_an_empty_one() {
    // A client that got zero bytes would draw a broken image. One that gets a
    // 404 draws the name, which is what it drew before pictures existed.
    let harness = Harness::start().await;
    let missing = ovs_types::id::MediaItemId::new();

    let (status, _) = harness
        .fetch(&format!("/api/v1/media/{missing}/thumbnail"))
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_an_item_takes_its_picture_with_it() {
    let harness = Harness::start().await;
    let (bytes, request) = a_still();
    let item = ingest(&harness, &bytes, &request).await;

    let deleted = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/media/{}", item.id))
        .header("authorization", format!("Bearer {}", harness.token))
        .body(Body::empty())
        .expect("builds");
    let (status, _) = send(&harness.app, deleted).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = harness
        .fetch(&format!("/api/v1/media/{}/thumbnail", item.id))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the picture went too");
}

#[tokio::test]
async fn an_upload_says_where_it_got_to() {
    // A client that was interrupted knows what it sent; it does not know what
    // arrived. Without this the only recoveries are to trust its own count or
    // to start again from nothing.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();

    let (status, body) = harness.declare(&request).await;
    assert_eq!(status, StatusCode::OK);
    let upload_id = body["upload_id"].as_str().expect("an upload id").to_owned();

    let (status, waiting) = harness
        .fetch_json(&format!("/api/v1/media/uploads/{upload_id}"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(waiting["received_bytes"], 0, "nothing has arrived yet");
    assert_eq!(waiting["byte_size"], bytes.len());
    assert!(
        waiting["chunk_size"].as_u64().is_some_and(|size| size > 0),
        "and it says how much to send at a time"
    );

    let half = bytes.len() / 2;
    let (status, _) = harness.send_chunk(&upload_id, 0, &bytes[..half]).await;
    assert_eq!(status, StatusCode::OK);

    let (_, partway) = harness
        .fetch_json(&format!("/api/v1/media/uploads/{upload_id}"))
        .await;
    assert_eq!(
        partway["received_bytes"], half,
        "and now it says how far it got"
    );
}

#[tokio::test]
async fn an_upload_the_server_has_never_heard_of_is_not_one_to_resume() {
    // A client that gets this should declare again rather than retry, so it
    // must be told apart from an upload that is merely empty.
    let harness = Harness::start().await;

    let (status, _) = harness
        .fetch_json("/api/v1/media/uploads/not-an-upload")
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_library_says_how_much_disc_an_item_takes() {
    // Not on the item, which never says where its content is or how it is
    // encoded. How much disc it takes says neither, and it is what an operator
    // uses to decide what to delete before a show.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();
    let item = ingest(&harness, &bytes, &request).await;

    let (status, listed) = harness.fetch_json("/api/v1/media").await;
    assert_eq!(status, StatusCode::OK);

    let entry = listed["items"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|entry| entry["id"] == item.id.to_string())
        .expect("the item that was just ingested");

    assert_eq!(entry["byte_size"], bytes.len());
    assert_eq!(
        entry["display_name"], "Backdrop",
        "and the item itself still reads as it did"
    );
    assert!(
        entry.get("object_key").is_none() && entry.get("codec").is_none(),
        "without saying where it is or how it is encoded: {entry}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_file_the_server_cannot_read_never_becomes_playable() {
    // The refusal the video path is down to, now that the server takes what it
    // is given: not "this is the wrong codec" but "there is nothing
    // in here to play". The same shape as a codec this build was not compiled
    // with, and the same message — the file, and what could not be done with it.
    //
    // Worth saying what is *not* refused any more. A still offered as a video
    // is accepted, because FFmpeg decodes a PNG as a one-frame clip and it is
    // one; H.264, HEVC, AV1 and the rest are accepted because they play.
    let harness = Harness::start().await;
    let bytes = b"this is not a video, whatever it was called".to_vec();

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let request = CreateUploadRequest {
        display_name: "Not a clip".to_owned(),
        original_filename: "notaclip.mov".to_owned(),
        folder: String::new(),
        sha256,
        byte_size: bytes.len() as u64,
        media_item_id: None,
        kind: MediaKind::Video,
        dimensions: SizePx::new(1920, 1080),
        duration_ms: Some(1000),
        frame_rate: Some(ovs_types::geometry::FrameRate::new(25, 1)),
        has_alpha: false,
        container: "mov".to_owned(),
        codec: "prores".to_owned(),
    };

    let (status, body) = harness.declare(&request).await;
    assert_eq!(status, StatusCode::OK, "declaring is fine");
    let upload_id = body["upload_id"].as_str().expect("an upload id").to_owned();

    let (status, _) = harness.send_chunk(&upload_id, 0, &bytes).await;
    assert_eq!(status, StatusCode::OK, "the bytes arrive");

    let (status, said) = harness.verify(&upload_id).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        said["message"]
            .as_str()
            .is_some_and(|message| message.contains("notaclip.mov")),
        "the refusal names the file the operator chose: {said}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_clip_outside_the_profile_is_refused_with_the_list_of_what_is_wrong() {
    // The owner's narrowing of 2026-09-23: HEVC 8-bit, HEVC 10-bit and HAP
    // reach the library, and nothing else does. The committed ProRes fixture is
    // a real file of a banned codec, which is exactly what this needs.
    //
    // The client transcodes before it uploads, so an operator should never see
    // this. That is why it is tested: *should never* is not a guarantee, and an
    // older client or a script reaches the same endpoint.
    let harness = Harness::start().await;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ovs-renderer/tests/media/colours.mov");
    let bytes = std::fs::read(&path).expect("the fixture is committed");

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let request = CreateUploadRequest {
        display_name: "Colours, ProRes".to_owned(),
        original_filename: "colours.mov".to_owned(),
        folder: String::new(),
        sha256,
        byte_size: bytes.len() as u64,
        media_item_id: None,
        kind: MediaKind::Video,
        dimensions: SizePx::new(64, 64),
        duration_ms: Some(1200),
        frame_rate: Some(ovs_types::geometry::FrameRate::new(25, 1)),
        has_alpha: false,
        container: "mov".to_owned(),
        codec: "prores".to_owned(),
    };

    let (status, body) = harness.declare(&request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let CreateUploadResponse::Accepted(session) =
        serde_json::from_value(body).expect("an upload response")
    else {
        panic!("the bytes are taken; it is the verify that refuses");
    };

    let (status, _) = harness.send_chunk(&session.upload_id, 0, &bytes).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = harness.verify(&session.upload_id).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a ProRes file must not reach the library"
    );

    // And the refusal has to be actionable. A message that only said "refused"
    // would leave somebody guessing which of eight things to change.
    let said = body.to_string();
    assert!(said.contains("colours.mov"), "names the file: {said}");
    assert!(said.contains("prores"), "names what it found: {said}");
    assert!(
        said.contains("HEVC") && said.contains("HAP"),
        "names what it wants: {said}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_hap_clip_is_ingested_like_any_other() {
    // The other half of what the policy accepts. A HAP file is a texture rather
    // than a coded picture — no pixel format, no colour tags, every frame a
    // keyframe — and the library records it the same way it records the HEVC
    // one. A test that only used HEVC would be testing half the policy.
    let harness = Harness::start().await;
    let (bytes, request) = a_long_gop_clip();

    let item = ingest(&harness, &bytes, &request).await;

    assert_eq!(item.state, MediaState::Ready);
    assert_eq!(item.dimensions, Some(SizePx::new(64, 64)));
    assert_eq!(item.duration_ms, Some(1200), "30 frames at 25 fps");
    assert_eq!(
        item.frame_rate,
        Some(ovs_types::geometry::FrameRate::new(25, 1))
    );

    // And the record an operator sees still carries no codec. That was true
    // when the server chose the format and has to stay true now the file does:
    // the codec lives on the asset, which is where a diagnostics view reads it
    // from.
    let listed = serde_json::to_value(&item).expect("serialises");
    assert!(
        listed.get("codec").is_none() && listed.get("container").is_none(),
        "a media item says what it is, not how it is encoded: {listed}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn what_the_library_says_about_a_long_gop_clip_comes_from_the_clip_too() {
    // The same check as for ProRes, on a container that reports its duration
    // differently. A codec the server never used to accept is a codec whose
    // metadata nothing here has ever read.
    let harness = Harness::start().await;
    let (bytes, honest) = a_long_gop_clip();
    let request = CreateUploadRequest {
        // Wrong on purpose: the file's own answer is the one that is kept.
        duration_ms: Some(999_999),
        ..honest
    };

    let item = ingest(&harness, &bytes, &request).await;
    assert_eq!(
        item.duration_ms,
        Some(1200),
        "the file's answer, not the claim"
    );
}

#[tokio::test]
async fn the_server_can_say_how_much_room_is_left_where_media_is_kept() {
    // Ingest refuses a file that would fill the disk, and that refusal is only
    // as good as this number. Finding it means matching a path against the
    // mounted filesystems by longest prefix, which is the part that goes wrong
    // quietly: a media root under a mount inside another mount answers about
    // the wrong disk, and the answer still looks like a plausible number of
    // bytes. The media root here does not exist yet either, which is the state
    // a fresh server is in and the one a path lookup is most likely to fail on.
    let harness = Harness::start().await;

    let available = harness
        .state
        .media_space_available()
        .expect("the temporary directory is on a filesystem");
    assert!(
        available > 0,
        "a filesystem the tests are writing to has room on it"
    );

    let sample = harness.state.host();
    // `host()` has never been refreshed here, so the sample is the empty one;
    // the point of the assertion is that the fresh read did not come from it.
    assert_eq!(sample.disk_total_bytes, 0);
}

#[tokio::test]
async fn an_upload_is_measured_against_the_room_that_is_actually_left() {
    // The rule itself is tested in `uploads::space`. What this covers is that
    // the handler is wired to it at all: an ordinary file is still accepted on
    // a disk with room, which is the case that would break if the check were
    // inverted or compared against the wrong number.
    let harness = Harness::start().await;
    let (bytes, request) = a_still();

    let available = harness.state.media_space_available().expect("a filesystem");
    assert!(
        ovs_core::uploads::fits(bytes.len() as u64, available),
        "the disk these tests run on has room for a few hundred bytes"
    );

    let (status, _) = harness.declare(&request).await;
    assert_eq!(status, StatusCode::OK);
}
