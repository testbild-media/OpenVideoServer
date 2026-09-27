//! The realtime WebSocket endpoint.
//!
//! The order here is the protocol's central promise: handshake, then a snapshot,
//! then updates. A client never receives an update it has no snapshot for, so
//! the gap detection on the other side means something.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use ovs_proto::realtime::{negotiate, ClientHello, ClientMessage, ServerHello, ServerMessage};
use ovs_types::time::Timestamp;
use tokio::sync::broadcast::error::RecvError;

use crate::server::app_state::AppState;

/// Upgrades an HTTP request to the realtime socket.
///
/// Authentication happens after the upgrade, in the handshake, where the client
/// presents the session token it got from the HTTP API. A browser cannot set
/// headers on a WebSocket handshake, and a token in the query string would be
/// written into every access log and proxy cache along the way.
/// A socket opened from the machine itself needs no token, for the same reason
/// the HTTP API does not. Decided here, before the upgrade,
/// because the handshake that follows has no headers of its own.
pub async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let local = crate::server::is_local(&headers);
    upgrade.on_upgrade(move |socket| serve(socket, state, local))
}

async fn serve(mut socket: WebSocket, state: AppState, local: bool) {
    let Some(hello) = read_hello(&mut socket).await else {
        return;
    };

    // Protocol version first, then the session. They are different failures:
    // one means "update the client", the other "sign in again", and a client
    // that cannot tell them apart cannot tell the operator either.
    match negotiate(&hello) {
        accepted @ ServerHello::Accepted { .. } => {
            if !local && !is_authenticated(&state, &hello.token) {
                reject(&mut socket, reject_unauthenticated()).await;
                return;
            }
            if !send_json(&mut socket, &accepted).await {
                return;
            }
        }
        refused => {
            reject(&mut socket, refused).await;
            return;
        }
    }

    // Subscribe before sending the snapshot. The other order has a window in
    // which a change lands after the snapshot is built but before the
    // subscription exists, and that update would be lost without leaving a gap
    // for the client to notice.
    let mut updates = state.subscribe();

    let snapshot = state.authoritative().snapshot();
    if !send_json(&mut socket, &ServerMessage::Snapshot(Box::new(snapshot))).await {
        return;
    }

    // What is on the network, once, after the snapshot. The list is only
    // broadcast when it changes, so a client that connects between two changes
    // would otherwise show an empty input browser until somebody switched a
    // sender on or off — which on a rig that is already set up is never.
    //
    // After the snapshot rather than before, because the order of this
    // protocol is its central promise and this is not part of the show state.
    if !send_json(
        &mut socket,
        &ServerMessage::NdiSources {
            sources: state.ndi_sources(),
        },
    )
    .await
    {
        return;
    }

    // And what this machine is wired to, for the same reason: it is only sent
    // when it moves, so a client that connects between two changes would
    // otherwise have nothing to fill a chooser with.
    if !send_json(
        &mut socket,
        &ServerMessage::Connectors {
            connectors: state.connector_infos(),
        },
    )
    .await
    {
        return;
    }

    loop {
        tokio::select! {
            update = updates.recv() => match update {
                Ok(message) => {
                    if !send_json(&mut socket, &message).await {
                        return;
                    }
                }
                Err(RecvError::Lagged(missed)) => {
                    // This client could not keep up. Rather than sending it
                    // updates that no longer follow on, tell it plainly; it will
                    // ask for a fresh snapshot.
                    tracing::warn!(missed, "a realtime client fell behind");
                    let fault = ServerMessage::Fault {
                        code: "lagged".to_owned(),
                        message: format!("missed {missed} updates; resynchronise"),
                    };
                    if !send_json(&mut socket, &fault).await {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if !handle_client_message(&mut socket, &state, &text).await {
                        return;
                    }
                }
                Some(Ok(Message::Close(_))) | None => return,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    tracing::debug!(%error, "realtime client disconnected");
                    return;
                }
            },
        }
    }
}

