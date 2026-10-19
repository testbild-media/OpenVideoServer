//! Media persistence.
//!
//! The rule this module exists to enforce: a partial upload never becomes valid
//! content. That is not a convention here. Registration
//! is a single transaction that writes the asset and flips the item to `Ready`
//! together, and the query that finds a playable asset filters on verification,
//! so an unverified asset cannot be handed to a decoder even if some other code
//! path asks for one.

use ovs_types::error::{ValidationError, ValidationErrors};
use ovs_types::geometry::{FrameRate, SizePx};
use ovs_types::id::MediaItemId;
use ovs_types::media::{
    check_folder, folder_ancestry, AssetValidation, MediaAsset, MediaItem, MediaKind, MediaState,
};
use ovs_types::time::Timestamp;
use rusqlite::{params, OptionalExtension, Row};

use crate::database::Database;
use crate::error::{DbError, Result};

fn kind_to_sql(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Video => "video",
        MediaKind::Image => "image",
    }
}

fn kind_from_sql(value: &str) -> Result<MediaKind> {
    match value {
        "video" => Ok(MediaKind::Video),
        "image" => Ok(MediaKind::Image),
        other => Err(DbError::Conflict(format!("unknown media kind {other:?}"))),
    }
}

fn state_to_sql(state: MediaState) -> &'static str {
    match state {
        MediaState::Importing => "importing",
        MediaState::Processing => "processing",
        MediaState::Uploading => "uploading",
        MediaState::Verifying => "verifying",
        MediaState::Ready => "ready",
        MediaState::Failed => "failed",
    }
}

fn state_from_sql(value: &str) -> Result<MediaState> {
    match value {
        "importing" => Ok(MediaState::Importing),
        "processing" => Ok(MediaState::Processing),
        "uploading" => Ok(MediaState::Uploading),
        "verifying" => Ok(MediaState::Verifying),
        "ready" => Ok(MediaState::Ready),
        "failed" => Ok(MediaState::Failed),
        other => Err(DbError::Conflict(format!("unknown media state {other:?}"))),
    }
}

/// SQLite has no unsigned integer type: `INTEGER` is always signed 64-bit.
///
/// The two unsigned values stored here are a duration in milliseconds and a file
/// size in bytes, both many orders of magnitude below `i64::MAX`. Converting
/// explicitly rather than casting means a value that somehow does not fit is
/// reported as corrupt data instead of silently wrapping to a negative number.
/// One field-level problem, as the error type the API layer already knows how
/// to turn into a refusal an operator can read.
pub(crate) fn one_problem(path: &str, message: impl Into<String>) -> DbError {
    let mut errors = ValidationErrors::new();
    errors.push(ValidationError::new(path, message));
    DbError::Invalid(errors)
}

fn u64_to_sql(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| DbError::Conflict(format!("{field} is too large to store: {value}")))
}

fn u64_from_sql(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| DbError::Conflict(format!("{field} is stored as a negative number: {value}")))
}

/// A media row as SQLite hands it over.
///
/// A rusqlite row callback can only fail with `rusqlite::Error`, so the columns
/// that need a fallible domain conversion are carried out as strings and turned
/// into typed values by [`MediaItemRow::into_item`], which can report a proper
/// error.
struct MediaItemRow {
    id: String,
    kind: String,
    state: String,
    display_name: String,
    original_filename: String,
    folder: String,
    duration_ms: Option<i64>,
    dimensions: Option<SizePx>,
    frame_rate: Option<FrameRate>,
    has_alpha: bool,
    failure_reason: Option<String>,
    created_at: Timestamp,
}

