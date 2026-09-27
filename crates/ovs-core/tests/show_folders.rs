//! Folders in the show list, over the HTTP API.
//!
//! The rule the whole feature exists under, and the one the owner set for the
//! library's folders on the same day: **filing changes nothing about the
//! thing filed.** A show plays what it played, from the same document, at
//! the same identity, wherever it sits in somebody's filing.
//!
//! So what is checked here is that a folder is a label and nothing else, and
//! the two cases where a well-meaning implementation loses somebody's work: a
//! folder that vanishes while it is empty, and a folder that takes its
//! contents with it when it is deleted.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{
    CreateAccountRequest, CreateFolderRequest, CreateShowRequest, FileShowRequest, LoginRequest,
    LoginResponse, RenameFolderRequest, ShowFoldersResponse, ShowListEntry,
};
use ovs_types::id::ShowId;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    app: axum::Router,
    token: String,
    show: ShowId,
}

impl Harness {
    /// A signed-in operator with one show on the server.
    async fn start() -> Self {
        let state = AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test");
        let app = router(state);

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

        let mut harness = Self {
            app,
            token: login.token,
            show: ShowId::new(),
        };

        let (status, body) = harness
            .post(
                "/api/v1/shows",
                &CreateShowRequest {
                    name: "Nachtfahrt".to_owned(),
                },
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let show: ovs_types::config::Show = serde_json::from_value(body).expect("a show");
        harness.show = show.id;
        harness
    }

    async fn make_folder(&self, path: &str) -> StatusCode {
        let (status, _) = self
            .post(
                "/api/v1/shows/folders",
                &CreateFolderRequest {
                    path: path.to_owned(),
                },
            )
            .await;
        status
    }

    async fn file(&self, folder: &str) -> (StatusCode, serde_json::Value) {
        self.put(
            &format!("/api/v1/shows/{}/folder", self.show),
            &FileShowRequest {
                folder: folder.to_owned(),
            },
        )
        .await
    }

    async fn folders(&self) -> Vec<String> {
        let (status, body) = self.get("/api/v1/shows/folders").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let answer: ShowFoldersResponse = serde_json::from_value(body).expect("the folders");
        answer.folders
    }

    async fn shows(&self) -> Vec<ShowListEntry> {
        let (status, body) = self.get("/api/v1/shows").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_value(body).expect("a show list")
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

    async fn delete(&self, path: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("DELETE")
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
        .expect("reads the body")
        .to_bytes();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn a_folder_exists_while_it_is_empty() {
    // The whole reason folders are a table of their own. An operator makes one
    // in order to put a show in it, and one derived from the shows in it
    // would vanish between being made and being filled.
    let harness = Harness::start().await;

    assert_eq!(
        harness.make_folder("2026/Summer tour").await,
        StatusCode::CREATED
    );

    // The folder above it too: a tree with a hole in it cannot be drawn.
    assert_eq!(
        harness.folders().await,
        vec!["2026".to_owned(), "2026/Summer tour".to_owned()],
    );
}

#[tokio::test]
async fn filing_a_show_changes_nothing_but_where_it_is_filed() {
    // "Projekte sollten ebenfalls Ordner und Unterordner haben können" (owner,
    // 2026-09-23), under the rule their media folders keep: the thing filed is
    // untouched. Same identity, same name, same document.
    let harness = Harness::start().await;
    assert_eq!(harness.make_folder("2026").await, StatusCode::CREATED);

    let before = harness.shows();
    let before = before.await;
    let was = before.first().expect("the show").clone();

    let (status, body) = harness.file("2026").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let now = harness.shows().await;
    let is = now.first().expect("the show");
    assert_eq!(is.folder, "2026", "filed where it was put");
    assert_eq!(is.id, was.id, "the same show");
    assert_eq!(is.name, was.name, "with the same name");
    assert_eq!(is.updated_at, was.updated_at, "and the document untouched");
}

#[tokio::test]
async fn a_show_can_be_taken_back_out_to_the_root() {
    let harness = Harness::start().await;
    assert_eq!(harness.make_folder("2026").await, StatusCode::CREATED);
    assert_eq!(harness.file("2026").await.0, StatusCode::NO_CONTENT);

    assert_eq!(harness.file("").await.0, StatusCode::NO_CONTENT);

    let now = harness.shows().await;
    assert_eq!(now.first().expect("the show").folder, "");
}

#[tokio::test]
async fn filing_a_show_somewhere_that_is_not_there_is_refused() {
    // Refused rather than made on the way past: a typo would otherwise become
    // a folder, and the show would be in it.
    let harness = Harness::start().await;

    let (status, _) = harness.file("Somewhere else").await;
    assert_ne!(status, StatusCode::NO_CONTENT);

    assert!(harness.folders().await.is_empty(), "and no folder was made");
    assert_eq!(harness.shows().await[0].folder, "");
}

#[tokio::test]
async fn deleting_a_folder_that_still_holds_a_show_is_refused() {
    // The case that loses somebody's work. Refused rather than taking the
    // shows with it, and refused rather than orphaning them.
    let harness = Harness::start().await;
    assert_eq!(harness.make_folder("2026").await, StatusCode::CREATED);
    assert_eq!(harness.file("2026").await.0, StatusCode::NO_CONTENT);

    let (status, _) = harness.delete("/api/v1/shows/folders?path=2026").await;
    assert_ne!(status, StatusCode::NO_CONTENT);

    assert_eq!(harness.folders().await, vec!["2026".to_owned()]);
    assert_eq!(harness.shows().await[0].folder, "2026");
}

#[tokio::test]
async fn an_empty_folder_is_removed_without_ceremony() {
    let harness = Harness::start().await;
    assert_eq!(harness.make_folder("2026").await, StatusCode::CREATED);

    let (status, _) = harness.delete("/api/v1/shows/folders?path=2026").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(harness.folders().await.is_empty());
}

#[tokio::test]
async fn renaming_a_folder_takes_everything_under_it_along() {
    // The path *is* the identity, so a rename is a rewrite of every path that
    // begins with it — the folders under it and the shows filed in any of
    // them.
    let harness = Harness::start().await;
    assert_eq!(
        harness.make_folder("2026/Summer tour").await,
        StatusCode::CREATED
    );
    assert_eq!(
        harness.file("2026/Summer tour").await.0,
        StatusCode::NO_CONTENT
    );

    let (status, body) = harness
        .put(
            "/api/v1/shows/folders",
            &RenameFolderRequest {
                from: "2026".to_owned(),
                to: "Twenty twenty-six".to_owned(),
            },
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    assert_eq!(
        harness.folders().await,
        vec![
            "Twenty twenty-six".to_owned(),
            "Twenty twenty-six/Summer tour".to_owned(),
        ],
    );
    assert_eq!(
        harness.shows().await[0].folder,
        "Twenty twenty-six/Summer tour",
        "the show went with it",
    );
}

#[tokio::test]
async fn a_folder_cannot_be_moved_inside_itself() {
    let harness = Harness::start().await;
    assert_eq!(harness.make_folder("2026").await, StatusCode::CREATED);

    let (status, _) = harness
        .put(
            "/api/v1/shows/folders",
            &RenameFolderRequest {
                from: "2026".to_owned(),
                to: "2026/Again".to_owned(),
            },
        )
        .await;
    assert_ne!(status, StatusCode::NO_CONTENT);
    assert_eq!(harness.folders().await, vec!["2026".to_owned()]);
}

#[tokio::test]
async fn a_show_called_folders_does_not_shadow_the_folders_endpoint() {
    // `/shows/folders` is a static segment and `/shows/{id}` a capture,
    // and the router matches the static one first. Worth a test rather than a
    // comment: the failure would be a folder list that answers with a show.
    let harness = Harness::start().await;
    let (status, body) = harness
        .post(
            "/api/v1/shows",
            &CreateShowRequest {
                name: "folders".to_owned(),
            },
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    assert_eq!(harness.make_folder("2026").await, StatusCode::CREATED);
    assert_eq!(harness.folders().await, vec!["2026".to_owned()]);
}