async fn read_hello(socket: &mut WebSocket) -> Option<ClientHello> {
    match socket.recv().await {
        Some(Ok(Message::Text(text))) => serde_json::from_str(&text).ok(),
        _ => None,
    }
}

/// Handles one client message. Returns false when the connection should close.
async fn handle_client_message(socket: &mut WebSocket, state: &AppState, text: &str) -> bool {
    let Ok(message) = serde_json::from_str::<ClientMessage>(text) else {
        // Anything the socket does not accept is almost always a command, and
        // saying "malformed" to a client that sent a perfectly good one would
        // send its author looking for a typo. The socket carries state
        // outwards; commands go over HTTP, where they carry a bearer token and
        // produce a status code.
        let fault = ServerMessage::Fault {
            code: "wrong_channel".to_owned(),
            message: "this socket accepts only \"resynchronise\" and \"ping\"; commands are sent over the HTTP API"
                .to_owned(),
        };
        return send_json(socket, &fault).await;
    };

    match message {
        ClientMessage::Ping { nonce } => send_json(socket, &ServerMessage::Pong { nonce }).await,
        ClientMessage::Resynchronise => {
            let snapshot = state.authoritative().snapshot();
            send_json(socket, &ServerMessage::Snapshot(Box::new(snapshot))).await
        }
    }
}

/// Sends a value as JSON. Returns false when the socket is gone.
async fn send_json<T: serde::Serialize>(socket: &mut WebSocket, value: &T) -> bool {
    let Ok(text) = serde_json::to_string(value) else {
        return false;
    };
    socket.send(Message::Text(text.into())).await.is_ok()
}

/// Sends a rejection and closes the socket cleanly.
///
/// A clean close rather than dropping the connection, so the client can show the
/// operator why it was refused instead of reporting a network error.
async fn reject(socket: &mut WebSocket, hello: ServerHello) {
    send_json(socket, &hello).await;
    let _ = socket.send(Message::Close(None)).await;
}

/// Rejects a connection that presented no valid session.
#[must_use]
fn reject_unauthenticated() -> ServerHello {
    ServerHello::Unauthenticated
}

/// Returns whether a token is currently valid.
#[must_use]
fn is_authenticated(state: &AppState, token: &str) -> bool {
    state
        .with_sessions(|sessions| sessions.account_for(token, Timestamp::now()))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use ovs_db::Database;
    use ovs_types::id::AccountId;

    use super::*;

    fn state() -> AppState {
        AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test")
    }

    #[test]
    fn an_unauthenticated_rejection_is_distinct_from_a_version_mismatch() {
        // The operator needs to know whether to sign in again or update.
        let unauthenticated = reject_unauthenticated();
        let outdated = negotiate(&ClientHello {
            protocol_version: 99,
            token: "t0ken".to_owned(),
            client_name: "Old".to_owned(),
            client_version: "0.0.1".to_owned(),
        });
        assert_ne!(unauthenticated, outdated);
        assert!(matches!(unauthenticated, ServerHello::Unauthenticated));
        assert!(matches!(outdated, ServerHello::IncompatibleProtocol { .. }));
    }

    #[test]
    fn only_a_live_token_authenticates() {
        let state = state();
        let token = state
            .with_sessions(|sessions| sessions.sign_in(AccountId::new(), Timestamp::now()))
            .expect("issues");

        assert!(is_authenticated(&state, &token.token));
        assert!(!is_authenticated(&state, "not a token"));

        state.with_sessions(|sessions| sessions.sign_out(&token.token));
        assert!(!is_authenticated(&state, &token.token));
    }

    #[test]
    fn a_current_client_negotiates_successfully() {
        let hello = ClientHello::current("t0ken", "OVS Desktop", "0.0.1");
        assert!(matches!(negotiate(&hello), ServerHello::Accepted { .. }));
    }
}
