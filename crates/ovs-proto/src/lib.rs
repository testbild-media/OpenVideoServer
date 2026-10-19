//! The versioned client/server contract for OpenVideoServer.
//!
//! Two surfaces, both defined once here and generated into TypeScript for the
//! desktop client:
//!
//! - [`http`] — request and response bodies for the REST API under `/api/v1`.
//! - [`realtime`] — the WebSocket protocol carrying authoritative state,
//!   telemetry and progress.
//! - [`fixtures`] — the DMX personalities, in a form a console programmer or a
//!   patch editor can read.
//! - [`generators`] — the generators a slot can hold, with their parameters.
//! - [`grandma1`] — the same personalities as fixture types a grandMA1 imports.
//!
//! This crate defines shapes, not behaviour. It has no HTTP server, no socket
//! and no async runtime, so the client and the server can both depend on it
//! without inheriting each other's machinery.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod desks;
pub mod effects;
pub mod fixtures;
pub mod generators;
pub mod grandma1;
pub mod http;
pub mod modules;
pub mod parameters;
pub mod realtime;
pub mod schema;

pub use effects::EffectCatalogueResponse;
pub use fixtures::{catalogue, FixtureCatalogueResponse};
pub use generators::GeneratorCatalogueResponse;
pub use http::{ApiError, API_PREFIX};
pub use realtime::{
    ClientHello, ClientMessage, Sequence, SequenceTracker, ServerHello, ServerMessage, StateChange,
    StateSnapshot, StreamError,
};

#[cfg(test)]
mod contract {
    //! Tests for the promises the protocol makes as a whole.

    use ovs_types::live::MasterLive;
    use ovs_types::telemetry::RendererStatus;

    use super::realtime::*;

    /// Plays a realistic connect, run, drop and reconnect against the tracker.
    #[test]
    fn a_client_that_follows_the_protocol_never_applies_state_it_guessed_at() {
        let mut tracker = SequenceTracker::new();

        // Connect.
        let hello = ClientHello::current("t0ken", "OVS Desktop", "0.0.1");
        assert!(matches!(negotiate(&hello), ServerHello::Accepted { .. }));

        // The server sends a snapshot before anything else.
        let snapshot = StateSnapshot {
            sequence: Sequence(100),
            show: None,
            master: MasterLive::default(),
            layers: Vec::new(),
            effects: Default::default(),
            generators: Default::default(),
            desk: Default::default(),
            renderer: RendererStatus::Running,
        };
        tracker.accept_snapshot(&snapshot);

        // The show runs.
        for sequence in 101..=110 {
            assert_eq!(tracker.accept_update(Sequence(sequence)), Ok(()));
        }

        // The network drops. The client reconnects and, before a new snapshot
        // arrives, an update from the live stream reaches it.
        tracker.reset();
        assert_eq!(
            tracker.accept_update(Sequence(111)),
            Err(StreamError::NoSnapshotYet),
            "a reconnected client must not apply an update to state it no longer has"
        );

        // A fresh snapshot restores the stream, from wherever the server is now.
        tracker.accept_snapshot(&StateSnapshot {
            sequence: Sequence(150),
            ..snapshot
        });
        assert_eq!(tracker.accept_update(Sequence(151)), Ok(()));
    }

    #[test]
    fn every_server_message_is_self_describing() {
        // A client switches on a tag, so every variant needs one.
        let message = ServerMessage::Pong { nonce: 1 };
        let json: serde_json::Value =
            serde_json::to_value(&message).expect("serialises to an object");
        assert_eq!(json["type"], "pong");
    }
}
