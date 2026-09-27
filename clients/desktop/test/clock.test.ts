// The beat strip: a prediction the server keeps honest.
//
// The pure half in `shared/clock.ts`, driven the way the screen drives it — a
// sample arrives, frames go by, another sample arrives. The properties held here
// are the ones nobody can check by watching a strip move: that it says nothing
// when the server has said nothing, that it runs at the rate the server
// reported, that a fresh sample replaces the prediction rather than nudging it,
// and that it never lights a beat the bar does not have.

import { describe, expect, it } from 'vitest';

import type { ClockTelemetry } from '@ovs/schema';
import {
  CLOCK_STALE_AFTER_MS,
  MAX_BEAT_MARKS,
  beatAt,
  beatNote,
  sampleClock,
  stripView,
  type BeatPosition,
} from '../src/shared/clock.js';

/** What the server sends: 128 BPM in four, which is what the drawing shows. */
function clock(overrides: Partial<ClockTelemetry> = {}): ClockTelemetry {
  return {
    bar: 7,
    beat_in_bar: 0,
    phase: 0,
    beats_per_bar: 4,
    // 128 BPM is 2.1333… beats a second. Two is used through most of this file
    // because a rate with an exact decimal makes the arithmetic legible.
    beats_per_second: 2,
    ...overrides,
  };
}

/** Where the strip says the beat is, or nothing at all. */
function at(sample: ReturnType<typeof sampleClock>, now: number): BeatPosition | null {
  const reading = beatAt(sample, now);
  return reading.state === 'live' ? reading.value : null;
}

