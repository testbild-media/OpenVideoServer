//! Handing an encoded surface to whoever is watching it.
//!
//! One socket per watcher per surface. The renderer encodes a surface once,
//! whoever is looking; this is the part that is per watcher, because a socket
//! is a socket.
//!
//! # Why a socket rather than a request
//!
//! A preview that answers "is it running" can be a picture a second over HTTP.
//! Cueing a show cannot: an operator watching a clip land on a beat needs what
//! is on the wall now, which means frames pushed as they are made rather than
//! asked for one at a time.
//!
//! # The frame on the wire
//!
//! Sixteen bytes of header and then the picture, as one binary message:
//!
//! ```text
//!   0       version, 1
//!   1       flags; bit 0 set on a frame a watcher can start from
//!   2..4    reserved, zero
//!   4..6    width in pixels, little endian
//!   6..8    height
//!   8..16   microseconds since this stream started, little endian
//!   16..    H.264, Annex-B
//! ```
//!
//! Every frame carries its size so a watcher needs nothing said first and
//! nothing remembered: a surface that changes mode mid-stream announces it in
//! the next frame. The payload is what a browser's `VideoDecoder` takes
//! directly — no container, no description, no library.
//!
//! # What the socket says in words
//!
//! Binary messages are pictures; text messages are about the stream itself, as
//! one JSON object each:
//!
//! ```text
//!   {"error": "why there will be no pictures"}
//!   {"encoding": {"encoder": "libx264", "hardware": false, "fps": 15.0}}
//! ```
//!
//! The second is the last clause of owner decision Q13. A software-encoded
//! preview is capped at fifteen frames a second so that watching a wall cannot
//! cost the show, and a picture arriving at a quarter speed with no explanation
//! is indistinguishable from a fault — so the watcher is told which encoder it
//! got and what rate it is really being given. It arrives a moment after the
//! socket opens, because the encoder is opened on its own thread, and again if
//! the surface is restarted onto a different encoder.
//!
//! A client that does not know a word here ignores it: everything on this
//! socket is additive, and a newer server saying more must not break an older
//! watcher.
//!
//! # Who may watch
//!
//! The same rule as everything else: a request from the machine itself needs no
//! session, one from the network does. A browser cannot put a
//! header on a socket and a token has no business in a URL, so a watcher from
//! the network proves itself in its first message and gets closed if it does
//! not.
//!
//! There is one way to prove it, and it is the narrow one. A **ticket** —
//! minted by `POST /preview/stream/{surface}/ticket`, which does need a session
//! — is good for one surface, for a minute, once.
//!
//! A session token used to work here as well, for a client written before
//! tickets existed. It no longer does (owner decision Q10, closed 2026-09-22).
//! Anything holding a session token can also load a show, move a fader and
//! stop the machine, and this is the one socket a subverted preview window can
//! reach — so the wide credential had no business being accepted on it. The
//! compatibility it bought was with a client that has never existed outside
//! this repository, because nothing has shipped.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use ovs_types::telemetry::StreamSurface;
use tokio::sync::broadcast::error::RecvError;

use crate::server::auth_routes::authenticate;
use crate::server::error::{ApiFailure, ApiResult};
use crate::server::{AppState, StreamChunk};

/// How long a watcher from the network has to prove it may watch.
const TOKEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The header in front of every picture.
const HEADER_BYTES: usize = 16;