impl MediaItemRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        let width: Option<u32> = row.get("width")?;
        let height: Option<u32> = row.get("height")?;
        let fps_numerator: Option<u32> = row.get("fps_numerator")?;
        let fps_denominator: Option<u32> = row.get("fps_denominator")?;

        Ok(Self {
            id: row.get("id")?,
            kind: row.get("kind")?,
            state: row.get("state")?,
            display_name: row.get("display_name")?,
            original_filename: row.get("original_filename")?,
            folder: row.get("folder")?,
            duration_ms: row.get("duration_ms")?,
            dimensions: width.zip(height).map(|(w, h)| SizePx::new(w, h)),
            frame_rate: fps_numerator
                .zip(fps_denominator)
                .map(|(numerator, denominator)| FrameRate::new(numerator, denominator)),
            has_alpha: row.get("has_alpha")?,
            failure_reason: row.get("failure_reason")?,
            created_at: Timestamp::from_millis(row.get("created_at")?),
        })
    }

    fn into_item(self) -> Result<MediaItem> {
        Ok(MediaItem {
            id: self.id.parse().map_err(|_| {
                DbError::Conflict(format!("media row has a malformed id: {}", self.id))
            })?,
            kind: kind_from_sql(&self.kind)?,
            state: state_from_sql(&self.state)?,
            display_name: self.display_name,
            original_filename: self.original_filename,
            folder: self.folder,
            duration_ms: self
                .duration_ms
                .map(|value| u64_from_sql(value, "duration_ms"))
                .transpose()?,
            dimensions: self.dimensions,
            frame_rate: self.frame_rate,
            has_alpha: self.has_alpha,
            failure_reason: self.failure_reason,
            created_at: self.created_at,
        })
    }
}

impl Database {
    /// Inserts or updates a media item.
    ///
    /// Used while the item is still moving through the pipeline. Registration of
    /// finished content goes through [`Database::register_media`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] when the item is internally inconsistent.
    pub fn upsert_media_item(&mut self, item: &MediaItem) -> Result<()> {
        item.validate().map_err(DbError::Invalid)?;

        self.connection().execute(
            "INSERT INTO media_items (
                 id, display_name, original_filename, kind, state, duration_ms,
                 width, height, fps_numerator, fps_denominator, has_alpha,
                 failure_reason, created_at, folder
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (id) DO UPDATE SET
                 display_name    = excluded.display_name,
                 kind            = excluded.kind,
                 state           = excluded.state,
                 duration_ms     = excluded.duration_ms,
                 width           = excluded.width,
                 height          = excluded.height,
                 fps_numerator   = excluded.fps_numerator,
                 fps_denominator = excluded.fps_denominator,
                 has_alpha       = excluded.has_alpha,
                 failure_reason  = excluded.failure_reason,
                 folder          = excluded.folder",
            params![
                item.id.to_string(),
                item.display_name,
                item.original_filename,
                kind_to_sql(item.kind),
                state_to_sql(item.state),
                item.duration_ms
                    .map(|value| u64_to_sql(value, "duration_ms"))
                    .transpose()?,
                item.dimensions.map(|size| size.width),
                item.dimensions.map(|size| size.height),
                item.frame_rate.map(|rate| rate.numerator),
                item.frame_rate.map(|rate| rate.denominator),
                item.has_alpha,
                item.failure_reason,
                item.created_at.as_millis(),
                // Where it is filed. Written here as well as by `file_media`,
                // because an upload declares its folder and rebuilding the row
                // without it filed every import at the root however carefully
                // it was dropped into a folder (owner, 2026-09-23).
                item.folder,
            ],
        )?;
        Ok(())
    }

