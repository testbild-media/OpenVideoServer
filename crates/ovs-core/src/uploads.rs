//! Uploads in flight.
//!
//! An upload is three requests: say what is coming, send it, ask for it to be
//! verified. What is tracked here is only the middle part — how much has
//! arrived — because everything else is either in the request the client made or
//! in the file on disk.
//!
//! # Not persisted, on purpose
//!
//! An upload in flight does not survive a restart. A client that was uploading
//! when the server went down starts again, which is what it would have to do
//! anyway: the only thing a persisted session would save is re-sending bytes,
//! and the cost of getting resumption wrong across a restart is a file that
//! passes its hash check while containing two halves of different uploads.
//!
//! # Contiguous, on purpose
//!
//! Bytes arrive in order from a single offset. A client resumes by asking where
//! to continue and sending from there. Out-of-order ranges would mean tracking a
//! set of intervals, and a file assembled from ranges that overlapped
//! differently than the client thought would be a file whose hash check is the
//! only thing standing between it and a show.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ovs_proto::http::{CreateUploadRequest, UploadSession};
use ovs_types::id::MediaItemId;

/// How much of a file the server asks for at a time.
///
/// Large enough that a big clip is not thousands of requests, small enough that
/// a failed chunk is not a large re-send and that the server is not holding many
/// megabytes per upload in memory.
pub const CHUNK_SIZE: u64 = 8 * 1024 * 1024;

/// How much room is left on the disk after an upload is allowed to start.
///
/// A media server whose disk reaches zero is not merely out of room for media.
/// The database cannot write its journal, the log cannot be written, and the
/// next thing to fail is unrelated to the file that filled it — which is how a
/// disk that filled during an afternoon's ingest becomes a show that will not
/// open that evening. So an upload is refused while there is still room to
/// recover in, rather than at the moment the last byte will not fit.
///
/// A gigabyte is far more than the database and the logs need. It is chosen to
/// be obviously enough rather than tuned, because the cost of it being too
/// large is one file an operator has to make room for, and the cost of it being
/// too small is a server that has to be rescued from a shell.
pub const DISK_HEADROOM: u64 = 1024 * 1024 * 1024;

/// Whether a file of `byte_size` may start on a disk with `available` bytes free.
///
/// Separated from the request handling so the rule can be asked directly. The
/// handler decides what to do when the free space is unknown; this decides only
/// what the numbers mean when they are both known.
#[must_use]
pub const fn fits(byte_size: u64, available: u64) -> bool {
    match byte_size.checked_add(DISK_HEADROOM) {
        Some(needed) => needed <= available,
        // A size that overflows when the headroom is added to it is larger than
        // any disk that could hold it, so it does not fit. Saturating instead
        // would make the largest expressible file appear to fit on the largest
        // expressible disk, which is the one case where the answer matters most.
        None => false,
    }
}

/// How long an upload may sit untouched before it is abandoned.
///
/// A client that stops halfway leaves a file in the staging directory and an
/// entry here. Neither is large, but neither goes away on its own, and a server
/// that runs for a season would accumulate both.
pub const ABANDONED_AFTER: Duration = Duration::from_secs(60 * 60);

/// One upload in flight.
#[derive(Debug, Clone)]
pub struct Upload {
    /// What the client said it was sending.
    ///
    /// Kept whole, because verification is a comparison against exactly this:
    /// the hash, the size, and for a still the dimensions.
    pub declared: CreateUploadRequest,
    /// The item these bytes will belong to.
    pub media_item_id: MediaItemId,
    /// How many bytes have arrived, contiguous from the start.
    pub received_bytes: u64,
    /// When the last chunk arrived.
    pub touched_at: Instant,
}

impl Upload {
    /// Returns the session as a client sees it.
    #[must_use]
    pub fn session(&self, upload_id: &str) -> UploadSession {
        UploadSession {
            upload_id: upload_id.to_owned(),
            media_item_id: self.media_item_id,
            byte_size: self.declared.byte_size,
            received_bytes: self.received_bytes,
            chunk_size: CHUNK_SIZE,
        }
    }

    /// Returns true when every declared byte has arrived.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.received_bytes >= self.declared.byte_size
    }
}

/// Every upload this server is currently expecting bytes for.
#[derive(Debug, Default)]
pub struct Uploads {
    in_flight: BTreeMap<String, Upload>,
}

impl Uploads {
    /// Creates an empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns how many uploads are in flight.
    #[must_use]
    pub fn len(&self) -> usize {
        self.in_flight.len()
    }

