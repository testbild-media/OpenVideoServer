//! Playback state machines.
//!
//! Where a source is in its own timeline, given where the master clock is. The
//! modes are Loop, Once→Freeze, Once→Black and Bounce, plus a
//! direction and a retrigger.
//!
//! # Why this takes a clock position rather than a delta
//!
//! The same reason the clock does. A state machine that advanced a position by
//! `delta` each frame would accumulate rounding for the length of the show, and
//! would give a different answer depending on how many times it was asked. This
//! one computes position from the master clock's beat count and the moment the
//! source was triggered, so it is a pure function of the two: asking twice gives
//! the same answer, and asking after six hours gives the right one.
//!
//! # Why beats and not seconds
//!
//! A layer's rate is a ratio to the master clock, so a clip's position
//! is naturally measured in beats since it started. Converting to a timeline
//! position happens once, at the end, using the clip's own duration.

use ovs_types::config::{PlaybackDirection, PlaybackMode};

/// What the renderer should show for a source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Playback {
    /// Show this position in the source timeline, from 0.0 to 1.0.
    At {
        /// How far through the source, from 0.0 up to and including 1.0.
        position: f64,
        /// How many complete passes have finished.
        ///
        /// Useful for diagnostics and for effects that care about repetition.
        completed_passes: u64,
    },
    /// The source has finished and its last frame is held.
    Frozen,
    /// The source has finished and nothing is shown.
    Ended,
}

impl Playback {
    /// Returns the position to show, if there is one.
    ///
    /// `None` when the source has ended, which is not the same as position 0.0.
    #[must_use]
    pub const fn position(&self) -> Option<f64> {
        match self {
            Self::At { position, .. } => Some(*position),
            Self::Frozen => Some(1.0),
            Self::Ended => None,
        }
    }

    /// Returns true when this source still puts something on screen.
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        !matches!(self, Self::Ended)
    }
}

/// A source that has been triggered and is playing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayingSource {
    /// The master beat count at which this source started.
    ///
    /// A retrigger moves this, which is the whole of what a retrigger is.
    pub started_at_beats: f64,
    /// How long one complete pass takes, in beats.
    ///
    /// Derived from the clip's duration and the layer's rate, so a clip runs at
    /// the tempo the show is at rather than at the tempo it was recorded at.
    pub pass_length_beats: f64,
    /// What happens at the end of a pass.
    pub mode: PlaybackMode,
    /// Which way the timeline runs.
    pub direction: PlaybackDirection,
}

impl PlayingSource {
    /// Creates a source that started at `started_at_beats`.
    #[must_use]
    pub const fn new(started_at_beats: f64, pass_length_beats: f64, mode: PlaybackMode) -> Self {
        Self {
            started_at_beats,
            pass_length_beats,
            mode,
            direction: PlaybackDirection::Forward,
        }
    }

    /// Returns a copy running in the given direction.
    #[must_use]
    pub const fn playing(mut self, direction: PlaybackDirection) -> Self {
        self.direction = direction;
        self
    }

    /// Restarts the source from the beginning, as of `now_beats`.
    ///
    /// This is Retrigger. It moves the start, which is the whole of what a
    /// retrigger is: nothing else about the source changes.
    pub const fn retrigger(&mut self, now_beats: f64) {
        self.started_at_beats = now_beats;
    }