/// Mints a permission to watch one surface.
///
/// Needs a session, and gives back something much smaller than one: a client
/// that holds a ticket can watch that surface for a minute and do nothing else
/// at all. A window with a video decoder in it should hold this and never a
/// session token.
///
/// # Errors
///
/// Returns [`ApiFailure`] when there is no session, when the surface is not one
/// this server has, or when a ticket could not be minted.
pub async fn ticket(
    State(state): State<AppState>,
    Path(surface): Path<String>,
    headers: HeaderMap,
) -> ApiResult<axum::Json<ovs_proto::http::StreamTicketResponse>> {
    authenticate(&state, &headers)?;

    let surface =
        parse(&surface).ok_or_else(|| ApiFailure::not_found("this server has no such surface"))?;

    let (ticket, expires_at) = state
        .mint_stream_ticket(surface)
        .map_err(|_| ApiFailure::internal("a permission to watch could not be issued"))?;

    Ok(axum::Json(ovs_proto::http::StreamTicketResponse {
        ticket,
        expires_at: expires_at.as_millis(),
    }))
}

/// Upgrades a request into a stream of one surface.
///
/// `surface` is `composition` or an output's identifier, which is what the
/// telemetry calls it.
pub async fn watch(
    State(state): State<AppState>,
    Path(surface): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let local = crate::server::is_local(&headers);
    let surface = parse(&surface);

    upgrade.on_upgrade(move |socket| async move {
        let Some(surface) = surface else {
            // Nothing to watch under that name. Closed rather than left open:
            // a client waiting for pictures that will never come has no way to
            // tell that from a quiet show.
            close(socket, "no such surface").await;
            return;
        };
        serve(socket, state, surface, local).await;
    })
}

/// Turns a path segment into a surface.
fn parse(surface: &str) -> Option<StreamSurface> {
    if surface.eq_ignore_ascii_case("composition") {
        return Some(StreamSurface::Composition);
    }
    // Hyphenated rather than a query parameter or a second route: a surface is
    // one path segment everywhere else, and a ticket is minted for the name
    // that was asked for, so a second spelling would be a second thing to keep
    // in step.
    if surface.eq_ignore_ascii_case("composition-masked") {
        return Some(StreamSurface::CompositionMasked);
    }

    surface
        .parse::<ovs_types::id::OutputId>()
        .ok()
        .map(|output_id| StreamSurface::Output { output_id })
}

/// Sends what the renderer makes of one surface until the watcher goes away.
async fn serve(mut socket: WebSocket, state: AppState, surface: StreamSurface, local: bool) {
    if !local && !authenticated(&mut socket, &state, surface).await {
        close(socket, "unauthenticated").await;
        return;
    }

    // Subscribing is what starts the encoder, so it happens after the watcher
    // has proved it may watch: an unauthenticated socket must not be able to
    // put load on a machine running a show.
    let crate::server::Watching {
        events: mut chunks,
        keyframe,
        failure,
        encoding,
    } = state.watch_stream(surface);

    // What it is being encoded with, when the encoder is already up because
    // somebody else is watching. A watcher who arrives first is told a moment
    // later instead, by the same message.
    if let Some(encoding) = encoding {
        let _ = socket
            .send(Message::Text(encoding_message(&encoding)))
            .await;
    }

    if let Some(reason) = failure {
        // The surface is known and cannot be streamed — no encoder on this
        // machine, most likely. Said out loud rather than left as silence.
        let _ = socket
            .send(Message::Text(
                format!("{{\"error\":{}}}", quoted(&reason)).into(),
            ))
            .await;
    }

    // The last frame a watcher can start from, so a preview that was opened
    // between keyframes shows a picture now rather than in two seconds.
    let mut alive = match keyframe {
        Some(chunk) => send(&mut socket, &chunk).await,
        None => true,
    };

    while alive {
        tokio::select! {
            received = chunks.recv() => match received {
                Ok(crate::server::StreamEvent::Chunk(chunk)) => {
                    alive = send(&mut socket, &chunk).await;
                }
                Ok(crate::server::StreamEvent::Encoding(encoding)) => {
                    alive = socket
                        .send(Message::Text(encoding_message(&encoding)))
                        .await
                        .is_ok();
                }
                // This watcher fell behind and missed frames. That is the
                // design — the picture it wants is the newest one — and the
                // next keyframe puts it right, so the only thing to do is
                // carry on.
                Err(RecvError::Lagged(missed)) => {
                    tracing::debug!(%surface, missed, "a watcher fell behind");
                }
                // The renderer stopped: nobody is watching any more, or the
                // link went away. Either way there is nothing to send.
                Err(RecvError::Closed) => break,
            },
            // A watcher that has gone away, or one saying something. Nothing
            // it can say matters here; what matters is noticing it has left,
            // so the encoder can be stopped.
            message = socket.recv() => match message {
                Some(Ok(_)) => {}
                _ => break,
            },
        }
    }

    state.unwatch_stream(surface);
}

