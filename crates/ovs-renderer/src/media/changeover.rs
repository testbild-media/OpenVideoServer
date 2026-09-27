//! Both slow halves of a slot change, off the render thread.
//!
//! # Why this exists
//!
//! Changing what a layer plays does two expensive things: it opens the incoming
//! clip and it lets go of the outgoing one. The scheduler did both inline, at the
//! moment a pad was triggered, and its own module documentation called the cost
//! acceptable because it is paid "when a slot changes rather than per frame". A
//! slot changes when an operator presses something, which is the one moment in a
//! show where a frame must not be missed.
//!
//! Measured on the reference machine with `ovs-decode-cost --churn`, sixteen
//! changeovers each, wall clock on the thread that did it — not processor time,
//! which hides a decoder waiting on the driver:
//!
//! | clip | opening | closing | a changeover at 60 Hz |
//! |------|---------|---------|-----------------------|
//! | ProRes 422 | 16.4 ms | 15.2 ms | 1.9 frames |
//! | H.264, on the processor | 47.7 ms | 32.0 ms | 4.8 frames |
//! | H.264, on the card (`h264_cuvid`) | 78.0 ms | **274.7 ms** | **21 frames** |
//! | HEVC, on the card | 49.6 ms | — | — |
//!
//! Closing is the larger half and by a long way: tearing down a hardware decoder
//! costs a quarter of a second, against fifteen milliseconds for a software one.
//! Dropping a [`Player`] joins its decode thread, and that thread does not finish
//! until the decoder it owns is gone, so the whole teardown landed on whichever
//! thread let the player go.
//!
//! That is what changed. A changeover was about two frames when the media was
//! ProRes and everything decoded on the processor — invisible. It is about
//! twenty-one frames with H.264 on the card, which is a third of a second of
//! frozen wall every time a pad is pressed (owner, 2026-09-25: "Es passiert
//! offensichtlich nur beim Starten der Medien", and "das Problem hatten wir
//! vorher nicht. Irgendwo muss sich was geändert haben" — it was `8423f46`,
//! *Decode on the card*, which made opening 1.6 times dearer and closing
//! eighteen times dearer).
//!
//! # What the wall shows while a clip opens
//!
//! Whatever it was showing. The scheduler keeps the outgoing player until the
//! incoming one arrives and retires it *here*, so a trigger looks exactly as
//! instant as it did when both halves were inline — the difference is that every
//! other layer keeps its frame rate while one clip changes over.
//!
//! # The playhead starts where the pad was pressed
//!
//! Not where the player arrived. The scheduler stamps the clock position of the
//! trigger and hands it back when the player lands, so a beat-synced clip is at
//! the frame the beat says it should be at. A clip that took eighty milliseconds
//! to open therefore begins five frames in, which is the truth about when it was
//! triggered; starting it at frame zero instead would leave it permanently that
//! far behind the beat.
//!
//! # One pool for both halves
//!
//! Opening is mostly processor work — the H.264 figures above are about nine
//! tenths of a core for their whole duration — so a cue that starts several
//! layers finishes in rounds rather than one after another.
//!
//! Retiring shares those threads rather than having its own. The case that costs
//! is a show change, which retires every layer at once and can leave an open
//! queued behind a few teardowns; a show change is not a cue, and nothing is
//! waiting on a frame in the middle of one. A cue, which is the case that
//! matters, queues one open and one retire per layer and has the whole pool for
//! them.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ovs_types::id::{LayerId, MediaItemId};

use crate::media::{FrameSource, MediaError, Player};

/// How many changeovers may run at once.
///
/// Two cores are left alone whatever the machine has: the render thread is one
/// of them and the driver's threads are the other, and starving either to open a
/// file sooner would be the fault this module exists to remove. Capped at four
/// because a cue that starts seven layers is then two rounds rather than seven,
/// and a deeper pool buys a third of a round.
fn pool_size() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |cores| cores.get().saturating_sub(2).clamp(1, 4))
}

/// Which trigger an open belongs to.
///
/// Carried out and back so a result can be matched to what the layer is waiting
/// for *now*. An operator who presses a pad three times while the first open is
/// running has superseded it twice, and the two stale players are retired on
/// arrival rather than put on a wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpeningFor {
    /// The layer that is waiting.
    pub layer_id: LayerId,
    /// The item being opened.
    pub media_item_id: MediaItemId,
    /// The slot it was triggered from.
    pub slot: u16,
    /// The trigger counter that asked for it.
    pub trigger: u64,
}

/// Why a clip did not open.
///
/// Two cases rather than one, because they are two different faults and the
/// scheduler says two different things about them: a file that cannot be decoded
/// is news about the show, and a thread that cannot be spawned is news about the
/// machine.
#[derive(Debug)]
pub enum NotOpened {
    /// The file could not be read or decoded.
    Source(MediaError),
    /// It opened, but no thread could be had to play it.
    NoThread(std::io::Error),
}