    /// Returns what to show at the given master beat count.
    #[must_use]
    pub fn at(&self, now_beats: f64) -> Playback {
        if !(self.pass_length_beats.is_finite() && self.pass_length_beats > 0.0) {
            // A clip with no length has one frame, and that frame is its whole
            // timeline. Better than dividing by zero somewhere deeper.
            return Playback::At {
                position: 0.0,
                completed_passes: 0,
            };
        }
        if !now_beats.is_finite() || !self.started_at_beats.is_finite() {
            return Playback::At {
                position: 0.0,
                completed_passes: 0,
            };
        }

        // Before the trigger. Nothing schedules one in the future any more —
        // the grid went on 2026-09-23 — but the arithmetic still has to answer
        // for one, because a clock re-anchored under a running pass can put
        // `now` behind where the pass started.
        let elapsed = now_beats - self.started_at_beats;
        if elapsed <= 0.0 {
            return Playback::At {
                position: self.oriented(0.0),
                completed_passes: 0,
            };
        }

        let passes = elapsed / self.pass_length_beats;
        let completed = passes.floor();
        let within = passes - completed;
        let completed_passes = completed.max(0.0) as u64;

        match self.mode {
            PlaybackMode::Loop => Playback::At {
                position: self.oriented(within),
                completed_passes,
            },

            PlaybackMode::Bounce => {
                // Odd passes run backwards. The turn happens at the end of a
                // pass, so a bounce is one pass forward and one pass back, not
                // a pass that plays at half speed in each direction.
                let reversed = completed_passes % 2 == 1;
                let raw = if reversed { 1.0 - within } else { within };
                Playback::At {
                    position: self.oriented(raw),
                    completed_passes,
                }
            }

            PlaybackMode::OnceFreeze => {
                if completed_passes >= 1 {
                    Playback::Frozen
                } else {
                    Playback::At {
                        position: self.oriented(within),
                        completed_passes: 0,
                    }
                }
            }

            PlaybackMode::OnceBlack => {
                if completed_passes >= 1 {
                    Playback::Ended
                } else {
                    Playback::At {
                        position: self.oriented(within),
                        completed_passes: 0,
                    }
                }
            }
        }
    }

