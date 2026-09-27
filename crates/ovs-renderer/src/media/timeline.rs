//! Where a source is, in beats.
//!
//! The clock is the master clock and the unit is the beat, because that is what
//! a show is timed against: a clip's length is however many beats it takes at
//! the current tempo and the layer's speed ratio, and it gets shorter when the
//! tempo goes up.
//!
//! # The renderer keeps its own clock
//!
//! The core owns the authoritative tempo; the renderer owns a clock that follows
//! it. Two clocks sound like one too many, but the alternative is the core
//! sending a beat position sixty times a second, which is a stream of numbers
//! that are stale by the time they arrive. Following means re-anchoring whenever
//! the tempo changes, so the error is one message's latency rather than
//! something that accumulates.
//!
//! That is also why a trigger is a *counter* in the live state rather than a
//! beat. A beat from the core would be on the core's timeline, and the two
//! timelines have different origins; a counter says only "this happened", and
//! the renderer stamps it on the timeline it actually draws against.

use std::time::Instant;

use ovs_clock::{MasterClock, PlayingSource};
use ovs_types::config::slot::SlotSync;
use ovs_types::config::{PlaybackDirection, PlaybackMode};
use ovs_types::live::MasterLive;

/// The renderer's own beat clock, following the tempo the core publishes.
#[derive(Debug)]
pub struct ShowClock {
    clock: MasterClock,
    /// When this clock started, for the passes measured in seconds.
    started: Instant,
}

impl ShowClock {
    /// Starts a clock at the default tempo.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        let master = MasterLive::default();
        Self {
            clock: MasterClock::new(master.bpm, 4, now),
            started: now,
        }
    }

    /// Follows the tempo and speed the core published.
    ///
    /// Re-anchors rather than restarting, so the beat position does not jump
    /// when a tempo changes mid-show — the same rule the core's own clock keeps.
    pub fn follow(&mut self, master: &MasterLive, now: Instant) {
        if master.bpm != self.clock.bpm() {
            self.clock.set_bpm(master.bpm, now);
        }
        if master.speed_scale != self.clock.speed_scale() {
            self.clock.set_speed_scale(master.speed_scale, now);
        }
    }

    /// Returns the beat position now.
    #[must_use]
    pub fn beats_at(&self, now: Instant) -> f64 {
        self.clock.beats_at(now)
    }

    /// Returns the tempo being followed.
    #[must_use]
    pub const fn bpm(&self) -> f64 {
        self.clock.bpm()
    }

    /// Returns how many seconds have run since this clock started.
    ///
    /// The other unit a pass can be measured in. Wall-clock and monotonic: a
    /// free-running clip is not supposed to notice the tempo at all, so it is
    /// not read off anything the tempo touches.
    #[must_use]
    pub fn seconds_at(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.started).as_secs_f64()
    }
}

/// How long a source is, and how finely it is divided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLength {
    /// Duration of one pass in milliseconds, at the source's own rate.
    pub duration_ms: u64,
    /// How many frames it holds.
    pub frame_count: u32,
}

/// Where a source is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playhead {
    /// Position within the pass, from `0.0` to `1.0`.
    pub position: f32,
    /// Which frame that is.
    pub frame: u32,
    /// Whether the source has run out and should show nothing.
    ///
    /// Only [`PlaybackMode::OnceBlack`] ever ends. A loop never does, and a
    /// freeze holds its last frame, which is a frame rather than nothing.
    pub ended: bool,
}

/// A source that has been triggered, and how long its pass is in beats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Triggered {
    /// Where the pass started, in whatever [`pass`](Self::pass) is measured in.
    pub started_at: f64,
    /// How long one pass is, and what it is measured in.
    pub pass: Pass,
    /// How many frames the source holds.
    pub frame_count: u32,
}