impl std::fmt::Display for NotOpened {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(error) => error.fmt(formatter),
            Self::NoThread(error) => write!(formatter, "cannot start a player: {error}"),
        }
    }
}

/// One finished attempt at opening a clip.
pub struct Opened {
    /// What this was opened for.
    pub what: OpeningFor,
    /// The player, or why there is none.
    pub outcome: Result<Player, NotOpened>,
}

/// One half of a changeover, for a worker to do.
enum Job {
    /// Open this file and start a player on it.
    Open(OpeningFor, PathBuf),
    /// Let go of this player, which joins its decode thread and tears down
    /// whatever decoder it owns.
    Retire(Player),
}

/// How a path becomes something a player can read.
///
/// A parameter rather than a call, so the pool can be driven by a source that
/// needs no file: the tests below open a synthetic one on a delay, which is the
/// only way to exercise the queue's ordering and its superseding on a machine
/// with no decoder compiled in. It is given what the open is *for* as well as the
/// path, so the one line that says a clip opened can name the layer it opened
/// for.
type OpenSource =
    dyn Fn(&OpeningFor, &Path) -> Result<Box<dyn FrameSource>, MediaError> + Send + Sync;

/// Opens and retires clips on worker threads.
///
/// Dropping it closes the queue and waits for the workers, so a renderer shutting
/// down does not leave a decoder opening a file in a directory that is about to
/// go away — the same reason [`crate::media::MediaLoader`] joins.
pub struct Changeover {
    jobs: Option<Sender<Job>>,
    results: Receiver<Opened>,
    workers: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Changeover {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Changeover")
            .field("workers", &self.workers.len())
            .finish()
    }
}

impl Changeover {
    /// Starts the pool, opening real files.
    ///
    /// Never fails: a machine that cannot spawn these threads gets a pool with
    /// none, [`open`](Self::open) and [`retire`](Self::retire) say so, and the
    /// caller does the work itself. That is the behaviour this whole path
    /// replaced, and it is better than a show that plays nothing because a thread
    /// was refused.
    #[must_use]
    pub fn spawn() -> Self {
        Self::with_source(Arc::new(open_source))
    }

    /// A pool with nobody in it.
    ///
    /// For a scheduler whose sources open no file — a synthetic one, in a test —
    /// so it does not carry four threads it will never ask anything of.
    #[must_use]
    pub fn none() -> Self {
        let (_, results) = mpsc::channel();
        Self {
            jobs: None,
            results,
            workers: Vec::new(),
        }
    }

    /// Starts the pool with a way of opening something other than a file.
    #[must_use]
    pub fn with_source(open: Arc<OpenSource>) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (result_tx, result_rx) = mpsc::channel::<Opened>();
        // Shared rather than one queue per worker, so whichever worker is free
        // takes the next job. Per-worker queues would leave a cue waiting behind
        // one slow open while three threads idled.
        let queue = Arc::new(Mutex::new(job_rx));

