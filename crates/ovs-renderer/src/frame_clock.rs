//! Presentation timing and dropped-frame accounting.
//!
//! The scheduler owns presentation timing, not the decoder, and
//! dropped frames are derived from what actually happened rather than from a
//! counter somebody remembered to increment (house rule §16).
//!
//! The definition used here: an output running at 60 Hz has a vblank interval
//! every 16.67 ms. A **dropped frame** is an interval that passed with nothing
//! presented into it. That is measurable, it matches what an operator sees, and
//! it cannot be fudged by the renderer being optimistic about its own work.
//!
//! Timestamps are passed in rather than read from the clock inside, so the
//! behaviour at an exact interval boundary is tested rather than hoped for.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ovs_types::geometry::FrameRate;
use ovs_types::telemetry::DroppedFrames;

/// How far back [`FrameClock::actual_fps`] averages.
const FPS_WINDOW: Duration = Duration::from_secs(1);

/// Tracks presentation timing for one output.
#[derive(Debug)]
pub struct FrameClock {
    target: FrameRate,
    frame_duration: Duration,
    epoch: Instant,
    presented: u64,
    last_interval: Option<u64>,
    dropped: DroppedFrames,
    recent: VecDeque<Instant>,
    last_render_time: Duration,
}

impl FrameClock {
    /// Creates a clock for an output running at `target`, starting at `epoch`.
    ///
    /// Returns `None` for a degenerate rate, because a clock with no interval
    /// cannot say anything about drops and a zero here would otherwise become a
    /// division by zero much further away.
    #[must_use]
    pub fn new(target: FrameRate, epoch: Instant) -> Option<Self> {
        let nanos = target.frame_duration_nanos()?;
        Some(Self {
            target,
            frame_duration: Duration::from_nanos(nanos),
            epoch,
            presented: 0,
            last_interval: None,
            dropped: DroppedFrames::default(),
            recent: VecDeque::new(),
            last_render_time: Duration::ZERO,
        })
    }

    /// Starts the accounting again, from `at`, with nothing counted yet.
    ///
    /// For the moment a display actually begins scanning out. Setting a mode on
    /// a panel takes a second or so of handshaking, and the intervals that pass
    /// while it happens are not frames an audience missed — they are the output
    /// coming up. Counting them would put ninety dropped frames on the Service
    /// Console at every boot, which is the fastest way to make an operator stop
    /// believing the number that matters.
    ///
    /// Everything measured is cleared, deliberately, including the lifetime
    /// counter that an operator cannot reset: there is nothing to preserve
    /// because nothing has been shown yet.
    pub fn begin(&mut self, at: Instant) {
        self.epoch = at;
        self.presented = 0;
        self.last_interval = None;
        self.dropped = DroppedFrames::default();
        self.recent.clear();
    }

    /// Returns the rate this output is meant to run at.
    #[must_use]
    pub const fn target(&self) -> FrameRate {
        self.target
    }

    /// Returns the length of one vblank interval.
    #[must_use]
    pub const fn frame_duration(&self) -> Duration {
        self.frame_duration
    }

    /// Returns which vblank interval a moment falls in, counting from the epoch.
    #[must_use]
    pub fn interval_at(&self, at: Instant) -> u64 {
        let elapsed = at.saturating_duration_since(self.epoch);
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        nanos
            / u64::try_from(self.frame_duration.as_nanos())
                .unwrap_or(1)
                .max(1)
    }

    /// Records that a frame was presented at `at`, having taken `render_time`.
    ///
    /// Any vblank interval between the previous present and this one that
    /// received no frame is counted as dropped.
    pub fn record_present(&mut self, at: Instant, render_time: Duration) {
        let interval = self.interval_at(at);

        if let Some(previous) = self.last_interval {
            // `saturating_sub` rather than a subtraction: two presents inside
            // one interval are not negative drops, they are zero drops.
            let missed = interval.saturating_sub(previous).saturating_sub(1);
            if missed > 0 {
                self.dropped.record(missed);
            }
        }

        self.last_interval = Some(interval);
        self.presented = self.presented.saturating_add(1);
        self.last_render_time = render_time;

        self.recent.push_back(at);
        while self
            .recent
            .front()
            .is_some_and(|oldest| at.saturating_duration_since(*oldest) > FPS_WINDOW)
        {
            self.recent.pop_front();
        }
    }

