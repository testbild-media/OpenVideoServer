//! Show persistence.
//!
//! A show is stored as a JSON document with its schema version beside it in a
//! real column, so the version can be read without decoding the document and a
//! show from an unsupported version is refused with an explanation rather
//! than half-parsed.
//!
//! This module accepts only [`ovs_types::config`] types. There is no entry point
//! that takes live state, which is what keeps a transient DMX value from being
//! written into a saved show.

use ovs_types::config::Show;
use ovs_types::id::ShowId;
use ovs_types::media::{check_folder, folder_ancestry};
use ovs_types::time::Timestamp;
use ovs_types::version::check_show_schema_version;
use rusqlite::{params, OptionalExtension};

use crate::database::Database;
use crate::error::{DbError, Result};
use crate::media::one_problem;

/// Enough of a show to list it without decoding the whole document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowSummary {
    /// Stable identifier.
    pub id: ShowId,
    /// Operator-visible name.
    pub name: String,
    /// Schema version the stored document uses.
    pub schema_version: u32,
    /// When the show was created.
    pub created_at: Timestamp,
    /// When the show was last written.
    pub updated_at: Timestamp,
    /// Where it is filed in this server's list of shows.
    ///
    /// Slash-separated, empty for the root. **Not on the document** (owner,
    /// 2026-09-23): where a show is filed is a fact about this server's
    /// list rather than about the show, so a show carried to another
    /// machine arrives at the root of whatever that machine has.
    pub folder: String,
}

impl ShowSummary {
    /// Returns true when this build can open the show.
    #[must_use]
    pub fn is_loadable(&self) -> bool {
        check_show_schema_version(self.schema_version).is_ok()
    }
}

impl Database {
    /// Writes a show, creating or replacing it.
    ///
    /// The show is validated first. A show that cannot be loaded back must
    /// never reach storage, so an invalid one is refused with every problem
    /// listed rather than with the first.
    ///
    /// `updated_at` is set from the clock; the caller does not have to remember to.
    /// It is stamped into the stored document as well as into the column beside
    /// it, because those are two records of the same fact and a show is read
    /// back out of the first while the list is ordered by the second. Storing
    /// whatever timestamp the caller happened to be holding meant a show
    /// reported one "last written" to whoever opened it and another to the
    /// show list — in practice the time of the *previous* save, because a
    /// client writes the answer to this call into its own copy and sends it back
    /// with the next one.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] when the show violates an invariant, and
    /// a database error if the write fails.
    pub fn save_show(&mut self, show: &Show) -> Result<Timestamp> {
        show.validate().map_err(DbError::Invalid)?;

        let now = Timestamp::now();
        let stamped = Show {
            updated_at: now,
            ..show.clone()
        };

        let document = serde_json::to_string(&stamped).map_err(|source| DbError::Encode {
            entity: "show",
            source,
        })?;

        self.connection().execute(
            "INSERT INTO shows (id, name, schema_version, document, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                 name           = excluded.name,
                 schema_version = excluded.schema_version,
                 document       = excluded.document,
                 updated_at     = excluded.updated_at",
            params![
                show.id.to_string(),
                show.name,
                show.schema_version,
                document,
                show.created_at.as_millis(),
                now.as_millis(),
            ],
        )?;

        Ok(now)
    }

