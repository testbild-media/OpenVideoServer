//! Sign-in, sign-out, first-run setup and the accounts themselves.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use ovs_db::Account;
use ovs_proto::http::{
    AccountSummary, CreateAccountRequest, LoginRequest, LoginResponse, SetPasswordRequest,
    UpdateAccountRequest, WhoAmIResponse,
};
use ovs_types::account::Role;
use ovs_types::id::AccountId;
use ovs_types::time::Timestamp;

use crate::auth;
use crate::server::app_state::AppState;
use crate::server::error::{ApiFailure, ApiResult};

/// Creates the first account, once, on a server that has none.
///
/// **An administrator, whatever the request says.** The first account is the
/// only thing that can create the second, so a server whose only account cannot
/// administer it is a server nobody can finish setting up — and the way back
/// would be a database editor.
pub async fn setup(
    State(state): State<AppState>,
    Json(request): Json<CreateAccountRequest>,
) -> ApiResult<StatusCode> {
    // Checked before hashing, so a second setup attempt cannot even spend the
    // time to compute a hash for credentials it will refuse.
    if state.with_database(|database| database.account_count())? > 0 {
        return Err(ApiFailure::conflict(
            "already_configured",
            "this server already has an account",
        ));
    }

    create(&state, &request, Role::Admin)?;
    Ok(StatusCode::CREATED)
}

/// Creates an account on a server that already has one.
///
/// An administrator's endpoint. The role defaults to the safe half: an account
/// created without anybody saying is an account nobody decided to trust with
/// the machine.
pub async fn create_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateAccountRequest>,
) -> ApiResult<Json<AccountSummary>> {
    administer(&state, &headers)?;
    let id = create(&state, &request, request.role.unwrap_or_default())?;
    summarise(&state, id).map(Json)
}

/// The shared half of both ways an account is created.
fn create(state: &AppState, request: &CreateAccountRequest, role: Role) -> ApiResult<AccountId> {
    let username = request.username.trim();
    if username.is_empty() {
        return Err(ApiFailure::bad_request(
            "invalid_username",
            "a username is required",
        ));
    }
    if state
        .with_database(|database| database.account_by_username(username))?
        .is_some()
    {
        return Err(ApiFailure::conflict(
            "username_taken",
            "an account with that name already exists",
        ));
    }

    let password_hash = auth::hash_password(&request.password)?;
    let id = AccountId::new();
    state.with_database(|database| {
        database.upsert_account(&Account {
            id,
            username: username.to_owned(),
            password_hash,
            role,
            created_at: Timestamp::now(),
            last_login_at: None,
        })
    })?;
    Ok(id)
}

/// Every account on this server.
pub async fn list_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<AccountSummary>>> {
    administer(&state, &headers)?;
    let accounts = state.with_database(|database| {
        let accounts = database.accounts()?;
        accounts
            .into_iter()
            .map(|account| {
                let last = database.is_last_admin(account.id)?;
                Ok(summary_of(&account, last))
            })
            .collect::<Result<Vec<_>, ovs_db::DbError>>()
    })?;
    Ok(Json(accounts))
}