/// What a pass is measured in, which is what "free run" and "beat sync" differ
/// about and the only thing they differ about.
///
/// Both are a length and a start, and both go through the same arithmetic — the
/// loop, the bounce, the freeze and the end are `PlayingSource`'s and are the
/// same either way. What changes is the clock the two numbers are read off.
///
/// **Free run is seconds.** A clip plays at the rate it was made at, whatever
/// the tempo is doing and whatever the master speed scale is set to. That is
/// what an operator expects of a file they have just imported, and it is the
/// default (owner, 2026-09-23).
///
/// **Beat sync is beats**, and the number of them comes from the slot. A clip
/// set to 8 beats takes 8 beats however long it actually is, so it follows the
/// tempo — raise the tempo and it plays faster, which is the point.
///
/// **What must never be recomputed is the unit.** A beat-synced clip whose pass
/// was turned into seconds from the current tempo would have the tempo cancel
/// out of its position entirely; a free-running one expressed in beats would be
/// dragged along by a tempo it is supposed to ignore, which is exactly the
/// fault the owner found — a clip playing at four times speed with every
/// control set to one. The pass stays in the unit its slot asked for, and the
/// tempo reaches a beat-locked clip by moving the clock rather than the pass.
///
/// **What may change under a running clip is the length**, because the beats
/// and the rate it is worked out from are live parameters (owner, 2026-09-23:
/// "Änderungen müssen sofort passieren"). That is [`Triggered::recut`], and it
/// keeps the picture where it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pass {
    /// One pass takes this many seconds, which is the clip's own length.
    Seconds(f64),
    /// One pass takes this many beats of the show's clock.
    Beats(f64),
}

impl Pass {
    /// How long one pass is, in its own units.
    #[must_use]
    pub const fn length(self) -> f64 {
        match self {
            Self::Seconds(length) | Self::Beats(length) => length,
        }
    }
}

/// Returns how long one pass of a source takes, and in what.
///
/// Worked out when the source is triggered and again whenever the slot's own
/// settings move under it — see [`Triggered::recut`], which is what makes the
/// beats and the rate live parameters rather than things that wait for the next
/// press of the pad.
///
/// `speed` is the slot's own rate, and it divides the pass in whichever unit
/// the pass is measured in: at `2.0` a free-running clip gets through its own
/// seconds in half of them, and a beat-locked one gets through its beats in
/// half of them. It is the only rate a free-running clip has — the master's
/// scale deliberately does not reach one, "weil 'Free' ist" (owner,
/// 2026-09-23) — and on a locked one it sits on top of the beats.
///
/// Returns `None` for a source with no duration — a still, which has one frame
/// and no timeline — for a beat-synced slot set to no beats, which is the same
/// thing said a different way, and for a rate that is not a positive number.
#[must_use]
pub fn pass_for(length: SourceLength, sync: SlotSync, speed: f64) -> Option<Pass> {
    // A rate of zero is a clip that never gets anywhere, and a negative one is
    // not how reverse is said (that is `PlaybackDirection`). Both are refused
    // here rather than turned into an enormous pass, which would look like a
    // clip that had frozen.
    if !(speed.is_finite() && speed > 0.0) {
        return None;
    }

    match sync.beats() {
        Some(beats) if beats > 0 => Some(Pass::Beats(f64::from(beats) / speed)),
        Some(_) => None,
        None => {
            let seconds = length.duration_ms as f64 / 1_000.0 / speed;
            (seconds > 0.0).then_some(Pass::Seconds(seconds))
        }
    }
}