    /// Returns the moment the next frame is due.
    #[must_use]
    pub fn next_deadline(&self) -> Instant {
        let next = self.last_interval.map_or(0, |interval| interval + 1);
        self.epoch
            + self.frame_duration * u32::try_from(next.min(u64::from(u32::MAX))).unwrap_or(u32::MAX)
    }

    /// Returns frames per second measured over the last second.
    ///
    /// Zero until at least two frames have been presented, because one
    /// timestamp measures no interval. Reporting the target rate instead would
    /// be inventing a number.
    #[must_use]
    pub fn actual_fps(&self) -> f32 {
        if self.recent.len() < 2 {
            return 0.0;
        }
        let (Some(first), Some(last)) = (self.recent.front(), self.recent.back()) else {
            return 0.0;
        };
        let span = last.saturating_duration_since(*first).as_secs_f64();
        if span <= 0.0 {
            return 0.0;
        }
        ((self.recent.len() - 1) as f64 / span) as f32
    }

    /// Returns the dropped-frame counters.
    #[must_use]
    pub const fn dropped_frames(&self) -> DroppedFrames {
        self.dropped
    }

    /// Returns how many frames have been presented since the epoch.
    #[must_use]
    pub const fn presented_frames(&self) -> u64 {
        self.presented
    }

    /// Returns how long the last frame took to render, in microseconds.
    #[must_use]
    pub fn last_render_time_us(&self) -> u32 {
        u32::try_from(self.last_render_time.as_micros()).unwrap_or(u32::MAX)
    }

