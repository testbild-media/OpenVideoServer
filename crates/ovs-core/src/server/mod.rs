//! The HTTP and WebSocket transport.
//!
//! Thin by design. Every rule that matters lives in [`crate::state`],
//! [`crate::auth`] and `ovs_db`, so it holds however a request arrived. This
//! layer parses, authenticates, delegates and serialises.

mod app_state;
mod auth_routes;
mod desk_fixtures;
mod downloads;
mod error;
mod export_routes;
mod live_routes;
mod local;
mod media_routes;
mod preview_routes;
mod realtime_routes;
mod server_routes;
mod show_edits;
mod show_routes;
/// What a server does for itself before it answers anything.
pub mod startup;
mod stream_routes;
mod system_routes;
mod upload_routes;
mod web_routes;

use axum::routing::{delete, get, post, put};
use axum::Router;
use ovs_proto::http::API_PREFIX;

pub use app_state::{AppState, Preview, StreamChunk, StreamEvent, Watching};
pub use error::ApiResult;
pub use local::{is_local, trusts_loopback, LOCAL_HEADER, TRUST_ENV};

/// Builds the complete application router.
///
/// Every route lives under [`API_PREFIX`] except the realtime socket, which is
/// also versioned through the handshake rather than the path.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        // Unauthenticated: a client needs these before it can sign in, and the
        // first of them is how it learns whether setup is still needed.
        .route("/server/capabilities", get(server_routes::capabilities))
        .route("/auth/setup", post(auth_routes::setup))
        .route("/auth/login", post(auth_routes::login))
        // Authenticated.
        .route("/auth/logout", post(auth_routes::logout))
        // Who this session is, which every interface asks before it draws:
        // a page offering user management to an operator is a page whose
        // buttons the server refuses.
        .route("/auth/me", get(auth_routes::who_am_i))
        // The accounts themselves. Administrators only, except the password of
        // the account doing the asking (owner, 2026-09-24).
        .route(
            "/accounts",
            get(auth_routes::list_accounts).post(auth_routes::create_account),
        )
        .route(
            "/accounts/{account_id}",
            put(auth_routes::update_account).delete(auth_routes::delete_account),
        )
        .route(
            "/accounts/{account_id}/password",
            put(auth_routes::set_password),
        )
        .route("/server/status", get(server_routes::status))
        .route("/fixtures", get(server_routes::fixtures))
        .route("/generators", get(server_routes::generators))
        .route("/effects", get(server_routes::effects))
        .route(
            "/server/renderer/restart",
            post(server_routes::restart_renderer),
        )
        .route("/server/power", post(server_routes::power))
        // Updating the server from its own administration page. Administrators
        // only; the repository is asked only when one presses the button.
        .route("/server/update", get(server_routes::update_status))
        .route(
            "/server/update/check",
            post(server_routes::check_for_update),
        )
        .route(
            "/server/update/install",
            post(server_routes::install_update),
        )
        .route(
            "/server/counters/reset",
            post(server_routes::reset_counters),
        )
        .route("/server/test-pattern", put(server_routes::set_test_pattern))
        .route(
            "/server/connectors/role",
            put(server_routes::set_connector_role),
        )
        .route("/server/dmx", put(server_routes::set_dmx_protocols))
        // What the machine is, as opposed to what it is showing: its name and
        // the connector the System screen is on. Administrators only, and the
        // change takes effect at the next start rather than now.
        .route(
            "/system/config",
            get(system_routes::read_config).put(system_routes::write_config),
        )
        .route(
            "/server/startup-policy",
            put(server_routes::set_startup_policy),
        )
        .route("/shows", get(show_routes::list).post(show_routes::create))
        .route(
            "/shows/{show_id}",
            get(show_routes::load)
                .put(show_routes::save)
                .delete(show_routes::delete),
        )
        .route("/shows/{show_id}/open", post(show_routes::open))
        // Folders for the show list (owner, 2026-09-23). Static before the
        // capture above it, which is how axum matches: `/shows/folders` is
        // this and never a show called "folders".
        .route(
            "/shows/folders",
            get(show_routes::folders)
                .post(show_routes::create_folder)
                .put(show_routes::rename_folder)
                .delete(show_routes::delete_folder),
        )
        .route("/shows/{show_id}/folder", put(show_routes::file))
        // Storing what is on stage into the open show. A save with no
        // document, because the server already holds both halves.
        .route("/shows/{show_id}/capture", post(show_routes::capture))
        // The edits an operator makes with a show running. Not whole-document
        // saves, because storing a show the server has open reloads it
        // there, mid-show. See `show_edits`.
        .route(
            "/shows/{show_id}/layers/{layer_id}/slots/{slot_index}",
            put(show_edits::assign).delete(show_edits::clear),
        )
        .route(
            "/shows/{show_id}/layers/{layer_id}/banks",
            put(show_edits::set_banks),
        )
        // The mapping, while an operator watches the wall it is about.
        .route("/shows/{show_id}/mapping", put(show_edits::set_mapping))
        .route(
            "/shows/{show_id}/layers/{layer_id}/effects",
            post(show_edits::add_layer_effect),
        )
        .route(
            "/shows/{show_id}/layers/{layer_id}/effects/{effect_id}",
            delete(show_edits::remove_layer_effect),
        )
        // **Before the one above in meaning, not in matching.** `order` is a
        // literal segment and `{effect_id}` is not, so a router prefers it; the
        // methods differ as well, and nothing can reach the wrong handler.
        .route(
            "/shows/{show_id}/layers/{layer_id}/effects/order",
            put(show_edits::order_layer_effects),
        )
        .route(
            "/shows/{show_id}/layers/{layer_id}/slots/{slot_index}/settings",
            put(show_edits::set_slot),
        )
        .route(
            "/shows/{show_id}/layers/{layer_id}/slots/{slot_index}/effects",
            post(show_edits::add_slot_effect),
        )
        .route(
            "/shows/{show_id}/layers/{layer_id}/slots/{slot_index}/effects/{effect_id}",
            delete(show_edits::remove_slot_effect),
        )
        .route(
            "/shows/{show_id}/layers/{layer_id}/slots/{slot_index}/effects/order",
            put(show_edits::order_slot_effects),
        )
        .route(
            "/shows/{show_id}/patch/address",
            get(show_routes::suggest_address),
        )
        .route("/shows/{show_id}/export", get(export_routes::export))
        .route("/shows/import", post(export_routes::import))
        .route("/live/layers/{layer_id}", put(live_routes::update_layer))
        .route(
            "/live/layers/{layer_id}/slot",
            post(live_routes::select_slot),
        )
        .route(
            "/live/effects/{effect_instance_id}",
            put(live_routes::update_effect),
        )
        .route(
            "/live/generators/{generator_instance_id}",
            put(live_routes::update_generator),
        )
        .route("/live/master", put(live_routes::update_master))
        .route("/live/tap", post(live_routes::tap))
        .route("/live/phase/reset", post(live_routes::reset_phase))
        .route("/preview", get(preview_routes::list))
        .route("/preview/enabled", put(preview_routes::set_enabled))
        .route("/preview/composition", get(preview_routes::composition))
        .route("/preview/{output_id}", get(preview_routes::show))
        // A socket rather than a route that returns: the pictures are pushed as
        // they are made. Under the API prefix like everything else, because a
        // client that speaks this speaks the rest.
        .route("/preview/stream/{surface}", get(stream_routes::watch))
        .route(
            "/preview/stream/{surface}/ticket",
            post(stream_routes::ticket),
        )
        .route("/media", get(media_routes::list))
        // Folders are the library's own structure and never a path on any
        // filesystem: organising media must not move the real files
        // (owner, 2026-09-23).
        .route(
            "/media/folders",
            post(media_routes::create_folder)
                .put(media_routes::rename_folder)
                .delete(media_routes::delete_folder),
        )
        .route("/media/uploads", post(upload_routes::create))
        .route("/media/uploads/{upload_id}", get(upload_routes::status))
        .route(
            "/media/uploads/{upload_id}",
            put(upload_routes::append).delete(upload_routes::cancel),
        )
        .route(
            "/media/uploads/{upload_id}/verify",
            post(upload_routes::verify),
        )
        .route("/media/{media_item_id}", put(media_routes::file))
        .route("/media/{media_item_id}/usage", get(media_routes::usage))
        .route(
            "/media/{media_item_id}/thumbnail",
            get(media_routes::thumbnail),
        )
        // Ten frames of a clip in one picture, for a pad that moves under the
        // pointer.
        .route("/media/{media_item_id}/strip", get(media_routes::strip))
        // The bytes themselves, one item per request, for an export that takes
        // the media with it.
        .route("/media/{media_item_id}/content", get(media_routes::content))
        .route("/media/{media_item_id}", delete(media_routes::delete))
        // A chunk is megabytes, and axum's default body limit is two. The
        // ceiling here is one chunk plus room for the framing; a client sending
        // more than it was told to is refused by the transport rather than
        // filling memory first and being refused after.
        .layer(axum::extract::DefaultBodyLimit::max(
            (crate::uploads::CHUNK_SIZE + 1024 * 1024) as usize,
        ))
        .with_state(state.clone());

    Router::new()
        .nest(API_PREFIX, api)
        .route("/realtime", get(realtime_routes::upgrade))
        // The Service Console. Served from here rather than
        // from a process of its own: it is one static page that uses the same
        // API as any other client, so a second listener would buy nothing the
        // browser boundary does not already give. It reads and never writes,
        // and a page that failed to load cannot reach anything on stage.
        .route("/console", get(console))
        // **The pages a browser gets.** `/` is what somebody sees for typing
        // this machine's address in; `/admin` is the alternative to a Service
        // Display on a server that has none (owner, 2026-09-24). Both are on
        // this same router, so the page and the API it calls are one origin and
        // the session cookie simply works.
        .route("/", get(web_routes::status_page))
        .route("/status.json", get(web_routes::public_status))
        .route("/admin", get(web_routes::admin_page))
        .route("/download/{name}", get(web_routes::download))
        .route("/fixtures.zip", get(web_routes::fixtures))
        .with_state(state)
        // Last, so it wraps everything: the one place that decides whether a
        // request came from this machine, and the only place that can say so.
        .layer(axum::middleware::from_fn(local::mark))
}

