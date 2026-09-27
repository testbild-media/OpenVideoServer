//! Ordered, forward-only schema migrations.
//!
//! Every change to the database schema is a new [`Migration`] appended to
//! [`MIGRATIONS`]. Migrations are never edited once released and never run out
//! of order, because a server in a rack may be several versions behind and must
//! still come up (house rule §13).
//!
//! Each migration runs inside a transaction together with the row that records
//! it, so a failure leaves the database at the previous version rather than
//! half-migrated.

use rusqlite::{Connection, Transaction};

use crate::error::{DbError, Result};

/// One ordered schema change.
pub struct Migration {
    /// Version this migration produces. Starts at 1 and increases by one.
    pub version: u32,
    /// Short name, used in error messages so a failure is actionable.
    pub name: &'static str,
    /// The statements to run.
    pub sql: &'static str,
}

/// Every migration this build ships, in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: include_str!("migrations/0001_initial_schema.sql"),
    },
    Migration {
        version: 2,
        name: "assets_record_what_the_file_is",
        sql: include_str!("migrations/0002_assets_record_what_the_file_is.sql"),
    },
    Migration {
        version: 3,
        name: "the_library_has_folders",
        sql: include_str!("migrations/0003_the_library_has_folders.sql"),
    },
    Migration {
        version: 4,
        name: "the_project_list_has_folders",
        sql: include_str!("migrations/0004_the_project_list_has_folders.sql"),
    },
    Migration {
        version: 5,
        name: "accounts_have_roles",
        sql: include_str!("migrations/0005_accounts_have_roles.sql"),
    },
    Migration {
        version: 6,
        name: "a_project_is_a_show",
        sql: include_str!("migrations/0006_a_project_is_a_show.sql"),
    },
];

/// Returns the schema version this build migrates up to.
#[must_use]
pub fn target_version() -> u32 {
    MIGRATIONS.last().map_or(0, |migration| migration.version)
}

/// Creates the bookkeeping table if it is missing.
fn ensure_migration_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT    NOT NULL,
             applied_at INTEGER NOT NULL
         ) STRICT;",
    )?;
    Ok(())
}

/// Returns the highest applied schema version, or 0 for a fresh database.
///
/// # Errors
///
/// Returns an error if the bookkeeping table cannot be read.
pub fn current_version(conn: &Connection) -> Result<u32> {
    ensure_migration_table(conn)?;
    let version: Option<u32> =
        conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;
    Ok(version.unwrap_or(0))
}

/// Returns every applied version, in order.
///
/// # Errors
///
/// Returns an error if the bookkeeping table cannot be read.
pub fn applied_versions(conn: &Connection) -> Result<Vec<u32>> {
    ensure_migration_table(conn)?;
    let mut statement = conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let versions = statement
        .query_map([], |row| row.get::<_, u32>(0))?
        .collect::<rusqlite::Result<Vec<u32>>>()?;
    Ok(versions)
}

/// Applies every migration the database has not seen yet.
///
/// Returns the versions that were applied, which is empty when the database is
/// already current. Running this on an up-to-date database is a no-op, so it is
/// safe to call on every start.
///
/// # Errors
///
/// Returns [`DbError::DatabaseTooNew`] when the database was created by a newer
/// build, and [`DbError::Migration`] naming the migration that failed.
pub fn apply_pending(conn: &mut Connection) -> Result<Vec<u32>> {
    let current = current_version(conn)?;
    let target = target_version();

    if current > target {
        return Err(DbError::DatabaseTooNew {
            found: current,
            supported: target,
        });
    }

    let mut applied = Vec::new();
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version > current)
    {
        let transaction = conn.transaction()?;
        apply_one(&transaction, migration).map_err(|source| DbError::Migration {
            version: migration.version,
            name: migration.name,
            source,
        })?;
        transaction.commit()?;
        applied.push(migration.version);
    }

    Ok(applied)
}