    /// Resets only the operator-visible dropped-frame counter.
    ///
    /// The lifetime counter is deliberately untouched, so a long-run problem
    /// stays visible however often the button is pressed.
    pub fn reset_operator_counter(&mut self) {
        self.dropped.reset_operator_counter();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> (Instant, FrameClock) {
        let epoch = Instant::now();
        (
            epoch,
            FrameClock::new(FrameRate::FPS_60, epoch).expect("valid rate"),
        )
    }

    /// The exact centre of interval `n`, so a test is never at a boundary by
    /// accident.
    fn mid_interval(epoch: Instant, clock: &FrameClock, index: u64) -> Instant {
        epoch
            + clock.frame_duration() * u32::try_from(index).expect("small")
            + clock.frame_duration() / 2
    }

    #[test]
    fn a_degenerate_rate_has_no_clock() {
        assert!(FrameClock::new(FrameRate::new(0, 1), Instant::now()).is_none());
        assert!(FrameClock::new(FrameRate::new(60, 0), Instant::now()).is_none());
    }

    #[test]
    fn presenting_every_interval_drops_nothing() {
        let (epoch, mut clock) = clock();
        for index in 0..600 {
            clock.record_present(
                mid_interval(epoch, &clock, index),
                Duration::from_micros(4_000),
            );
        }
        assert_eq!(clock.dropped_frames(), DroppedFrames::default());
        assert_eq!(clock.presented_frames(), 600);
    }

    #[test]
    fn a_missed_interval_is_one_dropped_frame() {
        let (epoch, mut clock) = clock();
        clock.record_present(mid_interval(epoch, &clock, 0), Duration::ZERO);
        clock.record_present(mid_interval(epoch, &clock, 2), Duration::ZERO);

        assert_eq!(clock.dropped_frames().since_reset, 1);
        assert_eq!(clock.dropped_frames().lifetime, 1);
    }

    #[test]
    fn a_long_stall_counts_every_missed_interval() {
        let (epoch, mut clock) = clock();
        clock.record_present(mid_interval(epoch, &clock, 0), Duration::ZERO);
        clock.record_present(mid_interval(epoch, &clock, 61), Duration::ZERO);

        // One second at 60 Hz with nothing presented: 60 intervals missed.
        assert_eq!(clock.dropped_frames().since_reset, 60);
    }

    #[test]
    fn two_presents_inside_one_interval_are_not_negative_drops() {
        let (epoch, mut clock) = clock();
        let interval = clock.frame_duration();
        clock.record_present(epoch + interval / 4, Duration::ZERO);
        clock.record_present(epoch + interval / 2, Duration::ZERO);

        assert_eq!(clock.dropped_frames(), DroppedFrames::default());
        assert_eq!(clock.presented_frames(), 2);
    }

    #[test]
    fn a_present_exactly_on_a_boundary_belongs_to_the_interval_it_starts() {
        let (epoch, clock) = clock();
        let interval = clock.frame_duration();
        assert_eq!(clock.interval_at(epoch), 0);
        assert_eq!(
            clock.interval_at(epoch + interval - Duration::from_nanos(1)),
            0
        );
        assert_eq!(clock.interval_at(epoch + interval), 1);
    }

    #[test]
    fn the_first_present_can_never_be_a_drop() {
        // There is no previous interval to have missed, however late it is.
        let (epoch, mut clock) = clock();
        clock.record_present(epoch + Duration::from_secs(10), Duration::ZERO);
        assert_eq!(clock.dropped_frames(), DroppedFrames::default());
    }

    #[test]
    fn resetting_the_operator_counter_leaves_the_lifetime_counter_alone() {
        let (epoch, mut clock) = clock();
        clock.record_present(mid_interval(epoch, &clock, 0), Duration::ZERO);
        clock.record_present(mid_interval(epoch, &clock, 5), Duration::ZERO);
        assert_eq!(clock.dropped_frames().since_reset, 4);

        clock.reset_operator_counter();
        assert_eq!(clock.dropped_frames().since_reset, 0);
        assert_eq!(
            clock.dropped_frames().lifetime,
            4,
            "the real problem stays visible"
        );
    }

    #[test]
    fn measured_fps_matches_a_steady_stream() {
        let (epoch, mut clock) = clock();
        for index in 0..61 {
            clock.record_present(mid_interval(epoch, &clock, index), Duration::ZERO);
        }
        let fps = clock.actual_fps();
        assert!(
            (fps - 60.0).abs() < 1.0,
            "expected about 60 fps, measured {fps}"
        );
    }

    #[test]
    fn measured_fps_reports_the_real_rate_when_frames_are_being_missed() {
        // Presenting into every second interval is 30 fps on a 60 Hz output, and
        // the number must say so rather than flattering the renderer.
        let (epoch, mut clock) = clock();
        for index in 0..31 {
            clock.record_present(mid_interval(epoch, &clock, index * 2), Duration::ZERO);
        }
        let fps = clock.actual_fps();
        assert!(
            (fps - 30.0).abs() < 1.0,
            "expected about 30 fps, measured {fps}"
        );
        assert_eq!(clock.dropped_frames().since_reset, 30);
    }

    #[test]
    fn fps_is_zero_rather_than_invented_before_two_frames_exist() {
        let (epoch, mut clock) = clock();
        assert_eq!(clock.actual_fps(), 0.0);
        clock.record_present(epoch, Duration::ZERO);
        assert_eq!(
            clock.actual_fps(),
            0.0,
            "one timestamp measures no interval"
        );
    }

    #[test]
    fn the_next_deadline_follows_the_last_presented_interval() {
        let (epoch, mut clock) = clock();
        assert_eq!(clock.next_deadline(), epoch);

        clock.record_present(mid_interval(epoch, &clock, 0), Duration::ZERO);
        assert_eq!(clock.next_deadline(), epoch + clock.frame_duration());

        clock.record_present(mid_interval(epoch, &clock, 9), Duration::ZERO);
        assert_eq!(clock.next_deadline(), epoch + clock.frame_duration() * 10);
    }

    #[test]
    fn render_time_is_reported_in_microseconds() {
        let (epoch, mut clock) = clock();
        clock.record_present(epoch, Duration::from_micros(4_321));
        assert_eq!(clock.last_render_time_us(), 4_321);
    }

    #[test]
    fn a_fractional_rate_keeps_its_interval() {
        let epoch = Instant::now();
        let clock = FrameClock::new(FrameRate::FPS_29_97, epoch).expect("valid rate");
        assert_eq!(clock.frame_duration(), Duration::from_nanos(33_366_667));
    }
}
