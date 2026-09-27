//! The live snapshot: the only live state that survives a restart.
//!
//! Stored in its own table, never inside the show document. That separation
//! is what lets a selected slot be restored without live state leaking into
//! show configuration.

use ovs_types::id::ShowId;
use ovs_types::live::LiveSnapshot;
use ovs_types::time::Timestamp;
use rusqlite::{params, OptionalExtension};

use crate::database::Database;
use crate::error::{DbError, Result};

impl Database {
    /// Records the live state that should survive a restart of this show.
    ///
    /// # Errors
    ///
    /// Returns an error if the show does not exist or the write fails.
    pub fn save_live_snapshot(&mut self, show_id: ShowId, snapshot: &LiveSnapshot) -> Result<()> {
        let document = serde_json::to_string(snapshot).map_err(|source| DbError::Encode {
            entity: "live snapshot",
            source,
        })?;

        self.connection().execute(
            "INSERT INTO live_snapshots (show_id, document, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (show_id) DO UPDATE SET
                 document   = excluded.document,
                 updated_at = excluded.updated_at",
            params![show_id.to_string(), document, Timestamp::now().as_millis()],
        )?;
        Ok(())
    }

    /// Loads the live snapshot for a show, if one was recorded.
    ///
    /// Returns `None` when the show has never run, in which case the restart
    /// path falls back entirely to show defaults.
    ///
    /// # Errors
    ///
    /// Returns an error if a stored snapshot cannot be decoded.
    pub fn load_live_snapshot(&self, show_id: ShowId) -> Result<Option<LiveSnapshot>> {
        let document: Option<String> = self
            .connection()
            .query_row(
                "SELECT document FROM live_snapshots WHERE show_id = ?1",
                params![show_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;

        document
            .map(|document| {
                serde_json::from_str(&document).map_err(|source| DbError::Decode {
                    entity: "live snapshot",
                    source,
                })
            })
            .transpose()
    }

    /// Discards the live snapshot for a show.
    ///
    /// Used when an operator wants the next start to come up from show
    /// defaults alone.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete fails.
    pub fn clear_live_snapshot(&mut self, show_id: ShowId) -> Result<bool> {
        let removed = self.connection().execute(
            "DELETE FROM live_snapshots WHERE show_id = ?1",
            params![show_id.to_string()],
        )?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::{Layer, Show, Slot};
    use ovs_types::id::MediaItemId;
    use ovs_types::live::{LayerLive, MasterLive};

    use super::*;

    fn show_with_a_layer() -> Show {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(3, MediaItemId::new()));
        show.layers.push(layer);
        show
    }

    #[test]
    fn a_snapshot_round_trips() {
        let mut database = Database::open_in_memory().expect("opens");
        let show = show_with_a_layer();
        database.save_show(&show).expect("saves");

        let mut snapshot = LiveSnapshot {
            bpm: Some(174.0),
            ..LiveSnapshot::default()
        };
        snapshot.record_selected_slot(show.layers[0].id, 3);
        database
            .save_live_snapshot(show.id, &snapshot)
            .expect("saves");

        assert_eq!(
            database.load_live_snapshot(show.id).expect("loads"),
            Some(snapshot)
        );
    }

    #[test]
    fn a_show_that_has_never_run_has_no_snapshot() {
        let mut database = Database::open_in_memory().expect("opens");
        let show = show_with_a_layer();
        database.save_show(&show).expect("saves");
        assert_eq!(database.load_live_snapshot(show.id).expect("loads"), None);
    }

    #[test]
    fn the_full_restart_path_works_end_to_end() {
        // This is the behaviour an operator sees after a renderer restart
        // mid-show, assembled from the pieces that actually ship.
        let mut database = Database::open_in_memory().expect("opens");
        let mut show = show_with_a_layer();
        show.master.default_dimmer = 0.8;
        show.master.default_crossfader = 0.0;
        show.master.default_bpm = 120.0;
        database.save_show(&show).expect("saves");

        // The show runs: tempo tapped to 174, layer switched to slot 3,
        // crossfader pushed fully to B by the console.
        let mut snapshot = LiveSnapshot {
            bpm: Some(174.0),
            ..LiveSnapshot::default()
        };
        snapshot.record_selected_slot(show.layers[0].id, 3);
        database
            .save_live_snapshot(show.id, &snapshot)
            .expect("saves");

        // Renderer restarts.
        let reloaded = database.load_show(show.id).expect("loads");
        let restored = database.load_live_snapshot(show.id).expect("loads");
        let master = MasterLive::on_restart(&reloaded.master, restored.as_ref());
        let layer = LayerLive::on_restart(&reloaded.layers[0], restored.as_ref());

        assert_eq!(master.bpm, 174.0, "the tapped tempo survives");
        assert_eq!(master.dimmer, 0.8, "the dimmer returns to the show default");
        assert_eq!(
            master.crossfader, 0.0,
            "the show does not come back mid-crossfade"
        );
        assert!(!master.freeze);
        assert_eq!(
            layer.active_slot, 3,
            "the show comes back showing its clips"
        );
    }

    #[test]
    fn a_snapshot_is_removed_with_its_show() {
        let mut database = Database::open_in_memory().expect("opens");
        let show = show_with_a_layer();
        database.save_show(&show).expect("saves");
        database
            .save_live_snapshot(show.id, &LiveSnapshot::default())
            .expect("saves");

        database.delete_show(show.id).expect("deletes");
        assert_eq!(database.load_live_snapshot(show.id).expect("loads"), None);
    }

    #[test]
    fn a_snapshot_cannot_outlive_a_show_that_never_existed() {
        let mut database = Database::open_in_memory().expect("opens");
        let error = database
            .save_live_snapshot(ShowId::new(), &LiveSnapshot::default())
            .expect_err("no such show");
        assert!(
            matches!(error, DbError::Sqlite(_)),
            "the foreign key must reject this"
        );
    }

    #[test]
    fn clearing_a_snapshot_returns_the_next_start_to_show_defaults() {
        let mut database = Database::open_in_memory().expect("opens");
        let show = show_with_a_layer();
        database.save_show(&show).expect("saves");
        database
            .save_live_snapshot(
                show.id,
                &LiveSnapshot {
                    bpm: Some(174.0),
                    ..LiveSnapshot::default()
                },
            )
            .expect("saves");

        assert!(database.clear_live_snapshot(show.id).expect("clears"));
        assert!(!database.clear_live_snapshot(show.id).expect("already gone"));

        let master = MasterLive::on_restart(&show.master, None);
        assert_eq!(master.bpm, show.master.default_bpm);
    }

    #[test]
    fn a_snapshot_never_contains_console_driven_values() {
        // If this ever fails, live state has started leaking into what survives
        // a restart, which is the thing the restart rule exists to prevent.
        let mut snapshot = LiveSnapshot {
            bpm: Some(128.0),
            ..LiveSnapshot::default()
        };
        snapshot.record_selected_slot(ovs_types::id::LayerId::new(), 1);
        let json = serde_json::to_string(&snapshot).expect("serialises");

        for forbidden in ["dimmer", "crossfader", "opacity", "transform", "freeze"] {
            assert!(
                !json.contains(forbidden),
                "a snapshot must not carry {forbidden}"
            );
        }
    }
}
