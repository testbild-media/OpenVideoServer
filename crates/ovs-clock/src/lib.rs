//! The global master clock.
//!
//! There is exactly one clock in OpenVideoServer. Nothing
//! else owns a tempo: a layer owns nothing about time at all, and a slot says
//! only how long its own pass is — in beats of this clock, or in its own
//! seconds — and how fast to run through one.
//!
//! - [`MasterClock`] — the clock itself: tempo, phase, bars, speed scale.
//! - [`TapTempo`] — turning an operator tapping a button into a tempo.
//!
//! There was a third, `quantize`, which decided where a trigger landed on a
//! grid. The owner removed the trigger grid on 2026-09-23 — "Trigger Grid weg.
//! Beats ist das was man braucht." — and a pad now takes effect when it is
//! pressed, so there is nothing left to decide.
//!
//! Pure arithmetic. No I/O, no GPU, no clock of its own: every function takes
//! the moment it is being asked about. That is what makes the behaviour at an
//! exact beat boundary, across a six-hour show, or during a tempo change
//! something a test can pin down rather than something that has to be watched
//! for on stage.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod master;
pub mod playback;
pub mod tap;

pub use master::{MasterClock, Position};
pub use playback::{Playback, PlayingSource};
pub use tap::TapTempo;

#[cfg(test)]
mod invariants {
    //! The rules the specification calls non-optional.

    use std::time::{Duration, Instant};

    use ovs_types::config::fixtures::MASTER_SCALE_STEPS;

    use super::*;

    #[test]
    fn there_is_one_clock_and_only_the_master_scales_it() {
        // The tempo a show runs at is the master's, times the master's own
        // scale, and there is nothing else in the multiplication. **A layer
        // has no rate of its own** (owner, 2026-09-23): a slot either runs
        // free at 1x, ignoring the tempo entirely, or is locked to a number
        // of beats — and beats are counted against this one clock.
        //
        // This used to assert the opposite, because a layer used to carry a
        // speed ratio. It is kept rather than deleted because the invariant
        // it guards is the same one it always guarded: one clock, and every
        // rate in the show derived from it rather than kept beside it.
        let start = Instant::now();
        let clock = MasterClock::new(120.0, 4, start);

        for scale in MASTER_SCALE_STEPS {
            let effective = clock.bpm() * scale;
            assert!(
                (effective - 120.0 * scale).abs() < 1e-9,
                "{scale}x should scale the master rate"
            );
        }
        assert!(
            MASTER_SCALE_STEPS.contains(&1.0),
            "a show has to be able to run at its own tempo"
        );
    }

    #[test]
    fn a_show_that_runs_all_night_stays_on_the_beat() {
        // Eight hours at 128 BPM, checked against the arithmetic rather than
        // against an accumulator that would have drifted by now.
        let start = Instant::now();
        let clock = MasterClock::new(128.0, 4, start);
        let eight_hours = Duration::from_secs(8 * 60 * 60);

        let beats = clock.beats_at(start + eight_hours);
        let expected = 128.0 * eight_hours.as_secs_f64() / 60.0;
        assert!(
            (beats - expected).abs() < 1e-6,
            "expected {expected}, got {beats}"
        );

        // And the reading an operator watches all night still says which bar and
        // which beat, which is the half of this that reaches a screen.
        let position = clock.position_at(start + eight_hours);
        assert_eq!(position.bar, (expected / 4.0).floor() as u64);
        assert!(position.beat_in_bar < 4);
    }
}
