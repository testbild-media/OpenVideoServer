//! The media model.
//!
//! Two levels, deliberately:
//!
//! - [`MediaItem`] is the logical thing an operator sees and a show references.
//! - [`MediaAsset`] is one physical representation of it on disk.
//!
//! A show only ever stores a [`crate::id::MediaItemId`]. Renaming or
//! reorganising storage cannot break a show, because nothing outside this
//! module knows a physical key exists.

pub mod playback_profile;

pub use playback_profile::{
    faults, keyframes_at_most, BitDepth, PlaybackProfile, ProbedVideo, ProfileFault,
    MOST_SECONDS_BETWEEN_KEYFRAMES,
};

use serde::{Deserialize, Serialize};

use crate::error::{ValidationError, ValidationErrors};
use crate::geometry::{FrameRate, SizePx};
use crate::id::{MediaAssetId, MediaItemId};
use crate::time::Timestamp;

/// The kind of thing a media item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A moving image with a timeline.
    Video,
    /// A still image.
    Image,
}

/// Where a media item is in the ingest pipeline.
///
/// These are the states the operator is shown. They deliberately say
/// nothing about codecs, because the operator never chooses one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaState {
    /// Accepted by the client, not yet analysed.
    Importing,
    /// Being analysed or transcoded on the client.
    Processing,
    /// Being uploaded to the server.
    Uploading,
    /// Uploaded, being verified before registration.
    Verifying,
    /// Registered and usable in a show.
    Ready,
    /// The pipeline failed. The reason is on the item.
    Failed,
}

impl MediaState {
    /// Returns true when this item may be assigned to a slot and played.
    ///
    /// Only [`MediaState::Ready`] qualifies. A partially uploaded item never
    /// appears as valid content.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Returns true when the pipeline is still working on this item.
    #[must_use]
    pub const fn is_in_progress(self) -> bool {
        matches!(
            self,
            Self::Importing | Self::Processing | Self::Uploading | Self::Verifying
        )
    }
}

/// The file extension a stored object should carry.
///
/// Part of the storage key, so it has to be something a filesystem will hold
/// and something `MediaAsset::object_key_for` will accept: ASCII alphanumerics,
/// lowercase, and short.
///
/// # Why the operator's own extension, first
///
/// The server used to fix this per profile, because it knew what it had
/// produced. It no longer produces anything — it stores the file it was given
/// — so the honest extension is the one the file already had.
/// FFmpeg identifies a container by reading it rather than by its name, so
/// nothing about playback depends on getting this right; it matters for a
/// person looking at the media store from a shell, which is exactly when a
/// `.mp4` called `.mp4` is worth having.
///
/// The probed container is the fallback, and it is a fallback because FFmpeg
/// reports a container as a list of every name that demuxer answers to —
/// `mov,mp4,m4a,3gp,3g2,mj2` — of which the first is as good a choice as any.
/// `bin` is the last resort, for a file with neither.
#[must_use]
pub fn storage_extension(original_filename: &str, container: &str) -> String {
    fn usable(candidate: &str) -> Option<String> {
        let candidate = candidate.trim().to_ascii_lowercase();
        (!candidate.is_empty()
            && candidate.len() <= 8
            && candidate.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then_some(candidate)
    }

    original_filename
        .rsplit_once('.')
        .and_then(|(_, extension)| usable(extension))
        .or_else(|| container.split(',').find_map(usable))
        .unwrap_or_else(|| "bin".to_owned())
}

/// Whether a physical asset has been verified against its recorded hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AssetValidation {
    /// Staged but not yet verified. Never playable.
    Pending,
    /// Hash and size matched what the client declared.
    Verified {
        /// When verification succeeded.
        at: Timestamp,
    },
    /// Verification failed. Never playable.
    Failed {
        /// Why it failed, for the diagnostics view.
        reason: String,
    },
}

impl AssetValidation {
    /// Returns true when this asset may be opened by a decoder.
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
}

