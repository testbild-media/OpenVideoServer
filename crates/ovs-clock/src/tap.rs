//! Tap tempo.
//!
//! An operator taps a button in time with the music and the clock follows. It
//! is on the Master Fixture, so it also arrives over DMX, where a
//! channel crossing a threshold is a tap.
//!
//! The awkward part is not the averaging. It is deciding which taps belong
//! together: an operator who taps four times, stops to do something else, and
//! taps again must not have the gap averaged in as a very slow beat.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ovs_types::config::{MAX_BPM, MIN_BPM};

/// How many intervals are averaged.
///
/// Four taps give three intervals, which is the smallest number that smooths a
/// shaky hand without making the tempo take a bar to respond.
const WINDOW: usize = 7;

/// How long a gap ends a run of taps.
///
/// One beat at the slowest tempo the clock accepts, derived rather than picked:
/// a gap longer than the longest possible beat cannot have been a beat. Writing
/// a number here instead would let the two drift apart, and the symptom would be
/// a tempo an operator can tap but the clock will not hold, or the reverse.
fn run_timeout() -> Duration {
    Duration::from_secs_f64(SECONDS_PER_MINUTE / MIN_BPM)
}

/// The shortest interval treated as a real tap.
///
/// Deliberately *not* derived from [`MAX_BPM`]. The clock accepts up to 999 BPM
/// because a layer running at a multiple of the master rate needs the headroom,
/// but nobody taps a button sixteen times a second. Anything faster than 100 ms
/// apart is contact bounce or a double press, and treating it as a tempo would
/// make a sticky button unusable.
const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Seconds in a minute, named so the conversions read as what they are.
const SECONDS_PER_MINUTE: f64 = 60.0;

/// Collects taps and turns them into a tempo.
#[derive(Debug, Clone, Default)]
pub struct TapTempo {
    last: Option<Instant>,
    intervals: VecDeque<Duration>,
}

impl TapTempo {
    /// Creates an empty tap tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a tap and returns the tempo it implies, if there is one yet.
    ///
    /// Returns `None` for the first tap of a run, because one tap is a moment
    /// and not an interval.
    pub fn tap(&mut self, now: Instant) -> Option<f64> {
        // The first tap of a run is a moment, not an interval.
        let previous = self.last.replace(now)?;
        let interval = now.saturating_duration_since(previous);

        if interval > run_timeout() {
            // The operator stopped and started again. Averaging the gap in
            // would produce a tempo they never tapped.
            self.intervals.clear();
            return None;
        }

        if interval < MIN_INTERVAL {
            // Contact bounce or a double press. Ignored, and deliberately not
            // recorded as the previous tap either, so the next real tap is
            // measured from the last real one.
            self.last = Some(previous);
            return None;
        }

        self.intervals.push_back(interval);
        while self.intervals.len() > WINDOW {
            self.intervals.pop_front();
        }

        self.tempo()
    }

    /// Returns the tempo implied by the taps so far.
    #[must_use]
    pub fn tempo(&self) -> Option<f64> {
        if self.intervals.is_empty() {
            return None;
        }

        let total: Duration = self.intervals.iter().sum();
        let average = total.as_secs_f64() / self.intervals.len() as f64;
        if average <= 0.0 {
            return None;
        }

        Some((60.0 / average).clamp(MIN_BPM, MAX_BPM))
    }

