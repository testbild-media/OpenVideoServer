//! The object store: content-addressed media files on disk.
//!
//! One rule, and the whole module exists to keep it: **a key becomes a path only
//! after it has been checked.** Keys come out of the database, and the database
//! is written by this server, so in ordinary operation every key is well formed.
//! That is exactly why the check matters — the case it guards against is the one
//! where something has already gone wrong, and without it a bad row would become
//! an arbitrary file read.
//!
//! The store holds files. What is in them, which item they belong to and whether
//! they are verified is the database's business; this module never opens one.

use std::path::{Path, PathBuf};

use ovs_types::media::MediaAsset;

/// Where media files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaStore {
    root: PathBuf,
}

/// What a sweep of the store removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swept {
    /// How many objects were deleted.
    pub objects: u64,
    /// How many bytes that freed.
    pub bytes: u64,
}

/// Whether a directory name is one of the store's shards.
///
/// Two characters of a lower-case hexadecimal hash, which is what
/// `MediaAsset::object_key_for` writes. `incoming` and `thumbnails` are not, and
/// that is how the sweep stays out of them.
fn is_shard(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl MediaStore {
    /// Names a store rooted at `root`.
    ///
    /// Creates nothing. A server with no media yet has no directory, and that is
    /// not a failure to report at startup.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates the root directory if it is not there.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] when the directory cannot be
    /// created.
    pub fn ensure_root(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.incoming_root())
    }

    /// Resolves a storage key to the file it names.
    ///
    /// Returns `None` when the key is not one this server could have written, so
    /// a malformed or hostile key yields no path at all rather than a path
    /// outside the store.
    #[must_use]
    pub fn path_for(&self, object_key: &str) -> Option<PathBuf> {
        if !MediaAsset::is_valid_object_key(object_key) {
            return None;
        }

        // Split rather than joined whole: `Path::join` on a string containing a
        // separator is fine here because the key has been validated, but
        // building it component by component means the validation and the
        // construction agree on what a component is.
        let (prefix, file) = object_key.split_once('/')?;
        Some(self.root.join(prefix).join(file))
    }

    /// Returns where a thumbnail of one item lives.
    ///
    /// By item rather than by content, unlike everything else in this store.
    /// Two items holding the same bytes are still two rows an operator named
    /// separately, and a library shows a picture per row.
    #[must_use]
    pub fn thumbnail_path_for(&self, media_item_id: ovs_types::id::MediaItemId) -> PathBuf {
        self.root
            .join("thumbnails")
            .join(format!("{media_item_id}.png"))
    }

    /// Returns where a clip's moving preview lives.
    ///
    /// Ten frames of the clip side by side in one picture, which is what the
    /// client steps through to animate it (owner, 2026-09-23). One file rather
    /// than ten, because ten requests per pad is a library nobody can scroll.
    #[must_use]
    pub fn strip_path_for(&self, media_item_id: ovs_types::id::MediaItemId) -> PathBuf {
        self.root
            .join("thumbnails")
            .join(format!("{media_item_id}.strip.jpg"))
    }

    /// Writes a moving preview, replacing whatever was there.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when it cannot be written.
    pub fn put_strip(
        &self,
        media_item_id: ovs_types::id::MediaItemId,
        jpeg: &[u8],
    ) -> std::io::Result<()> {
        let path = self.strip_path_for(media_item_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, jpeg)
    }

    /// Writes a thumbnail, replacing whatever was there.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when it cannot be written.
    pub fn put_thumbnail(
        &self,
        media_item_id: ovs_types::id::MediaItemId,
        png: &[u8],
    ) -> std::io::Result<()> {
        let path = self.thumbnail_path_for(media_item_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, png)
    }

    /// Forgets the thumbnail of an item that has gone.
    ///
    /// Missing is not a failure: an item registered before thumbnails existed,
    /// or one whose picture could not be taken, has none to remove.
    pub fn drop_thumbnail(&self, media_item_id: ovs_types::id::MediaItemId) {
        let _ = std::fs::remove_file(self.thumbnail_path_for(media_item_id));
        let _ = std::fs::remove_file(self.strip_path_for(media_item_id));
    }

    /// Returns the directory uploads are assembled in.
    ///
    /// Beside the objects rather than in the system temporary directory, so an
    /// upload and its final home are on the same filesystem and committing one
    /// is a rename rather than a copy — which matters at the size a show file
    /// is, and which is what makes the commit atomic.
    #[must_use]
    pub fn incoming_root(&self) -> PathBuf {
        self.root.join("incoming")
    }

    /// Returns where an upload in progress is written.
    ///
    /// Returns `None` for an identifier that is not one this server issued, so
    /// a client cannot name a path of its own choosing.
    #[must_use]
    pub fn incoming_path_for(&self, upload_id: &str) -> Option<PathBuf> {
        let plausible = !upload_id.is_empty()
            && upload_id.len() <= 64
            && upload_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        plausible.then(|| self.incoming_root().join(upload_id))
    }

    /// Moves a finished upload into the object store.
    ///
    /// A rename, so an object either exists complete or does not exist at all.
    /// A half-written file under a content-addressed name would be a file whose
    /// name is a lie about its contents.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when the file cannot be moved, including
    /// `InvalidInput` when either name is not one this server could have
    /// produced.
    pub fn commit(&self, upload_id: &str, object_key: &str) -> std::io::Result<PathBuf> {
        let invalid = |what: &str| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{what} is not a name this server issued"),
            )
        };

        let from = self
            .incoming_path_for(upload_id)
            .ok_or_else(|| invalid("the upload identifier"))?;
        let to = self
            .path_for(object_key)
            .ok_or_else(|| invalid("the storage key"))?;

        std::fs::create_dir_all(to.parent().ok_or_else(|| invalid("the storage key"))?)?;

        // Already there means another upload of the same content got here
        // first. The bytes are identical by definition, which is what content
        // addressing means, so the newcomer is dropped rather than overwriting
        // a file something may be reading this instant.
        if to.is_file() {
            let _ = std::fs::remove_file(&from);
            return Ok(to);
        }

        std::fs::rename(&from, &to)?;
        Ok(to)
    }

    /// Deletes an upload in progress, if it is there.
    pub fn discard(&self, upload_id: &str) {
        if let Some(path) = self.incoming_path_for(upload_id) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Deletes one object, and says how many bytes that freed.
    ///
    /// Zero for a key that does not resolve, and zero for a file that is not
    /// there. Neither is a failure worth reporting: an asset row whose file has
    /// already gone is a real situation — a half-restored backup, a hand-edited
    /// store — and the caller is deleting the row either way.
    ///
    /// The shard directory goes too when this was the last object in it, so a
    /// store emptied of media is a store with nothing left in it rather than two
    /// hundred and fifty-six empty directories. `remove_dir` refuses a directory
    /// that is not empty, which is exactly the test wanted, so there is no race
    /// between looking and removing.
    pub fn remove(&self, object_key: &str) -> u64 {
        let Some(path) = self.path_for(object_key) else {
            return 0;
        };
        let freed = std::fs::metadata(&path).map_or(0, |about| about.len());
        if std::fs::remove_file(&path).is_err() {
            return 0;
        }
        if let Some(shard) = path.parent() {
            let _ = std::fs::remove_dir(shard);
        }
        freed
    }

    /// Deletes every object the library does not reference.
    ///
    /// # Why this exists, and why it runs at startup
    ///
    /// Until 2026-09-25 deleting media deleted the row and left the file, so a
    /// store accumulates objects nothing can ever reach again: they are not in
    /// the library, no show can name them, and no request resolves to them. The
    /// only thing they do is take up the disc a show needs (owner: "Wenn ich
    /// etwas lösche, dann soll das weg sein und nicht weiter platz belegen").
    ///
    /// **At startup, and not on a timer.** On a running server an object that no
    /// row names yet is the ordinary middle of an upload: `commit` renames the
    /// file into place and the row is written after it. A sweep passing between
    /// those two would delete an operator's clip while they watched the progress
    /// bar. Before anything is served, a file with no row is certainly abandoned.
    ///
    /// # What it will not touch, and what makes that true
    ///
    /// **The module's one rule does it**: this deletes through
    /// [`remove`](Self::remove), which goes through [`path_for`](Self::path_for),
    /// which will not turn a name that is not an object key into a path at all.
    /// An upload in progress is `incoming/upload-1` and a thumbnail is
    /// `thumbnails/<item id>.png`; a prefix is two characters of a hash and a
    /// filename is all sixty-four of them, so no name under either can be made
    /// into a key. Nor can a file sitting in the *wrong* shard — the prefix has to
    /// be the hash's own first two characters — nor anything a person put there by
    /// hand. Deleting something this server could not have written is not tidying
    /// up; it is deleting somebody else's file.
    ///
    /// The two checks below — a shard name, then the key — are the same refusal
    /// made earlier and more cheaply. Established by removing them: with both
    /// gone an upload in progress and a thumbnail both still survive a sweep,
    /// because `remove` refuses them one level down. They are here so that a
    /// library of ten thousand thumbnails is not enumerated on every start for an
    /// answer that was never going to change, **not** because the sweep would be
    /// unsafe without them. Anyone tempted to save a line by dropping the check in
    /// `path_for` should read that sentence the other way round.
    pub fn sweep(&self, referenced: &std::collections::BTreeSet<String>) -> Swept {
        let mut swept = Swept::default();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return swept;
        };

        for shard in entries.filter_map(std::result::Result::ok) {
            let name = shard.file_name();
            let Some(prefix) = name.to_str() else {
                continue;
            };
            if !is_shard(prefix) || !shard.path().is_dir() {
                continue;
            }
            let Ok(objects) = std::fs::read_dir(shard.path()) else {
                continue;
            };
            for object in objects.filter_map(std::result::Result::ok) {
                let name = object.file_name();
                let Some(file) = name.to_str() else {
                    continue;
                };
                let key = format!("{prefix}/{file}");
                if !MediaAsset::is_valid_object_key(&key) || referenced.contains(&key) {
                    continue;
                }
                let freed = self.remove(&key);
                if freed > 0 || !object.path().exists() {
                    swept.objects += 1;
                    swept.bytes += freed;
                    tracing::debug!(object = %key, bytes = freed, "swept an object nothing references");
                }
            }
            // Empty now, perhaps. `remove` only prunes when its own removal
            // emptied it, and a shard that was already empty before the sweep
            // would otherwise stay.
            let _ = std::fs::remove_dir(shard.path());
        }

        swept
    }

    /// Resolves a key to a file that exists and can be opened.
    ///
    /// Returns `None` for a key that does not resolve, and for one that resolves
    /// to nothing on disk. A registered asset whose file has gone missing is a
    /// real situation — a half-restored backup, a hand-edited store — and it
    /// should read as "not available" rather than as a path the renderer will
    /// fail to open later.
    #[must_use]
    pub fn existing_path_for(&self, object_key: &str) -> Option<PathBuf> {
        let path = self.path_for(object_key)?;
        path.is_file().then_some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> String {
        MediaAsset::object_key_for(&"ab".repeat(32), "mov").expect("a valid key")
    }

    /// A key whose hash is one character repeated, so a test can have several.
    fn key_of(character: char) -> String {
        MediaAsset::object_key_for(&character.to_string().repeat(64), "mov").expect("a valid key")
    }

    /// Writes `bytes` under `object_key` and returns where it landed.
    fn store_object(store: &MediaStore, object_key: &str, bytes: &[u8]) -> PathBuf {
        let path = store.path_for(object_key).expect("resolves");
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("creates");
        std::fs::write(&path, bytes).expect("writes");
        path
    }

    #[test]
    fn an_object_that_is_deleted_is_gone_and_takes_its_shard_with_it() {
        // **Deleted means gone** (owner, 2026-09-25: "Wenn ich etwas lösche, dann
        // soll das weg sein und nicht weiter platz belegen"). The number of bytes
        // matters as much as the file: it is what the log tells an operator who
        // is deleting things because a disc is full.
        let directory = tempfile::tempdir().expect("a directory");
        let store = MediaStore::new(directory.path());
        let object_key = key_of('a');
        let path = store_object(&store, &object_key, b"eight!!!");

        assert_eq!(store.remove(&object_key), 8, "said what it freed");
        assert!(!path.exists(), "the file is still there");
        assert!(
            !path.parent().expect("has a parent").exists(),
            "the shard directory was left behind empty"
        );
    }

    #[test]
    fn deleting_a_shard_that_still_holds_something_keeps_it() {
        // Two hashes starting with the same two characters is the ordinary case
        // at any size of library — the shard is a fan-out, not one clip per
        // directory — and taking the directory with the first of them would take
        // the second one's file too.
        let directory = tempfile::tempdir().expect("a directory");
        let store = MediaStore::new(directory.path());
        let first = MediaAsset::object_key_for(&format!("ab{}", "1".repeat(62)), "mov")
            .expect("a valid key");
        let second = MediaAsset::object_key_for(&format!("ab{}", "2".repeat(62)), "mov")
            .expect("a valid key");
        store_object(&store, &first, b"one");
        let kept = store_object(&store, &second, b"two");

        store.remove(&first);
        assert!(kept.exists(), "the other object in the shard went with it");
    }

    #[test]
    fn deleting_something_that_is_not_there_frees_nothing_and_is_not_a_failure() {
        // An asset row whose file has already gone — a half-restored backup, a
        // hand-edited store — and the caller is deleting the row either way.
        let directory = tempfile::tempdir().expect("a directory");
        let store = MediaStore::new(directory.path());

        assert_eq!(store.remove(&key_of('a')), 0, "it was never there");
        assert_eq!(store.remove("../../etc/passwd"), 0, "and that is not a key");
    }

    #[test]
    fn a_sweep_keeps_what_the_library_references_and_removes_the_rest() {
        let directory = tempfile::tempdir().expect("a directory");
        let store = MediaStore::new(directory.path());
        let kept = key_of('a');
        let orphan = key_of('b');
        let another = key_of('c');
        let kept_path = store_object(&store, &kept, b"referenced");
        store_object(&store, &orphan, b"four");
        store_object(&store, &another, b"four");

        let referenced = std::collections::BTreeSet::from([kept.clone()]);
        let swept = store.sweep(&referenced);

        assert_eq!(swept.objects, 2, "both orphans");
        assert_eq!(swept.bytes, 8, "and their bytes");
        assert!(kept_path.exists(), "the referenced object was deleted");
        assert_eq!(store.existing_path_for(&orphan), None);
        assert_eq!(store.existing_path_for(&another), None);
    }

    #[test]
    fn a_sweep_leaves_an_upload_in_progress_and_a_thumbnail_alone() {
        // The fault this guards against is not a dropped frame; it is deleting an
        // upload an operator is watching the progress bar of.
        //
        // It asserts the outcome and not the mechanism, and it cannot be made to
        // fail by breaking the sweep: with the shard check gone, and then the key
        // check inside the sweep gone as well, both files still survived. The
        // safety is a level below, in `path_for`, which has its own tests.
        //
        // Kept anyway, and worth being clear about what it is — a guard on the
        // composition rather than on either check. What it would catch is the
        // sweep learning to delete by some other route than `remove`, which is
        // exactly the change somebody would make while thinking the tests had
        // them covered.
        let directory = tempfile::tempdir().expect("a directory");
        let store = MediaStore::new(directory.path());
        store.ensure_root().expect("creates the root");

        let arriving = store
            .incoming_path_for("upload-1")
            .expect("a valid identifier");
        std::fs::write(&arriving, b"halfway through").expect("writes");
        let thumbnail = store.thumbnail_path_for(ovs_types::id::MediaItemId::new());
        std::fs::create_dir_all(thumbnail.parent().expect("has a parent")).expect("creates");
        std::fs::write(&thumbnail, b"a picture").expect("writes");

        let swept = store.sweep(&std::collections::BTreeSet::new());

        assert_eq!(swept.objects, 0, "it swept something it should not have");
        assert!(arriving.exists(), "an upload in progress was deleted");
        assert!(thumbnail.exists(), "a thumbnail was deleted");
    }

    #[test]
    fn a_sweep_leaves_a_file_it_does_not_recognise() {
        // Inside a shard, but not a name this server could have written.
        // Deleting something we cannot account for is not tidying up.
        let directory = tempfile::tempdir().expect("a directory");
        let store = MediaStore::new(directory.path());
        let shard = directory.path().join("ab");
        std::fs::create_dir_all(&shard).expect("creates");
        let stranger = shard.join("notes.txt");
        std::fs::write(&stranger, b"somebody's").expect("writes");

        let swept = store.sweep(&std::collections::BTreeSet::new());

        assert_eq!(swept.objects, 0);
        assert!(stranger.exists(), "a file nobody understands was deleted");
    }

    #[test]
    fn a_valid_key_resolves_under_the_root() {
        let store = MediaStore::new("/srv/ovs/media");
        let path = store.path_for(&key()).expect("resolves");

        assert!(path.starts_with("/srv/ovs/media"));
        assert!(path.ends_with(format!("{}.mov", "ab".repeat(32))));
        assert_eq!(
            path.parent().and_then(Path::file_name),
            Some(std::ffi::OsStr::new("ab")),
            "sharded by the first two hash characters"
        );
    }

    #[test]
    fn a_key_that_would_escape_the_store_resolves_to_nothing() {
        let store = MediaStore::new("/srv/ovs/media");
        for hostile in [
            "../../etc/passwd",
            "ab/../../etc/passwd",
            "/etc/passwd",
            "ab/ab.mov",
            "",
        ] {
            assert_eq!(
                store.path_for(hostile),
                None,
                "{hostile:?} must not resolve"
            );
        }
    }

    #[test]
    fn a_missing_file_is_not_a_path() {
        // The difference between "this key is wrong" and "this file is gone"
        // matters to whoever has to fix it, but neither is something to hand to
        // a decoder.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = MediaStore::new(directory.path());
        let object_key = key();

        assert!(store.path_for(&object_key).is_some());
        assert_eq!(store.existing_path_for(&object_key), None);

        let path = store.path_for(&object_key).expect("resolves");
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("creates");
        std::fs::write(&path, b"pixels").expect("writes");

        assert_eq!(store.existing_path_for(&object_key), Some(path));
    }

    #[test]
    fn a_directory_is_not_a_file() {
        // An interrupted import can leave the shard directory behind with
        // nothing in it. Opening that would fail much later and much less
        // clearly.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = MediaStore::new(directory.path());
        let path = store.path_for(&key()).expect("resolves");
        std::fs::create_dir_all(&path).expect("creates a directory where a file belongs");

        assert_eq!(store.existing_path_for(&key()), None);
    }

    #[test]
    fn an_upload_identifier_that_is_not_ours_names_no_path() {
        let store = MediaStore::new("/srv/ovs/media");
        for hostile in ["", "../../etc/passwd", "a/b", "a b", &"a".repeat(65)] {
            assert_eq!(
                store.incoming_path_for(hostile),
                None,
                "{hostile:?} must not resolve"
            );
        }
        assert!(store.incoming_path_for("01JC-abc").is_some());
    }

    #[test]
    fn committing_an_upload_moves_it_under_its_content_name() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = MediaStore::new(directory.path());
        store.ensure_root().expect("creates");

        let upload_id = "01JCUPLOAD";
        std::fs::write(
            store.incoming_path_for(upload_id).expect("resolves"),
            b"pixels",
        )
        .expect("writes");

        let committed = store.commit(upload_id, &key()).expect("commits");

        assert_eq!(std::fs::read(&committed).expect("reads"), b"pixels");
        assert_eq!(store.existing_path_for(&key()), Some(committed));
        assert!(
            !store
                .incoming_path_for(upload_id)
                .expect("resolves")
                .exists(),
            "and it is no longer staged"
        );
    }

    #[test]
    fn committing_content_that_is_already_stored_keeps_what_is_there() {
        // Two operators importing the same clip at the same time. The bytes are
        // identical by definition, and overwriting would replace a file a show
        // may be reading this instant for no gain at all.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = MediaStore::new(directory.path());
        store.ensure_root().expect("creates");

        std::fs::write(store.incoming_path_for("first").expect("resolves"), b"one")
            .expect("writes");
        store.commit("first", &key()).expect("commits");

        std::fs::write(store.incoming_path_for("second").expect("resolves"), b"two")
            .expect("writes");
        let committed = store.commit("second", &key()).expect("commits");

        assert_eq!(
            std::fs::read(&committed).expect("reads"),
            b"one",
            "the file that was already there"
        );
        assert!(
            !store
                .incoming_path_for("second")
                .expect("resolves")
                .exists(),
            "and the duplicate is cleaned up"
        );
    }

    #[test]
    fn committing_under_a_key_this_server_could_not_have_issued_is_refused() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = MediaStore::new(directory.path());
        store.ensure_root().expect("creates");
        std::fs::write(store.incoming_path_for("upload").expect("resolves"), b"x").expect("writes");

        let error = store
            .commit("upload", "../../etc/passwd")
            .expect_err("refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn the_root_is_created_on_demand_and_creating_it_twice_is_fine() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = MediaStore::new(directory.path().join("media"));

        assert!(!store.root().exists());
        store.ensure_root().expect("creates");
        assert!(store.root().is_dir());
        store.ensure_root().expect("again");
        assert!(store.root().is_dir());
    }
}