        let mut workers = Vec::new();
        for index in 0..pool_size() {
            let queue = Arc::clone(&queue);
            let results = result_tx.clone();
            let open = Arc::clone(&open);
            let worker = std::thread::Builder::new()
                .name(format!("ovs-changeover-{index}"))
                .spawn(move || loop {
                    // The lock is held across the wait and released the moment a
                    // job is taken, so exactly one worker waits at a time and the
                    // rest are either working or next in line.
                    //
                    // A poisoned lock is stepped over: one worker that panicked
                    // on one file must not stop the others doing the rest (house
                    // rule §17).
                    let next = {
                        let queue = queue.lock().unwrap_or_else(|held| held.into_inner());
                        queue.recv()
                    };
                    let Ok(job) = next else {
                        return;
                    };

                    let (what, path) = match job {
                        Job::Open(what, path) => (what, path),
                        // The whole of retiring: letting the player go, here
                        // rather than on the render thread. Everything expensive
                        // about it is in its `Drop`.
                        Job::Retire(player) => {
                            drop(player);
                            continue;
                        }
                    };

                    let outcome =
                        open(&what, &path)
                            .map_err(NotOpened::Source)
                            .and_then(|source| {
                                Player::start(what.media_item_id, source)
                                    .map_err(NotOpened::NoThread)
                            });

                    // A closed result channel means the scheduler is gone, and
                    // the player goes with this message rather than being leaked.
                    if results.send(Opened { what, outcome }).is_err() {
                        return;
                    }
                });
            match worker {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    tracing::warn!("could not start a thread to change clips over on: {error}");
                }
            }
        }

        if workers.is_empty() {
            tracing::warn!("no thread to change clips over on; every trigger will do its own");
        }

        Self {
            jobs: Some(job_tx),
            results: result_rx,
            workers,
        }
    }

    /// Queues one clip to be opened.
    ///
    /// Returns false when there is nobody to open it, which the caller treats as
    /// "open it here" rather than as a failure of the clip.
    pub fn open(&self, what: OpeningFor, path: PathBuf) -> bool {
        self.send(Job::Open(what, path))
    }

    /// Queues one player to be let go of.
    ///
    /// Hands the player back when there is nobody to do it, so the caller can
    /// drop it itself rather than have it disappear — a swallowed player is a
    /// decoder left open on the machine least able to spare one. `None` means it
    /// has been taken.
    #[must_use]
    pub fn retire(&self, player: Player) -> Option<Player> {
        if self.workers.is_empty() {
            return Some(player);
        }
        let jobs = self.jobs.as_ref()?;
        match jobs.send(Job::Retire(player)) {
            Ok(()) => None,
            Err(held) => match held.0 {
                Job::Retire(player) => Some(player),
                Job::Open(..) => unreachable!("a retire came back as an open"),
            },
        }
    }

    /// Takes whatever has finished opening, without waiting.
    ///
    /// Called once per pass from the render loop, so it must never block: a clip
    /// still being opened simply is not in the result.
    pub fn collect(&self, into: &mut Vec<Opened>) {
        loop {
            match self.results.try_recv() {
                Ok(opened) => into.push(opened),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    fn send(&self, job: Job) -> bool {
        if self.workers.is_empty() {
            return false;
        }
        self.jobs
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok())
    }
}

impl Drop for Changeover {
    fn drop(&mut self) {
        // The queue closes first, so a worker waiting on it returns rather than
        // being joined while it waits.
        self.jobs = None;
        for worker in std::mem::take(&mut self.workers) {
            let _ = worker.join();
        }
    }
}

/// Opens a real file.
#[cfg(unix)]
fn open_source(what: &OpeningFor, path: &Path) -> Result<Box<dyn FrameSource>, MediaError> {
    let source = crate::media::VideoSource::open(path)?;
    let length = source.length();
    // Said here rather than where the player is adopted, because the width and
    // the height are the concrete source's and a boxed one cannot be asked.
    tracing::info!(
        layer = %what.layer_id,
        media_item_id = %what.media_item_id,
        path = %path.display(),
        width = source.size().width,
        height = source.size().height,
        frames = length.frame_count,
        duration_ms = length.duration_ms,
        "clip opened"
    );
    Ok(Box::new(source))
}

/// No decoder is compiled into this build.
///
/// The FFmpeg libraries are a Unix dependency; a Windows machine builds
/// everything else in this crate and none of the decoding.
#[cfg(not(unix))]
fn open_source(_what: &OpeningFor, path: &Path) -> Result<Box<dyn FrameSource>, MediaError> {
    Err(MediaError::Undecodable {
        path: path.to_path_buf(),
        reason: "this build has no video decoder in it".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::SyntheticSource;
    use ovs_types::geometry::SizePx;
    use std::time::{Duration, Instant};

    /// A pool whose source takes a known time and needs no file.
    fn slow_pool(delay: Duration) -> Changeover {
        Changeover::with_source(Arc::new(move |_what: &OpeningFor, _path: &Path| {
            std::thread::sleep(delay);
            Ok(
                Box::new(SyntheticSource::new(SizePx::new(64, 64), 1000, 25))
                    as Box<dyn FrameSource>,
            )
        }))
    }

    fn what(trigger: u64) -> OpeningFor {
        OpeningFor {
            layer_id: LayerId::new(),
            media_item_id: MediaItemId::new(),
            slot: 1,
            trigger,
        }
    }

    /// Waits for `count` results, or gives up. Tests only: the render loop never
    /// waits for one of these, which is the entire point of the pool.
    fn wait_for(pool: &Changeover, count: usize) -> Vec<Opened> {
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while got.len() < count && Instant::now() < deadline {
            pool.collect(&mut got);
            if got.len() < count {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        got
    }

    #[test]
    fn asking_for_a_clip_does_not_wait_for_it() {
        // The whole reason this exists. The request has to come back in
        // microseconds while the open itself takes a tenth of a second, or the
        // trigger still costs the render thread its frames.
        let pool = slow_pool(Duration::from_millis(100));

        let began = Instant::now();
        assert!(pool.open(what(1), PathBuf::from("anything")));
        let asking = began.elapsed();

        assert!(
            asking < Duration::from_millis(5),
            "asking took {asking:?}, which is a frame's worth of the thing this removes"
        );
        // And nothing has arrived yet, so `collect` is not secretly waiting.
        let mut arrived = Vec::new();
        pool.collect(&mut arrived);
        assert!(arrived.is_empty(), "collect waited for the open");

        assert_eq!(wait_for(&pool, 1).len(), 1, "and it does arrive");
    }

    #[test]
    fn letting_go_of_a_player_does_not_wait_for_it_either() {
        // The larger half, measured: 275 ms on the reference machine for a
        // hardware decoder. Handing it over has to cost nothing, because it
        // happens on the pass that puts the incoming clip on the wall.
        let pool = slow_pool(Duration::from_millis(1));
        assert!(pool.open(what(1), PathBuf::from("anything")));
        let arrived = wait_for(&pool, 1);
        let player = arrived
            .into_iter()
            .next()
            .expect("one player")
            .outcome
            .expect("it opened");

        let began = Instant::now();
        assert!(pool.retire(player).is_none(), "the pool would not take it");
        let handing_over = began.elapsed();

        assert!(
            handing_over < Duration::from_millis(5),
            "letting go took {handing_over:?}, on the thread that must not pay for it"
        );
    }

    #[test]
    fn a_result_says_which_trigger_it_was_for() {
        // What makes superseding possible: an operator pressing a pad three times
        // has two players in flight that must never reach the wall, and the only
        // thing that tells them apart is what they were asked for.
        let pool = slow_pool(Duration::from_millis(5));
        let layer = LayerId::new();
        let item = MediaItemId::new();

        for trigger in 1..=3 {
            assert!(pool.open(
                OpeningFor {
                    layer_id: layer,
                    media_item_id: item,
                    slot: 2,
                    trigger,
                },
                PathBuf::from("anything"),
            ));
        }

        let mut triggers: Vec<u64> = wait_for(&pool, 3)
            .iter()
            .map(|opened| opened.what.trigger)
            .collect();
        triggers.sort_unstable();
        assert_eq!(triggers, vec![1, 2, 3], "every trigger came back named");
    }

    #[test]
    fn several_clips_open_at_once_rather_than_one_after_another() {
        // A cue that starts a wall's worth of layers. Serially this is one delay
        // per clip; in a pool it is about one delay, and the difference is what an
        // operator sees as the second half of a cue arriving late.
        //
        // Returns early on a machine too small to have a pool, where one after
        // another is the honest answer and this would be asserting about the
        // machine rather than about the code.
        const DELAY: Duration = Duration::from_millis(120);
        let pool = slow_pool(DELAY);
        let workers = pool.workers.len();
        if workers < 2 {
            return;
        }

        let began = Instant::now();
        for trigger in 1..=workers as u64 {
            assert!(pool.open(what(trigger), PathBuf::from("anything")));
        }
        let all = wait_for(&pool, workers);
        let elapsed = began.elapsed();

        assert_eq!(all.len(), workers, "all of them opened");
        // Generous, because this is a timing test on a machine that may be doing
        // something else: the point is that it is nearer one delay than it is to
        // one delay per clip.
        assert!(
            elapsed < DELAY * workers as u32 / 2,
            "{workers} clips took {elapsed:?}, which is no better than one at a time"
        );
    }

    #[test]
    fn a_source_that_cannot_be_opened_comes_back_saying_so() {
        // The layer has to be told, once, rather than waiting for a player that
        // is never coming.
        let pool = Changeover::with_source(Arc::new(|_what: &OpeningFor, path: &Path| {
            Err(MediaError::Undecodable {
                path: path.to_path_buf(),
                reason: "nothing here can read this".to_owned(),
            })
        }));

        assert!(pool.open(what(1), PathBuf::from("broken.mov")));
        let arrived = wait_for(&pool, 1);
        assert_eq!(arrived.len(), 1);
        assert!(
            matches!(arrived[0].outcome, Err(NotOpened::Source(_))),
            "a broken file came back as something else"
        );
    }

    #[test]
    fn a_pool_with_nobody_in_it_hands_the_work_straight_back() {
        // The fallback that keeps a thread-starved machine playing: the caller
        // does it inline, which is what every caller did before this existed. A
        // retired player comes back rather than vanishing, or a slot change would
        // leak a decoder on the machine least able to spare one.
        let pool = Changeover::none();
        assert!(
            !pool.open(what(1), PathBuf::from("anything")),
            "a pool with nobody in it said it would open something"
        );

        let elsewhere = slow_pool(Duration::from_millis(1));
        assert!(elsewhere.open(what(1), PathBuf::from("anything")));
        let player = wait_for(&elsewhere, 1)
            .into_iter()
            .next()
            .expect("one player")
            .outcome
            .expect("it opened");
        assert!(
            pool.retire(player).is_some(),
            "a pool with nobody in it swallowed a player"
        );
    }
}