/// How many of a clip's own frames a pass asks for each second.
///
/// **The number that decides whether a clip can be played at all**, and the one
/// nothing was computing when the owner found slot 2 of layer 1 standing still
/// (2026-09-23: "Freerun alles gut mit BPM Sync läuft null").
///
/// A pass shorter than the clip runs the clip faster, and a decoder answers a
/// frame it has not reached by repeating the last one it gave — so past its
/// ceiling the picture does not slow down, it stops. The ceiling is in frames a
/// second, not in a multiple of real time: a 25 fps clip at 8x and a 50 fps clip
/// at 4x are the same work.
///
/// Measured on the reference server, one 4K HEVC clip on the card, 600 passes at
/// sixty a second, counting the passes that had to hold the frame they had:
///
/// | frames a second asked for | held, of 600 |
/// |---|---|
/// | 60 | 0 |
/// | 120 | 0 |
/// | 180 | 0 |
/// | 240 | 125 |
/// | 360 | 285 |
/// | 540 | 390 |
///
/// The processor went *down* across that range, 0.28 cores to 0.20: past the
/// ceiling the decoder is not working harder, it is seeking instead of
/// decoding. That is what "läuft null" looks like from the outside — a machine
/// that appears idle while the wall stands still.
///
/// `beats_per_second` is the master's, already scaled. Zero for anything with
/// no length to run through.
#[must_use]
pub fn demanded_fps(length: SourceLength, pass: Pass, beats_per_second: f64) -> f64 {
    let seconds = match pass {
        Pass::Seconds(seconds) => seconds,
        Pass::Beats(beats) => {
            if !(beats_per_second.is_finite() && beats_per_second > 0.0) {
                return 0.0;
            }
            beats / beats_per_second
        }
    };
    if !(seconds.is_finite() && seconds > 0.0) || length.frame_count == 0 {
        return 0.0;
    }
    f64::from(length.frame_count) / seconds
}

/// Frames a second past which a clip is unlikely to keep up.
///
/// From the table on [`demanded_fps`]: this machine held nothing at 180 and a
/// fifth of its passes at 240. It is a number to warn at, never one to enforce
/// — a small clip goes far past it and a 4K one falls over below it, and the
/// renderer has no business refusing what an operator asked for.
pub const FPS_WORTH_WARNING_ABOUT: f64 = 200.0;

impl Triggered {
    /// Changes how long the pass is without moving the picture.
    ///
    /// **A slot's beats and its rate are live parameters** (owner, 2026-09-23:
    /// "Änderungen müssen sofort passieren"). They used to reach a clip only at
    /// its next trigger, because the pass was worked out once and kept — so an
    /// operator setting 54 beats on a clip that was running at 7 saw nothing
    /// happen until they hit the pad again.
    ///
    /// What makes this safe to do mid-flight is keeping the **fractional number
    /// of passes elapsed** rather than the position within one. Everything
    /// downstream is `elapsed / pass`: the position, the completed count, which
    /// leg of a bounce is running and whether a `Once` has frozen. Preserve the
    /// quotient and all four survive — the clip carries on from exactly where
    /// it was, at the new speed.
    ///
    /// Keeping only the position within the pass would have been the obvious
    /// thing and is wrong: it puts a bouncing clip back on its forward leg and
    /// un-freezes one that had finished.
    ///
    /// `now` is the moment in the units of the **new** pass, and `was` the same
    /// moment in the units of the old one. The two differ whenever a slot moves
    /// between free run and beat sync, which is a change of clock as well as of
    /// length, and is why both are asked for.
    pub fn recut(&mut self, pass: Pass, was: f64, now: f64) {
        let old = self.pass.length();
        if !(old.is_finite() && old > 0.0) || !was.is_finite() || !now.is_finite() {
            // Nothing to keep the place with. Start the new pass here, which is
            // what a clip with no length was doing anyway.
            self.started_at = now;
            self.pass = pass;
            return;
        }

        let passes = (was - self.started_at) / old;
        let started_at = now - passes * pass.length();
        self.started_at = if started_at.is_finite() {
            started_at
        } else {
            now
        };
        self.pass = pass;
    }
}

/// Returns where a triggered source is now.
#[must_use]
pub fn playhead_at(
    now: f64,
    triggered: Triggered,
    mode: PlaybackMode,
    direction: PlaybackDirection,
) -> Playhead {
    // `now` is in the same units as the pass: seconds for a free-running clip,
    // beats for a synced one. The arithmetic below does not care which, and
    // that is the whole reason this is two numbers rather than two code paths.
    let source =
        PlayingSource::new(triggered.started_at, triggered.pass.length(), mode).playing(direction);

    // `Frozen` reports the end of the clip, which is its last frame held. That
    // is a frame, so it is not an ending: only `Ended` takes a layer off stage.
    let (position, ended) = match source.at(now).position() {
        Some(position) => (position, false),
        None => (0.0, true),
    };

    Playhead {
        position: position as f32,
        frame: frame_for(position, triggered.frame_count),
        ended,
    }
}