/// Who this request is acting as.
///
/// Every signed-in caller may ask, because the answer is about themselves and
/// an interface has to know what to draw before it draws it.
pub async fn who_am_i(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<WhoAmIResponse>> {
    let id = authenticate(&state, &headers)?;

    // **A request from the machine itself has no row to be summarised from.**
    // Its identity is minted per process so that endpoints can name a subject
    // without one existing, and looking it up finds nothing —
    // which used to come back as "no such account" and made the administration
    // page unusable on the Service Display, the one screen it was built for.
    //
    // It answers as what it is: this machine, which administers, because that
    // is the rule `administer` already applies to it.
    if crate::server::is_local(&headers) && state.with_database(|db| db.account(id))?.is_none() {
        return Ok(Json(WhoAmIResponse {
            account: AccountSummary {
                account_id: id,
                username: "this machine".to_owned(),
                role: Role::Admin,
                created_at: 0,
                last_login_at: None,
                last_administrator: false,
            },
        }));
    }

    Ok(Json(WhoAmIResponse {
        account: summarise(&state, id)?,
    }))
}

/// Changes an account's name or role.
pub async fn update_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(request): Json<UpdateAccountRequest>,
) -> ApiResult<Json<AccountSummary>> {
    administer(&state, &headers)?;
    let id = account_id_of(&account_id)?;

    state.with_database(|database| {
        let mut account = database
            .account(id)?
            .ok_or_else(|| ovs_db::DbError::Conflict("no such account".to_owned()))?;
        if let Some(username) = &request.username {
            let username = username.trim();
            if username.is_empty() {
                return Err(ovs_db::DbError::Conflict(
                    "a username is required".to_owned(),
                ));
            }
            match database.account_by_username(username)? {
                Some(other) if other.id != id => {
                    return Err(ovs_db::DbError::Conflict(
                        "an account with that name already exists".to_owned(),
                    ))
                }
                _ => account.username = username.to_owned(),
            }
        }
        if let Some(role) = request.role {
            account.role = role;
        }
        // `upsert_account` is what refuses to leave the server without an
        // administrator. It is not repeated here: one door, one rule.
        database.upsert_account(&account)
    })?;

    summarise(&state, id).map(Json)
}

/// Sets an account's password.
///
/// Two ways in, and the difference is `current`. Changing one's own proves the
/// password in use; an administrator resetting somebody else's does not have it
/// and does not need it. Anything else is refused.
pub async fn set_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    Json(request): Json<SetPasswordRequest>,
) -> ApiResult<StatusCode> {
    let acting = authenticate(&state, &headers)?;
    let id = account_id_of(&account_id)?;
    let own = acting == id;

    if !own {
        administer(&state, &headers)?;
    }

    let account = state
        .with_database(|database| database.account(id))?
        .ok_or_else(|| ApiFailure::not_found("no such account"))?;

    if own {
        // **Their own, so they prove they know it.** Without this, a screen left
        // unlocked for a minute is an account taken for good.
        let current = request.current.as_deref().ok_or_else(|| {
            ApiFailure::bad_request(
                "current_password_required",
                "changing your own password needs the one in use",
            )
        })?;
        auth::verify_password(current, &account.password_hash)?;
    }

    let password_hash = auth::hash_password(&request.password)?;
    state.with_database(|database| {
        database.upsert_account(&Account {
            password_hash,
            ..account
        })
    })?;

    // Every session of that account goes. A password changed because it was
    // guessed, and a session that outlived the change, are the same accident.
    state.with_sessions(|sessions| sessions.sign_out_account(id));
    Ok(StatusCode::NO_CONTENT)
}

/// Removes an account.
pub async fn delete_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> ApiResult<StatusCode> {
    let acting = administer(&state, &headers)?;
    let id = account_id_of(&account_id)?;
    if acting == id {
        // Not a rule about authority — the last-admin rule already covers the
        // server — but about the accident: an administrator who deletes
        // themselves mid-show is signed out of a running machine.
        return Err(ApiFailure::conflict(
            "cannot_remove_self",
            "an account cannot remove itself",
        ));
    }

    let removed = state.with_database(|database| database.delete_account(id))?;
    if !removed {
        return Err(ApiFailure::not_found("no such account"));
    }
    state.with_sessions(|sessions| sessions.sign_out_account(id));
    Ok(StatusCode::NO_CONTENT)
}

/// Reads one account back as the API says it.
fn summarise(state: &AppState, id: AccountId) -> ApiResult<AccountSummary> {
    let (account, last) = state.with_database(|database| {
        let account = database
            .account(id)?
            .ok_or_else(|| ovs_db::DbError::Conflict("no such account".to_owned()))?;
        let last = database.is_last_admin(id)?;
        Ok::<_, ovs_db::DbError>((account, last))
    })?;
    Ok(summary_of(&account, last))
}

/// One stored account, without its hash.
fn summary_of(account: &Account, last_administrator: bool) -> AccountSummary {
    AccountSummary {
        account_id: account.id,
        username: account.username.clone(),
        role: account.role,
        created_at: account.created_at.as_millis(),
        last_login_at: account.last_login_at.map(Timestamp::as_millis),
        last_administrator,
    }
}