/// One encoding, as the text message a watcher reads.
///
/// Serialised rather than formatted by hand, because `fps` is a float and a
/// hand-written one is how a client ends up parsing `15` where it expected
/// `15.0` — or `NaN`, which is not JSON at all.
fn encoding_message(
    encoding: &ovs_types::telemetry::StreamEncoding,
) -> axum::extract::ws::Utf8Bytes {
    serde_json::json!({ "encoding": encoding })
        .to_string()
        .into()
}

/// Waits for a watcher from the network to prove it may watch.
///
/// A ticket, and only a ticket. A ticket is spent by being tried, so a watcher
/// gets one attempt with it and a replayed message is refused — which is the
/// property that lets a page hold it at all.
async fn authenticated(socket: &mut WebSocket, state: &AppState, surface: StreamSurface) -> bool {
    let first = tokio::time::timeout(TOKEN_TIMEOUT, socket.recv()).await;

    let Ok(Some(Ok(Message::Text(text)))) = first else {
        return false;
    };

    let Ok(offered) = serde_json::from_str::<Offered>(&text) else {
        return false;
    };
    let Some(ticket) = offered.ticket else {
        // A message with no ticket in it, which since 2026-09-22 includes a
        // `ClientHello` carrying a session token. Said out loud once per
        // socket, because a deployment where this starts happening is one
        // running a client that predates the change.
        tracing::debug!(%surface, "a watcher offered no ticket");
        return false;
    };

    state.spend_stream_ticket(&ticket, surface)
}

/// What a watcher may send to prove itself.
///
/// One field, read out of whatever JSON object arrived: a client sending
/// something larger is not refused for its extra keys, only for the absence of
/// the one that matters.
#[derive(serde::Deserialize)]
struct Offered {
    /// A permission for this surface, minted for this socket.
    #[serde(default)]
    ticket: Option<String>,
}

/// Sends one picture, and says whether the socket is still there.
async fn send(socket: &mut WebSocket, chunk: &Arc<StreamChunk>) -> bool {
    let mut message = Vec::with_capacity(HEADER_BYTES + chunk.data.len());
    message.push(1);
    message.push(u8::from(chunk.keyframe));
    message.extend_from_slice(&[0, 0]);
    message.extend_from_slice(&(chunk.size.width as u16).to_le_bytes());
    message.extend_from_slice(&(chunk.size.height as u16).to_le_bytes());
    message.extend_from_slice(&chunk.timestamp_us.to_le_bytes());
    message.extend_from_slice(&chunk.data);

    socket.send(Message::Binary(message.into())).await.is_ok()
}

/// Says why, then goes.
async fn close(mut socket: WebSocket, reason: &str) {
    let _ = socket
        .send(Message::Text(
            format!("{{\"error\":{}}}", quoted(reason)).into(),
        ))
        .await;
    let _ = socket.send(Message::Close(None)).await;
}