/// How long a whole folder path may be, in characters.
///
/// **Not a limit on how deep folders nest.** They nest as deep as the
/// operator wants — folders in folders in folders, as far as they care to go
/// (owner, 2026-09-23). This is a limit on one string, so that a path cannot
/// be made long enough to be a problem for the column it is stored in or the
/// panel it is drawn in. A thousand characters is around a hundred and
/// twenty folders deep at eight characters a name, which nobody will meet by
/// organising a library.
pub const MAX_FOLDER_PATH: usize = 1024;

/// The longest one segment of a folder path may be.
pub const MAX_FOLDER_SEGMENT: usize = 64;

/// Why a folder path was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FolderError {
    /// A segment was empty, which would make two different paths read alike.
    #[error("a folder name cannot be empty")]
    EmptySegment,
    /// A segment was only spaces, or had them at its ends.
    #[error("a folder name cannot begin or end with a space")]
    Padded,
    /// A segment was too long to draw.
    #[error("a folder name may be at most {MAX_FOLDER_SEGMENT} characters")]
    SegmentTooLong,
    /// The whole path was longer than one string may be.
    ///
    /// Not about nesting, which is unlimited: about the length of the name
    /// that comes out of it.
    #[error("a folder path may be at most {MAX_FOLDER_PATH} characters")]
    PathTooLong,
    /// A character that would make the path mean something else.
    #[error("a folder name cannot contain {0:?}")]
    BadCharacter(char),
}

/// Checks a folder path, and returns it as it will be stored.
///
/// The empty path is the root and is always allowed: everything ever imported
/// is already in it.
///
/// # Errors
///
/// Returns [`FolderError`] for a path an operator could not tell apart from
/// another one, or could not read.
pub fn check_folder(path: &str) -> Result<&str, FolderError> {
    if path.is_empty() {
        return Ok(path);
    }

    if path.chars().count() > MAX_FOLDER_PATH {
        return Err(FolderError::PathTooLong);
    }
    for segment in path.split('/') {
        if segment.is_empty() {
            return Err(FolderError::EmptySegment);
        }
        if segment.trim() != segment {
            // Two folders whose names differ by a space are two folders an
            // operator cannot tell apart, which is one folder as far as they
            // are concerned and two as far as the library is.
            return Err(FolderError::Padded);
        }
        if segment.chars().count() > MAX_FOLDER_SEGMENT {
            return Err(FolderError::SegmentTooLong);
        }
        if let Some(bad) = segment
            .chars()
            .find(|character| character.is_control() || *character == '\\')
        {
            return Err(FolderError::BadCharacter(bad));
        }
    }
    Ok(path)
}

/// Every folder a path passes through, itself last.
///
/// `Intros/2026/Openers` is in `Intros` and `Intros/2026` as well as itself,
/// and a library that stored only the leaf would have a tree with holes in it.
#[must_use]
pub fn folder_ancestry(path: &str) -> Vec<String> {
    if path.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut so_far = String::new();
    for segment in path.split('/') {
        if !so_far.is_empty() {
            so_far.push('/');
        }
        so_far.push_str(segment);
        out.push(so_far.clone());
    }
    out
}
/// A logical media item: the only media identity a show ever stores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MediaItem {
    /// Stable identifier.
    pub id: MediaItemId,
    /// Name shown in the content browser. Freely renameable without breaking shows.
    pub display_name: String,
    /// The filename this item was imported from, kept for the operator's orientation only.
    ///
    /// Never used as identity and never used to find the file on disk.
    pub original_filename: String,
    /// Video or image.
    pub kind: MediaKind,
    /// Pipeline state.
    pub state: MediaState,
    /// Duration in milliseconds. `None` for stills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Pixel dimensions, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<SizePx>,
    /// Frame rate, once known. `None` for stills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<FrameRate>,
    /// Whether this item carries an alpha channel.
    pub has_alpha: bool,
    /// Why the item failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    /// Where this item is filed in the library.
    ///
    /// A slash-separated path, for example `Intros/2026`, and empty for the
    /// root. **Virtual, always**: it is a name the item is filed under and
    /// nothing more. The file on disk does not move, its object key does not
    /// change, and content addressing goes on working (owner, 2026-09-23).
    ///
    /// Never a path on any filesystem, and never used to find anything.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// When the item was first imported.
    pub created_at: Timestamp,
}

