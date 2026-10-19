//! The authoritative server core.
//!
//! Owns the show and live state, the session layer, and the boundary between
//! them and persistence. The HTTP and WebSocket transports sit on top of this;
//! the rules live here, so they hold whatever a request arrived over.
//!
//! - [`state`] — the authoritative show and live state, and the update stream.
//! - [`auth`] — password hashing and expiring session tokens.
//! - [`session`] — sessions in flight.
//! - [`server`] — the HTTP and WebSocket transport.
//! - [`renderer_link`] — the core's end of the control channel to the renderer.
//! - [`dmx_link`] — the core's end of DMX-over-IP.
//! - [`host`] — what the machine itself is doing.
//! - [`power`] — restarting and stopping the machine.
//! - [`media_store`] — where media files live on disk.
//! - [`parameters`] — the one way a control source changes a show.
//! - [`uploads`] — media arriving from a client.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod auth;
pub mod config_file;
pub mod discovery;
pub mod dmx_link;
pub mod host;
pub mod machine_connectors;
pub mod media_store;
pub mod network;
pub mod parameters;
pub mod power;
pub mod renderer_link;
pub mod server;
pub mod session;
pub mod state;
pub mod stream_tickets;
pub mod update;
pub mod uploads;

pub use auth::{AuthError, SessionToken};
pub use config_file::{ConnectorRef, ServerConfig};
pub use host::{HostMonitor, HostSample};
pub use media_store::MediaStore;
pub use server::{router, AppState};
pub use session::Sessions;
pub use state::{AuthoritativeState, SequencedChange, StateError};
pub use uploads::Uploads;

#[cfg(test)]
mod integration {
    //! The pieces of Phase 2 working together against a real database.

    use ovs_db::{Account, Database};
    use ovs_proto::realtime::{Sequence, SequenceTracker, StateSnapshot};
    use ovs_types::config::{Layer, Show, Slot, StartupPolicy};
    use ovs_types::id::{AccountId, MediaItemId};
    use ovs_types::telemetry::RendererStatus;
    use ovs_types::time::Timestamp;

    use super::*;

    fn show() -> Show {
        let mut show = Show::new("Show");
        show.master.default_bpm = 120.0;
        show.master.default_dimmer = 0.9;
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        layer.slots.push(Slot::media(3, MediaItemId::new()));
        show.layers.push(layer);
        show
    }

    #[test]
    fn a_server_comes_up_runs_a_show_and_restarts_where_the_policy_says() {
        let mut database = Database::open_in_memory().expect("opens");

        // First run: create the operator account and a show.
        let hash = auth::hash_password("correct horse battery staple").expect("hashes");
        database
            .upsert_account(&Account {
                id: AccountId::new(),
                username: "operator".to_owned(),
                password_hash: hash,
                role: ovs_types::account::Role::Admin,
                created_at: Timestamp::now(),
                last_login_at: None,
            })
            .expect("creates");

        let show = show();
        let layer_id = show.layers[0].id;
        database.save_show(&show).expect("saves");
        database
            .set_startup_policy(StartupPolicy::LastActive)
            .expect("sets");

        // The server loads it and the show runs.
        let mut state = AuthoritativeState::new();
        state.load_show(database.load_show(show.id).expect("loads"), None);
        state.set_renderer(RendererStatus::Running);
        state.select_slot(layer_id, 3).expect("valid");
        state.update_master(|master| {
            master.set_bpm(174.0);
            master.set_dimmer(0.2);
            master.set_crossfader(1.0);
        });

        // Auto-save of the live snapshot.
        database
            .save_live_snapshot(show.id, &state.live_snapshot())
            .expect("saves");

        // The server restarts.
        let reloaded = database.load_show(show.id).expect("loads");
        let snapshot = database.load_live_snapshot(show.id).expect("loads");
        let mut restarted = AuthoritativeState::new();
        restarted.load_show(reloaded, snapshot.as_ref());

        // Where it was left, all of it (owner, 2026-10-19: "Nach neustart den
        // kompletten status, so wie er war"), through the real database.
        assert_eq!(restarted.master().bpm, 174.0);
        assert_eq!(restarted.layer(layer_id).expect("loaded").active_slot, 3);
        assert_eq!(restarted.master().dimmer, 0.2, "the dimmer as it was left");
        assert_eq!(restarted.master().crossfader, 1.0);
        assert!(!restarted.master().freeze);
    }

