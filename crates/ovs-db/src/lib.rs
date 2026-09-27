//! Persistence for OpenVideoServer.
//!
//! SQLite in WAL mode with ordered, forward-only migrations.
//!
//! # What this crate guarantees
//!
//! - **A show that cannot be loaded back never reaches storage.** Every save
//!   validates first and reports every problem, not the first one.
//! - **A partial upload never becomes valid content.** Registration writes the
//!   asset and marks the item ready in one transaction, and the query that finds
//!   a playable asset filters on verification, so there is no code path that
//!   hands an unverified file to a decoder.
//! - **Live state cannot leak into a show.** Every write entry point takes
//!   [`ovs_types::config`] types. The live snapshot that survives a restart has
//!   its own table and its own type.
//! - **A database from a newer build is refused with an explanation** rather
//!   than half-understood.
//!
//! # Storage strategy
//!
//! Shows are stored as JSON documents with their schema version in a real
//! column. Media is relational.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

mod database;
mod error;
mod live;
mod media;
pub mod migrations;
mod settings;
mod shows;

pub use database::Database;
pub use error::{DbError, Result};
pub use settings::{connector_key, Account};
pub use shows::ShowSummary;

#[cfg(test)]
mod invariants {
    //! Cross-module invariants, checked against a real database.

    use ovs_types::config::{Layer, Show, Slot};
    use ovs_types::id::MediaItemId;
    use ovs_types::live::LiveSnapshot;
    use ovs_types::media::{AssetValidation, MediaAsset, MediaItem, MediaState};
    use ovs_types::time::Timestamp;

    use super::*;

    #[test]
    fn a_saved_show_never_contains_live_state() {
        // The structural guarantee, checked against what actually
        // lands in the database rather than against the type definitions.
        let mut database = Database::open_in_memory().expect("opens");
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        show.layers.push(layer);
        database.save_show(&show).expect("saves");

        let mut snapshot = LiveSnapshot {
            bpm: Some(174.0),
            ..LiveSnapshot::default()
        };
        snapshot.record_selected_slot(show.layers[0].id, 1);
        database
            .save_live_snapshot(show.id, &snapshot)
            .expect("saves");

        let document: String = database
            .connection_for_test()
            .query_row(
                "SELECT document FROM shows WHERE id = ?1",
                rusqlite::params![show.id.to_string()],
                |row| row.get(0),
            )
            .expect("reads");

        // Matched as exact JSON keys. A substring match would also hit
        // `default_dimmer` and `default_crossfader`, which are show
        // configuration: they are the values a restart returns to.
        // The live keys are the ones that must never appear.
        for forbidden in [
            "\"active_slot\"",
            "\"selected_slots\"",
            "\"dimmer\"",
            "\"crossfader\"",
            "\"freeze\"",
        ] {
            assert!(
                !document.contains(forbidden),
                "a stored show must not contain the live key {forbidden}, but it does:\n{document}"
            );
        }

        // The configuration counterparts must be there, so this test cannot pass
        // by the show simply having lost its master settings.
        assert!(document.contains("\"default_dimmer\""));
        assert!(document.contains("\"default_crossfader\""));
    }

    #[test]
    fn a_show_can_only_reference_media_by_its_logical_identity() {
        let mut database = Database::open_in_memory().expect("opens");

        let item = MediaItem {
            state: MediaState::Verifying,
            duration_ms: Some(1_000),
            dimensions: Some(ovs_types::geometry::SizePx::new(1920, 1080)),
            frame_rate: Some(ovs_types::geometry::FrameRate::FPS_60),
            ..MediaItem::importing("Intro", "intro.mov")
        };
        let asset = MediaAsset {
            id: ovs_types::id::MediaAssetId::new(),
            media_item_id: item.id,
            object_key: "ab/abcdef.mov".to_owned(),
            sha256: "a".repeat(64),
            byte_size: 1024,
            container: "mov".to_owned(),
            codec: "prores".to_owned(),
            validation: AssetValidation::Verified {
                at: Timestamp::now(),
            },
            created_at: Timestamp::now(),
        };
        database.register_media(&item, &asset).expect("registers");

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, item.id));
        show.layers.push(layer);
        database.save_show(&show).expect("saves");

        let document: String = database
            .connection_for_test()
            .query_row(
                "SELECT document FROM shows WHERE id = ?1",
                rusqlite::params![show.id.to_string()],
                |row| row.get(0),
            )
            .expect("reads");

        assert!(document.contains(&item.id.to_string()));
        assert!(
            !document.contains("ab/abcdef.mov"),
            "no physical key may reach a show"
        );
        assert!(
            !document.contains(&asset.sha256),
            "no content hash may reach a show"
        );
        assert!(
            !document.contains("intro.mov"),
            "no filename may reach a show"
        );
    }
}