impl MediaItem {
    /// Creates a newly imported item, before analysis.
    #[must_use]
    pub fn importing(
        display_name: impl Into<String>,
        original_filename: impl Into<String>,
    ) -> Self {
        Self {
            id: MediaItemId::new(),
            display_name: display_name.into(),
            original_filename: original_filename.into(),
            kind: MediaKind::Video,
            state: MediaState::Importing,
            duration_ms: None,
            dimensions: None,
            frame_rate: None,
            has_alpha: false,
            failure_reason: None,
            folder: String::new(),
            created_at: Timestamp::now(),
        }
    }

    /// Returns true when this item may be assigned to a slot.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        self.state.is_usable()
    }

    /// Checks the item's internal consistency.
    ///
    /// # Errors
    ///
    /// Returns every violated invariant.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut out = ValidationErrors::new();

        if self.display_name.trim().is_empty() {
            out.push(ValidationError::new("display_name", "must not be empty"));
        }

        if let Some(size) = self.dimensions {
            size.validate("dimensions", &mut out);
        }
        if let Some(rate) = self.frame_rate {
            rate.validate("frame_rate", &mut out);
        }

        match self.kind {
            MediaKind::Image => {
                if self.duration_ms.is_some() {
                    out.push(ValidationError::new(
                        "duration_ms",
                        "a still image has no duration",
                    ));
                }
                if self.frame_rate.is_some() {
                    out.push(ValidationError::new(
                        "frame_rate",
                        "a still image has no frame rate",
                    ));
                }
            }
            MediaKind::Video => {
                if self.state.is_usable() && self.duration_ms.is_none() {
                    out.push(ValidationError::new(
                        "duration_ms",
                        "a ready video must have a known duration",
                    ));
                }
            }
        }

        if self.state.is_usable() && self.dimensions.is_none() {
            out.push(ValidationError::new(
                "dimensions",
                "a ready item must have known dimensions",
            ));
        }

        if matches!(self.state, MediaState::Failed) && self.failure_reason.is_none() {
            out.push(ValidationError::new(
                "failure_reason",
                "a failed item must record why it failed",
            ));
        }

        out.into_result()
    }
}

/// One physical representation of a media item.
///
/// A media item can have several assets, and in practice has one: the file that
/// was uploaded for it. Nothing rewrites an asset, so a show keeps playing
/// exactly the bytes it was built with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MediaAsset {
    /// Stable identifier.
    pub id: MediaAssetId,
    /// The logical item this asset represents.
    pub media_item_id: MediaItemId,
    /// Content-addressed storage key, for example `ab/abcdef....mov`.
    ///
    /// Server-internal. Never sent to the client, never shown to an operator and
    /// never stored in a show.
    pub object_key: String,
    /// Lowercase hex SHA-256 of the file, used for verification and deduplication.
    pub sha256: String,
    /// Size on disk in bytes.
    pub byte_size: u64,
    /// Container format, as the server read it out of the file.
    ///
    /// FFmpeg's own name for the demuxer, which is a list where one demuxer
    /// answers to several: `mov,mp4,m4a,3gp,3g2,mj2`.
    pub container: String,
    /// Codec name, as the server read it out of the file.
    ///
    /// `h264`, `hevc`, `av1`, `prores`, `hap`, `png`. This is what the file
    /// holds, not what an upload claimed it held: the server probes every video
    /// it accepts, and a declaration that disagrees with the file loses.
    pub codec: String,
    /// Verification state.
    pub validation: AssetValidation,
    /// When the asset was registered.
    pub created_at: Timestamp,
}