    #[test]
    fn a_client_following_the_protocol_stays_in_step_with_the_server() {
        // Drives the real state machine against the real client-side tracker,
        // so the two halves of the contract are checked against each other
        // rather than each against a mock.
        let mut state = AuthoritativeState::new();
        let mut client = SequenceTracker::new();

        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);
        state.set_renderer(RendererStatus::Running);

        // The client connects and receives the snapshot.
        let snapshot: StateSnapshot = state.snapshot();
        client.accept_snapshot(&snapshot);

        // Everything the server emits from here is applicable in order.
        let changes = [
            state.select_slot(layer_id, 1).expect("valid"),
            state.update_master(|master| master.set_bpm(140.0)),
            state.select_slot(layer_id, 3).expect("valid"),
            state.update_master(|master| master.set_dimmer(0.5)),
        ];
        for change in changes.into_iter().flatten() {
            assert_eq!(
                client.accept_update(change.sequence),
                Ok(()),
                "the server emitted a sequence the client could not apply"
            );
        }
        assert_eq!(client.position(), Some(state.sequence()));
    }

    #[test]
    fn a_client_that_misses_an_update_is_told_to_resynchronise() {
        let mut state = AuthoritativeState::new();
        let mut client = SequenceTracker::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);
        client.accept_snapshot(&state.snapshot());

        // The server emits two changes; the network delivers only the second.
        let _dropped = state
            .select_slot(layer_id, 1)
            .expect("valid")
            .expect("changed");
        let delivered = state
            .update_master(|master| master.set_bpm(140.0))
            .expect("changed");

        assert!(client.accept_update(delivered.sequence).is_err());

        // Resynchronising puts the client back in step, at the real state.
        client.accept_snapshot(&state.snapshot());
        let next = state
            .select_slot(layer_id, 3)
            .expect("valid")
            .expect("changed");
        assert_eq!(client.accept_update(next.sequence), Ok(()));
    }

    #[test]
    fn a_refused_request_does_not_desynchronise_a_client() {
        // An error must not look like a dropped update, or every failed request
        // would cost a full resynchronisation.
        let mut state = AuthoritativeState::new();
        let mut client = SequenceTracker::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);
        client.accept_snapshot(&state.snapshot());

        assert!(state.select_slot(layer_id, 99).is_err());

        let next = state
            .select_slot(layer_id, 1)
            .expect("valid")
            .expect("changed");
        assert_eq!(client.accept_update(next.sequence), Ok(()));
    }

    #[test]
    fn signing_in_uses_the_stored_hash_and_records_the_login() {
        let mut database = Database::open_in_memory().expect("opens");
        let password = "correct horse battery staple";
        let account = Account {
            id: AccountId::new(),
            username: "operator".to_owned(),
            password_hash: auth::hash_password(password).expect("hashes"),
            role: ovs_types::account::Role::Admin,
            created_at: Timestamp::now(),
            last_login_at: None,
        };
        database.upsert_account(&account).expect("creates");

        let stored = database
            .account_by_username("operator")
            .expect("reads")
            .expect("exists");
        assert!(auth::verify_password(password, &stored.password_hash).is_ok());
        assert!(auth::verify_password("wrong", &stored.password_hash).is_err());

        let mut sessions = Sessions::new();
        let token = sessions
            .sign_in(stored.id, Timestamp::now())
            .expect("issues");
        database
            .record_login(stored.id, Timestamp::now())
            .expect("records");

        assert_eq!(
            sessions
                .account_for(&token.token, Timestamp::now())
                .expect("valid"),
            stored.id
        );
    }

    #[test]
    fn the_startup_policy_decides_what_a_restart_opens() {
        let mut database = Database::open_in_memory().expect("opens");
        let pinned = show();
        let other = Show::new("Something Else");
        database.save_show(&pinned).expect("saves");
        database.save_show(&other).expect("saves");
        database
            .set_startup_policy(StartupPolicy::Pinned { show_id: pinned.id })
            .expect("pins");

        let policy = database.startup_policy().expect("reads");
        let mut state = AuthoritativeState::new();
        match policy {
            StartupPolicy::Pinned { show_id } => {
                state.load_show(database.load_show(show_id).expect("loads"), None);
            }
            StartupPolicy::LastActive | StartupPolicy::None => panic!("expected a pinned show"),
        }
        assert_eq!(state.show_id(), Some(pinned.id));

        // No show means black outputs, not a failure to start.
        database
            .set_startup_policy(StartupPolicy::None)
            .expect("sets");
        let empty = AuthoritativeState::new();
        assert!(empty.show().is_none());
        assert_eq!(empty.snapshot().sequence, Sequence::START);
    }
}
