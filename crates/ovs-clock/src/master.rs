//! The master clock.
//!
//! There is exactly one of these in the whole system. Layers
//! do not own a BPM; they own a ratio to this one.
//!
//! # Why an anchor rather than an accumulator
//!
//! The obvious implementation adds `elapsed * bpm / 60` to a running total each
//! frame. That drifts: every addition rounds, and at 60 fps for a six-hour show
//! the rounding is over a million operations deep. The specification requires no
//! cumulative BPM drift, so position is *computed* from an anchor instead:
//!
//! ```text
//! beats = anchor_beats + (now - anchor_at) * bpm / 60 * speed_scale
//! ```
//!
//! Nothing accumulates, so nothing drifts. Two calls a microsecond apart give
//! answers a microsecond apart, however long the show has been running.
//!
//! The anchor also solves the other half of the problem. Changing BPM by
//! rewriting a rate would make the beat position jump, and a jump is audible as
//! a stumble. Instead the clock re-anchors at the position it is already at, so
//! tempo changes are continuous by construction: only the slope changes.

use std::time::{Duration, Instant};

use ovs_types::config::{MAX_BPM, MAX_SPEED_SCALE, MIN_BPM, MIN_SPEED_SCALE};

/// Seconds in a minute, named so the conversions read as what they are.
const SECONDS_PER_MINUTE: f64 = 60.0;

/// Where the clock is, in musical terms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// Beats since the last reset. Fractional between beats.
    pub beats: f64,
    /// Which bar, counting from zero.
    pub bar: u64,
    /// Which beat within the bar, counting from zero.
    pub beat_in_bar: u32,
    /// How far through the current beat, from 0.0 up to but not including 1.0.
    pub phase: f64,
}

/// The single global clock.
#[derive(Debug, Clone)]
pub struct MasterClock {
    bpm: f64,
    beats_per_bar: u32,
    speed_scale: f64,
    /// Beat position at the moment of the last anchor.
    anchor_beats: f64,
    /// When that position was current.
    anchor_at: Instant,
}

impl MasterClock {
    /// Creates a clock running at `bpm`, anchored at `now` with the phase at zero.
    ///
    /// The BPM and beats-per-bar are clamped into their valid ranges rather than
    /// refused: this is the live clock, and a value arriving from a console must
    /// not be able to stop it.
    #[must_use]
    pub fn new(bpm: f64, beats_per_bar: u32, now: Instant) -> Self {
        Self {
            bpm: clamp_bpm(bpm),
            beats_per_bar: beats_per_bar.max(1),
            speed_scale: 1.0,
            anchor_beats: 0.0,
            anchor_at: now,
        }
    }

    /// Returns the current tempo.
    #[must_use]
    pub const fn bpm(&self) -> f64 {
        self.bpm
    }

    /// Returns how many beats make a bar.
    #[must_use]
    pub const fn beats_per_bar(&self) -> u32 {
        self.beats_per_bar
    }

    /// Returns the global speed scale.
    #[must_use]
    pub const fn speed_scale(&self) -> f64 {
        self.speed_scale
    }

    /// Returns the effective rate in beats per second, tempo times speed scale.
    #[must_use]
    pub fn beats_per_second(&self) -> f64 {
        self.bpm / SECONDS_PER_MINUTE * self.speed_scale
    }