    /// Applies the direction to a forward position.
    fn oriented(&self, forward: f64) -> f64 {
        match self.direction {
            PlaybackDirection::Forward => forward,
            PlaybackDirection::Reverse => 1.0 - forward,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A four-beat clip, which is one bar in common time.
    fn clip(mode: PlaybackMode) -> PlayingSource {
        PlayingSource::new(0.0, 4.0, mode)
    }

    fn position_at(source: &PlayingSource, beats: f64) -> f64 {
        source.at(beats).position().expect("visible")
    }

    #[test]
    fn a_loop_runs_from_zero_to_one_and_starts_again() {
        let source = clip(PlaybackMode::Loop);

        assert!((position_at(&source, 0.0) - 0.0).abs() < 1e-9);
        assert!((position_at(&source, 1.0) - 0.25).abs() < 1e-9);
        assert!((position_at(&source, 2.0) - 0.5).abs() < 1e-9);
        assert!((position_at(&source, 3.999) - 0.99975).abs() < 1e-9);
        assert!(
            (position_at(&source, 4.0) - 0.0).abs() < 1e-9,
            "the loop point"
        );
        assert!((position_at(&source, 5.0) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn a_loop_does_not_drift_over_a_long_show() {
        // The reason position is computed rather than advanced. A thousand
        // passes in, the loop point must still be the loop point.
        let source = clip(PlaybackMode::Loop);
        for pass in [1_u64, 10, 100, 1_000, 10_000] {
            let at_pass = 4.0 * pass as f64;
            assert!(
                position_at(&source, at_pass).abs() < 1e-9,
                "pass {pass} did not land on the loop point"
            );
        }
    }

    #[test]
    fn asking_twice_gives_the_same_answer() {
        // A state machine that advanced on each call would not.
        let source = clip(PlaybackMode::Loop);
        let first = source.at(7.3);
        for _ in 0..1_000 {
            assert_eq!(source.at(7.3), first);
        }
    }

    #[test]
    fn once_freeze_holds_the_last_frame() {
        let source = clip(PlaybackMode::OnceFreeze);

        assert!((position_at(&source, 2.0) - 0.5).abs() < 1e-9);
        assert_eq!(source.at(4.0), Playback::Frozen);
        assert_eq!(source.at(400.0), Playback::Frozen, "and stays frozen");
        assert_eq!(source.at(4.0).position(), Some(1.0), "on the last frame");
        assert!(source.at(4.0).is_visible());
    }

    #[test]
    fn once_black_stops_showing_anything() {
        let source = clip(PlaybackMode::OnceBlack);

        assert!((position_at(&source, 3.9) - 0.975).abs() < 1e-9);
        assert_eq!(source.at(4.0), Playback::Ended);
        assert_eq!(
            source.at(4.0).position(),
            None,
            "ended is not position zero"
        );
        assert!(!source.at(4.0).is_visible());
    }

    #[test]
    fn the_difference_between_freeze_and_black_is_visible() {
        // The two modes differ only after the end, and that is the whole point
        // of having both.
        let freeze = clip(PlaybackMode::OnceFreeze);
        let black = clip(PlaybackMode::OnceBlack);

        assert_eq!(freeze.at(2.0), black.at(2.0), "identical while playing");
        assert_ne!(freeze.at(5.0), black.at(5.0), "and different after it");
        assert!(freeze.at(5.0).is_visible());
        assert!(!black.at(5.0).is_visible());
    }

    #[test]
    fn bounce_runs_forward_then_backward() {
        let source = clip(PlaybackMode::Bounce);

        // First pass forward.
        assert!((position_at(&source, 0.0) - 0.0).abs() < 1e-9);
        assert!((position_at(&source, 2.0) - 0.5).abs() < 1e-9);

        // Second pass backward.
        assert!((position_at(&source, 4.0) - 1.0).abs() < 1e-9, "the turn");
        assert!((position_at(&source, 6.0) - 0.5).abs() < 1e-9);
        assert!((position_at(&source, 7.999) - 0.00025).abs() < 1e-9);

        // Third pass forward again.
        assert!((position_at(&source, 8.0) - 0.0).abs() < 1e-9);
        assert!((position_at(&source, 10.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn bounce_turns_at_the_ends_and_nowhere_else() {
        // A bounce that reversed mid-pass would look like a stutter. Position
        // must move monotonically within each pass.
        let source = clip(PlaybackMode::Bounce);

        for pass in 0..4_u64 {
            let base = 4.0 * pass as f64;
            let mut previous = position_at(&source, base + 0.001);
            let mut rising: Option<bool> = None;

            for step in 2..100 {
                let now = position_at(&source, base + f64::from(step) * 0.04);
                let direction = now > previous;
                if let Some(expected) = rising {
                    assert_eq!(
                        direction, expected,
                        "pass {pass} changed direction mid-pass at step {step}"
                    );
                } else {
                    rising = Some(direction);
                }
                previous = now;
            }

            assert_eq!(rising, Some(pass % 2 == 0), "pass {pass} ran the wrong way");
        }
    }

    #[test]
    fn bounce_covers_a_whole_pass_in_each_direction() {
        // Not a pass that plays at half speed in each direction: one full pass
        // forward, then one full pass back.
        let source = clip(PlaybackMode::Bounce);
        assert!((position_at(&source, 0.0) - 0.0).abs() < 1e-9);
        assert!((position_at(&source, 4.0) - 1.0).abs() < 1e-9);
        assert!((position_at(&source, 8.0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn reverse_plays_the_timeline_backwards() {
        let source = clip(PlaybackMode::Loop).playing(PlaybackDirection::Reverse);

        assert!((position_at(&source, 0.0) - 1.0).abs() < 1e-9);
        assert!((position_at(&source, 1.0) - 0.75).abs() < 1e-9);
        assert!((position_at(&source, 2.0) - 0.5).abs() < 1e-9);
        assert!((position_at(&source, 4.0) - 1.0).abs() < 1e-9, "and loops");
    }

    #[test]
    fn reverse_and_bounce_compose() {
        // Bounce decides which way each pass runs; reverse mirrors the whole
        // timeline. Both apply, and the result is still continuous.
        let source = clip(PlaybackMode::Bounce).playing(PlaybackDirection::Reverse);

        assert!((position_at(&source, 0.0) - 1.0).abs() < 1e-9);
        assert!((position_at(&source, 2.0) - 0.5).abs() < 1e-9);
        assert!((position_at(&source, 4.0) - 0.0).abs() < 1e-9);
        assert!((position_at(&source, 6.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn retriggering_restarts_from_the_beginning() {
        let mut source = clip(PlaybackMode::Loop);
        assert!((position_at(&source, 3.0) - 0.75).abs() < 1e-9);

        source.retrigger(3.0);
        assert!(position_at(&source, 3.0).abs() < 1e-9, "back to the start");
        assert!(
            (position_at(&source, 4.0) - 0.25).abs() < 1e-9,
            "and running again"
        );
    }

    #[test]
    fn retriggering_revives_a_source_that_had_ended() {
        // Pressing a slot again after Once→Black must play it again, not do
        // nothing because the source is past its end.
        let mut source = clip(PlaybackMode::OnceBlack);
        assert_eq!(source.at(10.0), Playback::Ended);

        source.retrigger(10.0);
        assert!(source.at(10.0).is_visible());
        assert!((position_at(&source, 12.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_trigger_that_has_not_arrived_yet_holds_the_first_frame() {
        // A pass whose start is ahead of the moment being asked about, which is
        // what a clock re-anchored under a running source looks like. It shows
        // its first frame rather than playing from the middle.
        let source = PlayingSource::new(8.0, 4.0, PlaybackMode::Loop);

        assert!(position_at(&source, 0.0).abs() < 1e-9);
        assert!(position_at(&source, 7.999).abs() < 1e-9);
        assert!(
            position_at(&source, 8.0).abs() < 1e-9,
            "and starts on the boundary"
        );
        assert!((position_at(&source, 10.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn completed_passes_counts_whole_passes() {
        let source = clip(PlaybackMode::Loop);

        for (beats, expected) in [
            (0.0, 0),
            (3.9, 0),
            (4.0, 1),
            (7.9, 1),
            (8.0, 2),
            (400.0, 100),
        ] {
            let Playback::At {
                completed_passes, ..
            } = source.at(beats)
            else {
                panic!("a loop is always playing");
            };
            assert_eq!(completed_passes, expected, "at beat {beats}");
        }
    }

    #[test]
    fn a_clip_with_no_length_shows_its_one_frame() {
        // A still image, or a clip whose duration was never determined. Better
        // than dividing by zero somewhere deeper.
        for length in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let source = PlayingSource::new(0.0, length, PlaybackMode::Loop);
            assert_eq!(
                source.at(100.0),
                Playback::At {
                    position: 0.0,
                    completed_passes: 0
                }
            );
        }
    }

    #[test]
    fn a_corrupt_clock_position_shows_the_first_frame_rather_than_panicking() {
        let source = clip(PlaybackMode::Loop);
        assert_eq!(
            source.at(f64::NAN),
            Playback::At {
                position: 0.0,
                completed_passes: 0
            }
        );
    }

    #[test]
    fn a_loop_wraps_at_the_boundary_and_a_bounce_turns() {
        // The difference between the two modes, stated precisely. A loop is
        // discontinuous on purpose: that jump from the last frame back to the
        // first is what a loop is. A bounce is continuous: it reaches the end
        // and comes back, so a jump there would be a visible stutter.
        let just_before = 4.0 - 1e-6;
        let just_after = 4.0 + 1e-6;

        let looping = clip(PlaybackMode::Loop);
        assert!(
            position_at(&looping, just_before) > 0.999,
            "reaches the end"
        );
        assert!(
            position_at(&looping, just_after) < 0.001,
            "and starts again"
        );

        let bouncing = clip(PlaybackMode::Bounce);
        let before = position_at(&bouncing, just_before);
        let after = position_at(&bouncing, just_after);
        assert!(before > 0.999 && after > 0.999, "both sides are at the end");
        assert!(
            (before - after).abs() < 1e-4,
            "a bounce must turn, not jump: {before} to {after}"
        );
    }

    #[test]
    fn a_bounce_is_continuous_at_every_turn_not_just_the_first() {
        let source = clip(PlaybackMode::Bounce);
        for pass in 1..8_u64 {
            let boundary = 4.0 * pass as f64;
            let before = position_at(&source, boundary - 1e-6);
            let after = position_at(&source, boundary + 1e-6);
            assert!(
                (before - after).abs() < 1e-4,
                "turn {pass} jumps from {before} to {after}"
            );
        }
    }
}