    /// Returns true when nothing is being uploaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// Starts an upload and returns the session a client resumes from.
    pub fn begin(
        &mut self,
        upload_id: impl Into<String>,
        declared: CreateUploadRequest,
        media_item_id: MediaItemId,
    ) -> UploadSession {
        let upload_id = upload_id.into();
        let upload = Upload {
            declared,
            media_item_id,
            received_bytes: 0,
            touched_at: Instant::now(),
        };
        let session = upload.session(&upload_id);
        self.in_flight.insert(upload_id, upload);
        session
    }

    /// Returns an upload in flight.
    #[must_use]
    pub fn get(&self, upload_id: &str) -> Option<&Upload> {
        self.in_flight.get(upload_id)
    }

    /// Records that `count` more bytes arrived at `offset`.
    ///
    /// Returns the updated session, or `None` when there is no such upload.
    /// Returns an error when the offset is not where this upload continues, or
    /// when the bytes would take it past what was declared: both mean the client
    /// and the server disagree about the file, and continuing would produce one
    /// that is neither what was sent nor what was declared.
    pub fn receive(
        &mut self,
        upload_id: &str,
        offset: u64,
        count: u64,
    ) -> Option<Result<UploadSession, OutOfStep>> {
        let upload = self.in_flight.get_mut(upload_id)?;

        if offset != upload.received_bytes {
            return Some(Err(OutOfStep::WrongOffset {
                expected: upload.received_bytes,
                received: offset,
            }));
        }

        let total = offset.saturating_add(count);
        if total > upload.declared.byte_size {
            return Some(Err(OutOfStep::TooMuch {
                declared: upload.declared.byte_size,
                would_be: total,
            }));
        }

        upload.received_bytes = total;
        upload.touched_at = Instant::now();
        Some(Ok(upload.session(upload_id)))
    }

    /// Removes an upload and returns it.
    pub fn take(&mut self, upload_id: &str) -> Option<Upload> {
        self.in_flight.remove(upload_id)
    }

    /// Removes uploads nothing has arrived for in [`ABANDONED_AFTER`].
    ///
    /// Returns them so the caller can delete the staged files and say in the
    /// library why those items never became anything.
    pub fn take_abandoned(&mut self, now: Instant) -> Vec<(String, Upload)> {
        let abandoned: Vec<String> = self
            .in_flight
            .iter()
            .filter(|(_, upload)| {
                now.saturating_duration_since(upload.touched_at) >= ABANDONED_AFTER
            })
            .map(|(id, _)| id.clone())
            .collect();

        abandoned
            .into_iter()
            .filter_map(|id| self.in_flight.remove(&id).map(|upload| (id, upload)))
            .collect()
    }
}

/// Why a chunk was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OutOfStep {
    /// The client sent from somewhere other than where this upload continues.
    #[error("this upload continues at byte {expected}, not at {received}")]
    WrongOffset {
        /// Where the server expects the next byte.
        expected: u64,
        /// Where the client sent from.
        received: u64,
    },

    /// The chunk would take the file past its declared size.
    #[error(
        "this upload was declared as {declared} bytes and this chunk would make it {would_be}"
    )]
    TooMuch {
        /// The size the client declared.
        declared: u64,
        /// What the size would become.
        would_be: u64,
    },
}

#[cfg(test)]
mod space {
    use super::{fits, DISK_HEADROOM};

    #[test]
    fn a_file_that_would_leave_the_headroom_behind_fits() {
        assert!(fits(10, DISK_HEADROOM + 10));
    }

    #[test]
    fn a_file_that_would_leave_exactly_the_headroom_behind_fits() {
        // The boundary is included on purpose: refusing a file that leaves
        // precisely the room the rule asks for would make the rule mean
        // something other than what it says.
        assert!(fits(10, DISK_HEADROOM + 10));
        assert!(fits(DISK_HEADROOM, DISK_HEADROOM * 2));
    }

    #[test]
    fn a_file_that_would_eat_into_the_headroom_does_not_fit() {
        assert!(!fits(10, DISK_HEADROOM + 9));
    }

    #[test]
    fn a_file_larger_than_the_whole_disk_does_not_fit() {
        assert!(!fits(1024 * 1024 * 1024 * 1024, DISK_HEADROOM * 4));
    }

    #[test]
    fn a_file_whose_size_would_overflow_does_not_fit() {
        // Not reachable through the API, which caps a declared size long before
        // this — but a rule that answers "yes" by wrapping around is the kind
        // that is reachable later, from somewhere that does not cap it.
        assert!(!fits(u64::MAX, u64::MAX));
        assert!(!fits(u64::MAX - 1, u64::MAX));
    }