describe('the beat strip', () => {
  it('reads as unavailable when the server reports no clock', () => {
    // Telemetry carries no clock while no show is open, because there is no
    // show for a beat to belong to.
    expect(sampleClock(null, 1_000)).toBeNull();
    expect(sampleClock(undefined, 1_000)).toBeNull();
    expect(beatAt(null, 1_000)).toEqual({ state: 'unknown' });

    // And the strip draws that as nothing: no marks, no lit beat, an em dash.
    expect(stripView(beatAt(null, 1_000))).toEqual({ marks: 0, lit: -1, phase: 0, label: '—' });
    expect(beatNote(beatAt(null, 1_000))).toContain('has not reported');
  });

  it('draws a beat at zero as a beat rather than as nothing', () => {
    // The difference the whole thing turns on: a clock sitting on the downbeat
    // of bar zero is a position a running show passes through every bar, and it
    // is not the same answer as a server that has not reported a clock.
    const sample = sampleClock(clock({ bar: 0, beat_in_bar: 0, phase: 0 }), 1_000);
    const strip = stripView(beatAt(sample, 1_000));
    expect(strip).toEqual({ marks: 4, lit: 0, phase: 0, label: '' });
  });

  it('predicts a position half a second on as half a second further at the reported rate', () => {
    // Two beats a second: half a second is one whole beat.
    const sample = sampleClock(clock(), 1_000);
    const later = at(sample, 1_500);
    expect(later?.beatInBar).toBe(1);
    expect(later?.phase).toBeCloseTo(0, 10);

    // And a quarter of a second is half a beat, which is the phase running
    // through the mark rather than the mark moving on.
    const quarter = at(sample, 1_250);
    expect(quarter?.beatInBar).toBe(0);
    expect(quarter?.phase).toBeCloseTo(0.5, 10);

    // The rate is the server's own, tempo and master speed already multiplied:
    // at half the beats a second, half a second is half a beat.
    const slower = sampleClock(clock({ beats_per_second: 1 }), 1_000);
    expect(at(slower, 1_500)?.phase).toBeCloseTo(0.5, 10);
  });

  it('carries the phase on from where the sample was, not from the start of the beat', () => {
    const sample = sampleClock(clock({ beat_in_bar: 2, phase: 0.75 }), 1_000);
    const later = at(sample, 1_250);
    expect(later?.beatInBar).toBe(3);
    expect(later?.phase).toBeCloseTo(0.25, 10);
  });

  it('corrects a prediction that had drifted when a fresh sample arrives', () => {
    // A strip that has been predicting for nearly a second from a clock that was
    // reset in the meantime: it says beat 1, and the show is on beat 3.
    const stale = sampleClock(clock({ beat_in_bar: 0, phase: 0 }), 1_000);
    expect(at(stale, 1_900)?.beatInBar).toBe(1);

    const fresh = sampleClock(clock({ beat_in_bar: 3, phase: 0.5 }), 1_900);
    const corrected = at(fresh, 1_900);
    expect(corrected?.beatInBar).toBe(3);
    expect(corrected?.phase).toBeCloseTo(0.5, 10);

    // The correction replaces the prediction rather than being averaged into
    // it, so nothing accumulates: predicting on from the fresh sample is the
    // fresh sample plus the time since it, and owes the old one nothing.
    const after = at(fresh, 2_150);
    expect(after?.beatInBar).toBe(0);
    expect(after?.phase).toBeCloseTo(0, 10);
    expect(after?.bar).toBe(8);
  });

  it('never predicts past the bar into a beat that does not exist', () => {
    const sample = sampleClock(clock({ bar: 7, beat_in_bar: 3, phase: 0.9 }), 0);

    // Every frame of a second and a half at four beats a second, which crosses
    // three bar lines.
    for (let now = 0; now < 1_500; now += 8) {
      const position = at(sampleClock(clock({ beats_per_second: 4 }), 0), now);
      expect(position).not.toBeNull();
      expect(position!.beatInBar).toBeGreaterThanOrEqual(0);
      expect(position!.beatInBar).toBeLessThan(4);
      expect(position!.phase).toBeGreaterThanOrEqual(0);
      expect(position!.phase).toBeLessThan(1);
    }

    // The bar boundary itself: the fourth beat of four is followed by the first
    // beat of the next bar, never by a fifth.
    const overrun = at(sample, 100);
    expect(overrun?.beatInBar).toBe(0);
    expect(overrun?.bar).toBe(8);

    // And a bar of one beat has exactly one mark, whatever the prediction does.
    const single = at(sampleClock(clock({ beats_per_bar: 1, beat_in_bar: 0 }), 0), 3_000);
    expect(single?.beatInBar).toBe(0);
    expect(single?.bar).toBe(13);
  });

  it('never runs the beat backwards when the clock it is measured against does', () => {
    const sample = sampleClock(clock({ beat_in_bar: 1, phase: 0.25 }), 1_000);
    const before = at(sample, 900);
    expect(before?.beatInBar).toBe(1);
    expect(before?.phase).toBeCloseTo(0.25, 10);
  });

  it('gives up rather than beating on from a sample nothing has confirmed', () => {
    const sample = sampleClock(clock(), 1_000);
    expect(beatAt(sample, 1_000 + CLOCK_STALE_AFTER_MS - 1).state).toBe('live');
    // Telemetry is about one a second, so a dropped sample or two is ordinary
    // and must not blank the strip. Five seconds of silence is a server that has
    // stopped saying where its clock is.
    expect(beatAt(sample, 1_000 + CLOCK_STALE_AFTER_MS)).toEqual({ state: 'unknown' });
  });

  it('draws a mark for every beat the server says the bar has', () => {
    expect(stripView(beatAt(sampleClock(clock({ beats_per_bar: 3 }), 0), 0)).marks).toBe(3);
    expect(stripView(beatAt(sampleClock(clock({ beats_per_bar: 7 }), 0), 0)).marks).toBe(7);
  });

  it('says which beat it is when the bar is too long to draw a mark for each', () => {
    const long = MAX_BEAT_MARKS + 1;
    const sample = sampleClock(clock({ beats_per_bar: long, beat_in_bar: 12, phase: 0.5 }), 0);
    const strip = stripView(beatAt(sample, 0));
    expect(strip.marks).toBe(1);
    expect(strip.label).toBe(`13/${long}`);
    expect(strip.phase).toBeCloseTo(0.5, 10);

    // A bar exactly as long as there is room for is still drawn mark by mark.
    expect(stripView(beatAt(sampleClock(clock({ beats_per_bar: MAX_BEAT_MARKS }), 0), 0)).marks).toBe(
      MAX_BEAT_MARKS,
    );
  });

  it('refuses a sample that is not a beat of a bar rather than drawing one', () => {
    // A bar of no beats would be a division by nothing, and a beat outside its
    // own bar is a position no show is in. Neither is drawn.
    expect(sampleClock(clock({ beats_per_bar: 0 }), 0)).toBeNull();
    expect(sampleClock(clock({ beats_per_bar: 4, beat_in_bar: 4 }), 0)).toBeNull();
    expect(sampleClock(clock({ phase: 1.5 }), 0)).toBeNull();
    expect(sampleClock(clock({ phase: Number.NaN }), 0)).toBeNull();
    expect(sampleClock(clock({ beats_per_second: Number.NaN }), 0)).toBeNull();
    expect(sampleClock(clock({ beats_per_second: -2 }), 0)).toBeNull();
    expect(sampleClock(clock({ bar: -1 }), 0)).toBeNull();
  });

  it('says that the beat it shows is predicted, and which beat of the bar it is', () => {
    const sample = sampleClock(clock({ beat_in_bar: 2 }), 0);
    expect(beatNote(beatAt(sample, 0))).toBe(
      'Beat 3 of 4, predicted from the last sample and corrected by the next.',
    );
  });
});
