// The playhead bar: a position the server keeps honest.
//
// The pure half in `shared/playhead.ts`, driven the way the screen drives it — a
// sample arrives, frames go by, the pad changes under it, another sample
// arrives. The properties held here are the ones nobody can check by watching a
// bar move: that a layer the server said nothing about has no bar at all, that
// the bar travels at the rate the server reported and in the direction it
// reported, that it never leaves the clip, and that a sample taken before a pad
// change is thrown away rather than drawn over the clip that is playing now.

import { describe, expect, it } from 'vitest';

import type { LayerPlayhead } from '@ovs/schema';
import {
  PLAYHEAD_STALE_AFTER_MS,
  isStale,
  playheadAt,
  playheadLine,
  playheadNote,
  samplePlayhead,
  samplePlayheads,
  type PlayheadPosition,
  type PlayheadSample,
} from '../src/shared/playhead.js';
import type { LayerView, SlotView } from '../src/shared/view.js';

/** What the server sends: a twelve-second clip, four seconds in, at real time. */
function playhead(overrides: Partial<LayerPlayhead> = {}): LayerPlayhead {
  return {
    layer_id: 'LAYER1',
    slot: 3,
    media_item_id: 'MEDIA3',
    frame: 100,
    frame_count: 300,
    position_ms: 4_000,
    duration_ms: 12_000,
    // A thousand clip milliseconds a second is real time.
    speed_ms_per_second: 1_000,
    ...overrides,
  };
}

function slot(index: number, mediaItemId = `MEDIA${index}`): SlotView {
  return {
    index,
    source: { kind: 'media', mediaItemId },
    name: null,
    effects: [],
    sync: { kind: 'FREE_RUN' },
    speed: 1,
    active: false,
    playbackMode: 'loop',
    direction: 'forward',
  transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 },
  };
}

/** The layer the sample above belongs to: playing slot 3, which holds MEDIA3. */
function layer(overrides: Partial<LayerView> = {}): LayerView {
  return {
    id: 'LAYER1',
    name: 'Layer 1',
    banks: 1,
    enabled: true,
    // The default everywhere: nothing is patched and the client may choose.
    mayChooseSlot: true,
    held: { opacity: false, slot: false, transform: false },
    transport: 'playing',
    route: 'A',
    blendMode: 'normal',
    opacity: 1,
    transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 },
    activeSlot: 3,
    trigger: 4,
    defaults: { enabled: true, route: 'A', opacity: 1 },
    effects: [],
    slots: [slot(1), slot(2), slot(3), slot(4)],
    ...overrides,
  };
}

/** Where the bar says the playhead is, or nothing at all. */
function at(
  sample: PlayheadSample | null,
  now: number,
  on: LayerView = layer(),
): PlayheadPosition | null {
  const reading = playheadAt(sample, on, now);
  return reading.state === 'live' ? reading.value : null;
}

