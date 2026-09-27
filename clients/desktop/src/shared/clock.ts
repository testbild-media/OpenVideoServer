// Where the master clock is between the moments the server speaks.
//
// The beat strip the reference drawing puts under the tempo is
// the one moving thing in this client, and the only one that draws a number
// nobody sent this instant. That is not a hole in the rule the rest of the
// screen keeps — it is what a beat strip *is*, and `ClockTelemetry` says so: the
// beat moves sixty times a second and nobody edits it, so it is telemetry rather
// than state, and a client animates between samples from the rate the server
// reported and corrects whenever the next sample lands.
//
// So three properties, and all three are about honesty rather than smoothness:
//
//   - **No sample, no strip.** Telemetry carries no clock while no show is
//     open, because there is no show for a beat to belong to. That reads as
//     unknown, exactly as an undelivered fader does, and never as a beat at
//     zero — which is a real position a running clock passes through.
//   - **The prediction is the server's arithmetic, carried on.** It runs at
//     `beats_per_second`, which arrives already multiplied (tempo times master
//     speed scale), because the two numbers it comes from travel in different
//     messages and a screen that multiplied the wrong pair would drift.
//   - **A prediction nobody has corrected stops being one.** After
//     [`CLOCK_STALE_AFTER_MS`] with nothing from the server, the strip says it
//     does not know rather than going on beating on its own.
//
// Times in here are milliseconds from a monotonic clock — `performance.now()` in
// the window — passed in rather than read, so the behaviour that matters during
// a show is tested rather than hoped for.

import type { ClockTelemetry } from '@ovs/schema';

import { MIN_BEATS_PER_BAR } from './limits.js';
import type { Reading } from './live.js';

/**
 * How long a sample is worth predicting from.
 *
 * Telemetry is a periodic sample, about one a second, and a dropped one needs no
 * resynchronisation — so a gap of one or two is ordinary and must not blank the
 * strip. Several in a row is a server that has stopped saying where its clock
 * is, and a strip that went on beating from a sample that old would be this
 * client's own metronome wearing the server's colours.
 */
export const CLOCK_STALE_AFTER_MS = 5_000;

/**
 * The most marks the strip draws, one per beat.
 *
 * A drawing limit rather than a model one: `beats_per_bar` is a `u32` and the
 * editor accepts any of it (`MAX_BEATS_PER_BAR`), but a strip 78 pixels wide
 * cannot show a hundred marks and must not try to build four billion of them.
 * A longer bar is drawn as one mark and says which beat it is in words.
 */
export const MAX_BEAT_MARKS = 32;

/**
 * One sample of the master clock, and when this client received it.
 *
 * `at` is this client's own monotonic reading at the moment the sample arrived,
 * not anything the server said about time. The two machines' clocks are not the
 * same clock and nothing here pretends they are: what is measured is how long
 * ago *this* client heard.
 */
export interface ClockSample {
  /** Bars since the last reset. */
  bar: number;
  /** Which beat of the bar, counting from zero. */
  beatInBar: number;
  /** How far through that beat, in `0..1`. */
  phase: number;
  /** Beats in a bar, which is how many marks the strip has. */
  beatsPerBar: number;
  /** Beats a second, tempo and master speed already multiplied together. */
  beatsPerSecond: number;
  /** When this client received it, on a monotonic clock, in milliseconds. */
  at: number;
}

/** Where the strip draws the beat: a sample carried forward to now. */
export interface BeatPosition {
  /** Bars since the last reset, counting on from the sample's. */
  bar: number;
  /** Which beat of the bar is lit, counting from zero, always within the bar. */
  beatInBar: number;
  /** How far through that beat, in `0..1`. */
  phase: number;
  /** Beats in a bar. */
  beatsPerBar: number;
  /** The rate this position was predicted at. */
  beatsPerSecond: number;
}

/**
 * What the strip can say about the clock.
 *
 * The same `Reading` the live controls use, and deliberately so: a clock nobody
 * has reported is the same kind of nothing as a dimmer nobody has reported.
 * Only two of its four states can occur here — nothing on this screen asks the
 * server to move the beat, so there is nothing to be pending or refused about.
 */
export type BeatReading = Reading<BeatPosition>;

/**
 * A sample of what the server reported, or null when there is nothing to draw.
 *
 * Null covers two cases that look the same on screen and are worth keeping
 * apart in the comment: telemetry with no `clock` at all, which is a server with
 * no show open; and a sample whose numbers are not a beat of a bar — a bar of
 * no beats, a rate that is not a number, a beat outside its own bar. Neither can
 * be drawn, and inventing a position from either would be exactly the thing the
 * rest of this client refuses to do.
 */
