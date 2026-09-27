//! Folders in the media library, over the HTTP API.
//!
//! The rule the whole feature exists under is the owner's (2026-09-23):
//! **organising media must not move the real files.** So the things checked
//! here are that a folder is a label and nothing else — the asset keeps its
//! object key and the content hash it is found by — and the two cases where a
//! well-meaning implementation loses somebody's work: a folder that vanishes
//! while it is empty, and a folder that takes its contents with it when it is
//! deleted.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{
    CreateAccountRequest, CreateFolderRequest, FileMediaRequest, LoginRequest, LoginResponse,
    MediaListResponse, RenameFolderRequest,
};
use ovs_types::geometry::{FrameRate, SizePx};
use ovs_types::id::MediaItemId;
use ovs_types::media::{MediaItem, MediaKind, MediaState};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    app: axum::Router,
    token: String,
    state: AppState,
    clip: MediaItemId,
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

        let item = MediaItem {
            kind: MediaKind::Video,
            state: MediaState::Ready,
            duration_ms: Some(4_000),
            dimensions: Some(SizePx::new(1920, 1080)),
            frame_rate: Some(FrameRate::new(25, 1)),
            ..MediaItem::importing("Kaleidoskop", "kaleidoskop.mov")
        };
        let clip = item.id;
        state
            .with_database(|database| database.upsert_media_item(&item))
            .expect("stores the item");

        Self {
            app,
            token: login.token,
            state,
            clip,
        }
    }

    async fn make_folder(&self, path: &str) -> StatusCode {
        let (status, _) = self
            .post(
                "/api/v1/media/folders",
                &CreateFolderRequest {
                    path: path.to_owned(),
                },
            )
            .await;
        status
    }

    async fn file(&self, request: &FileMediaRequest) -> (StatusCode, serde_json::Value) {
        self.put(&format!("/api/v1/media/{}", self.clip), request)
            .await
    }

    async fn library(&self) -> MediaListResponse {
        let (status, body) = self.get("/api/v1/media").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_value(body).expect("a media list")
    }

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

    async fn delete_folder(&self, path: &str) -> StatusCode {
        let request = Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/media/folders?path={}", urlencoding(path)))
            .header("authorization", format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("builds");
        send(&self.app, request).await.0
    }
}

