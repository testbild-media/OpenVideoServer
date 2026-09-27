//! One playing source, decoded ahead of where it is being watched.
//!
//! The render thread asks for the frame at an index sixty times a second and
//! must never wait for one. So a player owns its source on a worker thread, runs
//! ahead of the playhead, and answers immediately with whatever it has.
//!
//! # A late decoder stutters; it does not flash
//!
//! When the frame that was asked for is not ready, the player answers with the
//! one it gave last time. Holding a frame for an extra tick is something an
//! audience may not notice; a black frame between two good ones is something
//! nobody misses. The miss is counted, because a decoder that cannot keep up is
//! a real fault and the operator should be able to see it.
//!
//! # Running ahead follows the playhead
//!
//! A clip playing backwards is not a pathological case — it is a playback
//! direction an operator chooses — so the decoder watches which way
//! the requested index is moving and runs ahead in that direction. This is
//! affordable because the codecs that matter here are all-intra: every ProRes
//! frame is a keyframe, so frame *n-1* costs exactly what frame *n+1* costs.
//!
//! # The ring is a memory budget
//!
//! Frames are held decoded, and a 1080p frame is eight megabytes. Twelve layers
//! each holding a large ring would be gigabytes of video memory's worth of host
//! memory, so the ring is deliberately short: far enough ahead to cover a
//! decode that runs long, not so far that a server with a full show cannot hold
//! it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use ovs_types::id::MediaItemId;

use crate::media::source::{Frame, FrameSource};
use crate::media::timeline::SourceLength;

/// How many frames a player keeps decoded ahead of the playhead.
///
/// Four is about seventy milliseconds at 60 fps: long enough to cover a decode
/// that runs long or a scheduling hiccup, short enough that twelve layers of
/// 1080p cost hundreds of megabytes rather than gigabytes.
///
/// A source may ask for more — [`FrameSource::lead_frames`] — and one
/// decoding on the card does, because a seek there costs two hundred and
/// twenty milliseconds and a loop may not show a still frame.
///
/// > A deeper ring was tried once and measured as *worse*: eight made one
/// > layer hold 645 stale frames in twenty seconds where four held six. That
/// > was this constant being used in two places — the worker's lead and the
/// > render side's discard window — so a worker running eight ahead had
/// > everything past four binned on arrival. The window follows the lead now.
pub const FRAMES_AHEAD: u32 = 4;

/// How long the worker waits when it has nothing to do.
///
/// Short enough that the ring refills the moment the render thread drains it,
/// long enough that an idle player is not a busy loop on a core the show needs.
const IDLE_SLEEP: Duration = Duration::from_millis(2);

/// How far the requested index may jump before this counts as a seek.
///
/// Beyond what is in flight there is nothing useful decoded, so catching up by
/// decoding forward would waste every frame it produced. Inside it, the frames
/// already on the way are the ones that were asked for.
///
/// Two more than the lead the worker is allowed, so that the ordinary
/// arithmetic of being exactly at the budget can never be mistaken for an
/// operator jumping. What happens when it is mistaken is in `lead`.
const fn seek_distance(ahead: u32) -> u32 {
    ahead + 2
}

/// What a player is doing, for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerReport {
    /// Whether the worker is still running.
    pub active: bool,
    /// Frames served from the ring, as asked for.
    pub served: u64,
    /// Frames the player had to hold because the next one was not ready.
    ///
    /// Not the same as a dropped frame: the show kept its rate and one frame
    /// was shown twice. Persistently non-zero is a decoder that cannot keep up.
    ///
    /// A frame asked for twice is not counted here. A clip whose rate is below
    /// the show's is asked for each of its frames several times over, and so is
    /// a layer that is simply not moving; neither is anything being late for.
    pub late: u64,
    /// Whether the source has failed.
    pub failed: bool,
}