    /// Returns how many beats have passed since the last reset.
    #[must_use]
    pub fn beats_at(&self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.anchor_at).as_secs_f64();
        self.anchor_beats + elapsed * self.beats_per_second()
    }

    /// Returns the full musical position.
    #[must_use]
    pub fn position_at(&self, now: Instant) -> Position {
        let beats = self.beats_at(now);
        let whole = beats.floor();
        let beat_index = whole.max(0.0) as u64;
        let per_bar = u64::from(self.beats_per_bar);

        Position {
            beats,
            bar: beat_index / per_bar,
            beat_in_bar: u32::try_from(beat_index % per_bar).unwrap_or(0),
            phase: beats - whole,
        }
    }

    /// Changes the tempo without moving the beat position.
    ///
    /// Re-anchors first, so the clock carries on from where it already was and
    /// only the slope changes. Setting a rate without re-anchoring would jump
    /// the position, which is audible as a stumble.
    pub fn set_bpm(&mut self, bpm: f64, now: Instant) {
        if bpm.is_nan() {
            // A corrupt value from a console must not change the tempo at all.
            return;
        }
        self.reanchor(now);
        self.bpm = clamp_bpm(bpm);
    }

    /// Changes the global speed scale without moving the beat position.
    pub fn set_speed_scale(&mut self, scale: f64, now: Instant) {
        if scale.is_nan() {
            return;
        }
        self.reanchor(now);
        self.speed_scale = scale.clamp(MIN_SPEED_SCALE, MAX_SPEED_SCALE);
    }

    /// Sets how many beats make a bar.
    ///
    /// Does not re-anchor: this changes how the same beat position is *described*,
    /// not where it is.
    pub fn set_beats_per_bar(&mut self, beats_per_bar: u32) {
        self.beats_per_bar = beats_per_bar.max(1);
    }

    /// Resets the phase: beat zero, bar zero, starting now.
    ///
    /// This is the Master Fixture's BPM Reset. The tempo is untouched; only the
    /// position is.
    pub fn reset(&mut self, now: Instant) {
        self.anchor_beats = 0.0;
        self.anchor_at = now;
    }

    /// Moves the anchor to the present without changing the position.
    fn reanchor(&mut self, now: Instant) {
        self.anchor_beats = self.beats_at(now);
        self.anchor_at = now;
    }

    /// Returns how long one beat lasts at the current rate.
    ///
    /// `None` when the clock is stopped, which a zero speed scale would mean.
    #[must_use]
    pub fn beat_duration(&self) -> Option<Duration> {
        let per_second = self.beats_per_second();
        if per_second <= 0.0 {
            return None;
        }
        Some(Duration::from_secs_f64(1.0 / per_second))
    }
}