/// Parses an account identifier from a path.
fn account_id_of(value: &str) -> ApiResult<AccountId> {
    value
        .parse()
        .map_err(|_| ApiFailure::bad_request("invalid_account_id", "not an account identifier"))
}

/// Exchanges credentials for a session token.
///
/// The answer carries the token twice: in the body, which is what the desktop
/// client reads, and as a cookie, which is the only thing a browser can send
/// back on a page it navigates to. One session either way — the administration
/// pages and the client are the same sign-in, not two.
pub async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let account =
        state.with_database(|database| database.account_by_username(request.username.trim()))?;

    // An unknown username and a wrong password produce the same error, on
    // purpose: the difference would enumerate accounts.
    let account = account.ok_or(auth::AuthError::InvalidCredentials)?;
    auth::verify_password(&request.password, &account.password_hash)?;

    let now = Timestamp::now();
    let token = state.with_sessions(|sessions| sessions.sign_in(account.id, now))?;
    state.with_database(|database| database.record_login(account.id, now))?;

    let seconds = token.expires_at.as_millis().saturating_sub(now.as_millis()) / 1_000;
    let cookie = session_cookie(&token.token, seconds);

    Ok((
        [(axum::http::header::SET_COOKIE, cookie)],
        Json(LoginResponse {
            token: token.token,
            expires_at: token.expires_at,
            account_id: token.account_id,
        }),
    ))
}

/// Ends the current session.
///
/// Clears the cookie as well as the session. Leaving it would put a browser in
/// the state where it presents a credential the server has already forgotten,
/// which reads as a server that keeps signing somebody out.
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl axum::response::IntoResponse> {
    let token = bearer_token(&headers)?;
    state.with_sessions(|sessions| sessions.sign_out(&token));
    Ok((
        [(axum::http::header::SET_COOKIE, cleared_cookie())],
        StatusCode::NO_CONTENT,
    ))
}

/// The cookie a browser carries its session in.
///
/// A browser cannot put a bearer token on a request it makes by following a
/// link, so the administration pages need this. It is the **same session** a
/// client holds — one store, two ways of presenting it — rather than a second
/// kind of credential with its own lifetime and its own way of going wrong.
pub const SESSION_COOKIE: &str = "ovs_session";

/// Builds the `Set-Cookie` value for a session.
///
/// `HttpOnly`, so a script on the page cannot read it; `SameSite=Strict`, so a
/// request started by another site does not carry it, which is what stops a
/// link in an email from acting as the signed-in administrator.
///
/// **No `Secure`.** The owner chose plain HTTP for this server (2026-09-24):
/// the flag would stop the cookie being sent at all, and the honest statement
/// is the one in the sign-in page — on this network a password travels in
/// clear, so the network is the boundary.
#[must_use]
pub fn session_cookie(token: &str, seconds: i64) -> String {
    format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={seconds}")
}

/// The `Set-Cookie` value that removes the session cookie.
#[must_use]
pub fn cleared_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// Extracts the session token from a request.
///
/// An `Authorization: Bearer` header, which is what the desktop client sends,
/// or the session cookie, which is what a browser can. The header wins when
/// both are there: a client that troubled itself to send one meant it.
///
/// # Errors
///
/// Returns an unauthorised failure when neither is present or usable.
pub fn bearer_token(headers: &HeaderMap) -> ApiResult<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .or_else(|| cookie_token(headers))
        .ok_or_else(|| ApiFailure::unauthorised("a session is required"))
}

/// Reads the session cookie out of a `Cookie` header.
///
/// Parsed rather than matched: a browser sends every cookie for the host in one
/// header, so ours is one of several and never reliably the first.
fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())?
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| name.trim() == SESSION_COOKIE)
        .map(|(_, token)| token.trim().to_owned())
        .filter(|token| !token.is_empty())
}

/// Resolves the account a request is acting as.
///
/// Every protected handler starts with this. V0.x has one account, but the
/// identity is resolved rather than assumed, so adding accounts later does not
/// mean revisiting every endpoint.
///
/// A request from the machine itself needs no session: it is the touchscreen in
/// the rack, or something else already running on the server, and both are
/// inside the boundary a password is there to defend. Whether a
/// request really is local is decided in [`crate::server::local`], which
/// removes the claim from anything that arrived over a network before it looks.
///
/// # Errors
///
/// Returns an unauthorised failure for a missing, unknown or expired token.
pub fn authenticate(state: &AppState, headers: &HeaderMap) -> ApiResult<AccountId> {
    if crate::server::is_local(headers) {
        return Ok(state.local_account());
    }

    let token = bearer_token(headers)?;
    let account = state.with_sessions(|sessions| sessions.account_for(&token, Timestamp::now()))?;
    Ok(account)
}