impl MediaAsset {
    /// Returns the content-addressed storage key for a hash and file extension.
    ///
    /// The first two hex characters become a directory so that no single
    /// directory ends up holding an entire media library.
    ///
    /// Returns `None` when the hash is not a 64-character lowercase hex string,
    /// which keeps a malformed hash from ever becoming a filesystem path.
    #[must_use]
    pub fn object_key_for(sha256: &str, extension: &str) -> Option<String> {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return None;
        }
        if extension.is_empty() || !extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        Some(format!("{}/{}.{}", &sha256[..2], sha256, extension))
    }

    /// Returns true when `key` is one this server could have produced.
    ///
    /// Whatever resolves a key to a file on disk checks it here first. The keys
    /// in the database were written by [`object_key_for`], so in ordinary
    /// operation this always passes; it exists for the case where something has
    /// gone wrong, because that is the case in which a key becomes a path and a
    /// path becomes a file the server should never have opened.
    ///
    /// [`object_key_for`]: MediaAsset::object_key_for
    #[must_use]
    pub fn is_valid_object_key(key: &str) -> bool {
        let Some((prefix, file)) = key.split_once('/') else {
            return false;
        };
        let Some((hash, extension)) = file.split_once('.') else {
            return false;
        };
        // Rebuilt rather than inspected piecemeal, so the rule lives in exactly
        // one place and the two cannot drift apart.
        Self::object_key_for(hash, extension).is_some_and(|expected| {
            expected == key && prefix.len() == 2 && hash.starts_with(prefix)
        })
    }

    /// Returns true when a decoder may open this asset.
    #[must_use]
    pub const fn is_playable(&self) -> bool {
        self.validation.is_verified()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_video() -> MediaItem {
        MediaItem {
            kind: MediaKind::Video,
            state: MediaState::Ready,
            duration_ms: Some(10_000),
            dimensions: Some(SizePx::new(1920, 1080)),
            frame_rate: Some(FrameRate::FPS_60),
            ..MediaItem::importing("Intro", "intro.mov")
        }
    }

    #[test]
    fn folders_nest_as_deep_as_an_operator_wants() {
        // The owner's instruction (2026-09-23): folders in folders in folders,
        // as far as they care to go. There is no depth ceiling, only a limit on
        // how long one string may be.
        let deep = (1..=40)
            .map(|level| format!("Level{level}"))
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(check_folder(&deep), Ok(deep.as_str()));
        assert_eq!(folder_ancestry(&deep).len(), 40);
    }

    #[test]
    fn a_path_longer_than_one_string_may_be_is_refused() {
        // Not about nesting: about the length of the name that comes out of it.
        let far_too_long = vec!["a"; MAX_FOLDER_PATH].join("/");
        assert_eq!(check_folder(&far_too_long), Err(FolderError::PathTooLong));
    }

    #[test]
    fn the_root_is_a_folder_everything_is_already_in() {
        assert_eq!(check_folder(""), Ok(""));
        assert_eq!(folder_ancestry(""), Vec::<String>::new());
    }

    #[test]
    fn a_path_an_operator_could_not_tell_apart_from_another_is_refused() {
        // Two folders whose names differ by a space are one folder as far as
        // anybody reading the screen is concerned, and two as far as the
        // library is.
        assert_eq!(check_folder("Intros /2026"), Err(FolderError::Padded));
        assert_eq!(check_folder(" Intros"), Err(FolderError::Padded));
        assert_eq!(check_folder("Intros//2026"), Err(FolderError::EmptySegment));
        assert_eq!(check_folder("Intros/"), Err(FolderError::EmptySegment));
        assert!(matches!(
            check_folder("Intros\\2026"),
            Err(FolderError::BadCharacter(_))
        ));
    }

    #[test]
    fn every_folder_a_path_passes_through_is_named_with_itself_last() {
        // A library that stored only the leaf would have a tree with holes in it.
        assert_eq!(
            folder_ancestry("Intros/2026/Openers"),
            vec![
                "Intros".to_owned(),
                "Intros/2026".to_owned(),
                "Intros/2026/Openers".to_owned(),
            ]
        );
    }

    #[test]
    fn an_item_starts_in_the_root_and_the_folder_never_reaches_the_wire_empty() {
        let item = MediaItem::importing("Intro", "intro.mov");
        assert_eq!(item.folder, "");
        let json = serde_json::to_string(&item).expect("serialises");
        assert!(!json.contains("folder"), "{json}");
    }

    #[test]
    fn only_ready_media_is_usable() {
        for state in [
            MediaState::Importing,
            MediaState::Processing,
            MediaState::Uploading,
            MediaState::Verifying,
            MediaState::Failed,
        ] {
            assert!(
                !state.is_usable(),
                "{state:?} must never be assignable to a slot"
            );
        }
        assert!(MediaState::Ready.is_usable());
    }

    #[test]
    fn in_progress_states_are_exactly_the_pipeline_states() {
        assert!(MediaState::Importing.is_in_progress());
        assert!(MediaState::Verifying.is_in_progress());
        assert!(!MediaState::Ready.is_in_progress());
        assert!(!MediaState::Failed.is_in_progress());
    }

    #[test]
    fn a_ready_video_must_know_its_duration_and_size() {
        assert_eq!(ready_video().validate(), Ok(()));

        let no_duration = MediaItem {
            duration_ms: None,
            ..ready_video()
        };
        assert!(no_duration.validate().is_err());

        let no_size = MediaItem {
            dimensions: None,
            ..ready_video()
        };
        assert!(no_size.validate().is_err());
    }

    #[test]
    fn an_item_still_uploading_need_not_know_everything_yet() {
        let uploading = MediaItem {
            state: MediaState::Uploading,
            duration_ms: None,
            dimensions: None,
            frame_rate: None,
            ..MediaItem::importing("Intro", "intro.mov")
        };
        assert_eq!(uploading.validate(), Ok(()));
        assert!(!uploading.is_usable());
    }

    #[test]
    fn a_still_image_has_neither_duration_nor_frame_rate() {
        let image = MediaItem {
            kind: MediaKind::Image,
            state: MediaState::Ready,
            duration_ms: None,
            frame_rate: None,
            dimensions: Some(SizePx::new(1920, 1080)),
            ..MediaItem::importing("Logo", "logo.png")
        };
        assert_eq!(image.validate(), Ok(()));

        let with_duration = MediaItem {
            duration_ms: Some(1),
            ..image
        };
        assert!(with_duration.validate().is_err());
    }

    #[test]
    fn a_failed_item_must_say_why() {
        let failed = MediaItem {
            state: MediaState::Failed,
            failure_reason: None,
            ..MediaItem::importing("Broken", "broken.mov")
        };
        assert!(failed.validate().is_err());

        let explained = MediaItem {
            failure_reason: Some("unsupported codec".to_owned()),
            ..failed
        };
        assert_eq!(explained.validate(), Ok(()));
    }

    #[test]
    fn a_stored_object_keeps_the_extension_the_file_came_with() {
        assert_eq!(storage_extension("winter loop.mov", "mov,mp4"), "mov");
        assert_eq!(storage_extension("CLIP.MP4", "mov,mp4"), "mp4");
        assert_eq!(storage_extension("logo.PNG", "png_pipe"), "png");
    }

    #[test]
    fn a_file_with_no_usable_extension_borrows_the_container_s() {
        // A demuxer answers to several names and FFmpeg reports all of them.
        // The first is as good as any.
        assert_eq!(storage_extension("clip", "mov,mp4,m4a,3gp,3g2,mj2"), "mov");
        assert_eq!(
            storage_extension("no dot here", "matroska,webm"),
            "matroska"
        );
        // Dot with nothing after it, and an extension too long to be one.
        assert_eq!(storage_extension("trailing.", "avi"), "avi");
        assert_eq!(storage_extension("odd.californication", "avi"), "avi");
    }

    #[test]
    fn an_extension_nothing_can_offer_falls_back_rather_than_failing() {
        assert_eq!(storage_extension("clip", ""), "bin");
        assert_eq!(storage_extension("clip.", "?!"), "bin");
    }

    #[test]
    fn every_extension_this_produces_is_one_a_storage_key_can_hold() {
        // The extension goes into the object key, and a key is only valid when
        // its extension is alphanumeric. This is the rule that keeps a file
        // called `../../etc/passwd` from becoming a path.
        let hash = "c".repeat(64);
        for (name, container) in [
            ("clip.mov", "mov,mp4"),
            ("clip.MP4", "mov,mp4"),
            ("../../etc/passwd", ""),
            ("clip.tar.gz", "matroska,webm"),
            ("", ""),
            ("dots...", "mpegts"),
        ] {
            let extension = storage_extension(name, container);
            assert!(
                MediaAsset::object_key_for(&hash, &extension).is_some(),
                "{name:?} produced {extension:?}, which a key cannot hold"
            );
        }
    }

    #[test]
    fn object_keys_shard_by_the_first_two_hash_characters() {
        let hash = "a".repeat(64);
        assert_eq!(
            MediaAsset::object_key_for(&hash, "mov"),
            Some(format!("aa/{hash}.mov"))
        );
    }

    #[test]
    fn a_malformed_hash_never_becomes_a_path() {
        assert_eq!(MediaAsset::object_key_for("short", "mov"), None);
        assert_eq!(MediaAsset::object_key_for(&"A".repeat(64), "mov"), None);
        assert_eq!(MediaAsset::object_key_for(&"z".repeat(64), "mov"), None);
        assert_eq!(
            MediaAsset::object_key_for(&format!("{}/..", "a".repeat(62)), "mov"),
            None
        );
        assert_eq!(MediaAsset::object_key_for(&"a".repeat(64), "../etc"), None);
        assert_eq!(MediaAsset::object_key_for(&"a".repeat(64), ""), None);
    }

    #[test]
    fn a_key_this_server_wrote_validates() {
        let hash = "0123456789abcdef".repeat(4);
        let key = MediaAsset::object_key_for(&hash, "mov").expect("a valid key");
        assert!(MediaAsset::is_valid_object_key(&key));
    }

    #[test]
    fn a_key_that_could_escape_the_store_is_refused() {
        let hash = "a".repeat(64);
        for key in [
            "",
            "aa",
            &hash,
            &format!("{hash}.mov"),
            &format!("../{hash}.mov"),
            &format!("aa/../{hash}.mov"),
            &format!("aa/{hash}.mov/../../etc/passwd"),
            &format!("/aa/{hash}.mov"),
            &format!("aa\\{hash}.mov"),
            // The right shape, but sharded under the wrong prefix: a key whose
            // directory does not follow from its hash was not written here.
            &format!("bb/{hash}.mov"),
            // Uppercase hex would be a second key for the same content, and on a
            // case-insensitive filesystem a second name for the same file.
            &format!("AA/{}.mov", "A".repeat(64)),
        ] {
            assert!(
                !MediaAsset::is_valid_object_key(key),
                "{key:?} must not resolve to a path"
            );
        }
    }

    #[test]
    fn an_unverified_asset_is_never_playable() {
        let asset = MediaAsset {
            id: MediaAssetId::new(),
            media_item_id: MediaItemId::new(),
            object_key: "aa/aaaa.mov".to_owned(),
            sha256: "a".repeat(64),
            byte_size: 1024,
            container: "mov".to_owned(),
            codec: "prores".to_owned(),
            validation: AssetValidation::Pending,
            created_at: Timestamp::now(),
        };
        assert!(!asset.is_playable());

        let failed = MediaAsset {
            validation: AssetValidation::Failed {
                reason: "hash mismatch".to_owned(),
            },
            ..asset.clone()
        };
        assert!(!failed.is_playable());

        let verified = MediaAsset {
            validation: AssetValidation::Verified {
                at: Timestamp::now(),
            },
            ..asset
        };
        assert!(verified.is_playable());
    }

    #[test]
    fn a_media_item_never_serialises_a_physical_location() {
        let json = serde_json::to_string(&ready_video()).expect("serialises");
        assert!(!json.contains("object_key"));
        assert!(!json.contains("sha256"));
    }

    #[test]
    fn a_media_item_never_reveals_how_its_content_is_encoded() {
        // The operator sees import, processing, ready. What the file turned out
        // to be lives on the asset, which is server-internal — and stays there
        // now that the file decides rather than the server.
        let json = serde_json::to_string(&ready_video()).expect("serialises");
        assert!(!json.contains("codec"));
        assert!(!json.contains("container"));
        assert!(!json.contains("prores"));
        assert!(!json.contains("h264"));
    }
}