    /// Registers verified content atomically.
    ///
    /// Writes the asset and moves the item to `Ready` in one transaction, so
    /// there is no instant at which a show could reference an item that is
    /// ready but has no asset behind it.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Conflict`] when the asset is not verified, when the
    /// item is not in a state that can become ready, or when the same content
    /// is already registered for this profile version.
    pub fn register_media(&mut self, item: &MediaItem, asset: &MediaAsset) -> Result<()> {
        if !asset.is_playable() {
            return Err(DbError::Conflict(
                "refusing to register an asset that has not been verified".to_owned(),
            ));
        }
        if asset.media_item_id != item.id {
            return Err(DbError::Conflict(
                "asset does not belong to the item being registered".to_owned(),
            ));
        }

        let ready = MediaItem {
            state: MediaState::Ready,
            ..item.clone()
        };
        ready.validate().map_err(DbError::Invalid)?;

        let transaction = self.connection_mut().transaction()?;

        let existing: Option<String> = transaction
            .query_row(
                "SELECT id FROM media_assets WHERE sha256 = ?1",
                params![asset.sha256],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            return Err(DbError::Conflict(format!(
                "this content is already registered as asset {existing}"
            )));
        }

        transaction.execute(
            "INSERT INTO media_items (
                 id, display_name, original_filename, kind, state, duration_ms,
                 width, height, fps_numerator, fps_denominator, has_alpha,
                 failure_reason, created_at, folder
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (id) DO UPDATE SET
                 display_name    = excluded.display_name,
                 kind            = excluded.kind,
                 state           = excluded.state,
                 duration_ms     = excluded.duration_ms,
                 width           = excluded.width,
                 height          = excluded.height,
                 fps_numerator   = excluded.fps_numerator,
                 fps_denominator = excluded.fps_denominator,
                 has_alpha       = excluded.has_alpha,
                 failure_reason  = NULL,
                 folder          = excluded.folder",
            params![
                ready.id.to_string(),
                ready.display_name,
                ready.original_filename,
                kind_to_sql(ready.kind),
                state_to_sql(ready.state),
                ready
                    .duration_ms
                    .map(|value| u64_to_sql(value, "duration_ms"))
                    .transpose()?,
                ready.dimensions.map(|size| size.width),
                ready.dimensions.map(|size| size.height),
                ready.frame_rate.map(|rate| rate.numerator),
                ready.frame_rate.map(|rate| rate.denominator),
                ready.has_alpha,
                Option::<String>::None,
                ready.created_at.as_millis(),
                ready.folder,
            ],
        )?;

        let (validated_at, validation_reason) = match &asset.validation {
            AssetValidation::Verified { at } => (Some(at.as_millis()), None),
            AssetValidation::Pending => (None, None),
            AssetValidation::Failed { reason } => (None, Some(reason.clone())),
        };

        transaction.execute(
            "INSERT INTO media_assets (
                 id, media_item_id, object_key, sha256,
                 byte_size, container, codec, validation_state, validated_at,
                 validation_reason, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                asset.id.to_string(),
                asset.media_item_id.to_string(),
                asset.object_key,
                asset.sha256,
                u64_to_sql(asset.byte_size, "byte_size")?,
                asset.container,
                asset.codec,
                "verified",
                validated_at,
                validation_reason,
                asset.created_at.as_millis(),
            ],
        )?;

