//! Who may do what, over the API a web page and a client both meet.
//!
//! The owner asked for this on 2026-09-24: "Mir fehlt noch für den Login eine
//! Benutzerverwaltung. Die sollte aber auf dem Server passieren und nicht im
//! Clienten", with admin and operator kept apart.
//!
//! Q7 settled V0.x on one operator/admin account and said in the same breath
//! that the model must not make more accounts impossible later. What is covered
//! here is the part that is easy to get subtly wrong and catastrophic to get
//! wrong at all: that a server can never be left with nobody able to administer
//! it, and that an operator cannot quietly become one.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use ovs_proto::http::{
    AccountSummary, CreateAccountRequest, LoginRequest, LoginResponse, SetPasswordRequest,
    UpdateAccountRequest, WhoAmIResponse,
};
use ovs_types::account::Role;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    app: axum::Router,
    /// The first account, which is always an administrator.
    admin: String,
}

impl Harness {
    /// A server set up with one account, signed in.
    async fn start() -> Self {
        let state = AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test");
        let app = router(state);

        let (status, _) = send(
            &app,
            json(
                "POST",
                "/api/v1/auth/setup",
                None,
                &CreateAccountRequest {
                    username: "chief".to_owned(),
                    password: PASSWORD.to_owned(),
                    role: None,
                },
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let admin = sign_in(&app, "chief", PASSWORD).await;
        Self { app, admin }
    }

    async fn accounts(&self, token: &str) -> (StatusCode, Vec<AccountSummary>) {
        let (status, body) = send(&self.app, json_get("/api/v1/accounts", token)).await;
        let accounts = serde_json::from_value(body).unwrap_or_default();
        (status, accounts)
    }
}

#[tokio::test]
async fn the_first_account_administers_whatever_the_request_asked_for() {
    // It is the only thing that can create the second, so a server whose only
    // account cannot administer it is a server nobody can finish setting up.
    let state = AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test");
    let app = router(state);

    let (status, _) = send(
        &app,
        json(
            "POST",
            "/api/v1/auth/setup",
            None,
            &CreateAccountRequest {
                username: "chief".to_owned(),
                password: PASSWORD.to_owned(),
                // Asking for the lesser role, which setup must not honour.
                role: Some(Role::Operator),
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let token = sign_in(&app, "chief", PASSWORD).await;
    let (status, body) = send(&app, json_get("/api/v1/auth/me", &token)).await;
    assert_eq!(status, StatusCode::OK);
    let me: WhoAmIResponse = serde_json::from_value(body).expect("a who-am-i response");
    assert_eq!(me.account.role, Role::Admin);
    assert!(me.account.last_administrator);

    // And setup is a one-time door.
    let (status, _) = send(
        &app,
        json(
            "POST",
            "/api/v1/auth/setup",
            None,
            &CreateAccountRequest {
                username: "someone else".to_owned(),
                password: PASSWORD.to_owned(),
                role: None,
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn an_account_created_without_a_role_said_is_the_safe_half() {
    let harness = Harness::start().await;

    let (status, body) = send(
        &harness.app,
        json(
            "POST",
            "/api/v1/accounts",
            Some(&harness.admin),
            &CreateAccountRequest {
                username: "guest".to_owned(),
                password: PASSWORD.to_owned(),
                role: None,
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let created: AccountSummary = serde_json::from_value(body).expect("a summary");
    assert_eq!(created.role, Role::Operator);
    assert!(!created.last_administrator);

    // A name is taken once.
    let (status, _) = send(
        &harness.app,
        json(
            "POST",
            "/api/v1/accounts",
            Some(&harness.admin),
            &CreateAccountRequest {
                username: "  guest  ".to_owned(),
                password: PASSWORD.to_owned(),
                role: None,
            },
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "names are trimmed, then taken"
    );
}

#[tokio::test]
async fn an_operator_drives_the_show_and_cannot_change_the_machine() {
    let harness = Harness::start().await;
    let guest = make(&harness, "guest", Role::Operator).await;
    let token = sign_in(&harness.app, "guest", PASSWORD).await;

    // The accounts are not theirs to see or to add to.
    let (status, _) = harness.accounts(&token).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = send(
        &harness.app,
        json(
            "POST",
            "/api/v1/accounts",
            Some(&token),
            &CreateAccountRequest {
                username: "another".to_owned(),
                password: PASSWORD.to_owned(),
                role: Some(Role::Admin),
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Nor is the machine: turning it off is the clearest of them.
    let (status, _) = send(
        &harness.app,
        json(
            "POST",
            "/api/v1/server/power",
            Some(&token),
            &serde_json::json!({ "action": "shutdown" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // **Forbidden and not unauthorised.** Signing in again is the answer to one
    // of these and not to the other, and a client that could not tell them
    // apart would send an operator round the sign-in screen for ever.
    let (status, _) = send(&harness.app, json_get("/api/v1/accounts", "nonsense")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // What they may do: read the show, and know who they are.
    let (status, body) = send(&harness.app, json_get("/api/v1/auth/me", &token)).await;
    assert_eq!(status, StatusCode::OK);
    let me: WhoAmIResponse = serde_json::from_value(body).expect("a who-am-i response");
    assert_eq!(me.account.account_id, guest);
    assert_eq!(me.account.role, Role::Operator);

    let (status, _) = send(&harness.app, json_get("/api/v1/server/status", &token)).await;
    assert_eq!(status, StatusCode::OK, "an operator can watch the machine");
}

#[tokio::test]
async fn a_server_can_never_be_left_with_nobody_able_to_administer_it() {
    // The rule the whole model hangs on. A machine whose last admin has been
    // demoted or deleted cannot create an account, change a password or point
    // the Service Display at anything — in a rack, during a show.
    let harness = Harness::start().await;
    let (_, accounts) = harness.accounts(&harness.admin).await;
    let me = accounts[0].account_id;
    assert!(accounts[0].last_administrator);

    let (status, _) = send(
        &harness.app,
        json(
            "PUT",
            &format!("/api/v1/accounts/{me}"),
            Some(&harness.admin),
            &UpdateAccountRequest {
                role: Some(Role::Operator),
                ..UpdateAccountRequest::default()
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "the last admin was demoted");

    // Deleting oneself is refused before the rule even applies: an admin who
    // does it mid-show is signed out of a running machine.
    let (status, _) = send(
        &harness.app,
        delete(&format!("/api/v1/accounts/{me}"), &harness.admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // With a second administrator, the first may step down.
    let second = make(&harness, "stefan", Role::Admin).await;
    let (status, _) = send(
        &harness.app,
        json(
            "PUT",
            &format!("/api/v1/accounts/{me}"),
            Some(&harness.admin),
            &UpdateAccountRequest {
                role: Some(Role::Operator),
                ..UpdateAccountRequest::default()
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "now there is somebody else");

    // And the one that is left cannot be removed in turn.
    let other = sign_in(&harness.app, "stefan", PASSWORD).await;
    let (status, _) = send(
        &harness.app,
        delete(&format!("/api/v1/accounts/{second}"), &other),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn changing_your_own_password_needs_the_one_in_use_and_an_admin_reset_does_not() {
    let harness = Harness::start().await;
    make(&harness, "guest", Role::Operator).await;
    let guest_id = {
        let (_, accounts) = harness.accounts(&harness.admin).await;
        accounts
            .iter()
            .find(|each| each.username == "guest")
            .expect("there")
            .account_id
    };
    let guest = sign_in(&harness.app, "guest", PASSWORD).await;

    // **Their own, so they prove they know it.** Without this, a screen left
    // unlocked for a minute is an account taken for good.
    let (status, _) = send(
        &harness.app,
        json(
            "PUT",
            &format!("/api/v1/accounts/{guest_id}/password"),
            Some(&guest),
            &SetPasswordRequest {
                current: None,
                password: "a new one entirely".to_owned(),
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = send(
        &harness.app,
        json(
            "PUT",
            &format!("/api/v1/accounts/{guest_id}/password"),
            Some(&guest),
            &SetPasswordRequest {
                current: Some("not it".to_owned()),
                password: "a new one entirely".to_owned(),
            },
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the wrong current password"
    );

    let (status, _) = send(
        &harness.app,
        json(
            "PUT",
            &format!("/api/v1/accounts/{guest_id}/password"),
            Some(&guest),
            &SetPasswordRequest {
                current: Some(PASSWORD.to_owned()),
                password: "a new one entirely".to_owned(),
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The session goes with the password. One that outlived the change, and a
    // password changed because it was guessed, are the same accident.
    let (status, _) = send(&harness.app, json_get("/api/v1/auth/me", &guest)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The new one works, which is the other half of a password change. `sign_in`
    // asserts the status itself, so calling it is the check.
    sign_in(&harness.app, "guest", "a new one entirely").await;

    // An administrator resetting somebody else's does not have the old one and
    // does not need it.
    let (status, _) = send(
        &harness.app,
        json(
            "PUT",
            &format!("/api/v1/accounts/{guest_id}/password"),
            Some(&harness.admin),
            &SetPasswordRequest {
                current: None,
                password: "reset by an admin".to_owned(),
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Their sessions went with that reset too, so this is the token they have
    // now. Reusing the old one would test the session rule a second time and
    // never reach the rule below it.
    let guest = sign_in(&harness.app, "guest", "reset by an admin").await;
    let others = other_than(&harness, guest_id).await;

    // An operator cannot reset anybody else's, which would be the whole point
    // of the split undone.
    let (status, _) = send(
        &harness.app,
        json(
            "PUT",
            &format!("/api/v1/accounts/{others}/password"),
            Some(&guest),
            &SetPasswordRequest {
                current: None,
                password: "taking the machine".to_owned(),
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// --- the plumbing -------------------------------------------------------------

/// Creates an account and returns its identifier.
async fn make(harness: &Harness, username: &str, role: Role) -> ovs_types::id::AccountId {
    let (status, body) = send(
        &harness.app,
        json(
            "POST",
            "/api/v1/accounts",
            Some(&harness.admin),
            &CreateAccountRequest {
                username: username.to_owned(),
                password: PASSWORD.to_owned(),
                role: Some(role),
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_value::<AccountSummary>(body)
        .expect("a summary")
        .account_id
}

/// Some account that is not the one given.
async fn other_than(harness: &Harness, id: ovs_types::id::AccountId) -> ovs_types::id::AccountId {
    let (_, accounts) = harness.accounts(&harness.admin).await;
    accounts
        .iter()
        .find(|each| each.account_id != id)
        .expect("there is another")
        .account_id
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

fn json<T: serde::Serialize>(
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &T,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    request
        .body(Body::from(serde_json::to_vec(body).expect("serialises")))
        .expect("builds")
}

fn json_get(path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("builds")
}

fn delete(path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("builds")
}

/// Signs in and returns the token.
async fn sign_in(app: &axum::Router, username: &str, password: &str) -> String {
    let (status, body) = send(
        app,
        json(
            "POST",
            "/api/v1/auth/login",
            None,
            &LoginRequest {
                username: username.to_owned(),
                password: password.to_owned(),
            },
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_value::<LoginResponse>(body)
        .expect("a login response")
        .token
}