/// A string as JSON, without pulling in a serializer for one field.
fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_surface_is_named_the_way_the_telemetry_names_it() {
        assert_eq!(parse("composition"), Some(StreamSurface::Composition));
        assert_eq!(parse("COMPOSITION"), Some(StreamSurface::Composition));
        assert_eq!(
            parse("composition-masked"),
            Some(StreamSurface::CompositionMasked)
        );
        assert_eq!(
            parse("Composition-Masked"),
            Some(StreamSurface::CompositionMasked)
        );
        assert_eq!(parse("composition_masked"), None, "one spelling, not two");

        let output_id = ovs_types::id::OutputId::new();
        assert_eq!(
            parse(&output_id.to_string()),
            Some(StreamSurface::Output { output_id })
        );
    }

    #[test]
    fn anything_else_is_not_a_surface() {
        // Closed rather than left open: a client waiting for pictures that
        // will never come cannot tell that from a quiet show.
        assert_eq!(parse("wall-2"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("../composition"), None);
    }

    #[test]
    fn a_session_token_is_not_a_way_to_watch() {
        // The exact message a client written before tickets sent: a
        // `ClientHello`, carrying the credential that can also load a show,
        // move a fader and stop the machine. It used to be accepted here
        // (owner decision Q10). It parses — extra keys are not a refusal —
        // and carries no ticket, which is now the only thing that opens a
        // socket.
        let hello = serde_json::json!({
            "protocol_version": 1,
            "token": "SESSION",
            "client_name": "OpenVideoServer Desktop",
            "client_version": "0.0.1",
        })
        .to_string();

        let offered: Offered = serde_json::from_str(&hello).expect("an object is an object");
        assert!(
            offered.ticket.is_none(),
            "there is nothing in a hello that opens this socket"
        );

        // And a ticket still is one, whatever else travels beside it.
        let offered: Offered = serde_json::from_str(r#"{"ticket":"T","token":"SESSION"}"#)
            .expect("an object is an object");
        assert_eq!(offered.ticket.as_deref(), Some("T"));
    }

    #[test]
    fn an_encoding_is_said_in_the_shape_the_protocol_documents() {
        // A client parses this. The rate is a float and a hand-written one is
        // how a watcher ends up reading `15` where it expected `15.0`.
        let message = encoding_message(&ovs_types::telemetry::StreamEncoding {
            encoder: "libx264".to_owned(),
            hardware: false,
            fps: 15.0,
        });

        let parsed: serde_json::Value = serde_json::from_str(message.as_str()).expect("valid JSON");
        assert_eq!(parsed["encoding"]["encoder"], "libx264");
        assert_eq!(parsed["encoding"]["hardware"], false);
        assert_eq!(parsed["encoding"]["fps"], 15.0);

        // And it is not an error, because a watcher that read it as one would
        // put "libx264" in front of an operator as a fault.
        assert!(parsed.get("error").is_none());
    }

    #[test]
    fn a_picture_says_what_it_is_before_it_says_what_it_shows() {
        // Sixteen bytes, and every one of them readable without anything
        // having been said first.
        let chunk = Arc::new(StreamChunk {
            size: ovs_types::geometry::SizePx::new(1280, 720),
            keyframe: true,
            timestamp_us: 33_366,
            data: vec![0, 0, 0, 1, 0x65],
        });

        let mut message = Vec::new();
        message.push(1);
        message.push(u8::from(chunk.keyframe));
        message.extend_from_slice(&[0, 0]);
        message.extend_from_slice(&(chunk.size.width as u16).to_le_bytes());
        message.extend_from_slice(&(chunk.size.height as u16).to_le_bytes());
        message.extend_from_slice(&chunk.timestamp_us.to_le_bytes());
        message.extend_from_slice(&chunk.data);

        assert_eq!(message[0], 1, "version");
        assert_eq!(message[1], 1, "a frame to start from");
        assert_eq!(u16::from_le_bytes([message[4], message[5]]), 1280);
        assert_eq!(u16::from_le_bytes([message[6], message[7]]), 720);
        assert_eq!(
            u64::from_le_bytes(message[8..16].try_into().expect("eight bytes")),
            33_366
        );
        assert_eq!(&message[16..], &[0, 0, 0, 1, 0x65], "and then the picture");
    }
}