        transaction.commit()?;
        Ok(())
    }

    /// Every folder the library holds, in path order.
    ///
    /// Stored rather than derived from the items, because an operator makes a
    /// folder in order to put things in it: a derived one could not exist while
    /// it was empty (owner, 2026-09-23).
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn list_media_folders(&self) -> Result<Vec<String>> {
        let mut statement = self
            .connection()
            .prepare("SELECT path FROM media_folders ORDER BY path")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Makes a folder, and every folder above it that is not there yet.
    ///
    /// Making `Intros/2026` when `Intros` does not exist makes both, because a
    /// tree with a hole in it cannot be drawn. Making one that already exists
    /// changes nothing and is not an error.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] when the path is not one the library
    /// allows.
    pub fn create_media_folder(&mut self, path: &str) -> Result<()> {
        check_folder(path).map_err(|error| one_problem("path", error.to_string()))?;
        if path.is_empty() {
            // The root is always there and is not a row.
            return Ok(());
        }

        let now = Timestamp::now().as_millis();
        let transaction = self.connection_mut().transaction()?;
        for ancestor in folder_ancestry(path) {
            transaction.execute(
                "INSERT INTO media_folders (path, created_at) VALUES (?1, ?2)
                 ON CONFLICT (path) DO NOTHING",
                params![ancestor, now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Removes a folder that has nothing in it.
    ///
    /// Nothing in it means no item filed there and no folder under it. Deleting
    /// a folder that still holds things would either orphan them or delete
    /// somebody's media because they tidied up, and neither is a thing to do
    /// without being asked.
    ///
    /// Returns whether a folder was removed.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Conflict`] when the folder still holds something.
    pub fn delete_media_folder(&mut self, path: &str) -> Result<bool> {
        let under = format!("{path}/");
        let items: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM media_items WHERE folder = ?1 OR SUBSTR(folder, 1, LENGTH(?2)) = ?2",
            params![path, under],
            |row| row.get(0),
        )?;
        if items > 0 {
            return Err(DbError::Conflict(format!(
                "{path} still holds {items} item(s)"
            )));
        }

        let children: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM media_folders WHERE SUBSTR(path, 1, LENGTH(?1)) = ?1",
            params![under],
            |row| row.get(0),
        )?;
        if children > 0 {
            return Err(DbError::Conflict(format!(
                "{path} still holds {children} folder(s)"
            )));
        }

        let removed = self
            .connection()
            .execute("DELETE FROM media_folders WHERE path = ?1", params![path])?;
        Ok(removed > 0)
    }

    /// Renames or moves one item, or both.
    ///
    /// **Neither touches the file.** A display name is a label and a folder is
    /// a label; the asset keeps its object key and the content hash it is found
    /// by, so a show that plays this item goes on playing it.
    ///
    /// A `None` leaves that half alone, so a rename need not send a folder and
    /// a move need not send a name.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] when there is no such item,
    /// [`DbError::Invalid`] when the name or path is not one the library
    /// allows, and [`DbError::Conflict`] when the folder does not exist.
    pub fn file_media_item(
        &mut self,
        id: MediaItemId,
        display_name: Option<&str>,
        folder: Option<&str>,
    ) -> Result<MediaItem> {
        if let Some(name) = display_name {
            if name.trim().is_empty() {
                return Err(one_problem("display_name", "must not be empty"));
            }
        }
        if let Some(path) = folder {
            check_folder(path).map_err(|error| one_problem("folder", error.to_string()))?;
            // Filed somewhere that is not there is filed nowhere an operator
            // can navigate to, which is the same as losing it.
            if !path.is_empty() {
                let exists: i64 = self.connection().query_row(
                    "SELECT COUNT(*) FROM media_folders WHERE path = ?1",
                    params![path],
                    |row| row.get(0),
                )?;
                if exists == 0 {
                    return Err(DbError::Conflict(format!("there is no folder {path}")));
                }
            }
        }

        let changed = self.connection().execute(
            "UPDATE media_items
                SET display_name = COALESCE(?2, display_name),
                    folder       = COALESCE(?3, folder)
              WHERE id = ?1",
            params![id.to_string(), display_name, folder],
        )?;
        if changed == 0 {
            return Err(DbError::NotFound {
                entity: "media item",
                id: id.to_string(),
            });
        }
        self.load_media_item(id)
    }

    /// Moves every item of one folder, and of the folders under it, to another.
    ///
    /// What renaming a folder is: the folder rows are rewritten and every item
    /// filed under the old path follows. One statement per table rather than a
    /// read-modify-write, so a library of ten thousand clips renames as quickly
    /// as one of ten.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Invalid`] for a path the library does not allow and
    /// [`DbError::Conflict`] when something is already at the new path.
    pub fn rename_media_folder(&mut self, from: &str, to: &str) -> Result<()> {
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
            "SELECT COUNT(*) FROM media_folders WHERE path = ?1",
            params![to],
            |row| row.get(0),
        )?;
        if taken > 0 {
            return Err(DbError::Conflict(format!("there is already a folder {to}")));
        }

        let under = format!("{from}/");
        let transaction = self.connection_mut().transaction()?;
        transaction.execute(
            "UPDATE media_folders SET path = ?2 WHERE path = ?1",
            params![from, to],
        )?;
        transaction.execute(
            "UPDATE media_folders
                SET path = ?2 || SUBSTR(path, LENGTH(?1) + 1)
              WHERE SUBSTR(path, 1, LENGTH(?1)) = ?1",
            params![under, format!("{to}/")],
        )?;
        transaction.execute(
            "UPDATE media_items SET folder = ?2 WHERE folder = ?1",
            params![from, to],
        )?;
        transaction.execute(
            "UPDATE media_items
                SET folder = ?2 || SUBSTR(folder, LENGTH(?1) + 1)
              WHERE SUBSTR(folder, 1, LENGTH(?1)) = ?1",
            params![under, format!("{to}/")],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Loads a media item.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] when no such item exists.
    pub fn load_media_item(&self, id: MediaItemId) -> Result<MediaItem> {
        let row = self
            .connection()
            .query_row(
                "SELECT * FROM media_items WHERE id = ?1",
                params![id.to_string()],
                MediaItemRow::read,
            )
            .optional()?;

        row.ok_or_else(|| DbError::NotFound {
            entity: "media item",
            id: id.to_string(),
        })?
        .into_item()
    }

    /// Lists the media library, most recently imported first.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn list_media_items(&self) -> Result<Vec<MediaItem>> {
        let mut statement = self
            .connection()
            .prepare("SELECT * FROM media_items ORDER BY created_at DESC, id DESC")?;
        let rows = statement
            .query_map([], MediaItemRow::read)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(MediaItemRow::into_item).collect()
    }

    /// Returns an asset a decoder may actually open.
    ///
    /// Only verified assets are returned. An item that is not `Ready`, or whose
    /// asset failed verification, yields `None` rather than a path that would
    /// open a partial file.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn playable_asset_for(&self, id: MediaItemId) -> Result<Option<MediaAsset>> {
        let row = self
            .connection()
            .query_row(
                "SELECT a.* FROM media_assets a
                 JOIN media_items i ON i.id = a.media_item_id
                 WHERE a.media_item_id = ?1
                   AND a.validation_state = 'verified'
                   AND i.state = 'ready'
                 ORDER BY a.created_at DESC
                 LIMIT 1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>("id")?,
                        row.get::<_, String>("media_item_id")?,
                        row.get::<_, String>("object_key")?,
                        row.get::<_, String>("sha256")?,
                        row.get::<_, i64>("byte_size")?,
                        row.get::<_, String>("container")?,
                        row.get::<_, String>("codec")?,
                        row.get::<_, Option<i64>>("validated_at")?,
                        row.get::<_, i64>("created_at")?,
                    ))
                },
            )
            .optional()?;

        let Some((
            asset_id,
            item_id,
            object_key,
            sha256,
            byte_size,
            container,
            codec,
            validated_at,
            created_at,
        )) = row
        else {
            return Ok(None);
        };

        Ok(Some(MediaAsset {
            id: asset_id
                .parse()
                .map_err(|_| DbError::Conflict(format!("malformed asset id {asset_id}")))?,
            media_item_id: item_id
                .parse()
                .map_err(|_| DbError::Conflict(format!("malformed item id {item_id}")))?,
            object_key,
            sha256,
            byte_size: u64_from_sql(byte_size, "byte_size")?,
            container,
            codec,
            validation: AssetValidation::Verified {
                at: Timestamp::from_millis(validated_at.unwrap_or(created_at)),
            },
            created_at: Timestamp::from_millis(created_at),
        }))
    }

    /// Finds the media item that already holds this content, if any.
    ///
    /// This is what stops an operator uploading the same show file twice.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn media_item_for_content(&self, sha256: &str) -> Result<Option<MediaItemId>> {
        let found: Option<String> = self
            .connection()
            .query_row(
                "SELECT media_item_id FROM media_assets WHERE sha256 = ?1",
                params![sha256],
                |row| row.get(0),
            )
            .optional()?;

        found
            .map(|id| {
                id.parse()
                    .map_err(|_| DbError::Conflict(format!("malformed item id {id}")))
            })
            .transpose()
    }

    /// Deletes a media item and its assets.
    ///
    /// Returns the object keys that are now unreferenced, so the caller can
    /// remove the files. Physical deletion happens only after this reference
    /// check, never before.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete fails.
    pub fn delete_media_item(&mut self, id: MediaItemId) -> Result<Vec<String>> {
        let transaction = self.connection_mut().transaction()?;

        // **The objects nothing else names**, not every object this item had.
        //
        // Today those are the same set: `media_assets_content` is unique on
        // `sha256`, an object key is derived from the hash, so one key belongs to
        // exactly one row and the subquery removes nothing. It is a query rather
        // than an assumption because the caller *deletes files* with the answer.
        // The index was unique on `(sha256, profile_version)` until migration
        // 0002, and two rows could then share one file; the day anything relaxes
        // it again — the same bytes filed twice, an asset per container — this is
        // the code that would otherwise delete a file another item still plays,
        // and that item would look healthy in the library until somebody
        // triggered it.
        //
        // Read inside the transaction and before the delete, because the cascade
        // takes the asset rows with the item and there would be nothing left to
        // ask afterwards.
        let keys = {
            let mut statement = transaction.prepare(
                "SELECT object_key FROM media_assets
                  WHERE media_item_id = ?1
                    AND object_key NOT IN (
                        SELECT object_key FROM media_assets WHERE media_item_id <> ?1
                    )",
            )?;
            let keys = statement
                .query_map(params![id.to_string()], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            keys
        };

        transaction.execute(
            "DELETE FROM media_items WHERE id = ?1",
            params![id.to_string()],
        )?;
        transaction.commit()?;
        Ok(keys)
    }

    /// Returns every storage object the library references.
    ///
    /// For the sweep that deletes the rest. A set, because the caller asks it
    /// once per file in the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn referenced_object_keys(&self) -> Result<std::collections::BTreeSet<String>> {
        let mut statement = self
            .connection()
            .prepare("SELECT object_key FROM media_assets")?;
        let keys = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<std::collections::BTreeSet<String>>>()?;
        Ok(keys)
    }

    /// Returns every show that references a media item, by name.
    ///
    /// Deleting media must warn where it is used rather than silently breaking
    /// an archived show.
    ///
    /// # Errors
    ///
    /// Returns an error if a stored show cannot be decoded.
    pub fn shows_using_media(&self, id: MediaItemId) -> Result<Vec<String>> {
        let mut using = Vec::new();
        for summary in self.list_shows()? {
            if !summary.is_loadable() {
                continue;
            }
            let show = self.load_show(summary.id)?;
            if show.referenced_media().contains(&id) {
                using.push(show.name);
            }
        }
        Ok(using)
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::{Layer, Show, Slot};
    use ovs_types::id::MediaAssetId;

    use super::*;

    fn verified_asset(item_id: MediaItemId, sha: &str) -> MediaAsset {
        MediaAsset {
            id: MediaAssetId::new(),
            media_item_id: item_id,
            object_key: format!("{}/{sha}.mov", &sha[..2]),
            sha256: sha.to_owned(),
            byte_size: 4096,
            container: "mov".to_owned(),
            codec: "prores".to_owned(),
            validation: AssetValidation::Verified {
                at: Timestamp::now(),
            },
            created_at: Timestamp::now(),
        }
    }

    fn analysed_item() -> MediaItem {
        MediaItem {
            kind: MediaKind::Video,
            state: MediaState::Verifying,
            duration_ms: Some(12_000),
            dimensions: Some(SizePx::new(1920, 1080)),
            frame_rate: Some(FrameRate::FPS_60),
            ..MediaItem::importing("Intro", "intro.mov")
        }
    }

    #[test]
    fn moving_a_folder_moves_what_is_under_it_and_nothing_else() {
        // The owner's names carry underscores, and `_` in a `LIKE` matches any
        // character — so moving `Visual_Smiley` used to take `VisualXSmiley`'s
        // folders with it, and moving `loops` took `Loops`', because `LIKE`
        // ignores case too.
        let mut database = Database::open_in_memory().expect("opens");
        for path in [
            "Visual_Smiley",
            "Visual_Smiley/Night",
            "VisualXSmiley",
            "VisualXSmiley/Day",
            "loops",
            "Loops",
            "Loops/Tunnel",
            "Archive",
        ] {
            database.create_media_folder(path).expect("creates");
        }
        let mut under = analysed_item();
        under.folder = "Visual_Smiley/Night".to_owned();
        database.upsert_media_item(&under).expect("stores");
        let mut neighbour = analysed_item();
        neighbour.folder = "VisualXSmiley/Day".to_owned();
        database.upsert_media_item(&neighbour).expect("stores");

        database
            .rename_media_folder("Visual_Smiley", "Archive/Visual_Smiley")
            .expect("moves");
        database
            .rename_media_folder("loops", "Archive/loops")
            .expect("moves");

        let folders = database.list_media_folders().expect("lists");
        for kept in [
            "VisualXSmiley",
            "VisualXSmiley/Day",
            "Loops",
            "Loops/Tunnel",
        ] {
            assert!(
                folders.iter().any(|path| path == kept),
                "{kept} stays: {folders:?}"
            );
        }
        assert!(folders
            .iter()
            .any(|path| path == "Archive/Visual_Smiley/Night"));
        assert_eq!(
            database.load_media_item(under.id).expect("loads").folder,
            "Archive/Visual_Smiley/Night"
        );
        assert_eq!(
            database
                .load_media_item(neighbour.id)
                .expect("loads")
                .folder,
            "VisualXSmiley/Day",
            "a neighbour whose name differs by one character stays where it was"
        );
    }

    #[test]
    fn a_folder_with_folders_under_it_is_not_deleted() {
        let mut database = Database::open_in_memory().expect("opens");
        database.create_media_folder("A_B").expect("creates");
        database.create_media_folder("AxB/child").expect("creates");
        // Nothing under A_B itself; AxB's child must not count as its.
        assert!(database.delete_media_folder("A_B").expect("deletes"));
        assert!(
            database.delete_media_folder("AxB").is_err(),
            "AxB holds a folder"
        );
    }

    #[test]
    fn registration_makes_an_item_ready_and_playable() {
        let mut database = Database::open_in_memory().expect("opens");
        let item = analysed_item();
        let asset = verified_asset(item.id, &"a".repeat(64));

        database.register_media(&item, &asset).expect("registers");

        let loaded = database.load_media_item(item.id).expect("loads");
        assert_eq!(loaded.state, MediaState::Ready);
        assert!(loaded.is_usable());
        assert!(database
            .playable_asset_for(item.id)
            .expect("queries")
            .is_some());
    }

    #[test]
    fn an_unverified_asset_is_never_registered() {
        let mut database = Database::open_in_memory().expect("opens");
        let item = analysed_item();
        let asset = MediaAsset {
            validation: AssetValidation::Pending,
            ..verified_asset(item.id, &"b".repeat(64))
        };

        let error = database.register_media(&item, &asset).expect_err("refused");
        assert!(error.to_string().contains("has not been verified"));
        assert!(database.list_media_items().expect("lists").is_empty());
    }

    #[test]
    fn a_failed_asset_is_never_registered() {
        let mut database = Database::open_in_memory().expect("opens");
        let item = analysed_item();
        let asset = MediaAsset {
            validation: AssetValidation::Failed {
                reason: "hash mismatch".to_owned(),
            },
            ..verified_asset(item.id, &"c".repeat(64))
        };
        assert!(database.register_media(&item, &asset).is_err());
    }

    #[test]
    fn an_item_still_uploading_is_not_playable() {
        let mut database = Database::open_in_memory().expect("opens");
        let item = MediaItem {
            state: MediaState::Uploading,
            ..analysed_item()
        };
        database.upsert_media_item(&item).expect("saves");

        assert!(!database
            .load_media_item(item.id)
            .expect("loads")
            .is_usable());
        assert_eq!(database.playable_asset_for(item.id).expect("queries"), None);
    }

    #[test]
    fn the_same_content_cannot_be_registered_twice() {
        let mut database = Database::open_in_memory().expect("opens");
        let sha = "d".repeat(64);

        let first = analysed_item();
        database
            .register_media(&first, &verified_asset(first.id, &sha))
            .expect("registers");

        let second = analysed_item();
        let error = database
            .register_media(&second, &verified_asset(second.id, &sha))
            .expect_err("duplicate content");
        assert!(error.to_string().contains("already registered"));
    }

    #[test]
    fn duplicate_content_is_found_before_it_is_uploaded_again() {
        let mut database = Database::open_in_memory().expect("opens");
        let sha = "e".repeat(64);
        let item = analysed_item();
        database
            .register_media(&item, &verified_asset(item.id, &sha))
            .expect("registers");

        assert_eq!(
            database.media_item_for_content(&sha).expect("queries"),
            Some(item.id)
        );
        assert_eq!(
            database
                .media_item_for_content(&"f".repeat(64))
                .expect("queries"),
            None
        );
    }

    #[test]
    fn an_asset_belonging_to_another_item_is_refused() {
        let mut database = Database::open_in_memory().expect("opens");
        let item = analysed_item();
        let stray = verified_asset(MediaItemId::new(), &"1".repeat(64));
        let error = database
            .register_media(&item, &stray)
            .expect_err("mismatched");
        assert!(error.to_string().contains("does not belong"));
    }

    #[test]
    fn a_failed_registration_leaves_nothing_behind() {
        let mut database = Database::open_in_memory().expect("opens");
        let sha = "2".repeat(64);
        let first = analysed_item();
        database
            .register_media(&first, &verified_asset(first.id, &sha))
            .expect("registers");

        let second = analysed_item();
        let _ = database.register_media(&second, &verified_asset(second.id, &sha));

        // The second item must not exist at all: the transaction rolled back.
        assert!(database.load_media_item(second.id).is_err());
        assert_eq!(database.list_media_items().expect("lists").len(), 1);
    }

    #[test]
    fn deleting_an_item_returns_the_files_that_are_now_unreferenced() {
        let mut database = Database::open_in_memory().expect("opens");
        let item = analysed_item();
        let asset = verified_asset(item.id, &"3".repeat(64));
        database.register_media(&item, &asset).expect("registers");

        let keys = database.delete_media_item(item.id).expect("deletes");
        assert_eq!(keys, vec![asset.object_key]);
        assert!(database.load_media_item(item.id).is_err());
    }

    #[test]
    fn deleting_an_item_cascades_to_its_assets() {
        let mut database = Database::open_in_memory().expect("opens");
        let item = analysed_item();
        database
            .register_media(&item, &verified_asset(item.id, &"4".repeat(64)))
            .expect("registers");
        database.delete_media_item(item.id).expect("deletes");

        let remaining: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM media_assets", [], |row| row.get(0))
            .expect("counts");
        assert_eq!(remaining, 0, "foreign keys must cascade");
    }

    #[test]
    fn deleting_media_warns_which_shows_use_it() {
        let mut database = Database::open_in_memory().expect("opens");
        let item = analysed_item();
        database
            .register_media(&item, &verified_asset(item.id, &"5".repeat(64)))
            .expect("registers");

        let mut show = Show::new("Summer Tour");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, item.id));
        show.layers.push(layer);
        database.save_show(&show).expect("saves");

        assert_eq!(
            database.shows_using_media(item.id).expect("checks"),
            vec!["Summer Tour"]
        );
        assert!(database
            .shows_using_media(MediaItemId::new())
            .expect("checks")
            .is_empty());
    }

    #[test]
    fn the_library_lists_most_recently_imported_first() {
        let mut database = Database::open_in_memory().expect("opens");
        let older = MediaItem {
            created_at: Timestamp::from_millis(1_000),
            ..MediaItem::importing("Older", "older.mov")
        };
        let newer = MediaItem {
            created_at: Timestamp::from_millis(2_000),
            ..MediaItem::importing("Newer", "newer.mov")
        };
        database.upsert_media_item(&older).expect("saves");
        database.upsert_media_item(&newer).expect("saves");

        let names: Vec<String> = database
            .list_media_items()
            .expect("lists")
            .into_iter()
            .map(|item| item.display_name)
            .collect();
        assert_eq!(names, vec!["Newer", "Older"]);
    }

    #[test]
    fn renaming_an_item_does_not_touch_its_physical_identity() {
        let mut database = Database::open_in_memory().expect("opens");
        let item = analysed_item();
        let asset = verified_asset(item.id, &"6".repeat(64));
        database.register_media(&item, &asset).expect("registers");

        let renamed = MediaItem {
            display_name: "Completely Different".to_owned(),
            state: MediaState::Ready,
            ..item.clone()
        };
        database.upsert_media_item(&renamed).expect("renames");

        let playable = database
            .playable_asset_for(item.id)
            .expect("queries")
            .expect("still there");
        assert_eq!(playable.object_key, asset.object_key);
        assert_eq!(playable.sha256, asset.sha256);
    }
}