/// Percent-encodes what a folder path can hold: slashes and spaces.
///
/// Enough for the paths these tests use, and written out rather than pulled in
/// as a dependency for one line of test code.
fn urlencoding(path: &str) -> String {
    path.chars()
        .map(|character| match character {
            '/' => "%2F".to_owned(),
            ' ' => "%20".to_owned(),
            other => other.to_string(),
        })
        .collect()
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
async fn a_folder_exists_while_it_is_still_empty() {
    // The whole reason folders are a table rather than something worked out
    // from the items: an operator makes one in order to put things in it.
    let harness = Harness::start().await;

    assert_eq!(harness.make_folder("Intros").await, StatusCode::NO_CONTENT);

    let library = harness.library().await;
    assert_eq!(library.folders, vec!["Intros".to_owned()]);
}

#[tokio::test]
async fn folders_nest_as_deep_as_an_operator_wants() {
    // The owner's instruction (2026-09-23): folders in folders in folders, as
    // far as they care to go.
    let harness = Harness::start().await;
    let deep = "Tour 2026/September/Berlin/Set One/Openers/Warm";

    assert_eq!(harness.make_folder(deep).await, StatusCode::NO_CONTENT);

    let library = harness.library().await;
    assert_eq!(
        library.folders,
        vec![
            "Tour 2026".to_owned(),
            "Tour 2026/September".to_owned(),
            "Tour 2026/September/Berlin".to_owned(),
            "Tour 2026/September/Berlin/Set One".to_owned(),
            "Tour 2026/September/Berlin/Set One/Openers".to_owned(),
            "Tour 2026/September/Berlin/Set One/Openers/Warm".to_owned(),
        ],
        "every folder on the way has to exist, or the tree has holes in it"
    );
}

#[tokio::test]
async fn filing_a_clip_changes_where_it_is_listed_and_nothing_about_the_file() {
    // "Organising media must not move the real files" (owner, 2026-09-23).
    let harness = Harness::start().await;
    harness.make_folder("Intros").await;
    let before = harness
        .state
        .with_database(|database| database.load_media_item(harness.clip))
        .expect("loads");

    let (status, body) = harness
        .file(&FileMediaRequest {
            display_name: Some("Kaleidoskop, langsam".to_owned()),
            folder: Some("Intros".to_owned()),
        })
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let after = harness
        .state
        .with_database(|database| database.load_media_item(harness.clip))
        .expect("loads");
    assert_eq!(after.folder, "Intros");
    assert_eq!(after.display_name, "Kaleidoskop, langsam");
    assert_eq!(
        after.original_filename, before.original_filename,
        "the filename it came in as is not a label and must not follow a rename"
    );
    assert_eq!(after.id, before.id, "a rename is not a new item");
}

#[tokio::test]
async fn a_field_left_out_leaves_that_half_alone() {
    // A rename need not send a folder and a move need not send a name.
    let harness = Harness::start().await;
    harness.make_folder("Intros").await;
    harness
        .file(&FileMediaRequest {
            display_name: None,
            folder: Some("Intros".to_owned()),
        })
        .await;

    harness
        .file(&FileMediaRequest {
            display_name: Some("Renamed".to_owned()),
            folder: None,
        })
        .await;

    let item = harness
        .state
        .with_database(|database| database.load_media_item(harness.clip))
        .expect("loads");
    assert_eq!(item.display_name, "Renamed");
    assert_eq!(item.folder, "Intros", "a rename moved it back to the root");
}

#[tokio::test]
async fn filing_a_clip_somewhere_that_does_not_exist_is_refused() {
    // Filed somewhere that is not there is filed nowhere an operator can
    // navigate to, which is the same as losing it.
    let harness = Harness::start().await;

    let (status, _) = harness
        .file(&FileMediaRequest {
            display_name: None,
            folder: Some("Nowhere".to_owned()),
        })
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn renaming_a_folder_takes_everything_under_it_along() {
    let harness = Harness::start().await;
    harness.make_folder("Intros/2026").await;
    harness
        .file(&FileMediaRequest {
            display_name: None,
            folder: Some("Intros/2026".to_owned()),
        })
        .await;

    let (status, body) = harness
        .put(
            "/api/v1/media/folders",
            &RenameFolderRequest {
                from: "Intros".to_owned(),
                to: "Openers".to_owned(),
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let library = harness.library().await;
    assert_eq!(
        library.folders,
        vec!["Openers".to_owned(), "Openers/2026".to_owned()]
    );
    let item = harness
        .state
        .with_database(|database| database.load_media_item(harness.clip))
        .expect("loads");
    assert_eq!(
        item.folder, "Openers/2026",
        "the clip was left pointing at a folder that no longer exists"
    );
}

#[tokio::test]
async fn a_folder_cannot_be_moved_inside_itself() {
    let harness = Harness::start().await;
    harness.make_folder("Intros").await;

    let (status, _) = harness
        .put(
            "/api/v1/media/folders",
            &RenameFolderRequest {
                from: "Intros".to_owned(),
                to: "Intros/Again".to_owned(),
            },
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn deleting_a_folder_that_still_holds_something_is_refused() {
    // The alternative is orphaning an operator's clips, or deleting their media
    // because they tidied up. Neither is a thing to do without being asked.
    let harness = Harness::start().await;
    harness.make_folder("Intros").await;
    harness
        .file(&FileMediaRequest {
            display_name: None,
            folder: Some("Intros".to_owned()),
        })
        .await;

    assert_eq!(harness.delete_folder("Intros").await, StatusCode::CONFLICT);

    // Empty it, and it goes.
    harness
        .file(&FileMediaRequest {
            display_name: None,
            folder: Some(String::new()),
        })
        .await;
    assert_eq!(
        harness.delete_folder("Intros").await,
        StatusCode::NO_CONTENT
    );
    assert!(harness.library().await.folders.is_empty());
}

#[tokio::test]
async fn a_folder_with_folders_under_it_is_refused_too() {
    let harness = Harness::start().await;
    harness.make_folder("Intros/2026").await;

    assert_eq!(harness.delete_folder("Intros").await, StatusCode::CONFLICT);
    assert_eq!(
        harness.delete_folder("Intros/2026").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        harness.delete_folder("Intros").await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn a_name_nobody_could_tell_from_another_is_refused() {
    let harness = Harness::start().await;

    assert_eq!(
        harness.make_folder("Intros ").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(harness.make_folder("").await, StatusCode::NO_CONTENT);
    assert!(
        harness.library().await.folders.is_empty(),
        "the root is not a folder and is not a row"
    );
}
