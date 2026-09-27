//! What "on the machine itself" means, and what it is allowed to skip.
//!
//! The Service Display is a touchscreen wired to this server, in the same rack,
//! showing what the machine is doing and offering to stop it. There is nobody
//! to type a password at it and nowhere to keep one, and a screen that asked
//! for credentials before it would show a temperature would be a screen an
//! engineer walks past.
//!
//! So a request that arrives over the loopback interface is treated as coming
//! from the machine itself and needs no session. Everything that arrives over a
//! network still does.
//!
//! # Why a header, and why it is removed first
//!
//! Handlers already receive the headers, so marking the request there is the
//! one change that does not touch twenty-five signatures. The risk with a
//! trusted header is obvious — a client could send it — so this layer
//! **removes it from every request before deciding**. What a handler reads can
//! therefore only have been put there here, by a socket whose peer really was
//! loopback.
//!
//! Nothing is taken from `Forwarded` or `X-Forwarded-For`: those are claims
//! made by whatever is in front, and there is nothing in front of this.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

/// The header that says a request came from this machine.
pub const LOCAL_HEADER: &str = "x-ovs-local";

/// Environment variable that turns the exemption off.
pub const TRUST_ENV: &str = "OVS_TRUST_LOOPBACK";

/// Whether loopback requests are trusted on this server.
///
/// On by default: the local console is the reason this exists, and a server
/// whose own screen needs a password is a server with a screen nobody uses.
/// `OVS_TRUST_LOOPBACK=0` turns it off for a deployment that would rather have
/// the console sign in like anything else.
#[must_use]
pub fn trusts_loopback() -> bool {
    match std::env::var(TRUST_ENV) {
        Ok(value) => !matches!(value.trim(), "0" | "off" | "false" | "no"),
        Err(_) => true,
    }
}

/// Marks requests that arrived from this machine, and unmarks every other one.
///
/// The peer is read from the request's extensions rather than extracted, so a
/// request that carries none is simply not local. That is the right answer for
/// the tests that call the router directly — a test about authentication keeps
/// testing authentication — and for anything else that could reach a handler
/// without having come off a socket.
pub async fn mark(mut request: Request, next: Next) -> Response {
    // First, and unconditionally: whatever a client sent under this name is
    // gone. Everything below is this server's own judgement.
    request.headers_mut().remove(LOCAL_HEADER);

    let local = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(address)| address.ip().is_loopback());
    if local && trusts_loopback() {
        request
            .headers_mut()
            .insert(LOCAL_HEADER, HeaderValue::from_static("1"));
    }

    next.run(request).await
}

/// Whether this request came from the machine the server is running on.
#[must_use]
pub fn is_local(headers: &HeaderMap) -> bool {
    headers.contains_key(LOCAL_HEADER)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    use super::*;

    /// A router that answers with whether it considered the request local.
    fn router() -> Router {
        Router::new()
            .route(
                "/",
                get(|headers: HeaderMap| async move {
                    if is_local(&headers) {
                        "local"
                    } else {
                        "remote"
                    }
                }),
            )
            .layer(axum::middleware::from_fn(mark))
    }

    async fn answer(request: HttpRequest<Body>) -> String {
        let response = router().oneshot(request).await.expect("responds");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64)
            .await
            .expect("reads");
        String::from_utf8(body.to_vec()).expect("utf-8")
    }

    #[tokio::test]
    async fn a_request_that_claims_to_be_local_is_not_believed() {
        // The whole reason this is safe. A header a client can set is a header
        // this server must throw away before it looks at anything.
        let request = HttpRequest::builder()
            .uri("/")
            .header(LOCAL_HEADER, "1")
            .body(Body::empty())
            .expect("builds");

        assert_eq!(answer(request).await, "remote");
    }

    #[tokio::test]
    async fn a_request_from_the_loopback_interface_is_local() {
        let mut request = HttpRequest::builder()
            .uri("/")
            .body(Body::empty())
            .expect("builds");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 51234))));

        assert_eq!(answer(request).await, "local");
    }

    #[tokio::test]
    async fn a_request_from_the_network_is_not() {
        let mut request = HttpRequest::builder()
            .uri("/")
            .body(Body::empty())
            .expect("builds");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 12], 51234))));

        assert_eq!(answer(request).await, "remote");
    }

    #[tokio::test]
    async fn ipv6_loopback_counts_too() {
        // `localhost` resolves to ::1 first on most machines, so a console
        // pointed at it would otherwise be refused for the wrong reason.
        let mut request = HttpRequest::builder()
            .uri("/")
            .body(Body::empty())
            .expect("builds");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((
                std::net::Ipv6Addr::LOCALHOST,
                51234,
            ))));

        assert_eq!(answer(request).await, "local");
    }
}