fn clamp_bpm(bpm: f64) -> f64 {
    if bpm.is_nan() {
        return 128.0;
    }
    bpm.clamp(MIN_BPM, MAX_BPM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, seconds: f64) -> Instant {
        start + Duration::from_secs_f64(seconds)
    }

    #[test]
    fn a_hundred_and_twenty_bpm_is_two_beats_a_second() {
        let start = Instant::now();
        let clock = MasterClock::new(120.0, 4, start);
        assert!((clock.beats_at(at(start, 1.0)) - 2.0).abs() < 1e-9);
        assert!((clock.beats_at(at(start, 0.5)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_clock_does_not_drift_over_a_long_show() {
        // The reason position is computed rather than accumulated. Six hours at
        // 128 BPM is 46,080 beats, and it has to be exactly that.
        let start = Instant::now();
        let clock = MasterClock::new(128.0, 4, start);

        let six_hours = 6.0 * 60.0 * 60.0;
        let expected = 128.0 * six_hours / 60.0;
        let actual = clock.beats_at(at(start, six_hours));

        assert!(
            (actual - expected).abs() < 1e-6,
            "after six hours: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn reading_the_clock_repeatedly_does_not_move_it() {
        // An accumulator would advance on every read. This must not.
        let start = Instant::now();
        let clock = MasterClock::new(128.0, 4, start);
        let moment = at(start, 12.345);

        let first = clock.beats_at(moment);
        for _ in 0..10_000 {
            assert_eq!(clock.beats_at(moment), first);
        }
    }

    #[test]
    fn changing_tempo_does_not_move_the_beat_position() {
        // A jump here is audible as a stumble, which is why the clock
        // re-anchors rather than rewriting a rate.
        let start = Instant::now();
        let mut clock = MasterClock::new(120.0, 4, start);

        let moment = at(start, 3.0);
        let before = clock.beats_at(moment);
        assert!((before - 6.0).abs() < 1e-9);

        clock.set_bpm(174.0, moment);
        let after = clock.beats_at(moment);

        assert!(
            (after - before).abs() < 1e-9,
            "the position jumped: {before} to {after}"
        );
        assert_eq!(clock.bpm(), 174.0);
    }

    #[test]
    fn a_tempo_change_only_changes_the_slope() {
        let start = Instant::now();
        let mut clock = MasterClock::new(60.0, 4, start);

        // One beat a second for two seconds: two beats.
        let change_at = at(start, 2.0);
        assert!((clock.beats_at(change_at) - 2.0).abs() < 1e-9);

        // Two beats a second from here: two more seconds is four more beats.
        clock.set_bpm(120.0, change_at);
        assert!((clock.beats_at(at(start, 4.0)) - 6.0).abs() < 1e-9);
    }

    #[test]
    fn the_speed_scale_multiplies_the_rate_without_moving_the_position() {
        let start = Instant::now();
        let mut clock = MasterClock::new(120.0, 4, start);

        let change_at = at(start, 1.0);
        let before = clock.beats_at(change_at);
        clock.set_speed_scale(2.0, change_at);

        assert!((clock.beats_at(change_at) - before).abs() < 1e-9);
        // Double rate from here: one more second is four more beats.
        assert!((clock.beats_at(at(start, 2.0)) - (before + 4.0)).abs() < 1e-9);
    }

    #[test]
    fn a_reset_zeroes_the_phase_without_touching_the_tempo() {
        // The Master Fixture's BPM Reset.
        let start = Instant::now();
        let mut clock = MasterClock::new(174.0, 4, start);

        let reset_at = at(start, 7.3);
        assert!(clock.beats_at(reset_at) > 0.0);

        clock.reset(reset_at);
        assert_eq!(clock.beats_at(reset_at), 0.0);
        assert_eq!(clock.bpm(), 174.0, "a reset is not a tempo change");

        // And it carries on from zero at the same rate.
        assert!((clock.beats_at(at(start, 7.3 + 60.0 / 174.0)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_corrupt_tempo_leaves_the_clock_alone() {
        // A NaN from a console must not be able to stop a show.
        let start = Instant::now();
        let mut clock = MasterClock::new(128.0, 4, start);
        clock.set_bpm(f64::NAN, at(start, 1.0));
        assert_eq!(clock.bpm(), 128.0);

        clock.set_speed_scale(f64::NAN, at(start, 1.0));
        assert_eq!(clock.speed_scale(), 1.0);
    }

    #[test]
    fn out_of_range_values_clamp_rather_than_refusing() {
        let start = Instant::now();
        let mut clock = MasterClock::new(10_000.0, 4, start);
        assert_eq!(clock.bpm(), MAX_BPM);

        clock.set_bpm(0.0, start);
        assert_eq!(clock.bpm(), MIN_BPM);

        clock.set_speed_scale(100.0, start);
        assert_eq!(clock.speed_scale(), MAX_SPEED_SCALE);
    }

    #[test]
    fn a_bar_is_as_many_beats_as_it_is_told() {
        let start = Instant::now();
        let clock = MasterClock::new(60.0, 4, start);

        // One beat a second, so second 5 is bar 1, beat 1.
        let position = clock.position_at(at(start, 5.0));
        assert_eq!(position.bar, 1);
        assert_eq!(position.beat_in_bar, 1);
        assert!((position.beats - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_three_four_bar_wraps_after_three_beats() {
        let start = Instant::now();
        let clock = MasterClock::new(60.0, 3, start);
        assert_eq!(clock.position_at(at(start, 3.0)).bar, 1);
        assert_eq!(clock.position_at(at(start, 3.0)).beat_in_bar, 0);
        assert_eq!(clock.position_at(at(start, 2.0)).bar, 0);
    }

    #[test]
    fn changing_the_bar_length_redescribes_the_same_position() {
        // It changes how a position is counted, not where the clock is.
        let start = Instant::now();
        let mut clock = MasterClock::new(60.0, 4, start);
        let moment = at(start, 6.0);

        let beats = clock.beats_at(moment);
        assert_eq!(clock.position_at(moment).bar, 1);

        clock.set_beats_per_bar(3);
        assert_eq!(clock.beats_at(moment), beats, "the position must not move");
        assert_eq!(clock.position_at(moment).bar, 2);
    }

    #[test]
    fn phase_runs_from_zero_up_to_but_not_including_one() {
        let start = Instant::now();
        let clock = MasterClock::new(60.0, 4, start);

        assert!((clock.position_at(at(start, 0.0)).phase - 0.0).abs() < 1e-9);
        assert!((clock.position_at(at(start, 0.25)).phase - 0.25).abs() < 1e-9);
        assert!((clock.position_at(at(start, 0.999)).phase - 0.999).abs() < 1e-6);
        assert!((clock.position_at(at(start, 1.0)).phase - 0.0).abs() < 1e-9);
    }

    #[test]
    fn a_beat_lasts_what_the_tempo_says() {
        let start = Instant::now();
        let clock = MasterClock::new(120.0, 4, start);
        let beat = clock.beat_duration().expect("running");
        assert!((beat.as_secs_f64() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_stopped_clock_is_not_something_this_can_be_asked_for() {
        // The scale has a floor, so a clock that has been told to stop is still
        // advancing. Four tests used to sit beside this one, about where a
        // trigger landed on a grid; the grid went on 2026-09-23 and they went
        // with it, along with `next_boundary` and `time_to_boundary`, which
        // nothing was left to call.
        let start = Instant::now();
        let mut clock = MasterClock::new(120.0, 4, start);
        clock.set_speed_scale(0.0, start);

        assert_eq!(
            clock.speed_scale(),
            MIN_SPEED_SCALE,
            "the scale has a floor"
        );
        assert!(clock.beats_per_second() > 0.0, "so it never actually stops");
    }
}