fn apply_one(transaction: &Transaction<'_>, migration: &Migration) -> rusqlite::Result<()> {
    transaction.execute_batch(migration.sql)?;
    transaction.execute(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            migration.version,
            migration.name,
            ovs_types::time::Timestamp::now().as_millis()
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_numbered_from_one_without_gaps() {
        // A gap or a duplicate would mean a migration silently never runs.
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            let expected = u32::try_from(index + 1).expect("migration count fits in u32");
            assert_eq!(
                migration.version, expected,
                "migration {:?} is out of order",
                migration.name
            );
        }
    }

    #[test]
    fn migrations_have_distinct_names() {
        let mut seen = std::collections::BTreeSet::new();
        for migration in MIGRATIONS {
            assert!(
                seen.insert(migration.name),
                "duplicate migration name {}",
                migration.name
            );
        }
    }

    #[test]
    fn no_migration_is_empty() {
        for migration in MIGRATIONS {
            assert!(
                !migration.sql.trim().is_empty(),
                "migration {} has no statements",
                migration.name
            );
        }
    }

    #[test]
    fn a_fresh_database_migrates_to_the_target_version() {
        let mut conn = Connection::open_in_memory().expect("in-memory database");
        assert_eq!(current_version(&conn).expect("version"), 0);

        let applied = apply_pending(&mut conn).expect("migrates");
        assert_eq!(applied, (1..=target_version()).collect::<Vec<_>>());
        assert_eq!(current_version(&conn).expect("version"), target_version());
    }

    #[test]
    fn migrating_twice_changes_nothing() {
        let mut conn = Connection::open_in_memory().expect("in-memory database");
        apply_pending(&mut conn).expect("first run");
        let second = apply_pending(&mut conn).expect("second run");
        assert!(
            second.is_empty(),
            "a migrated database must not re-run migrations"
        );
        assert_eq!(
            applied_versions(&conn).expect("versions"),
            (1..=target_version()).collect::<Vec<_>>()
        );
    }

    /// Builds a database at version 1 and stops, the way a released server
    /// that has been running for months has one.
    ///
    /// Written out rather than taken from `MIGRATIONS[0]` on purpose: the point
    /// of a migration test is that a database written by *that* build, whose
    /// source may since have been edited, still comes forward.
    fn a_database_from_the_first_release() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory database");
        conn.execute_batch(MIGRATIONS[0].sql).expect("first schema");
        ensure_migration_table(&conn).expect("bookkeeping");
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (1, 'initial_schema', 0)",
            [],
        )
        .expect("record it");
        conn
    }

    #[test]
    fn a_library_from_the_first_release_comes_forward_with_its_files() {
        let mut conn = a_database_from_the_first_release();
        conn.execute_batch(
            "INSERT INTO media_items (
                 id, display_name, original_filename, kind, state, has_alpha, created_at
             ) VALUES ('item1', 'Intro', 'intro.mov', 'video', 'ready', 0, 0);
             INSERT INTO media_assets (
                 id, media_item_id, profile, profile_version, object_key, sha256,
                 byte_size, container, codec, validation_state, created_at
             ) VALUES ('asset1', 'item1', 'prores_422', 1, 'aa/aa.mov', 'aa',
                       1024, 'mov', 'prores', 'verified', 0);",
        )
        .expect("a library");

        apply_pending(&mut conn).expect("migrates");

        // The file is still there and still points at the same object. An
        // operator's library surviving the upgrade is the whole requirement.
        let key: String = conn
            .query_row("SELECT object_key FROM media_assets", [], |row| row.get(0))
            .expect("the asset is still there");
        assert_eq!(key, "aa/aa.mov");

        let columns: Vec<String> = conn
            .prepare("SELECT * FROM media_assets")
            .expect("prepare")
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert!(
            !columns.iter().any(|name| name.starts_with("profile")),
            "the profile columns are gone: {columns:?}"
        );
    }

    #[test]
    fn the_same_bytes_cannot_be_registered_twice_after_migrating() {
        let mut conn = a_database_from_the_first_release();
        apply_pending(&mut conn).expect("migrates");
        conn.execute_batch(
            "INSERT INTO media_items (
                 id, display_name, original_filename, kind, state, has_alpha, created_at
             ) VALUES ('item1', 'Intro', 'intro.mov', 'video', 'ready', 0, 0);
             INSERT INTO media_assets (
                 id, media_item_id, object_key, sha256, byte_size, container,
                 codec, validation_state, created_at
             ) VALUES ('asset1', 'item1', 'aa/aa.mov', 'aa', 1, 'mov', 'h264', 'verified', 0);",
        )
        .expect("one asset");

        let again = conn.execute_batch(
            "INSERT INTO media_assets (
                 id, media_item_id, object_key, sha256, byte_size, container,
                 codec, validation_state, created_at
             ) VALUES ('asset2', 'item1', 'aa/aa.mov', 'aa', 1, 'mov', 'h264', 'verified', 0);",
        );
        assert!(again.is_err(), "the same bytes are one asset");
    }

    #[test]
    fn two_assets_holding_the_same_bytes_become_one() {
        // Deduplication used to be by (sha256, profile_version), so two rows
        // could hold the same content. The new index cannot be built over them,
        // and a migration that cannot run is a server that will not start.
        let mut conn = a_database_from_the_first_release();
        conn.execute_batch(
            "INSERT INTO media_items (
                 id, display_name, original_filename, kind, state, has_alpha, created_at
             ) VALUES ('item1', 'Intro', 'intro.mov', 'video', 'ready', 0, 0);
             INSERT INTO media_assets (
                 id, media_item_id, profile, profile_version, object_key, sha256,
                 byte_size, container, codec, validation_state, created_at
             ) VALUES ('old', 'item1', 'prores_422', 1, 'aa/aa.mov', 'aa',
                       1, 'mov', 'prores', 'verified', 0),
                      ('new', 'item1', 'prores_422', 2, 'aa/aa.mov', 'aa',
                       1, 'mov', 'prores', 'verified', 0);",
        )
        .expect("two rows, one content");

        apply_pending(&mut conn).expect("migrates");

        let kept: String = conn
            .query_row("SELECT id FROM media_assets", [], |row| row.get(0))
            .expect("exactly one row is left");
        assert_eq!(kept, "new", "the one playback would have chosen");
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused() {
        let mut conn = Connection::open_in_memory().expect("in-memory database");
        apply_pending(&mut conn).expect("migrates");
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, 'future', 0)",
            [target_version() + 5],
        )
        .expect("insert");

        let error = apply_pending(&mut conn).expect_err("newer than this build");
        assert!(matches!(error, DbError::DatabaseTooNew { .. }));
        assert!(error.to_string().contains("update OpenVideoServer"));
    }

    #[test]
    fn a_failing_migration_leaves_the_version_untouched() {
        let mut conn = Connection::open_in_memory().expect("in-memory database");
        apply_pending(&mut conn).expect("migrates");
        let before = current_version(&conn).expect("version");

        let broken = Migration {
            version: 99,
            name: "broken",
            sql: "NOT VALID SQL;",
        };
        let transaction = conn.transaction().expect("transaction");
        assert!(apply_one(&transaction, &broken).is_err());
        drop(transaction);

        assert_eq!(current_version(&conn).expect("version"), before);
    }
}