/// Resolves the account a request is acting as, and refuses it unless that
/// account may administer the machine.
///
/// **The one gate.** Every endpoint that touches accounts or system settings
/// starts here, and none of them compares a role itself: a handler asking
/// `role == Admin` directly would be a second answer to go out of step with
/// [`Role::may_administer`] the day a third role exists.
///
/// A request from the machine itself administers. It is the touchscreen in the
/// rack, which is inside the boundary a password defends — and
/// a server whose Service Display could not change a setting would be a server
/// nobody standing at it can fix.
///
/// # Errors
///
/// Unauthorised for a missing or expired session; forbidden for an account that
/// may not administer.
pub fn administer(state: &AppState, headers: &HeaderMap) -> ApiResult<AccountId> {
    let id = authenticate(state, headers)?;
    if crate::server::is_local(headers) {
        return Ok(id);
    }

    let role = state
        .with_database(|database| database.account(id))?
        .map(|account| account.role)
        .ok_or_else(|| ApiFailure::unauthorised("this session has no account"))?;

    if role.may_administer() {
        Ok(id)
    } else {
        Err(ApiFailure::forbidden(
            "this account may drive the show but not change the machine",
        ))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(value).expect("valid header"),
        );
        headers
    }

    #[test]
    fn a_bearer_token_is_extracted() {
        assert_eq!(
            bearer_token(&headers("Bearer abc123")).expect("extracted"),
            "abc123"
        );
    }

    /// A request carrying cookies, the way a browser sends them.
    fn cookies(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_str(value).expect("valid header"),
        );
        headers
    }

    #[test]
    fn a_browser_presents_its_session_in_a_cookie() {
        assert_eq!(
            bearer_token(&cookies("ovs_session=abc123")).expect("extracted"),
            "abc123"
        );
        // One of several, which is how a browser sends them, and never reliably
        // first. Matching a prefix would read the wrong one.
        assert_eq!(
            bearer_token(&cookies("theme=dark; ovs_session=abc123; lang=de")).expect("extracted"),
            "abc123"
        );
        assert_eq!(
            bearer_token(&cookies("not_ovs_session=wrong; ovs_session=right")).expect("extracted"),
            "right"
        );
        assert!(bearer_token(&cookies("ovs_session=")).is_err(), "empty");
        assert!(bearer_token(&cookies("something=else")).is_err(), "absent");
    }

    #[test]
    fn a_header_beats_a_cookie() {
        // A client that troubled itself to send one meant it.
        let mut headers = headers("Bearer from-the-header");
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_static("ovs_session=from-the-cookie"),
        );
        assert_eq!(
            bearer_token(&headers).expect("extracted"),
            "from-the-header"
        );
    }

    #[test]
    fn the_cookie_cannot_be_read_by_a_script_or_carried_from_another_site() {
        // Two flags and what each one is for. Without `HttpOnly` a script that
        // got onto the page could take the session; without `SameSite=Strict` a
        // link in an email could act as the signed-in administrator.
        let cookie = session_cookie("abc123", 3_600);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.starts_with("ovs_session=abc123;"));

        // And clearing it is the same cookie with no life left.
        assert!(cleared_cookie().contains("Max-Age=0"));
    }

    #[test]
    fn a_missing_or_malformed_header_is_refused() {
        assert!(bearer_token(&HeaderMap::new()).is_err());
        assert!(bearer_token(&headers("abc123")).is_err(), "no scheme");
        assert!(
            bearer_token(&headers("Basic abc123")).is_err(),
            "wrong scheme"
        );
        assert!(bearer_token(&headers("Bearer ")).is_err(), "empty token");
        assert!(
            bearer_token(&headers("bearer abc123")).is_err(),
            "scheme is case-sensitive here"
        );
    }
}