export function sampleClock(clock: ClockTelemetry | null | undefined, at: number): ClockSample | null {
  if (clock === null || clock === undefined) {
    return null;
  }

  const { bar, beat_in_bar: beatInBar, phase, beats_per_bar: beatsPerBar, beats_per_second: rate } = clock;
  const whole = (value: number): boolean => Number.isInteger(value) && value >= 0;

  if (!whole(beatsPerBar) || beatsPerBar < MIN_BEATS_PER_BAR) {
    return null;
  }
  if (!whole(bar) || !whole(beatInBar) || beatInBar >= beatsPerBar) {
    return null;
  }
  if (!Number.isFinite(phase) || phase < 0 || phase > 1) {
    return null;
  }
  if (!Number.isFinite(rate) || rate < 0) {
    return null;
  }
  if (!Number.isFinite(at)) {
    return null;
  }

  return { bar, beatInBar, phase, beatsPerBar, beatsPerSecond: rate, at };
}

/**
 * Where the beat is now, given the last sample.
 *
 * The whole of the prediction: the sample's position plus the beats that have
 * passed since it arrived, at the rate it reported. A fresh sample replaces the
 * one it was predicted from, which is the correction — nothing here accumulates,
 * so an error in one prediction cannot become drift in the next.
 *
 * It never runs past the end of the bar into a beat the bar does not have: a
 * fifth beat of four is not a position the show can be in, and a strip that lit
 * a mark that is not there would be worse than one that stood still.
 */
export function beatAt(sample: ClockSample | null, now: number): BeatReading {
  if (sample === null) {
    return { state: 'unknown' };
  }
  if (!Number.isFinite(now) || now - sample.at >= CLOCK_STALE_AFTER_MS) {
    // Nobody has corrected this for long enough that it is a guess.
    return { state: 'unknown' };
  }

  // Never backwards. A monotonic clock does not go back, and if one ever did,
  // rewinding the beat would say the show had.
  const elapsed = Math.max(0, now - sample.at) / 1000;
  const beats = sample.beatInBar + sample.phase + elapsed * sample.beatsPerSecond;
  const { bars, beatInBar, phase } = withinBar(beats, sample.beatsPerBar);

  return {
    state: 'live',
    value: {
      bar: sample.bar + bars,
      beatInBar,
      phase,
      beatsPerBar: sample.beatsPerBar,
      beatsPerSecond: sample.beatsPerSecond,
    },
  };
}

/** Beats since the start of a bar, split into whole bars, a beat and its phase. */
function withinBar(beats: number, beatsPerBar: number): { bars: number; beatInBar: number; phase: number } {
  let bars = Math.floor(beats / beatsPerBar);
  let rest = beats - bars * beatsPerBar;

  // Floating point, not musical: a value a hair under the length of the bar can
  // come back out of that subtraction as the length of the bar itself, which
  // would name a beat the bar does not have.
  if (rest >= beatsPerBar) {
    bars += 1;
    rest = 0;
  }
  if (rest < 0) {
    rest = 0;
  }

  const beatInBar = Math.min(Math.floor(rest), beatsPerBar - 1);
  return { bars, beatInBar, phase: rest - beatInBar };
}

/** How the strip draws one reading. */
export interface StripView {
  /** How many marks to draw. None when there is nothing to say. */
  marks: number;
  /** Which mark is lit, counting from zero, or -1 when none is. */
  lit: number;
  /** How far through the lit mark, in `0..1`. */
  phase: number;
  /**
   * What to write beside the marks.
   *
   * Empty when the marks say it themselves. An em dash when the server has not
   * reported a clock, which is how the rest of this screen writes "nothing has
   * been said". The beat in figures when the bar is too long to draw.
   */
  label: string;
}

/**
 * The strip for one reading.
 *
 * A mark per beat while a bar has few enough of them to be read at a glance.
 * Beyond that the marks stop being marks — a hundred of them in the width of the
 * tempo field is a smear — so the strip becomes one bar running through the beat
 * with the count written beside it, which is still the truth and is still legible.
 */
export function stripView(reading: BeatReading): StripView {
  if (reading.state !== 'live') {
    return { marks: 0, lit: -1, phase: 0, label: '—' };
  }

  const { beatInBar, phase, beatsPerBar } = reading.value;
  if (beatsPerBar <= MAX_BEAT_MARKS) {
    return { marks: beatsPerBar, lit: beatInBar, phase, label: '' };
  }
  return { marks: 1, lit: 0, phase, label: `${beatInBar + 1}/${beatsPerBar}` };
}

/**
 * What the strip is saying, in words, for its tooltip and its label.
 *
 * It says that the position is predicted, because it is: between samples this is
 * this client's arithmetic on the server's numbers, and an operator reading a
 * beat off a screen is entitled to know which of the two they are looking at.
 */
export function beatNote(reading: BeatReading): string {
  if (reading.state !== 'live') {
    // Why it is silent is not something this client can tell from here — no
    // show open, no connection, or a server that has stopped sampling — so
    // it says what it knows and states the ordinary reason rather than picking one.
    return 'The server has not reported the master clock. There is none while no show is open.';
  }
  const { beatInBar, beatsPerBar } = reading.value;
  return `Beat ${beatInBar + 1} of ${beatsPerBar}, predicted from the last sample and corrected by the next.`;
}