    /// Returns how many intervals are being averaged.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.intervals.len()
    }

    /// Forgets every tap.
    pub fn clear(&mut self) {
        self.last = None;
        self.intervals.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Taps `count` times at `bpm`, starting at `start`, and returns the tempo.
    fn tap_at(tracker: &mut TapTempo, start: Instant, bpm: f64, count: usize) -> Option<f64> {
        let interval = Duration::from_secs_f64(60.0 / bpm);
        let mut tempo = None;
        for index in 0..count {
            tempo = tracker.tap(start + interval * u32::try_from(index).expect("small"));
        }
        tempo
    }

    #[test]
    fn one_tap_is_a_moment_not_a_tempo() {
        let mut tracker = TapTempo::new();
        assert_eq!(tracker.tap(Instant::now()), None);
        assert_eq!(tracker.sample_count(), 0);
    }

    #[test]
    fn two_taps_already_give_a_tempo() {
        let start = Instant::now();
        let mut tracker = TapTempo::new();
        tracker.tap(start);
        let tempo = tracker
            .tap(start + Duration::from_millis(500))
            .expect("a tempo");
        assert!((tempo - 120.0).abs() < 0.01, "got {tempo}");
    }

    #[test]
    fn steady_tapping_gives_the_tempo_that_was_tapped() {
        let start = Instant::now();
        for bpm in [60.0, 90.0, 120.0, 128.0, 174.0] {
            let mut tracker = TapTempo::new();
            let tempo = tap_at(&mut tracker, start, bpm, 8).expect("a tempo");
            assert!((tempo - bpm).abs() < 0.01, "tapped {bpm}, got {tempo}");
        }
    }

    #[test]
    fn a_shaky_hand_is_smoothed() {
        // Alternating early and late by 20 ms around 120 BPM.
        let start = Instant::now();
        let mut tracker = TapTempo::new();
        let mut at = start;
        for index in 0..8 {
            tracker.tap(at);
            let wobble = if index % 2 == 0 { 20 } else { -20 };
            at += Duration::from_millis((500 + wobble) as u64);
        }
        let tempo = tracker.tempo().expect("a tempo");
        assert!(
            (tempo - 120.0).abs() < 1.0,
            "expected about 120, got {tempo}"
        );
    }

    #[test]
    fn stopping_and_starting_again_does_not_average_the_gap() {
        // The failure this prevents: four taps at 120, a pause to do something
        // else, then more taps, and the tempo collapsing to something nobody
        // tapped.
        let start = Instant::now();
        let mut tracker = TapTempo::new();
        tap_at(&mut tracker, start, 120.0, 4);
        assert!(tracker.tempo().is_some());

        // A long gap ends the run.
        let after_gap = start + Duration::from_secs(30);
        assert_eq!(
            tracker.tap(after_gap),
            None,
            "the first tap of a new run has no interval"
        );
        assert_eq!(tracker.sample_count(), 0, "and the old run is forgotten");

        let tempo = tap_at(&mut tracker, after_gap, 174.0, 6).expect("a tempo");
        assert!((tempo - 174.0).abs() < 0.01, "got {tempo}");
    }

    #[test]
    fn contact_bounce_is_ignored_without_losing_the_beat() {
        // A double press must not register, and must not become the reference
        // point for the next real tap either.
        let start = Instant::now();
        let mut tracker = TapTempo::new();

        tracker.tap(start);
        // A bounce 5 ms later.
        assert_eq!(tracker.tap(start + Duration::from_millis(5)), None);
        // The next real tap is measured from the first, not from the bounce.
        let tempo = tracker
            .tap(start + Duration::from_millis(500))
            .expect("a tempo");
        assert!((tempo - 120.0).abs() < 0.01, "got {tempo}");
    }

    #[test]
    fn only_the_most_recent_taps_count() {
        // An operator who speeds up should end at the new tempo, not halfway
        // between the old and the new.
        let start = Instant::now();
        let mut tracker = TapTempo::new();
        tap_at(&mut tracker, start, 60.0, 8);

        let switch_at = start + Duration::from_secs(8);
        tap_at(&mut tracker, switch_at, 174.0, 12);

        let tempo = tracker.tempo().expect("a tempo");
        assert!(
            (tempo - 174.0).abs() < 0.5,
            "expected the new tempo, got {tempo}"
        );
        assert_eq!(tracker.sample_count(), WINDOW);
    }

    #[test]
    fn the_slowest_tappable_tempo_is_the_slowest_the_clock_holds() {
        // The timeout and the tempo floor are the same fact, so every tempo the
        // clock accepts can actually be tapped, and nothing slower survives.
        let start = Instant::now();
        let mut tracker = TapTempo::new();

        // Just inside the timeout: the slowest real beat.
        tracker.tap(start);
        let tempo = tracker
            .tap(start + run_timeout() - Duration::from_millis(10))
            .expect("a tempo");
        assert!(
            (tempo - MIN_BPM).abs() < 0.2,
            "expected about the floor of {MIN_BPM}, got {tempo}"
        );

        // Just outside it: not a beat at all.
        tracker.clear();
        tracker.tap(start);
        assert_eq!(
            tracker.tap(start + run_timeout() + Duration::from_millis(10)),
            None,
            "a gap longer than the longest possible beat ends the run"
        );
    }

    #[test]
    fn a_sticky_button_does_not_become_a_tempo() {
        // The clock accepts up to 999 BPM because layers run at multiples of the
        // master rate, but nobody taps sixteen times a second. Treating bounce
        // as tempo would make a worn button unusable.
        let start = Instant::now();
        let mut tracker = TapTempo::new();
        tracker.tap(start);
        for offset_ms in [3, 7, 12, 20, 45, 80] {
            assert_eq!(
                tracker.tap(start + Duration::from_millis(offset_ms)),
                None,
                "{offset_ms} ms apart is bounce, not a beat"
            );
        }
        assert_eq!(tracker.sample_count(), 0);
    }

    #[test]
    fn clearing_forgets_everything() {
        let start = Instant::now();
        let mut tracker = TapTempo::new();
        tap_at(&mut tracker, start, 120.0, 4);
        assert!(tracker.tempo().is_some());

        tracker.clear();
        assert_eq!(tracker.tempo(), None);
        assert_eq!(tracker.sample_count(), 0);
        assert_eq!(tracker.tap(start + Duration::from_secs(10)), None);
    }

    #[test]
    fn a_tapped_tempo_can_be_handed_straight_to_the_clock() {
        // The two halves of the Master Fixture's tempo controls, working
        // together: tap, then set, and the beat position does not jump.
        use crate::MasterClock;

        let start = Instant::now();
        let mut clock = MasterClock::new(120.0, 4, start);
        let mut tracker = TapTempo::new();

        let tap_start = start + Duration::from_secs(2);
        let tempo = tap_at(&mut tracker, tap_start, 174.0, 6).expect("a tempo");

        let moment = tap_start + Duration::from_secs(2);
        let before = clock.beats_at(moment);
        clock.set_bpm(tempo, moment);

        assert!(
            (clock.beats_at(moment) - before).abs() < 1e-9,
            "the position must not jump"
        );
        assert!((clock.bpm() - 174.0).abs() < 0.01);
    }
}