    /// Loads a show by identifier.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] when no such show exists, and
    /// [`DbError::Schema`] when it was written by an unsupported version.
    pub fn load_show(&self, id: ShowId) -> Result<Show> {
        let row: Option<(u32, String)> = self
            .connection()
            .query_row(
                "SELECT schema_version, document FROM shows WHERE id = ?1",
                params![id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (schema_version, document) = row.ok_or_else(|| DbError::NotFound {
            entity: "show",
            id: id.to_string(),
        })?;

        // Checked before decoding: a document from a newer build may contain
        // fields this one would silently drop.
        check_show_schema_version(schema_version)?;

        serde_json::from_str(&document).map_err(|source| DbError::Decode {
            entity: "show",
            source,
        })
    }

    /// Lists shows, most recently updated first.
    ///
    /// Includes shows this build cannot open, so the operator sees that they
    /// exist rather than finding them silently missing.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn list_shows(&self) -> Result<Vec<ShowSummary>> {
        let mut statement = self.connection().prepare(
            "SELECT id, name, schema_version, created_at, updated_at, folder
             FROM shows ORDER BY updated_at DESC",
        )?;

        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        rows.into_iter()
            .map(
                |(id, name, schema_version, created_at, updated_at, folder)| {
                    Ok(ShowSummary {
                        id: id.parse().map_err(|_| {
                            DbError::Conflict(format!("show row has a malformed id: {id}"))
                        })?,
                        name,
                        schema_version,
                        created_at: Timestamp::from_millis(created_at),
                        updated_at: Timestamp::from_millis(updated_at),
                        folder,
                    })
                },
            )
            .collect()
    }

    /// Every folder the show list holds, in path order.
    ///
    /// Sent whole rather than worked out from the shows. An operator makes
    /// a folder in order to put a show in it, and one derived from its contents
    /// could not exist between being made and being filled — the same rule the
    /// library's folders keep.
    ///
    /// # Errors
    ///
    /// Returns [`DbError`] when the table cannot be read.
    pub fn list_show_folders(&self) -> Result<Vec<String>> {
        let mut statement = self
            .connection()
            .prepare("SELECT path FROM show_folders ORDER BY path")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Makes a show folder, and every folder above it that is not there yet.
    ///
    /// Making one that already exists changes nothing and is not an error: a
    /// tree with a hole in it cannot be drawn, and an operator who typed a path
    /// two levels deep meant both levels.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] when the path is not one the list allows.
    pub fn create_show_folder(&mut self, path: &str) -> Result<()> {
        check_folder(path).map_err(|error| one_problem("path", error.to_string()))?;
        if path.is_empty() {
            // The root is always there and is not a row.
            return Ok(());
        }

        let now = Timestamp::now().as_millis();
        let transaction = self.connection_mut().transaction()?;
        for ancestor in folder_ancestry(path) {
            transaction.execute(
                "INSERT INTO show_folders (path, created_at) VALUES (?1, ?2)
                 ON CONFLICT (path) DO NOTHING",
                params![ancestor, now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Removes a show folder that has nothing in it.
    ///
    /// Nothing in it means no show filed there and no folder under it.
    /// Deleting one that still holds shows would either orphan them or delete
    /// somebody's work because they tidied up, and neither is a thing to do
    /// without being asked.
    ///
    /// Returns whether a folder was removed.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Conflict`] when the folder still holds something.
    pub fn delete_show_folder(&mut self, path: &str) -> Result<bool> {
        let filed: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM shows WHERE folder = ?1 OR SUBSTR(folder, 1, LENGTH(?2)) = ?2",
            params![path, format!("{path}/")],
            |row| row.get(0),
        )?;
        if filed > 0 {
            return Err(DbError::Conflict(format!(
                "{path} still holds {filed} show(s)"
            )));
        }

        // A prefix compared exactly rather than a `LIKE`. This was `LIKE ?1 ||
        // '/'` with no wildcard after it, which matched only a folder called
        // `<path>/` — none — so a folder with folders under it was deleted
        // out from under them. And `LIKE` would have been wrong as a fix:
        // `_` in a name matches any character, and it ignores case.
        let children: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM show_folders WHERE SUBSTR(path, 1, LENGTH(?1)) = ?1",
            params![format!("{path}/")],
            |row| row.get(0),
        )?;
        if children > 0 {
            return Err(DbError::Conflict(format!(
                "{path} still holds {children} folder(s)"
            )));
        }

        let removed = self
            .connection()
            .execute("DELETE FROM show_folders WHERE path = ?1", params![path])?;
        Ok(removed > 0)
    }

    /// Renames a show folder, which moves everything under it.
    ///
    /// The path *is* the identity, so a rename is a rewrite of every path that
    /// begins with it — the folder, the folders under it, and the shows
    /// filed in any of them — in one transaction.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] for a path the list does not allow, and
    /// [`DbError::Conflict`] when the folder is not there or the new path would
    /// be inside the old one.
    pub fn rename_show_folder(&mut self, from: &str, to: &str) -> Result<()> {
        for path in [from, to] {
            check_folder(path).map_err(|error| one_problem("path", error.to_string()))?;
        }
        if from.is_empty() || to.is_empty() {
            return Err(DbError::Conflict(
                "the root is not a folder and cannot be renamed".to_owned(),
            ));
        }
        if to == from || to.starts_with(&format!("{from}/")) {
            // Moving a folder inside itself would leave the rows pointing at a
            // path that no longer has a parent.
            return Err(DbError::Conflict(format!("{to} is {from} or is inside it")));
        }

        let taken: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM show_folders WHERE path = ?1",
            params![to],
            |row| row.get(0),
        )?;
        if taken > 0 {
            return Err(DbError::Conflict(format!("there is already a folder {to}")));
        }

        let under = format!("{from}/");
        let transaction = self.connection_mut().transaction()?;

        // Everything *above* the new path, and not the path itself: the row for
        // that is the one being renamed, and making it first would collide with
        // the rename a line later.
        let now = Timestamp::now().as_millis();
        if let Some((parent, _)) = to.rsplit_once('/') {
            for ancestor in folder_ancestry(parent) {
                transaction.execute(
                    "INSERT INTO show_folders (path, created_at) VALUES (?1, ?2)
                     ON CONFLICT (path) DO NOTHING",
                    params![ancestor, now],
                )?;
            }
        }

        transaction.execute(
            "UPDATE show_folders SET path = ?2 WHERE path = ?1",
            params![from, to],
        )?;
        transaction.execute(
            "UPDATE show_folders
                SET path = ?2 || SUBSTR(path, LENGTH(?1) + 1)
              WHERE SUBSTR(path, 1, LENGTH(?1)) = ?1",
            params![under, format!("{to}/")],
        )?;
        transaction.execute(
            "UPDATE shows SET folder = ?2 WHERE folder = ?1",
            params![from, to],
        )?;
        transaction.execute(
            "UPDATE shows
                SET folder = ?2 || SUBSTR(folder, LENGTH(?1) + 1)
              WHERE SUBSTR(folder, 1, LENGTH(?1)) = ?1",
            params![under, format!("{to}/")],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Files one show in a folder. The document is untouched.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] for a path the list does not allow, and
    /// [`DbError::Conflict`] when the folder does not exist or the show does
    /// not.
    pub fn file_show(&mut self, show_id: ShowId, folder: &str) -> Result<()> {
        check_folder(folder).map_err(|error| one_problem("folder", error.to_string()))?;

        if !folder.is_empty() {
            let exists: i64 = self.connection().query_row(
                "SELECT COUNT(*) FROM show_folders WHERE path = ?1",
                params![folder],
                |row| row.get(0),
            )?;
            if exists == 0 {
                return Err(DbError::Conflict(format!("there is no folder {folder}")));
            }
        }

        let moved = self.connection().execute(
            "UPDATE shows SET folder = ?2 WHERE id = ?1",
            params![show_id.to_string(), folder],
        )?;
        if moved == 0 {
            return Err(DbError::Conflict(format!("there is no show {show_id}")));
        }
        Ok(())
    }

    /// Deletes a show and its live snapshot.
    ///
    /// Returns true when a show was removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete fails.
    pub fn delete_show(&mut self, id: ShowId) -> Result<bool> {
        let removed = self
            .connection()
            .execute("DELETE FROM shows WHERE id = ?1", params![id.to_string()])?;
        Ok(removed > 0)
    }

    /// Returns true when a show with this identifier exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn show_exists(&self, id: ShowId) -> Result<bool> {
        let found: Option<i64> = self
            .connection()
            .query_row(
                "SELECT 1 FROM shows WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::{Layer, Slot};
    use ovs_types::id::MediaItemId;

    use super::*;

    fn show() -> Show {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        show.layers.push(layer);
        show
    }

    #[test]
    fn a_show_folder_with_folders_under_it_is_not_deleted() {
        // It was, because the check looked for a folder called `<path>/`.
        let mut database = Database::open_in_memory().expect("opens");
        database.create_show_folder("Tours/2026").expect("creates");
        assert!(database.delete_show_folder("Tours").is_err());
        assert!(database
            .list_show_folders()
            .expect("lists")
            .iter()
            .any(|path| path == "Tours/2026"));
    }

    #[test]
    fn moving_a_show_folder_leaves_a_neighbour_of_a_similar_name_alone() {
        let mut database = Database::open_in_memory().expect("opens");
        for path in ["A_B/x", "AxB/y", "Archive"] {
            database.create_show_folder(path).expect("creates");
        }
        database
            .rename_show_folder("A_B", "Archive/A_B")
            .expect("moves");
        let folders = database.list_show_folders().expect("lists");
        assert!(folders.iter().any(|path| path == "AxB/y"), "{folders:?}");
        assert!(
            folders.iter().any(|path| path == "Archive/A_B/x"),
            "{folders:?}"
        );
    }

    #[test]
    fn a_show_round_trips() {
        let mut database = Database::open_in_memory().expect("opens");
        let show = show();
        let updated_at = database.save_show(&show).expect("saves");

        // Everything the caller wrote, and the one field it does not own: when
        // the show was written is the database's answer, not the caller's,
        // and the document has to say the same thing the answer did.
        let loaded = database.load_show(show.id).expect("loads");
        assert_eq!(loaded.updated_at, updated_at);
        assert_eq!(loaded, Show { updated_at, ..show });
    }

    #[test]
    fn a_saved_show_reads_back_with_the_time_the_save_answered_with() {
        // The two records of one fact. A client keeps what a save answered with
        // and sends it back on the next one, so a document that stored the
        // caller's timestamp would report the previous save's time to whoever
        // opened it while the list showed the latest — and the editor and the
        // show list would disagree about the same show.
        let mut database = Database::open_in_memory().expect("opens");
        let mut show = show();
        database.save_show(&show).expect("saves");

        show.name = "Renamed".to_owned();
        let second = database.save_show(&show).expect("saves again");

        let loaded = database.load_show(show.id).expect("loads");
        let listed = database.list_shows().expect("lists");
        assert_eq!(loaded.updated_at, second);
        assert_eq!(listed[0].updated_at, second);
    }

    #[test]
    fn saving_twice_updates_rather_than_duplicating() {
        let mut database = Database::open_in_memory().expect("opens");
        let mut show = show();
        database.save_show(&show).expect("saves");

        show.name = "Renamed".to_owned();
        database.save_show(&show).expect("saves again");

        assert_eq!(database.list_shows().expect("lists").len(), 1);
        assert_eq!(database.load_show(show.id).expect("loads").name, "Renamed");
    }

    #[test]
    fn saving_preserves_the_original_creation_time() {
        let mut database = Database::open_in_memory().expect("opens");
        let show = show();
        database.save_show(&show).expect("saves");
        database.save_show(&show).expect("saves again");

        let summary = database.list_shows().expect("lists").remove(0);
        assert_eq!(summary.created_at, show.created_at);
    }

    #[test]
    fn an_invalid_show_never_reaches_storage() {
        let mut database = Database::open_in_memory().expect("opens");
        let mut show = show();
        // Slot 0 is permanently black and cannot be configured.
        show.layers[0]
            .slots
            .push(Slot::media(0, MediaItemId::new()));

        let error = database.save_show(&show).expect_err("refused");
        assert!(matches!(error, DbError::Invalid(_)));
        assert!(error.to_string().contains("slot 0 is permanently Black"));
        assert!(database.list_shows().expect("lists").is_empty());
    }

    #[test]
    fn loading_an_unknown_show_says_so() {
        let database = Database::open_in_memory().expect("opens");
        let error = database.load_show(ShowId::new()).expect_err("not found");
        assert!(matches!(error, DbError::NotFound { entity: "show", .. }));
    }

    #[test]
    fn a_show_from_a_newer_build_is_refused_before_it_is_decoded() {
        let mut database = Database::open_in_memory().expect("opens");
        let show = show();
        database.save_show(&show).expect("saves");

        database
            .connection()
            .execute(
                "UPDATE shows SET schema_version = ?1 WHERE id = ?2",
                params![ovs_types::SHOW_SCHEMA_VERSION + 1, show.id.to_string()],
            )
            .expect("bump");

        let error = database.load_show(show.id).expect_err("too new");
        assert!(matches!(error, DbError::Schema(_)));
        assert!(error.to_string().contains("update OpenVideoServer"));
    }

    #[test]
    fn a_show_this_build_cannot_open_still_appears_in_the_list() {
        // Hiding it would make it look deleted, which is worse than showing it
        // as unopenable.
        let mut database = Database::open_in_memory().expect("opens");
        let show = show();
        database.save_show(&show).expect("saves");
        database
            .connection()
            .execute(
                "UPDATE shows SET schema_version = ?1 WHERE id = ?2",
                params![ovs_types::SHOW_SCHEMA_VERSION + 1, show.id.to_string()],
            )
            .expect("bump");

        let summaries = database.list_shows().expect("lists");
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].is_loadable());
    }

    #[test]
    fn shows_are_listed_most_recently_updated_first() {
        let mut database = Database::open_in_memory().expect("opens");
        let first = Show::new("First");
        let second = Show::new("Second");
        database.save_show(&first).expect("saves");
        database.save_show(&second).expect("saves");

        // Both may land in the same millisecond, so nudge the older one back.
        database
            .connection()
            .execute(
                "UPDATE shows SET updated_at = updated_at - 1000 WHERE id = ?1",
                params![first.id.to_string()],
            )
            .expect("nudge");

        let names: Vec<String> = database
            .list_shows()
            .expect("lists")
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["Second", "First"]);
    }

    #[test]
    fn deleting_reports_whether_anything_was_removed() {
        let mut database = Database::open_in_memory().expect("opens");
        let show = show();
        database.save_show(&show).expect("saves");

        assert!(database.delete_show(show.id).expect("deletes"));
        assert!(!database.delete_show(show.id).expect("already gone"));
        assert!(!database.show_exists(show.id).expect("checks"));
    }

    #[test]
    fn a_large_show_survives_the_round_trip_intact() {
        let mut database = Database::open_in_memory().expect("opens");
        let mut show = Show::new("Big Show");
        for order in 0..32 {
            let mut layer = Layer::new(format!("Layer {order}"), order);
            for index in 1..=84 {
                layer.slots.push(Slot::media(index, MediaItemId::new()));
            }
            show.layers.push(layer);
        }
        let updated_at = database.save_show(&show).expect("saves");
        assert_eq!(
            database.load_show(show.id).expect("loads"),
            Show { updated_at, ..show }
        );
    }
}