/// Turns a position into a frame index.
///
/// The last frame is held rather than wrapping to the first: a position of
/// exactly `1.0` is the end of the clip, and showing frame zero there would put
/// a visible flash of the beginning at the end of every pass.
#[must_use]
pub fn frame_for(position: f64, frame_count: u32) -> u32 {
    if frame_count == 0 {
        return 0;
    }
    let last = frame_count - 1;
    if !position.is_finite() || position <= 0.0 {
        return 0;
    }

    let scaled = position * f64::from(frame_count);
    // `as` saturates at the integer's bounds for a finite float, and the clamp
    // covers the case above, so a position at or past the end holds the last
    // frame rather than reaching past it.
    (scaled as u32).min(last)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A four-second clip at 25 fps: one hundred frames.
    const CLIP: SourceLength = SourceLength {
        duration_ms: 4_000,
        frame_count: 100,
    };

    /// A slot that plays through in this many beats.
    const fn synced(beats: u32) -> SlotSync {
        SlotSync::BeatSync { beats }
    }

    #[test]
    fn a_free_running_clip_is_measured_in_its_own_seconds() {
        // The default, and the fault it was made the default by. A four-second
        // clip takes four seconds. Not four seconds at the tempo it was
        // triggered at, not four seconds divided by whatever the master speed
        // scale happens to be — four seconds.
        //
        // What went wrong before: the pass was worked out in beats at the
        // stored tempo while the beat itself advanced at tempo times the master
        // scale. With the scale at four, every clip played at four times speed
        // with every control in the show reading one, and no retrigger fixed
        // it because the wrong number was being kept rather than recomputed.
        let pass = pass_for(CLIP, SlotSync::FreeRun, 1.0).expect("a length");
        assert_eq!(pass, Pass::Seconds(4.0));

        // Half way through is half way through, at two seconds.
        let triggered = Triggered {
            started_at: 0.0,
            pass,
            frame_count: 100,
        };
        let half = playhead_at(
            2.0,
            triggered,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );
        assert!(
            (half.position - 0.5).abs() < 0.001,
            "two seconds into four: {}",
            half.position
        );
    }

    #[test]
    fn a_synced_clip_takes_the_beats_the_slot_asks_for() {
        // Beats rather than bars, and the number is the operator's: a clip set
        // to eight beats takes eight beats whatever it is actually four
        // seconds of. That is what makes it follow the tempo — the beats come
        // sooner when the tempo rises, so the clip plays faster.
        assert_eq!(pass_for(CLIP, synced(8), 1.0), Some(Pass::Beats(8.0)));
        assert_eq!(pass_for(CLIP, synced(1), 1.0), Some(Pass::Beats(1.0)));

        // Zero beats is not a length, and a slot that says so is refused the
        // same way a still is: there is no timeline to put a playhead on.
        assert_eq!(pass_for(CLIP, synced(0), 1.0), None);
    }

    /// The clip, triggered at beat zero at the given tempo.
    fn triggered_at(bpm: f64) -> Triggered {
        Triggered {
            started_at: 0.0,
            // Four seconds of clip, in beats at this tempo — which is what an
            // operator would set on the slot to have it run at its own rate at
            // this tempo, and the only way to say "one times" in beats.
            pass: pass_for(CLIP, synced((4.0 * bpm / 60.0).round() as u32), 1.0).expect("a length"),
            frame_count: CLIP.frame_count,
        }
    }

    fn at(beats: f64, mode: PlaybackMode) -> Playhead {
        // 60 BPM, so one beat is one second and the clip is four beats.
        playhead_at(beats, triggered_at(60.0), mode, PlaybackDirection::Forward)
    }

    #[test]
    fn a_clip_starts_at_its_first_frame() {
        let head = at(0.0, PlaybackMode::Loop);
        assert_eq!(head.frame, 0);
        assert!(head.position.abs() < f32::EPSILON);
        assert!(!head.ended);
    }

    #[test]
    fn the_playhead_moves_with_the_beat() {
        // A four-beat clip of a hundred frames: one beat in is a quarter of the
        // way through, which is frame twenty-five.
        assert_eq!(at(1.0, PlaybackMode::Loop).frame, 25);
        assert_eq!(at(2.0, PlaybackMode::Loop).frame, 50);
        assert_eq!(at(3.0, PlaybackMode::Loop).frame, 75);
    }

    #[test]
    fn a_clip_is_as_many_beats_long_as_the_tempo_it_was_triggered_at_makes_it() {
        // Four seconds is four beats at 60 BPM and eight at 120. Both play the
        // clip at its own rate; what differs is the grid it is expressed on, and
        // that grid is what a later tempo change then stretches.
        assert!((triggered_at(60.0).pass.length() - 4.0).abs() < 0.001);
        assert!((triggered_at(120.0).pass.length() - 8.0).abs() < 0.001);
    }

    #[test]
    fn a_clip_stays_the_length_in_beats_it_was_triggered_at() {
        // The property the whole design rests on. A clip triggered at 120 BPM is
        // eight beats long; the tempo going up afterwards makes those eight
        // beats pass sooner, so the clip plays faster and stays with the music.
        // Recomputing its length from the current tempo would instead move the
        // playhead the instant the tempo did.
        let triggered = triggered_at(120.0);

        let halfway = playhead_at(
            4.0,
            triggered,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );
        assert_eq!(halfway.frame, 50, "four of eight beats in");

        // The same beat, whatever the tempo now is: the pass length came with
        // the trigger.
        let same = playhead_at(
            4.0,
            triggered,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );
        assert_eq!(same.frame, halfway.frame);
    }

    #[test]
    fn the_rate_a_pass_demands_is_the_number_the_owner_was_never_told() {
        // 2026-09-23, from the running server: layer 1, slot 2, a 74.88-second
        // clip of 1872 frames, set to 7 beats, master at 120 BPM. The renderer
        // reported 274,479 late frames and no reason for one of them.
        let clip = SourceLength {
            duration_ms: 74_880,
            frame_count: 1_872,
        };
        let pass = pass_for(clip, synced(7), 1.0).expect("a length");
        let fps = demanded_fps(clip, pass, 2.0);

        // 1872 frames in 3.5 seconds.
        assert!((fps - 534.857).abs() < 0.01, "got {fps}");
        assert!(
            fps > FPS_WORTH_WARNING_ABOUT,
            "this is the setting that stopped the wall"
        );

        // Free run on the same clip is exactly its own rate, which is 25 fps and
        // well inside what the machine holds. That is why "Freerun alles gut".
        let free = pass_for(clip, SlotSync::FreeRun, 1.0).expect("a length");
        let own = demanded_fps(clip, free, 2.0);
        assert!((own - 25.0).abs() < 0.01, "got {own}");
        assert!(own <= FPS_WORTH_WARNING_ABOUT);

        // And the slot's own rate moves it, on free run as much as on beats,
        // because the master's scale does not reach a free-running clip at all.
        let doubled = pass_for(clip, SlotSync::FreeRun, 2.0).expect("a length");
        assert!((demanded_fps(clip, doubled, 2.0) - 50.0).abs() < 0.01);
        let halved = pass_for(clip, synced(7), 0.5).expect("a length");
        assert!((demanded_fps(clip, halved, 2.0) - fps / 2.0).abs() < 0.01);
    }

    #[test]
    fn re_cutting_a_pass_keeps_the_picture_and_changes_the_speed() {
        // The owner, 2026-09-23: "Im Slot Speedscale und Beats a Pass ändern.
        // Es erfordert jedes mal ein restart des Slots. Das darf nicht sein."
        //
        // Three passes and a quarter into an eight-beat loop, changed to four
        // beats. The picture must not move, and from there it must run twice as
        // fast.
        let mut triggered = Triggered {
            started_at: 0.0,
            pass: Pass::Beats(8.0),
            frame_count: 100,
        };
        let now = 26.0; // 3.25 passes of eight beats.
        let before = playhead_at(
            now,
            triggered,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );

        triggered.recut(Pass::Beats(4.0), now, now);
        let after = playhead_at(
            now,
            triggered,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );

        assert!(
            (f64::from(after.position) - f64::from(before.position)).abs() < 1e-9,
            "the picture moved: {} to {}",
            before.position,
            after.position
        );
        assert_eq!(after.frame, before.frame, "and so did the frame");

        // A beat later is now half a pass rather than an eighth.
        let later = playhead_at(
            now + 1.0,
            triggered,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );
        assert!(
            (f64::from(later.position - after.position) - 0.25).abs() < 1e-9,
            "got {}",
            later.position - after.position
        );
    }

    #[test]
    fn re_cutting_a_pass_does_not_un_freeze_or_turn_a_bounce_around() {
        // What the fractional pass count is for. Keeping only the position
        // within a pass — the obvious thing — loses both of these.
        let frozen_before = Triggered {
            started_at: 0.0,
            pass: Pass::Beats(4.0),
            frame_count: 100,
        };
        let now = 9.0; // Two and a quarter passes: long finished.
        let mut frozen = frozen_before;
        frozen.recut(Pass::Beats(16.0), now, now);
        let held = playhead_at(
            now,
            frozen,
            PlaybackMode::OnceFreeze,
            PlaybackDirection::Forward,
        );
        assert!(
            (held.position - 1.0).abs() < 1e-6,
            "a finished clip started again: {}",
            held.position
        );

        // And the return leg of a bounce stays the return leg.
        let mut bouncing = Triggered {
            started_at: 0.0,
            pass: Pass::Beats(4.0),
            frame_count: 100,
        };
        let now = 6.0; // One and a half passes: the way back.
        let before = playhead_at(
            now,
            bouncing,
            PlaybackMode::Bounce,
            PlaybackDirection::Forward,
        );
        bouncing.recut(Pass::Beats(2.0), now, now);
        let after = playhead_at(
            now,
            bouncing,
            PlaybackMode::Bounce,
            PlaybackDirection::Forward,
        );
        assert!(
            (after.position - before.position).abs() < 1e-6,
            "the bounce turned around: {} to {}",
            before.position,
            after.position
        );
    }

    #[test]
    fn re_cutting_across_the_two_clocks_keeps_the_place() {
        // Free run to beat sync is a change of clock as well as of length: the
        // pass stops being measured in seconds and starts being measured in
        // beats, and the moment has a different number in each.
        let mut triggered = Triggered {
            started_at: 10.0, // seconds
            pass: Pass::Seconds(4.0),
            frame_count: 100,
        };
        let seconds = 17.0; // 1.75 passes in.
        let beats = 34.0; // the same moment, at 120 BPM
        let before = playhead_at(
            seconds,
            triggered,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );

        triggered.recut(Pass::Beats(8.0), seconds, beats);
        let after = playhead_at(
            beats,
            triggered,
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );

        assert!(
            (after.position - before.position).abs() < 1e-6,
            "the picture moved: {} to {}",
            before.position,
            after.position
        );
    }

    #[test]
    fn a_rate_that_is_not_a_rate_is_not_a_pass() {
        // Zero is a clip that never gets anywhere and a negative one is not how
        // reverse is said. Refused here rather than turned into an enormous
        // pass, which on a wall would look like a clip that had frozen.
        assert_eq!(pass_for(CLIP, SlotSync::FreeRun, 0.0), None);
        assert_eq!(pass_for(CLIP, SlotSync::FreeRun, -1.0), None);
        assert_eq!(pass_for(CLIP, synced(8), f64::NAN), None);
    }

    #[test]
    fn the_speed_ratio_makes_a_clip_longer_or_shorter() {
        // Half speed is twice as many beats, which is a clip that takes twice as
        // long to get through.
        let normal = pass_for(CLIP, synced(8), 1.0).expect("length").length();
        let half = pass_for(CLIP, synced(16), 1.0).expect("length").length();
        let double = pass_for(CLIP, synced(4), 1.0).expect("length").length();

        assert!(
            (half - normal * 2.0).abs() < 0.001,
            "twice the beats, half the speed"
        );
        assert!(
            (double - normal / 2.0).abs() < 0.001,
            "half the beats, twice the speed"
        );
    }

    #[test]
    fn a_loop_comes_back_to_the_beginning() {
        assert_eq!(at(4.0, PlaybackMode::Loop).frame, 0);
        assert_eq!(at(5.0, PlaybackMode::Loop).frame, 25);
    }

    #[test]
    fn a_clip_that_has_run_out_says_so_rather_than_holding_a_frame() {
        // OnceBlack is the mode an operator picks when the clip should leave the
        // stage when it is done. Holding the last frame would be OnceFreeze.
        let head = at(5.0, PlaybackMode::OnceBlack);
        assert!(head.ended);

        let frozen = at(5.0, PlaybackMode::OnceFreeze);
        assert!(!frozen.ended);
        assert_eq!(frozen.frame, 99, "the last frame, held");
    }

    #[test]
    fn a_trigger_restarts_the_clip_wherever_the_beat_is() {
        // Two beats into the show, an operator hits the pad. The clip starts at
        // its first frame, not two beats in.
        let head = playhead_at(
            2.0,
            Triggered {
                started_at: 2.0,
                ..triggered_at(60.0)
            },
            PlaybackMode::Loop,
            PlaybackDirection::Forward,
        );

        assert_eq!(head.frame, 0);
    }

    #[test]
    fn a_still_has_no_pass_length() {
        let still = SourceLength {
            duration_ms: 0,
            frame_count: 1,
        };
        assert!(pass_for(still, SlotSync::FreeRun, 1.0).is_none());
    }

    #[test]
    fn the_end_of_a_pass_holds_the_last_frame_rather_than_flashing_the_first() {
        // A position of exactly 1.0 is the end of the clip. Frame zero there
        // would be one frame of the beginning at the end of every pass, which
        // on a loop is a visible stutter once a bar.
        assert_eq!(frame_for(1.0, 100), 99);
        assert_eq!(frame_for(0.999, 100), 99);
        assert_eq!(frame_for(2.0, 100), 99, "and past the end too");
    }

    #[test]
    fn a_nonsense_position_is_the_first_frame() {
        assert_eq!(frame_for(f64::NAN, 100), 0);
        assert_eq!(frame_for(f64::NEG_INFINITY, 100), 0);
        assert_eq!(frame_for(-1.0, 100), 0);
    }

    #[test]
    fn a_source_with_no_frames_has_no_frame_to_show() {
        assert_eq!(frame_for(0.5, 0), 0);
    }

    #[test]
    fn the_clock_follows_a_tempo_change_without_jumping() {
        // Re-anchoring rather than restarting. An operator changing the tempo
        // mid-show must not make every clip on stage jump.
        let start = Instant::now();
        let mut clock = ShowClock::new(start);

        let master = MasterLive {
            bpm: 120.0,
            ..MasterLive::default()
        };
        clock.follow(&master, start);

        let one_second = start + Duration::from_secs(1);
        let before = clock.beats_at(one_second);

        let faster = MasterLive {
            bpm: 240.0,
            ..MasterLive::default()
        };
        clock.follow(&faster, one_second);

        let after = clock.beats_at(one_second);
        assert!(
            (before - after).abs() < 0.001,
            "the beat position jumped: {before} to {after}"
        );
        assert!((clock.bpm() - 240.0).abs() < f64::EPSILON);
    }

    #[test]
    fn following_the_same_tempo_twice_changes_nothing() {
        let start = Instant::now();
        let mut clock = ShowClock::new(start);
        let master = MasterLive::default();

        clock.follow(&master, start);
        let later = start + Duration::from_secs(2);
        let before = clock.beats_at(later);
        clock.follow(&master, later);

        assert!((clock.beats_at(later) - before).abs() < f64::EPSILON);
    }
}