describe('the playhead bar', () => {
  it('draws no bar for a layer the server reported no playhead for', () => {
    // A generator layer, a black layer and a clip that has played once and
    // blacked all report nothing, and nothing is not a position at zero.
    expect(samplePlayheads([], 1_000).size).toBe(0);
    expect(samplePlayheads(null, 1_000).size).toBe(0);
    expect(samplePlayheads(undefined, 1_000).size).toBe(0);

    expect(playheadAt(null, layer(), 1_000)).toEqual({ state: 'unknown' });
    expect(playheadAt(undefined, layer(), 1_000)).toEqual({ state: 'unknown' });
    // And nothing for the bar to be labelled with, rather than an em dash beside
    // a track that is not there.
    expect(playheadLine(playheadAt(null, layer(), 1_000))).toBe('');
    expect(playheadNote(playheadAt(null, layer(), 1_000))).toContain('has not reported');
  });

  it('draws a clip at its beginning as a bar at the start rather than as nothing', () => {
    // The difference the whole thing turns on: a clip one frame in is a position
    // every playing layer passes through, and it is not the same answer as a
    // layer nobody reported a playhead for.
    const sample = samplePlayhead(playhead({ position_ms: 0, frame: 0 }), 1_000);
    const position = at(sample, 1_000);
    expect(position?.through).toBe(0);
    expect(position?.positionMs).toBe(0);
    expect(playheadLine(playheadAt(sample, layer(), 1_000))).toBe('0.0 / 12.0 s');
  });

  it('predicts a position half a second on as half a second further at the reported rate', () => {
    const sample = samplePlayhead(playhead(), 1_000);
    expect(at(sample, 1_500)?.positionMs).toBeCloseTo(4_500, 6);

    // The rate is the server's own, already multiplied out: at half real time,
    // half a second of wall clock is a quarter of a second of clip.
    const half = samplePlayhead(playhead({ speed_ms_per_second: 500 }), 1_000);
    expect(at(half, 1_500)?.positionMs).toBeCloseTo(4_250, 6);

    // And the fraction the fill is drawn at follows the position.
    expect(at(sample, 1_500)?.through).toBeCloseTo(4_500 / 12_000, 10);
  });

  it('runs the bar backwards when the server reports a negative rate', () => {
    // A reversed slot, or the return leg of a bounce. Not a special case to
    // hide: it is what the layer is doing.
    const sample = samplePlayhead(playhead({ speed_ms_per_second: -1_000 }), 1_000);
    expect(at(sample, 1_500)?.positionMs).toBeCloseTo(3_500, 6);
    expect(at(sample, 2_000)?.positionMs).toBeCloseTo(3_000, 6);
    expect(playheadNote(playheadAt(sample, layer(), 1_500))).toContain('running backwards');
  });

  it('does not move the bar at all when the server reports a rate of zero', () => {
    // A clip that has played out and frozen. A bar animating over a held frame
    // would be an animation of something that has stopped.
    const sample = samplePlayhead(playhead({ speed_ms_per_second: 0 }), 1_000);
    expect(at(sample, 1_000)?.positionMs).toBe(4_000);
    expect(at(sample, 3_400)?.positionMs).toBe(4_000);
    expect(at(sample, 5_900)?.positionMs).toBe(4_000);
    expect(playheadNote(playheadAt(sample, layer(), 3_400))).toContain('held');
  });

  it('never predicts past the end of the clip or back before its start', () => {
    // What happens at the end is the slot's — loop, freeze, black, bounce — and
    // only the server knows when it happened. The bar waits at the end for the
    // next sample rather than guessing which of the four it was.
    const forwards = samplePlayhead(playhead({ position_ms: 11_000 }), 0);
    for (let now = 0; now < PLAYHEAD_STALE_AFTER_MS; now += 16) {
      const position = at(forwards, now);
      expect(position).not.toBeNull();
      expect(position!.positionMs).toBeGreaterThanOrEqual(0);
      expect(position!.positionMs).toBeLessThanOrEqual(12_000);
      expect(position!.through).toBeGreaterThanOrEqual(0);
      expect(position!.through).toBeLessThanOrEqual(1);
    }
    expect(at(forwards, 4_000)?.positionMs).toBe(12_000);

    const backwards = samplePlayhead(playhead({ position_ms: 1_000, speed_ms_per_second: -1_000 }), 0);
    expect(at(backwards, 500)?.positionMs).toBeCloseTo(500, 6);
    expect(at(backwards, 4_000)?.positionMs).toBe(0);
    expect(at(backwards, 4_000)?.through).toBe(0);

    // And wall-clock time never runs backwards either: a `now` before the sample
    // arrived is the sample itself, not the clip rewound.
    expect(at(forwards, -500)?.positionMs).toBe(11_000);
  });

  it('throws away a sample taken before the pad changed rather than drawing it over the new clip', () => {
    // A sample can be most of a second old and a pad hit is instant. The old
    // clip's position drawn on the new one is the screen disagreeing with the
    // wall at the moment somebody is watching it.
    const sample = samplePlayhead(playhead({ slot: 3 }), 1_000);
    expect(at(sample, 1_200)).not.toBeNull();
    expect(at(sample, 1_200, layer({ activeSlot: 4 }))).toBeNull();

    // The item as well as the slot, because a show may hold the same item in
    // two slots and an operator may reassign one in EDIT.
    const swapped = layer({ slots: [slot(1), slot(2), slot(3, 'MEDIA9'), slot(4)] });
    expect(at(sample, 1_200, swapped)).toBeNull();

    // A sample for another layer is another layer's, whatever it says.
    expect(at(sample, 1_200, layer({ id: 'LAYER2' }))).toBeNull();
  });

  it('draws no bar over a generator or over black, whatever a sample says', () => {
    // A generator draws itself and has no timeline to be a position in, and slot
    // 0 is permanently black on every layer and holds no clip.
    const generated: SlotView = {
      ...slot(3),
      source: {
        kind: 'generator',
        instanceId: 'GENERATOR-1',
        live: {},
        typeId: 'ovs.solid',
        version: 1,
        parameters: {},
        held: [],
        locked: false,
      },
    };
    const sample = samplePlayhead(playhead(), 1_000);
    expect(at(sample, 1_000, layer({ slots: [generated] }))).toBeNull();

    const black = samplePlayhead(playhead({ slot: 0 }), 1_000);
    expect(at(black, 1_000, layer({ activeSlot: 0 }))).toBeNull();
  });

  it('corrects a prediction that had drifted when a fresh sample arrives', () => {
    // Nearly a second of predicting from a clip that was retriggered in the
    // meantime: the bar says four and a half seconds, and the clip is at one.
    const stale = samplePlayhead(playhead(), 1_000);
    expect(at(stale, 1_500)?.positionMs).toBeCloseTo(4_500, 6);

    const fresh = samplePlayhead(playhead({ position_ms: 1_000 }), 1_500);
    expect(at(fresh, 1_500)?.positionMs).toBe(1_000);
    // The correction replaces the prediction rather than being averaged into it,
    // so nothing accumulates: predicting on from the fresh sample owes the old
    // one nothing.
    expect(at(fresh, 2_000)?.positionMs).toBeCloseTo(1_500, 6);
  });

  it('gives up rather than running on from a sample nothing has confirmed', () => {
    const sample = samplePlayhead(playhead(), 1_000);
    expect(sample).not.toBeNull();
    expect(isStale(sample!, 1_000 + PLAYHEAD_STALE_AFTER_MS - 1)).toBe(false);
    expect(playheadAt(sample, layer(), 1_000 + PLAYHEAD_STALE_AFTER_MS - 1).state).toBe('live');

    // Telemetry is about one a second, so a dropped sample or two is ordinary
    // and must not blank a bar in front of somebody cueing. Five seconds of
    // silence is a server that has stopped saying where its playheads are.
    expect(isStale(sample!, 1_000 + PLAYHEAD_STALE_AFTER_MS)).toBe(true);
    expect(playheadAt(sample, layer(), 1_000 + PLAYHEAD_STALE_AFTER_MS)).toEqual({
      state: 'unknown',
    });
  });

  it('refuses a sample that is not a position in a clip rather than drawing one', () => {
    // A clip of no length is the one case nothing can draw a bar for: a bar at
    // zero would claim it had just started.
    expect(samplePlayhead(playhead({ duration_ms: 0 }), 0)).toBeNull();
    expect(samplePlayhead(playhead({ duration_ms: Number.NaN }), 0)).toBeNull();
    expect(samplePlayhead(playhead({ position_ms: -1 }), 0)).toBeNull();
    expect(samplePlayhead(playhead({ position_ms: Number.NaN }), 0)).toBeNull();
    expect(samplePlayhead(playhead({ speed_ms_per_second: Number.NaN }), 0)).toBeNull();
    expect(samplePlayhead(playhead({ speed_ms_per_second: Number.POSITIVE_INFINITY }), 0)).toBeNull();
    expect(samplePlayhead(playhead({ slot: -1 }), 0)).toBeNull();
    expect(samplePlayhead(playhead({ frame: 1.5 }), 0)).toBeNull();
    expect(samplePlayhead(playhead({ layer_id: '' }), 0)).toBeNull();
    expect(samplePlayhead(playhead(), Number.NaN)).toBeNull();

    // A sample a hair past the end of its own clip is the end of it, not a
    // position beyond one.
    expect(samplePlayhead(playhead({ position_ms: 12_004 }), 0)?.positionMs).toBe(12_000);

    // And a refused entry is left out of the set rather than taking the layer
    // down with it.
    const set = samplePlayheads(
      [playhead({ duration_ms: 0 }), playhead({ layer_id: 'LAYER2', slot: 1 })],
      0,
    );
    expect([...set.keys()]).toEqual(['LAYER2']);
  });

  it('reads the position and the length in seconds, which is what an operator cues in', () => {
    // "Eight of twelve" is the question being asked while a hand hovers over the
    // next pad. The frames are on the sample too, and are in the tooltip.
    const sample = samplePlayhead(playhead({ position_ms: 8_000 }), 0);
    expect(playheadLine(playheadAt(sample, layer(), 0))).toBe('8.0 / 12.0 s');

    // A tenth of a second, because a four-second loop is an ordinary thing to
    // put on a layer. Floored, so a clip a hair short of its end does not read
    // as finished while it is still on the wall.
    const loop = samplePlayhead(playhead({ position_ms: 3_250, duration_ms: 4_000 }), 0);
    expect(playheadLine(playheadAt(loop, layer(), 0))).toBe('3.2 / 4.0 s');
    const nearly = samplePlayhead(playhead({ position_ms: 3_990, duration_ms: 4_000 }), 0);
    expect(playheadLine(playheadAt(nearly, layer(), 0))).toBe('3.9 / 4.0 s');

    // Minutes and seconds from a minute up, where a tenth is noise. The length
    // chooses the form, so the pair does not change shape as the clip plays.
    const long = samplePlayhead(playhead({ position_ms: 65_400, duration_ms: 200_000 }), 0);
    expect(playheadLine(playheadAt(long, layer(), 0))).toBe('1:05 / 3:20');
    const justOver = samplePlayhead(playhead({ position_ms: 0, duration_ms: 60_000 }), 0);
    expect(playheadLine(playheadAt(justOver, layer(), 0))).toBe('0:00 / 1:00');
  });

  it('says that the position is predicted, and which frame the server last reported', () => {
    const sample = samplePlayhead(playhead(), 0);
    expect(playheadNote(playheadAt(sample, layer(), 0))).toBe(
      'Predicted from the last sample the server sent, and corrected by the next. Frame 100 of 300, counting from zero, when the server last spoke.',
    );

    // The frame is carried from the sample rather than advanced with the
    // prediction: a frame index worked out here would be a number no server
    // sent.
    expect(at(sample, 2_000)?.frame).toBe(100);
    expect(at(sample, 2_000)?.frameCount).toBe(300);
  });
});