/// One source, playing.
#[derive(Debug)]
pub struct Player {
    media_item_id: MediaItemId,
    length: SourceLength,
    /// How many bits a sample of this source carries. Fixed once it is open.
    decode_depth: ovs_types::depth::DecodeDepth,
    /// What the render thread last asked for, read by the worker.
    wanted: Arc<AtomicU32>,
    running: Arc<AtomicBool>,
    late: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    frames: Receiver<Frame>,
    worker: Option<JoinHandle<()>>,
    /// Frames that have arrived and not yet been asked for.
    ready: Vec<Frame>,
    /// The frame last handed to the render thread.
    held: Option<Frame>,
    /// The index a late frame was last counted for.
    ///
    /// The render loop runs faster than any output presents — it sleeps at most
    /// five milliseconds, so it passes here two hundred times a second or more
    /// while a sixty hertz wall takes sixty frames. Without this, every one of
    /// those passes counts the same missed frame again, and the number an
    /// operator reads is a multiple of the loop's speed rather than a count of
    /// anything. Measured before it was fixed: a single 1080p60 layer reported a
    /// hundred and four late frames a second while sixty were being asked for.
    missed: Option<u32>,
    served: u64,
    /// How many frames this player keeps decoded ahead of the playhead.
    ///
    /// The source's own answer ([`FrameSource::lead_frames`]), and the same
    /// number on both sides of the ring: the worker stops decoding this far in
    /// front, and `frame` keeps a decoded frame this far from the playhead
    /// rather than binning it. Two different numbers there is what made a
    /// deeper ring measure as worse than a shallow one.
    ahead: u32,
}

