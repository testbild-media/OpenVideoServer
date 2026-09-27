//! Opening and configuring the database.

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;
use crate::migrations;

/// A configured, migrated OpenVideoServer database.
///
/// Opening one always brings the schema up to date, so a server that has been
/// several versions behind in a rack comes up rather than refusing to start.
pub struct Database {
    connection: Connection,
}

impl Database {
    /// Opens the database at `path`, creating and migrating it if needed.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or a migration fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        Self::configure_and_migrate(connection)
    }

    /// Opens a private in-memory database, for tests.
    ///
    /// # Errors
    ///
    /// Returns an error if a migration fails.
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        Self::configure_and_migrate(connection)
    }

    fn configure_and_migrate(connection: Connection) -> Result<Self> {
        // WAL lets the renderer and the API read while a save is in flight, and
        // survives an unclean shutdown far better than the rollback journal
        //. An in-memory database silently stays in `memory`
        // mode, which is fine and is why the result is not asserted on.
        connection.pragma_update(None, "journal_mode", "WAL")?;

        // SQLite does not enforce foreign keys unless asked, so an asset could
        // otherwise outlive the media item it belongs to.
        connection.pragma_update(None, "foreign_keys", true)?;

        // NORMAL is the WAL-appropriate setting: durable across an application
        // crash, and only at risk in a power loss, which a rack server answers
        // with hardware rather than with an fsync per write.
        connection.pragma_update(None, "synchronous", "NORMAL")?;

        // A save must not fail because the renderer happened to be reading.
        connection.busy_timeout(std::time::Duration::from_secs(5))?;

        let mut database = Self { connection };
        migrations::apply_pending(&mut database.connection)?;
        Ok(database)
    }

    /// Returns the schema version currently applied.
    ///
    /// # Errors
    ///
    /// Returns an error if the bookkeeping table cannot be read.
    pub fn schema_version(&self) -> Result<u32> {
        migrations::current_version(&self.connection)
    }

    /// Borrows the underlying connection.
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Borrows the underlying connection mutably, for transactions.
    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// Borrows the connection so a test can assert on raw stored rows.
    ///
    /// Some invariants are about what actually lands on disk, not about what the
    /// typed API returns, and those have to be checked against the row itself.
    #[cfg(test)]
    pub(crate) fn connection_for_test(&self) -> &Connection {
        &self.connection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_creates_and_migrates() {
        let database = Database::open_in_memory().expect("opens");
        assert_eq!(
            database.schema_version().expect("version"),
            migrations::target_version()
        );
    }

    #[test]
    fn foreign_keys_are_enforced() {
        // Off by default in SQLite, which would let an asset outlive its item.
        let database = Database::open_in_memory().expect("opens");
        let enabled: bool = database
            .connection()
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("pragma");
        assert!(enabled, "foreign keys must be enforced");
    }

    #[test]
    fn a_file_database_uses_write_ahead_logging() {
        let dir = tempfile::tempdir().expect("temp dir");
        let database = Database::open(dir.path().join("ovs.db")).expect("opens");
        let mode: String = database
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("pragma");
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn reopening_an_existing_database_does_not_re_migrate() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("ovs.db");

        let first = Database::open(&path).expect("creates");
        let version = first.schema_version().expect("version");
        drop(first);

        let second = Database::open(&path).expect("reopens");
        assert_eq!(second.schema_version().expect("version"), version);
        assert_eq!(
            migrations::applied_versions(second.connection())
                .expect("versions")
                .len(),
            migrations::MIGRATIONS.len(),
            "reopening must not apply a migration twice"
        );
    }
}