    #[test]
    fn nothing_fits_on_a_disk_with_nothing_left() {
        assert!(!fits(1, 0));
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::geometry::SizePx;

    use super::*;

    fn declared(byte_size: u64) -> CreateUploadRequest {
        CreateUploadRequest {
            display_name: "Backdrop".to_owned(),
            original_filename: "backdrop.png".to_owned(),
            folder: String::new(),
            sha256: "a".repeat(64),
            byte_size,
            media_item_id: None,
            kind: ovs_types::media::MediaKind::Image,
            dimensions: SizePx::new(1920, 1080),
            duration_ms: None,
            frame_rate: None,
            has_alpha: false,
            container: "png".to_owned(),
            codec: "png".to_owned(),
        }
    }

    fn begin(uploads: &mut Uploads, byte_size: u64) -> String {
        let id = "01JCUPLOAD".to_owned();
        uploads.begin(&id, declared(byte_size), MediaItemId::new());
        id
    }

    #[test]
    fn an_upload_starts_at_the_beginning() {
        let mut uploads = Uploads::new();
        let id = begin(&mut uploads, 100);

        let session = uploads.get(&id).expect("in flight").session(&id);
        assert_eq!(session.received_bytes, 0);
        assert_eq!(session.resume_offset(), 0);
        assert_eq!(session.byte_size, 100);
        assert_eq!(session.chunk_size, CHUNK_SIZE);
        assert!(!session.is_complete());
    }

    #[test]
    fn chunks_in_order_fill_the_file() {
        let mut uploads = Uploads::new();
        let id = begin(&mut uploads, 30);

        for offset in [0, 10, 20] {
            let session = uploads
                .receive(&id, offset, 10)
                .expect("in flight")
                .expect("in step");
            assert_eq!(session.received_bytes, offset + 10);
        }
        assert!(uploads.get(&id).expect("in flight").is_complete());
    }

    #[test]
    fn a_chunk_from_the_wrong_place_is_refused_and_says_where_to_resume() {
        // A client that lost its answer retries. Telling it where the server
        // actually is turns a failed upload into a resumed one.
        let mut uploads = Uploads::new();
        let id = begin(&mut uploads, 30);
        uploads.receive(&id, 0, 10).expect("in flight").expect("ok");

        let error = uploads
            .receive(&id, 20, 10)
            .expect("in flight")
            .expect_err("out of step");
        assert_eq!(
            error,
            OutOfStep::WrongOffset {
                expected: 10,
                received: 20
            }
        );
        assert!(error.to_string().contains("continues at byte 10"));

        // And the upload is untouched, so the client can carry on from 10.
        assert_eq!(uploads.get(&id).expect("in flight").received_bytes, 10);
    }

    #[test]
    fn a_chunk_that_would_overrun_the_declared_size_is_refused() {
        // The hash check would catch it later. Catching it here means the
        // difference between a rejected chunk and a rejected upload.
        let mut uploads = Uploads::new();
        let id = begin(&mut uploads, 30);

        let error = uploads
            .receive(&id, 0, 31)
            .expect("in flight")
            .expect_err("too much");
        assert_eq!(
            error,
            OutOfStep::TooMuch {
                declared: 30,
                would_be: 31
            }
        );
        assert_eq!(uploads.get(&id).expect("in flight").received_bytes, 0);
    }

    #[test]
    fn an_upload_nobody_started_is_not_in_flight() {
        let mut uploads = Uploads::new();
        assert!(uploads.get("nothing").is_none());
        assert!(uploads.receive("nothing", 0, 1).is_none());
        assert!(uploads.take("nothing").is_none());
    }

    #[test]
    fn taking_an_upload_removes_it() {
        let mut uploads = Uploads::new();
        let id = begin(&mut uploads, 10);

        assert_eq!(uploads.len(), 1);
        let taken = uploads.take(&id).expect("in flight");
        assert_eq!(taken.declared.byte_size, 10);
        assert!(uploads.is_empty());
    }

    #[test]
    fn an_upload_nothing_has_arrived_for_is_eventually_abandoned() {
        // A client that stops halfway leaves a staged file behind. Nothing
        // cleans it up unless somebody decides when to.
        let mut uploads = Uploads::new();
        let id = begin(&mut uploads, 10);

        let now = Instant::now();
        assert!(uploads.take_abandoned(now).is_empty(), "still fresh");

        let later = now + ABANDONED_AFTER + Duration::from_secs(1);
        let abandoned = uploads.take_abandoned(later);
        assert_eq!(abandoned.len(), 1);
        assert_eq!(abandoned[0].0, id);
        assert!(uploads.is_empty());
    }

    #[test]
    fn a_chunk_keeps_an_upload_alive() {
        let mut uploads = Uploads::new();
        let id = begin(&mut uploads, 100);
        uploads.receive(&id, 0, 10).expect("in flight").expect("ok");

        // The clock the table uses is the one the chunk touched, so an upload
        // that is still moving is never collected out from under its client.
        let just_before = Instant::now() + ABANDONED_AFTER - Duration::from_secs(1);
        assert!(uploads.take_abandoned(just_before).is_empty());
    }
}