/// The System screen.
///
/// Compiled in rather than read from disk, so a server is one file to deploy
/// and a console cannot go missing because somebody tidied a directory.
///
/// Served only to the machine itself. It is the page for the touchscreen in the
/// rack, it carries no sign-in, and every request it makes is exempt for the
/// same reason it is — so putting it on the network would be handing the show
/// to anybody who could reach the port.
async fn console(headers: axum::http::HeaderMap) -> axum::response::Response {
    use axum::response::IntoResponse;

    if !local::is_local(&headers) {
        // Not "forbidden": from outside this machine there is no such page.
        return (
            axum::http::StatusCode::NOT_FOUND,
            "the System screen is served only on this machine",
        )
            .into_response();
    }

    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../../../../service-console/index.html"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use ovs_db::Database;
    use ovs_proto::http::{
        CapabilitiesResponse, CreateAccountRequest, CreateShowRequest, LoginRequest, LoginResponse,
    };
    use ovs_types::config::{Show, TestPatternMode};
    use ovs_types::telemetry::CounterGroup;
    use serde::de::DeserializeOwned;
    use tower::ServiceExt;

    use super::*;

    const PASSWORD: &str = "correct horse battery staple";

    fn app() -> (Router, AppState) {
        let database = Database::open_in_memory().expect("opens");
        let state = AppState::new(database, "0.0.1-test");
        (router(state.clone()), state)
    }

    async fn send(app: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
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

    fn get(path: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("builds")
    }

    fn get_auth(path: &str, token: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .expect("builds")
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

    fn decode<T: DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).expect("decodes")
    }

    async fn signed_in(app: &Router) -> String {
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
        decode::<LoginResponse>(body).token
    }

    #[tokio::test]
    async fn a_fresh_server_reports_that_it_needs_setup() {
        let (app, _) = app();
        let (status, body) = send(&app, get("/api/v1/server/capabilities")).await;
        assert_eq!(status, StatusCode::OK);

        let capabilities: CapabilitiesResponse = decode(body);
        assert!(
            !capabilities.configured,
            "a server with no account is unconfigured"
        );
        assert_eq!(capabilities.api_version, ovs_types::API_VERSION);
        assert_eq!(
            capabilities.show_schema_version,
            ovs_types::SHOW_SCHEMA_VERSION
        );
    }

    #[tokio::test]
    async fn setup_creates_the_account_once_and_only_once() {
        let (app, _) = app();
        let request = CreateAccountRequest {
            username: "operator".to_owned(),
            password: PASSWORD.to_owned(),
            role: None,
        };

        let (status, _) = send(&app, post_json("/api/v1/auth/setup", &request)).await;
        assert_eq!(status, StatusCode::CREATED);

        // A second setup must not be able to replace the operator's credentials.
        let (status, body) = send(&app, post_json("/api/v1/auth/setup", &request)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "already_configured");
    }

    #[tokio::test]
    async fn setup_refuses_a_short_password() {
        let (app, _) = app();
        let (status, body) = send(
            &app,
            post_json(
                "/api/v1/auth/setup",
                &CreateAccountRequest {
                    username: "operator".to_owned(),
                    password: "short".to_owned(),
                    role: None,
                },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["message"]
            .as_str()
            .expect("message")
            .contains("12 characters"));
    }

    #[tokio::test]
    async fn signing_in_with_the_wrong_password_is_refused() {
        let (app, _) = app();
        let _ = signed_in(&app).await;

        let (status, body) = send(
            &app,
            post_json(
                "/api/v1/auth/login",
                &LoginRequest {
                    username: "operator".to_owned(),
                    password: "wrong password here".to_owned(),
                },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["message"], "invalid username or password");
    }

    #[tokio::test]
    async fn an_unknown_username_is_refused_identically_to_a_wrong_password() {
        // The difference would tell an attacker which usernames exist.
        let (app, _) = app();
        let _ = signed_in(&app).await;

        let (wrong_status, wrong_body) = send(
            &app,
            post_json(
                "/api/v1/auth/login",
                &LoginRequest {
                    username: "operator".to_owned(),
                    password: "wrong password here".to_owned(),
                },
            ),
        )
        .await;
        let (unknown_status, unknown_body) = send(
            &app,
            post_json(
                "/api/v1/auth/login",
                &LoginRequest {
                    username: "nobody".to_owned(),
                    password: PASSWORD.to_owned(),
                },
            ),
        )
        .await;

        assert_eq!(wrong_status, unknown_status);
        assert_eq!(wrong_body["message"], unknown_body["message"]);
        assert_eq!(wrong_body["code"], unknown_body["code"]);
    }

    #[tokio::test]
    async fn every_protected_route_refuses_an_unauthenticated_request() {
        let (app, _) = app();
        let _ = signed_in(&app).await;

        for path in [
            "/api/v1/server/status",
            "/api/v1/shows",
            "/api/v1/media",
            "/api/v1/fixtures",
            "/api/v1/generators",
        ] {
            let (status, _) = send(&app, get(path)).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{path} must require a token"
            );
        }
    }

    #[tokio::test]
    async fn the_fixture_catalogue_is_served_whole() {
        // What somebody standing at the machine programmes a console from.
        let (app, _) = app();
        let token = signed_in(&app).await;

        let (status, body) = send(&app, get_auth("/api/v1/fixtures", &token)).await;
        assert_eq!(status, StatusCode::OK);

        let fixtures = body["fixtures"]
            .as_array()
            .expect("a list of personalities");
        // The two this build publishes plus every module that declares channels
        // of its own — one list, because a console programmer does not care
        // which of the two a personality came from (owner decision Q19).
        assert!(fixtures.len() > ovs_types::config::PUBLISHED.len());
        for wanted in ["ovs.master", "ovs.layer", "ovs.blur", "ovs.gradient"] {
            assert!(
                fixtures
                    .iter()
                    .any(|personality| personality["type_id"] == wanted),
                "{wanted} is not in the catalogue"
            );
        }
        // And a module that declares nothing is not in it at all: a personality
        // with no channels would take an address and do nothing.
        assert!(
            !fixtures
                .iter()
                .any(|personality| personality["footprint"] == 0),
            "a personality with no channels was published"
        );
    }

    #[tokio::test]
    async fn restarting_the_renderer_is_recorded_for_the_link_to_perform() {
        // The core does not start processes. It asks the renderer to stop and
        // whatever supervises it starts it again, which is the same path a
        // crashed renderer takes.
        let (app, state) = app();
        let token = signed_in(&app).await;

        assert!(!state.take_renderer_restart(), "nothing asked for yet");

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/server/renderer/restart")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .expect("a request");
        let (status, _) = send(&app, request).await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(
            state.take_renderer_restart(),
            "the link has something to do"
        );
        assert!(
            !state.take_renderer_restart(),
            "and taking it clears it, so a reconnect does not stop it again"
        );
    }

    #[tokio::test]
    async fn the_power_endpoint_refuses_a_request_without_a_token() {
        // Rebooting a server in the middle of a show is exactly the thing that
        // must not be reachable by anybody who can see the port. This test
        // deliberately never sends a valid token: the endpoint it is pointed at
        // would reboot whatever machine ran the test.
        let (app, _) = app();
        let _ = signed_in(&app).await;

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/server/power")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"action":"reboot"}"#))
            .expect("a request");
        let (status, _) = send(&app, request).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn updating_the_server_is_for_an_administrator_and_nobody_else() {
        // Installing a package is root on the machine by another route, so
        // none of the three answers a request without a session.
        let (app, _) = app();
        let _ = signed_in(&app).await;

        let (status, _) = send(&app, get("/api/v1/server/update")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        for path in [
            "/api/v1/server/update/check",
            "/api/v1/server/update/install",
        ] {
            let (status, _) = send(
                &app,
                post_json(path, &serde_json::json!({ "version": "9.9.9" })),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        }
    }

    #[tokio::test]
    async fn an_administrator_sees_the_installed_version_and_cannot_go_back() {
        let (app, _) = app();
        let token = signed_in(&app).await;

        let (status, body) = send(&app, get_auth("/api/v1/server/update", &token)).await;
        assert_eq!(status, StatusCode::OK);
        let update: ovs_proto::http::UpdateStatus = decode(body);
        assert_eq!(update.installed, "0.0.1-test");

        // Refused before anything is run: an older version, the same one, and
        // something that is not a version at all. None of these can start the
        // installer on the machine running the test.
        for version in ["0.0.0", "0.0.1", "1.2.3; reboot", "../9.9.9"] {
            let (status, body) = send(
                &app,
                post_json_auth(
                    "/api/v1/server/update/install",
                    &token,
                    &ovs_proto::http::InstallUpdateRequest {
                        version: version.to_owned(),
                    },
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{version}");
            let answer: ovs_proto::http::InstallUpdateResponse = decode(body);
            assert!(!answer.accepted, "{version} was accepted");
        }
    }

    #[tokio::test]
    async fn restarting_the_renderer_needs_a_token_too() {
        let (app, state) = app();
        let _ = signed_in(&app).await;

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/server/renderer/restart")
            .body(Body::empty())
            .expect("a request");
        let (status, _) = send(&app, request).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!state.take_renderer_restart(), "and nothing was recorded");
    }

    #[tokio::test]
    async fn a_patch_editor_is_told_where_a_fixture_fits() {
        // What stops an operator counting channels by hand, and what makes
        // "collision detection and address suggestions" a feature rather than
        // an error message.
        use ovs_types::config::patch::FixtureTarget;
        use ovs_types::config::{FixtureInstance, Show, LAYER_V1, MASTER_V1};

        let (app, state) = app();
        let token = signed_in(&app).await;

        let mut show = Show::new("Patched");
        show.fixture_patch.push(FixtureInstance {
            id: ovs_types::id::FixtureInstanceId::new(),
            type_id: MASTER_V1.type_id.to_owned(),
            version: MASTER_V1.version,
            target: FixtureTarget::Master,
            universe: 1,
            start_address: 1,
            footprint: MASTER_V1.footprint,
            enabled: true,
        });
        let show_id = show.id;
        state.with_database(|database| database.save_show(&show).expect("saves"));

        let layer = LAYER_V1.footprint;
        let (status, body) = send(
            &app,
            get_auth(
                &format!("/api/v1/shows/{show_id}/patch/address?universe=1&footprint={layer}"),
                &token,
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["start_address"],
            i64::from(MASTER_V1.footprint) + 1,
            "after the master fixture"
        );

        // A universe nothing is patched in starts at one.
        let (_, body) = send(
            &app,
            get_auth(
                &format!("/api/v1/shows/{show_id}/patch/address?universe=2&footprint={layer}"),
                &token,
            ),
        )
        .await;
        assert_eq!(body["start_address"], 1);
    }

    /// Asks for one of the two patterns, or for neither.
    async fn set_pattern(app: &Router, token: &str, mode: &str) -> StatusCode {
        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/server/test-pattern")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"mode":"{mode}"}}"#)))
            .expect("a request");
        send(app, request).await.0
    }

    #[tokio::test]
    async fn each_test_pattern_goes_on_and_off_and_telemetry_says_which() {
        // An engineer lining a wall up, and every other client entitled to know
        // why the wall is not showing the show — and which of the two patterns
        // it is showing, because only one of them goes through the mapping.
        let (app, state) = app();
        let token = signed_in(&app).await;

        assert_eq!(
            state.test_pattern(),
            TestPatternMode::Off,
            "off unless somebody asks"
        );

        for (asked, expected) in [
            ("COMPOSITION", TestPatternMode::Composition),
            ("OUTPUTS", TestPatternMode::Outputs),
            ("OFF", TestPatternMode::Off),
        ] {
            assert_eq!(
                set_pattern(&app, &token, asked).await,
                StatusCode::NO_CONTENT
            );
            assert_eq!(state.test_pattern(), expected);

            let request = Request::builder()
                .uri("/api/v1/server/status")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("a request");
            let (status, reported) = send(&app, request).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                reported["telemetry"]["test_pattern"],
                serde_json::json!(asked),
                "the telemetry says which pattern is on"
            );
        }
    }

    #[tokio::test]
    async fn clearing_the_counters_is_a_request_on_its_way_to_the_renderer() {
        // The counters live in the renderer, so the core takes the request and
        // says so rather than pretending to have done it.
        let (app, state) = app();
        let token = signed_in(&app).await;

        assert_eq!(state.take_counter_reset(), None, "nothing asked for yet");

        // Tapping one count clears that count. Tapping the row on a System
        // screen is the only way this is ever called, and it names what was
        // tapped.
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/server/counters/reset")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"group":"LATE_FRAMES"}"#))
            .expect("a request");
        let (status, _) = send(&app, request).await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            state.take_counter_reset(),
            Some(CounterGroup::LateFrames),
            "and it is on its way"
        );
        assert_eq!(state.take_counter_reset(), None, "once, not for ever");

        // A client that says nothing means all of them.
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/server/counters/reset")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .expect("a request");
        send(&app, request).await;

        assert_eq!(state.take_counter_reset(), Some(CounterGroup::All));
    }

    #[tokio::test]
    async fn clearing_the_counters_needs_a_token() {
        let (app, state) = app();
        let _ = signed_in(&app).await;

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/server/counters/reset")
            .body(Body::empty())
            .expect("a request");
        let (status, _) = send(&app, request).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            state.take_counter_reset(),
            None,
            "and nothing was asked for"
        );
    }

    #[tokio::test]
    async fn the_test_pattern_needs_a_token() {
        // It puts a grid on a wall an audience may be looking at.
        let (app, state) = app();
        let _ = signed_in(&app).await;

        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/server/test-pattern")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"mode":"OUTPUTS"}"#))
            .expect("a request");
        let (status, _) = send(&app, request).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            state.test_pattern(),
            TestPatternMode::Off,
            "and nothing changed"
        );
    }

    #[tokio::test]
    async fn a_test_pattern_is_never_restored_after_a_restart() {
        // Unlike the preview setting, which is. A server that came back from a
        // reboot with a grid on the wall would be a dark show that looks like a
        // working one.
        let (server, state) = app();
        let token = signed_in(&server).await;

        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/server/test-pattern")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"mode":"COMPOSITION"}"#))
            .expect("a request");
        send(&server, request).await;
        assert_eq!(state.test_pattern(), TestPatternMode::Composition);

        // What a restart looks like: a new server over a fresh database, since
        // nothing about the pattern was ever written to one.
        let (_, restarted) = app();
        restarted.load_previews_enabled();

        assert_eq!(restarted.test_pattern(), TestPatternMode::Off);
    }

    #[tokio::test]
    async fn previews_can_be_switched_off_and_stay_off() {
        // A preview costs a readback and an encode. On a machine at its limit
        // an operator needs to be able to take that back — and to find it still
        // taken back in the morning.
        let (app, state) = app();
        let token = signed_in(&app).await;

        assert!(
            state.previews_enabled(),
            "on unless somebody says otherwise"
        );

        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/preview/enabled")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"enabled":false}"#))
            .expect("a request");
        let (status, _) = send(&app, request).await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!state.previews_enabled());
        assert!(
            !state
                .with_database(|database| database.previews_enabled())
                .expect("reads"),
            "and it is written down"
        );
    }

    #[tokio::test]
    async fn switching_previews_needs_a_token() {
        let (app, state) = app();
        let _ = signed_in(&app).await;

        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/preview/enabled")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"enabled":false}"#))
            .expect("a request");
        let (status, _) = send(&app, request).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(state.previews_enabled(), "and nothing changed");
    }

    #[tokio::test]
    async fn the_composition_preview_is_served_once_the_renderer_has_sent_one() {
        // What the show is, rather than what one wall has. Until a renderer
        // says, the honest answer is that there is no picture — not a black
        // one, which would look like a show that is running and dark.
        let (app, state) = app();
        let token = signed_in(&app).await;

        let (status, _) = send(&app, get_auth("/api/v1/preview/composition", &token)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "nothing has rendered yet");

        state.set_composition_preview(Some(b"not really a png".to_vec()));
        let (status, _) = send(&app, get_auth("/api/v1/preview/composition", &token)).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn the_composition_preview_needs_a_token() {
        // A preview is a picture of what is on stage. Anybody who can see the
        // port must not be able to watch the show.
        let (app, state) = app();
        let _ = signed_in(&app).await;
        state.set_composition_preview(Some(b"not really a png".to_vec()));

        let (status, _) = send(&app, get("/api/v1/preview/composition")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_forged_token_is_refused() {
        let (app, _) = app();
        let _ = signed_in(&app).await;
        let (status, _) = send(&app, get_auth("/api/v1/shows", &"f".repeat(64))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn signing_out_makes_the_token_stop_working() {
        let (app, _) = app();
        let token = signed_in(&app).await;

        assert_eq!(
            send(&app, get_auth("/api/v1/shows", &token)).await.0,
            StatusCode::OK
        );

        let (status, _) = send(
            &app,
            post_json_auth("/api/v1/auth/logout", &token, &serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert_eq!(
            send(&app, get_auth("/api/v1/shows", &token)).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_show_can_be_created_listed_saved_and_opened() {
        let (app, state) = app();
        let token = signed_in(&app).await;

        let (status, body) = send(
            &app,
            post_json_auth(
                "/api/v1/shows",
                &token,
                &CreateShowRequest {
                    name: "Summer Tour".to_owned(),
                },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let created: Show = decode(body);
        assert_eq!(created.name, "Summer Tour");

        let (status, body) = send(&app, get_auth("/api/v1/shows", &token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_array().expect("a list").len(), 1);
        assert_eq!(body[0]["loadable"], true);

        let (status, _) = send(
            &app,
            post_json_auth(
                &format!("/api/v1/shows/{}/open", created.id),
                &token,
                &serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(state.authoritative().show_id(), Some(created.id));
    }

    #[tokio::test]
    async fn saving_an_invalid_show_reports_every_problem() {
        let (app, _) = app();
        let token = signed_in(&app).await;

        let (_, body) = send(
            &app,
            post_json_auth(
                "/api/v1/shows",
                &token,
                &CreateShowRequest {
                    name: "Show".to_owned(),
                },
            ),
        )
        .await;
        let mut show: Show = decode(body);

        // Loaded first, because a show that is not cannot be edited at all
        // (owner, 2026-09-24). Without this the save is refused for being the
        // wrong show rather than for being invalid, and this test would be
        // measuring the rule above it instead of the validation below it.
        let open = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/shows/{}/open", show.id))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .expect("builds");
        let (status, _) = send(&app, open).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Two problems at once: an empty name and an out-of-range BPM.
        show.name = "   ".to_owned();
        show.master.default_bpm = 5.0;

        let request = Request::builder()
            .method("PUT")
            .uri(format!("/api/v1/shows/{}", show.id))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(
                serde_json::to_vec(&ovs_proto::http::SaveShowRequest { show: show.clone() })
                    .expect("serialises"),
            ))
            .expect("builds");

        let (status, body) = send(&app, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "invalid");
        assert_eq!(
            body["details"].as_array().expect("details").len(),
            2,
            "an operator should see every problem at once"
        );
    }

    #[tokio::test]
    async fn opening_a_show_that_does_not_exist_says_so() {
        let (app, _) = app();
        let token = signed_in(&app).await;

        let (status, body) = send(
            &app,
            post_json_auth(
                &format!("/api/v1/shows/{}/open", ovs_types::id::ShowId::new()),
                &token,
                &serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "not_found");
    }

    #[tokio::test]
    async fn a_malformed_show_id_is_a_bad_request_not_a_server_error() {
        let (app, _) = app();
        let token = signed_in(&app).await;
        let (status, _) = send(&app, get_auth("/api/v1/shows/not-a-ulid", &token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn deleting_media_reports_which_shows_use_it_first() {
        let (app, _) = app();
        let token = signed_in(&app).await;
        let id = ovs_types::id::MediaItemId::new();

        let (status, body) =
            send(&app, get_auth(&format!("/api/v1/media/{id}/usage"), &token)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["used_by_shows"].as_array().expect("a list").is_empty());
    }

    #[tokio::test]
    async fn the_status_endpoint_reports_real_telemetry_not_a_score() {
        let (app, _) = app();
        let token = signed_in(&app).await;

        let (status, body) = send(&app, get_auth("/api/v1/server/status", &token)).await;
        assert_eq!(status, StatusCode::OK);

        let telemetry = &body["telemetry"];
        for field in ["cpu_percent", "ram_percent", "disk_used_bytes", "uptime_s"] {
            assert!(
                telemetry.get(field).is_some(),
                "telemetry must report {field}"
            );
        }
        assert!(
            telemetry.get("health_score").is_none(),
            "no invented aggregate score"
        );
        assert_eq!(body["startup_policy"]["kind"], "last_active");
    }
}