impl Player {
    /// Starts playing a source.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] when the worker thread cannot
    /// be spawned.
    pub fn start(
        media_item_id: MediaItemId,
        mut source: Box<dyn FrameSource>,
    ) -> std::io::Result<Self> {
        let length = source.length();
        let frame_count = length.frame_count.max(1);
        // Asked once, here, because it cannot change: a source decodes what its
        // stream is for as long as it is open. Kept on the player so a telemetry
        // pass can read it without reaching across the thread the source now
        // lives on.
        let decode_depth = source.decode_depth();
        // Asked of the source rather than assumed: a decoder that is slow to
        // jump needs a deeper ring to pay for one out of.
        let ahead = source.lead_frames().max(1);
        let seek_distance = seek_distance(ahead);

        let wanted = Arc::new(AtomicU32::new(0));
        let running = Arc::new(AtomicBool::new(true));
        let late = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let (sender, frames) = mpsc::sync_channel::<Frame>(ahead as usize);

        let worker_wanted = Arc::clone(&wanted);
        let worker_running = Arc::clone(&running);
        let worker_failed = Arc::clone(&failed);

        let worker = std::thread::Builder::new()
            .name("ovs-player".to_owned())
            .spawn(move || {
                // Where the decoder has got to. Starts where the playhead is
                // rather than at zero, so a clip triggered part-way through
                // does not decode everything before it first.
                let mut cursor = worker_wanted.load(Ordering::Relaxed);
                let mut previous_wanted = cursor;
                let mut pending: Option<Frame> = None;
                // Which way the playhead was last seen moving. Remembered
                // rather than recomputed each pass: this thread runs far faster
                // than the playhead moves, so "it has not changed since I last
                // looked" is the normal case and must not be read as a change
                // of direction.
                let mut forwards = true;

                while worker_running.load(Ordering::Relaxed) {
                    let wanted = worker_wanted.load(Ordering::Relaxed);

                    // Watched rather than told, so reverse playback needs
                    // nothing said about it.
                    if wanted != previous_wanted {
                        forwards = travelling_forwards(previous_wanted, wanted, frame_count);
                        previous_wanted = wanted;
                    }

                    if distance(cursor, wanted, frame_count) > seek_distance {
                        // A trigger, a loop point, or an operator scrubbing.
                        // What is in flight is no longer what is wanted.
                        cursor = wanted;
                        pending = None;
                    }

                    // Far enough ahead. Counted here rather than left to the
                    // channel, which is not the budget it looks like: the
                    // render thread empties it into a list of its own on every
                    // pass, so it is nearly always empty and would let this
                    // thread run about twice `FRAMES_AHEAD` in front.
                    if pending.is_none()
                        && lead(cursor, wanted, forwards, frame_count)
                            .is_some_and(|in_front| in_front >= ahead)
                    {
                        std::thread::sleep(IDLE_SLEEP);
                        continue;
                    }

                    // A frame decoded but not yet handed over, because the
                    // ring was full. Kept rather than dropped: with a real
                    // codec, decoding it again to throw it away again is the
                    // most expensive thing this thread could do.
                    let frame = match pending.take() {
                        Some(frame) => frame,
                        None => match source.frame(cursor) {
                            Ok(frame) => frame,
                            Err(error) => {
                                // A source that fails once is assumed to keep
                                // failing. Saying so once and stopping beats
                                // filling a log at sixty lines a second.
                                tracing::warn!("{error}");
                                worker_failed.store(true, Ordering::Relaxed);
                                return;
                            }
                        },
                    };

                    match sender.try_send(frame) {
                        Ok(()) => cursor = advance(cursor, forwards, frame_count),
                        Err(mpsc::TrySendError::Full(frame)) => {
                            // Far enough ahead. Wait for the render thread to
                            // take one, and keep what was decoded.
                            pending = Some(frame);
                            std::thread::sleep(IDLE_SLEEP);
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                    }
                }
            })?;

        Ok(Self {
            media_item_id,
            length,
            decode_depth,
            wanted,
            running,
            late,
            failed,
            frames,
            worker: Some(worker),
            ready: Vec::new(),
            held: None,
            missed: None,
            ahead,
            served: 0,
        })
    }

    /// Which item this player is playing.
    #[must_use]
    pub const fn media_item_id(&self) -> MediaItemId {
        self.media_item_id
    }

    /// How long the source is.
    #[must_use]
    pub const fn length(&self) -> SourceLength {
        self.length
    }

    /// How many bits a sample of what this player is playing carries.
    ///
    /// One of three depths that are not each other: this is the material, and
    /// what the compositor and the cable do with it are two other numbers on
    /// `OutputTelemetry` (`ovs_types::depth`).
    #[must_use]
    pub const fn decode_depth(&self) -> ovs_types::depth::DecodeDepth {
        self.decode_depth
    }

    /// Returns the frame at an index, without ever waiting for it.
    ///
    /// Answers with the frame it last gave when the one asked for is not ready,
    /// and `None` only before the first frame has arrived at all — a layer that
    /// has just been triggered and has nothing yet, which draws its placeholder
    /// for a tick.
    pub fn frame(&mut self, index: u32) -> Option<Frame> {
        self.wanted.store(index, Ordering::Relaxed);
        self.collect();

        if let Some(position) = self.ready.iter().position(|frame| frame.index == index) {
            let frame = self.ready.swap_remove(position);
            // What is still near the playhead is what the ring is for. What is
            // not is a frame decoded before a seek, and handing that back later
            // would show a piece of where the clip used to be.
            let frame_count = self.length.frame_count.max(1);
            self.ready
                .retain(|ready| distance(index, ready.index, frame_count) <= self.ahead);
            self.served += 1;
            self.held = Some(frame.clone());
            self.missed = None;
            return Some(frame);
        }

        if let Some(held) = &self.held {
            if held.index == index {
                // The same frame, asked for again. Ordinary, and the common
                // case: a 25 fps clip on a 60 Hz output is asked for each of
                // its frames between two and three times, and a layer held
                // still is asked for one frame for as long as it is held.
                //
                // Counting these as late made a healthy show report thirty-five
                // late frames a second and put a number on the System screen
                // that sends an operator looking for a fault there is not.
                return Some(held.clone());
            }
            // Once for the frame, not once for every pass that wanted it.
            if self.missed != Some(index) {
                self.missed = Some(index);
                self.late.fetch_add(1, Ordering::Relaxed);
            }
        }

        self.held.clone()
    }

    /// Returns what this player is doing.
    #[must_use]
    pub fn report(&self) -> PlayerReport {
        PlayerReport {
            active: self.running.load(Ordering::Relaxed) && !self.failed.load(Ordering::Relaxed),
            served: self.served,
            late: self.late.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }

    /// Returns true when the source could not be decoded.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Takes whatever the worker has produced since the last look.
    fn collect(&mut self) {
        loop {
            match self.frames.try_recv() {
                Ok(frame) => self.ready.push(frame),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }
}

impl Drop for Player {
    /// Stops the worker and waits for it.
    ///
    /// A renderer changing slots must not leave a thread decoding a file that
    /// is about to be deleted.
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Returns true when the playhead is moving forwards.
///
/// A loop point is a jump backwards over nearly the whole clip, and a rewind is
/// a step backwards of one; telling them apart is what keeps a looping clip
/// decoding forwards through its own loop point.
fn travelling_forwards(previous: u32, current: u32, frame_count: u32) -> bool {
    if current == previous {
        return true;
    }
    let forward_gap = (current + frame_count - previous) % frame_count;
    let backward_gap = (previous + frame_count - current) % frame_count;
    forward_gap <= backward_gap
}

/// Returns how far in front of the playhead the decoder is, or `None` if it is
/// behind.
///
/// Being behind is what falling late looks like, and it is never a reason to
/// wait, so it is a different answer rather than a large number.
///
/// # Why this exists, and what it cost not to have it
///
/// `FRAMES_AHEAD` is a memory budget: four decoded 1080p frames is thirty-two
/// megabytes a layer, and the comment on it says twelve layers must fit. But
/// nothing was counting the lead. The channel was doing it by accident, and
/// the channel is emptied by the render thread on every pass — so the worker
/// could hold four in the channel, one in `pending`, one being decoded, *and*
/// the render thread's own list of four, which is ten frames rather than four.
///
/// Twice the memory was the smaller half. The other half was that `cursor`
/// then stood far enough in front of the playhead to trip `SEEK_DISTANCE` on
/// nearly every pass of a clip that was simply playing. Each trip threw away a
/// decoded frame and put the cursor back on the playhead, which made the source
/// seek backwards to reach it. On `ProRes` a seek costs one frame's decode
/// wherever it lands, so this was invisible. On anything with a GOP it costs a
/// keyframe and every frame between, and it was ruinous.
///
/// Measured on the test machine, one 1080p60 layer, 360 passes at sixty a
/// second, counting the passes that had to hold the frame they already had:
///
/// | clip | held, before | held, after |
/// |---|---|---|
/// | `ProRes` 422, 308 Mbit/s | 0 | 0 |
/// | H.264, 60-frame GOP, B-frames | 51 | 0 |
/// | H.264, 60-frame GOP, no B-frames | 27 | 0 |
/// | H.264, every frame a keyframe | 0 | 0 |
///
/// The gradient in the "before" column is the tell: the cost tracked the
/// distance to the last keyframe, and an all-intra H.264 clip behaved exactly
/// like `ProRes`. Nothing about the codec was ever the problem — the renderer
/// was seeking when nothing had asked it to.
fn lead(cursor: u32, wanted: u32, forwards: bool, frame_count: u32) -> Option<u32> {
    let frame_count = frame_count.max(1);
    let gap = if forwards {
        (cursor + frame_count - wanted % frame_count) % frame_count
    } else {
        (wanted % frame_count + frame_count - cursor % frame_count) % frame_count
    };
    // A cursor one frame behind reads as one short of a whole clip in front.
    // Half way round is the only place to divide the two, and a real lead never
    // comes near it — `SEEK_DISTANCE` has caught anything that does.
    (gap <= frame_count / 2).then_some(gap)
}

/// Returns how far apart two positions are, the short way round.
fn distance(from: u32, to: u32, frame_count: u32) -> u32 {
    let forwards = (to + frame_count - from % frame_count) % frame_count;
    let backwards = (from % frame_count + frame_count - to) % frame_count;
    forwards.min(backwards)
}

/// Returns the next position in the direction of travel.
///
/// Wraps at both ends, so a looping clip's loop point is already decoded when
/// the playhead reaches it rather than being a seek every pass.
fn advance(cursor: u32, forwards: bool, frame_count: u32) -> u32 {
    if forwards {
        (cursor + 1) % frame_count
    } else {
        (cursor + frame_count - 1) % frame_count
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ovs_types::geometry::SizePx;

    use super::*;
    use crate::media::source::{StillSource, SyntheticSource};
    use crate::media::still::{DecodedImage, MediaError};
    use crate::media::timeline::SourceLength;

    const CLIP_FRAMES: u32 = 100;

    fn player() -> Player {
        Player::start(
            MediaItemId::new(),
            Box::new(SyntheticSource::new(SizePx::new(8, 8), 4_000, CLIP_FRAMES)),
        )
        .expect("a player")
    }

    /// Asks for a frame until the right one arrives, or gives up.
    ///
    /// The worker is a real thread, so a test that asked once would be testing
    /// the scheduler of whatever machine it ran on.
    fn wait_for(player: &mut Player, index: u32) -> Frame {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(frame) = player.frame(index) {
                if frame.index == index {
                    return frame;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("frame {index} never arrived");
    }

    /// A source that records every index it was asked to decode.
    ///
    /// The whole point of a ring is that each frame is decoded once. Nothing
    /// could see whether that was true until this existed, and it was not.
    struct Recording {
        inner: SyntheticSource,
        asked: Arc<std::sync::Mutex<Vec<u32>>>,
    }

    impl FrameSource for Recording {
        fn length(&self) -> SourceLength {
            self.inner.length()
        }

        fn frame(&mut self, index: u32) -> Result<Frame, MediaError> {
            self.asked.lock().expect("not poisoned").push(index);
            self.inner.frame(index)
        }
    }

    /// A source that is quick in a straight line and slow to jump.
    ///
    /// What a hardware decoder is: two milliseconds a frame and two hundred
    /// and twenty to seek. The numbers are scaled down so the test runs in a
    /// moment, and the ratio — a jump costing a hundred sequential frames — is
    /// what matters.
    struct SlowToJump {
        inner: SyntheticSource,
        next: Option<u32>,
        ahead: u32,
    }

    impl FrameSource for SlowToJump {
        fn length(&self) -> SourceLength {
            self.inner.length()
        }

        fn frame(&mut self, index: u32) -> Result<Frame, MediaError> {
            let sequential = self.next == Some(index);
            self.next = Some((index + 1) % CLIP_FRAMES.max(1));
            if !sequential {
                std::thread::sleep(Duration::from_millis(20));
            }
            self.inner.frame(index)
        }

        fn lead_frames(&self) -> u32 {
            self.ahead
        }
    }

    #[test]
    fn a_clip_going_round_does_not_stop() {
        // The owner's requirement, and not negotiable: "Bei einem Loop darf
        // kein Standbild sein." A loop point is a jump backwards to the start,
        // and on a decoder that is slow to jump the playhead runs out of frames
        // while it happens — which on a wall is a clip that freezes every time
        // it goes round.
        //
        // The ring is what pays for it, and this is what says the ring is deep
        // enough and that both halves of it agree. With the render side's
        // discard window pinned at `FRAMES_AHEAD` while the worker ran further
        // ahead, everything past four was binned on arrival and a deeper ring
        // made things *worse*; this fails in that state.
        let period = Duration::from_millis(2);
        // Twenty milliseconds of jump, at two milliseconds a pass, is ten
        // passes to cover. Sixteen is the same margin the card gets.
        let mut player = Player::start(
            MediaItemId::new(),
            Box::new(SlowToJump {
                inner: SyntheticSource::new(SizePx::new(8, 8), 4_000, CLIP_FRAMES),
                next: None,
                ahead: 16,
            }),
        )
        .expect("a player");

        // Round the clip once to fill the ring, then twice more through the
        // loop point with the playhead paced as a show would pace it.
        for index in 0..CLIP_FRAMES {
            let _ = player.frame(index);
            std::thread::sleep(period);
        }
        let settled = player.report().late;

        let mut wrong = Vec::new();
        for pass in 0..CLIP_FRAMES * 2 {
            let index = pass % CLIP_FRAMES;
            match player.frame(index) {
                Some(frame) if frame.index == index => {}
                other => wrong.push((index, other.map(|frame| frame.index))),
            }
            std::thread::sleep(period);
        }

        assert!(
            wrong.is_empty(),
            "the clip stopped at its loop point: {wrong:?}"
        );
        assert_eq!(
            player.report().late - settled,
            0,
            "no frame was late across two loops"
        );
    }

    /// A player whose source says what it was asked for.
    fn recording_player() -> (Player, Arc<std::sync::Mutex<Vec<u32>>>) {
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = Recording {
            inner: SyntheticSource::new(SizePx::new(8, 8), 4_000, CLIP_FRAMES),
            asked: Arc::clone(&asked),
        };
        let player = Player::start(MediaItemId::new(), Box::new(source)).expect("a player");
        (player, asked)
    }

    #[test]
    fn a_clip_playing_forwards_decodes_each_frame_once() {
        // The regression this exists for. The worker's lead was bounded by the
        // channel rather than counted, and the render thread empties the
        // channel on every pass — so the cursor ran about twice `FRAMES_AHEAD`
        // in front, far enough for `SEEK_DISTANCE` to read a clip that was
        // simply playing as an operator jumping. Every one of those threw away
        // a decoded frame and made the source seek backwards to re-decode it.
        //
        // On all-intra media that was invisible: a seek costs one frame's
        // decode wherever it lands. On a long-GOP codec it costs a keyframe and
        // every frame between, which is why it stayed hidden until the server
        // started accepting H.264.
        let (mut player, asked) = recording_player();
        wait_for(&mut player, 0);

        for index in 1..20 {
            wait_for(&mut player, index);
        }
        // Let the worker settle so that what it decoded after the last ask is
        // counted too, rather than landing after the assertion.
        std::thread::sleep(Duration::from_millis(50));

        let asked = asked.lock().expect("not poisoned").clone();
        let mut seen = std::collections::BTreeSet::new();
        let repeated: Vec<u32> = asked
            .iter()
            .copied()
            .filter(|index| !seen.insert(*index))
            .collect();
        assert!(
            repeated.is_empty(),
            "frames decoded more than once: {repeated:?} of {asked:?}"
        );
    }

    #[test]
    fn a_player_never_runs_further_ahead_than_its_budget() {
        // `FRAMES_AHEAD` is a memory budget — four decoded 1080p frames is
        // thirty-two megabytes a layer, and twelve layers have to fit. A worker
        // that quietly runs ten ahead is two and a half times that.
        let (mut player, asked) = recording_player();
        wait_for(&mut player, 0);
        std::thread::sleep(Duration::from_millis(50));

        let furthest = asked
            .lock()
            .expect("not poisoned")
            .iter()
            .copied()
            .max()
            .unwrap_or(0);
        assert!(
            furthest <= FRAMES_AHEAD,
            "the worker decoded as far as {furthest} while the playhead sat on 0"
        );
    }

    #[test]
    fn being_behind_the_playhead_is_not_a_lead() {
        // The difference that keeps a late decoder hurrying rather than
        // sleeping: one frame behind reads as ninety-nine in front on a
        // hundred-frame clip, and answering with that number would have the
        // worker wait exactly when it must not.
        assert_eq!(lead(4, 0, true, 100), Some(4));
        assert_eq!(lead(0, 0, true, 100), Some(0));
        assert_eq!(lead(0, 1, true, 100), None, "one frame behind");
        assert_eq!(
            lead(0, 40, true, 100),
            None,
            "far enough behind to be lapped"
        );

        // Backwards, where the same arithmetic runs the other way.
        assert_eq!(lead(0, 4, false, 100), Some(4));
        assert_eq!(lead(96, 0, false, 100), Some(4), "across the loop point");
        assert_eq!(lead(1, 0, false, 100), None, "one frame behind");
    }

    #[test]
    fn a_player_answers_with_the_frame_that_was_asked_for() {
        let mut player = player();
        let frame = wait_for(&mut player, 0);

        assert_eq!(frame.index, 0);
        assert_eq!(SyntheticSource::index_in(&frame.image), Some(0));
    }

    #[test]
    fn a_player_runs_ahead_of_the_playhead() {
        // The reason it exists: at 60 Hz a frame is 16.7 ms, and a decode on
        // the render path is a dropped frame every time.
        let mut player = player();
        wait_for(&mut player, 0);

        // Give the worker a moment to fill the ring, then walk forward and
        // check the frames are there without waiting for any of them.
        std::thread::sleep(Duration::from_millis(50));
        for index in 1..=3 {
            let frame = player.frame(index).expect("a frame");
            assert_eq!(frame.index, index, "frame {index} was not ready");
        }
    }

    #[test]
    fn a_frame_that_is_not_ready_holds_the_last_one_rather_than_showing_nothing() {
        // A black frame between two good ones is something nobody misses.
        let mut player = player();
        let first = wait_for(&mut player, 0);

        // A frame nothing has decoded and which is too far away to be in
        // flight. The player answers at once, with what it had.
        let held = player.frame(90).expect("something to show");
        assert_eq!(held.index, first.index, "the last good frame is held");
        assert!(player.report().late >= 1, "and the miss is counted");
    }

    #[test]
    fn the_same_frame_asked_for_twice_is_not_a_late_frame() {
        // What every clip slower than the show does, all the time. A 25 fps
        // clip on a 60 Hz output is asked for each of its frames between two
        // and three times, and none of those asks is a decoder falling behind.
        let mut player = player();
        let first = wait_for(&mut player, 0);

        let again = player.frame(first.index).expect("the same frame");

        assert_eq!(again.index, first.index);
        assert_eq!(
            player.report().late,
            0,
            "holding the frame that was asked for is not holding a frame back"
        );
    }

    #[test]
    fn a_frame_that_was_missed_is_counted_once_however_often_it_is_asked_for() {
        // The render loop passes here far more often than any wall presents: it
        // sleeps at most five milliseconds, so two hundred passes a second meet
        // a sixty hertz output. Counting a miss on every pass made the number an
        // operator reads a multiple of the loop's speed — one 1080p60 layer
        // reported a hundred and four late frames a second while sixty were
        // being asked for, which is not a thing that can happen to frames.
        let mut player = player();
        wait_for(&mut player, 0);

        // Far enough ahead that the decoder cannot have it, asked for the way
        // the render loop asks: over and over at the same moment.
        let beyond = player.length().frame_count.saturating_sub(1).max(1);
        for _ in 0..50 {
            let _ = player.frame(beyond);
        }

        assert_eq!(
            player.report().late,
            1,
            "fifty passes wanting one frame is one frame that was not there"
        );
    }

    #[test]
    fn a_second_frame_that_is_missed_is_counted_again() {
        // The other half of it: counting once per frame must not become
        // counting once ever. A decoder that is behind for a second is behind
        // for sixty frames and should say so.
        let mut player = player();
        wait_for(&mut player, 0);

        let count = player.length().frame_count.max(2);
        let first = count.saturating_sub(1);
        let second = count.saturating_sub(2);
        for _ in 0..5 {
            let _ = player.frame(first);
        }
        for _ in 0..5 {
            let _ = player.frame(second);
        }

        assert!(
            player.report().late >= 2,
            "two different frames were wanted and neither was there: {}",
            player.report().late
        );
    }

    #[test]
    fn nothing_is_shown_before_the_first_frame_arrives() {
        // A layer just triggered draws its placeholder for a tick, which is
        // honest: there is nothing to show yet.
        let mut player = player();
        assert!(player.frame(0).is_none() || player.report().served > 0);
    }

    #[test]
    fn a_seek_is_followed_rather_than_caught_up_to() {
        // An operator triggering a clip part-way through, or a loop point. The
        // decoder abandons what it was doing rather than decoding everything in
        // between.
        let mut player = player();
        wait_for(&mut player, 0);

        let frame = wait_for(&mut player, 70);
        assert_eq!(frame.index, 70);
    }

    #[test]
    fn a_clip_playing_backwards_is_decoded_backwards() {
        // A playback direction an operator chooses, not a pathological case —
        // and affordable because every ProRes frame is a keyframe.
        let mut player = player();
        wait_for(&mut player, 50);
        wait_for(&mut player, 49);

        std::thread::sleep(Duration::from_millis(50));
        for index in [48_u32, 47, 46] {
            let frame = player.frame(index).expect("a frame");
            assert_eq!(frame.index, index, "frame {index} was not ready");
        }
    }

    #[test]
    fn a_still_plays_through_the_same_player() {
        // One path for "what is on this layer". The single-frame case going
        // through the scheduler is what stops it rotting.
        let image = DecodedImage::new(vec![9; 4 * 4 * 4], SizePx::new(4, 4)).expect("an image");
        let mut player =
            Player::start(MediaItemId::new(), Box::new(StillSource::new(image))).expect("a player");

        let frame = wait_for(&mut player, 0);
        assert_eq!(frame.index, 0);
        assert_eq!(player.length().frame_count, 1);
    }

    #[test]
    fn a_source_that_fails_says_so_once_and_stops() {
        // Sixty log lines a second about the same broken file would bury the
        // one line that explained it.
        struct Broken;
        impl FrameSource for Broken {
            fn length(&self) -> SourceLength {
                SourceLength {
                    duration_ms: 1_000,
                    frame_count: 25,
                }
            }
            fn frame(&mut self, _index: u32) -> Result<Frame, MediaError> {
                Err(MediaError::Undecodable {
                    path: "broken.mov".into(),
                    reason: "made of nothing".to_owned(),
                })
            }
        }

        let mut player = Player::start(MediaItemId::new(), Box::new(Broken)).expect("a player");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !player.failed() {
            let _ = player.frame(0);
            std::thread::sleep(Duration::from_millis(2));
        }

        assert!(player.failed(), "the failure is visible");
        assert!(!player.report().active, "and the player is not running");
    }

    #[test]
    fn dropping_a_player_stops_its_worker() {
        // A renderer changing slots must not leave a thread decoding a file
        // that is about to be deleted.
        let mut player = player();
        wait_for(&mut player, 0);
        drop(player);
        // Returning from drop() is the assertion: it joins the worker.
    }

    #[test]
    fn a_loop_point_is_a_step_rather_than_a_seek() {
        // Frame 99 to frame 0 is one frame forward in a looping clip, not a
        // jump back over the whole thing, and the decoder that treats it as a
        // seek stutters once per pass.
        assert!(travelling_forwards(99, 0, CLIP_FRAMES));
        assert!(!travelling_forwards(50, 49, CLIP_FRAMES));
        assert!(travelling_forwards(49, 50, CLIP_FRAMES));
        assert_eq!(distance(99, 0, CLIP_FRAMES), 1);
        assert_eq!(advance(99, true, CLIP_FRAMES), 0);
        assert_eq!(advance(0, false, CLIP_FRAMES), 99);
    }

    #[test]
    fn a_long_jump_is_a_seek_in_either_direction() {
        assert!(distance(10, 70, CLIP_FRAMES) > seek_distance(FRAMES_AHEAD));
        assert!(distance(70, 10, CLIP_FRAMES) > seek_distance(FRAMES_AHEAD));
        assert!(distance(2, 4, CLIP_FRAMES) <= seek_distance(FRAMES_AHEAD));
    }
}
