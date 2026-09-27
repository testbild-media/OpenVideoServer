// Where a playing layer's clip has got to, between the moments the server says.
//
// The second thing on this screen that moves, and it moves for the reason the
// beat strip does: `LayerPlayhead` is telemetry, sampled about once a second,
// and a bar that only moved when a sample landed would tick once a second
// beside a wall running smoothly. So this is the beat strip's arithmetic again —
// predict from the rate the server sent, replace the prediction outright with
// every fresh sample, give up after a staleness window — with two rules the beat
// does not have:
//
//   - **A layer nobody reported a playhead for has no bar at all.** Not an empty
//     one, not one at zero. A generator layer draws itself and has no timeline
//     to be a position in, a black layer has no clip, and a clip that played
//     once and blacked has finished; all three report nothing, and "at the
//     beginning" is a different picture from "there is no clip"
//     (`LayerPlayhead`).
//   - **A sample belongs to the clip it was taken on.** It carries its slot and
//     its media item precisely so that a sample most of a second old is thrown
//     away when the pad has moved under it, rather than drawn over the clip that
//     is playing now.
//
// The rate is signed and may be zero, and neither is hidden. A reversed slot and
// the return leg of a bounce run the bar backwards; a frozen clip stops it,
// because a bar animating over a held frame would be an animation of something
// that is not moving. Both are what the layer is actually doing.
//
// Times in here are milliseconds from a monotonic clock — `performance.now()` in
// the window — passed in rather than read, so the behaviour that matters during
// a show is tested rather than hoped for.

import type { LayerPlayhead } from '@ovs/schema';

import { BLACK_SLOT_INDEX } from './limits.js';
import type { Reading } from './live.js';
import type { LayerView } from './view.js';

/**
 * How long a sample is worth predicting from.
 *
 * The beat strip's window, for the beat strip's reason: playheads ride on the
 * same telemetry, about one a second, so a dropped sample or two is ordinary and
 * must not blank a bar in front of somebody cueing. Several in a row is a server
 * that has stopped saying where its playheads are, and a bar still travelling
 * from a sample that old would be this client animating a clip it knows nothing
 * about.
 */
export const PLAYHEAD_STALE_AFTER_MS = 5_000;

/** Below this, a length reads in seconds; at or above it, in minutes and seconds. */
const MINUTE_MS = 60_000;

/**
 * One sample of one layer's playhead, and when this client received it.
 *
 * `at` is this client's own monotonic reading at the moment the sample arrived,
 * not anything the server said about time: the two machines' clocks are not the
 * same clock, and what is measured here is how long ago *this* client heard.
 */
export interface PlayheadSample {
  /** Which layer this is the playhead of. */
  layerId: string;
  /** Which slot of that layer was playing when the sample was taken. */
  slot: number;
  /** What was playing. */
  mediaItemId: string;
  /** The frame the playhead was on, counting from zero. */
  frame: number;
  /** How many frames one pass of the clip holds. */
  frameCount: number;
  /** How far through the clip the playhead was, in milliseconds. */
  positionMs: number;
  /** How long one pass of the clip is, in milliseconds, at its own rate. */
  durationMs: number;
  /** Clip milliseconds a second: a thousand is real time, signed, zero when held. */
  speedMsPerSecond: number;
  /** When this client received it, on a monotonic clock, in milliseconds. */
  at: number;
}

/** Where the bar draws the playhead: a sample carried forward to now. */
export interface PlayheadPosition {
  /** How far through the clip, in milliseconds, never outside the clip. */
  positionMs: number;
  /** How long one pass of the clip is. */
  durationMs: number;
  /** The same thing as a fraction, `0..1`, which is the width of the fill. */
  through: number;
  /** The rate this position was predicted at, signed. */
  speedMsPerSecond: number;
  /**
   * The frame the last sample reported, not a predicted one.
   *
   * Carried rather than advanced: a frame index cannot be worked out from a
   * millisecond position without the clip's own rate, and a client that divided
   * its way to one would be putting a frame number in front of an operator that
   * no server ever sent.
   */
  frame: number;
  /** How many frames one pass holds. */
  frameCount: number;
}

/**
 * What the bar can say about a layer's playhead.
 *
 * The same `Reading` the live controls use. Only two of its four states can
 * occur: nothing on this screen asks the server to move a playhead, so there is
 * nothing to be pending or refused about — and a layer nobody reported one for
 * is unknown, which is what draws no bar.
 */
export type PlayheadReading = Reading<PlayheadPosition>;

/**
 * A sample of what the server reported, or null when there is nothing to draw.
 *
 * Null covers a sample whose numbers are not a position in a clip: a clip of no
 * length, which nothing can say how far through something is; a position or a
 * rate that is not a number. Inventing a bar from any of those would be exactly
 * the thing the rest of this client refuses to do.
 */
export function samplePlayhead(playhead: LayerPlayhead, at: number): PlayheadSample | null {
  const {
    layer_id: layerId,
    slot,
    media_item_id: mediaItemId,
    frame,
    frame_count: frameCount,
    position_ms: positionMs,
    duration_ms: durationMs,
    speed_ms_per_second: speed,
  } = playhead;
  const whole = (value: number): boolean => Number.isInteger(value) && value >= 0;

  if (typeof layerId !== 'string' || layerId === '' || typeof mediaItemId !== 'string') {
    return null;
  }
  if (!whole(slot) || !whole(frame) || !whole(frameCount)) {
    return null;
  }
  if (!Number.isFinite(durationMs) || durationMs <= 0) {
    // A clip with no length is the one case nothing can draw a bar for, and
    // `LayerPlayhead::fraction` says the same thing on the other side of the
    // wire. A bar at zero would claim it had just started.
    return null;
  }
  if (!Number.isFinite(positionMs) || positionMs < 0) {
    return null;
  }
  if (!Number.isFinite(speed) || !Number.isFinite(at)) {
    return null;
  }

  return {
    layerId,
    slot,
    mediaItemId,
    frame,
    frameCount,
    // A sample that arrived a hair past the end of its own clip — a wrap the
    // server caught mid-publish — is the end of the clip, not a position beyond
    // one.
    positionMs: Math.min(positionMs, durationMs),
    durationMs,
    speedMsPerSecond: speed,
    at,
  };
}

/**
 * Every playhead in one telemetry sample, by layer.
 *
 * The whole map is replaced each time rather than merged into, and that is the
 * point: a layer the server has stopped reporting is a layer with no playhead,
 * and a bar left behind from the last sample would go on describing a clip that
 * has finished. An entry that is not a position in a clip is left out rather
 * than drawn.
 */
export function samplePlayheads(
  playheads: readonly LayerPlayhead[] | null | undefined,
  at: number,
): Map<string, PlayheadSample> {
  const samples = new Map<string, PlayheadSample>();
  for (const playhead of playheads ?? []) {
    const sample = samplePlayhead(playhead, at);
    if (sample !== null) {
      samples.set(sample.layerId, sample);
    }
  }
  return samples;
}

/** Whether a sample is too old to predict from. */
export function isStale(sample: PlayheadSample, now: number): boolean {
  return !Number.isFinite(now) || now - sample.at >= PLAYHEAD_STALE_AFTER_MS;
}

/**
 * Where the playhead is now, given the last sample for that layer.
 *
 * The whole of the prediction: the sample's position plus the clip milliseconds
 * that have passed since it arrived, at the signed rate it reported. A fresh
 * sample replaces the one it was predicted from, which is the correction —
 * nothing accumulates, so an error in one prediction cannot become drift in the
 * next.
 *
 * It never runs past the end of the clip or back before its start. What happens
 * at the end is the slot's (`loop`, `once_freeze`, `once_black`, `bounce`) and
 * only the server knows when it happened, so the bar waits at the end for the
 * next sample rather than guessing which of the four it was.
 */
export function playheadAt(
  sample: PlayheadSample | null | undefined,
  layer: LayerView,
  now: number,
): PlayheadReading {
  if (sample === null || sample === undefined) {
    return { state: 'unknown' };
  }
  if (isStale(sample, now)) {
    // Nobody has corrected this for long enough that it is a guess.
    return { state: 'unknown' };
  }
  if (!describes(sample, layer)) {
    return { state: 'unknown' };
  }

  // Never backwards in wall-clock time. A monotonic clock does not go back, and
  // if one ever did, running the clip back with it would say the show had.
  const elapsed = Math.max(0, now - sample.at) / 1000;
  const positionMs = Math.min(
    sample.durationMs,
    Math.max(0, sample.positionMs + elapsed * sample.speedMsPerSecond),
  );

  return {
    state: 'live',
    value: {
      positionMs,
      durationMs: sample.durationMs,
      through: positionMs / sample.durationMs,
      speedMsPerSecond: sample.speedMsPerSecond,
      frame: sample.frame,
      frameCount: sample.frameCount,
    },
  };
}

/**
 * Whether a sample still describes what the layer is playing.
 *
 * A sample can be most of a second old, and a pad hit is instant. Drawing the
 * old clip's position over the new one would be the screen disagreeing with the
 * wall at exactly the moment an operator is watching it, so a sample whose slot
 * or item is not the one on stage is thrown away and the bar waits for the next.
 */
function describes(sample: PlayheadSample, layer: LayerView): boolean {
  if (sample.layerId !== layer.id || sample.slot !== layer.activeSlot) {
    return false;
  }
  if (layer.activeSlot === BLACK_SLOT_INDEX) {
    // Slot 0 is permanently black on every layer and holds no clip, so nothing
    // the server could say about it is a position in one.
    return false;
  }

  const playing = layer.slots.find((slot) => slot.index === layer.activeSlot) ?? null;
  if (playing === null) {
    // The server says this layer is on a slot the open show has nothing in.
    // The slot itself matched, which is what the sample is about; there is no
    // item here to disagree with it.
    return true;
  }
  if (playing.source.kind !== 'media') {
    // A generator has no timeline to be a position in.
    return false;
  }
  return playing.source.mediaItemId === sample.mediaItemId;
}

/**
 * The position and the length, in the unit an operator cues in.
 *
 * Seconds rather than frames, although the sample carries both. The question
 * being asked while a hand hovers over the next pad is how long there is left,
 * and that is eight of twelve — not frame 197 of 300 (`LayerPlayhead`, "both
 * units, because two things are reading"). The frame is in the tooltip, where
 * somebody who wants it can read it and can see that it is the one the server
 * last reported rather than a predicted one.
 *
 * One decimal below a minute, because a four-second loop is an ordinary thing to
 * put on a layer and `0:03 / 0:04` would barely move; minutes and seconds at a
 * minute and above, where a tenth is noise. The length chooses the form rather
 * than the position, so the pair does not change shape as the clip plays.
 *
 * Empty when there is nothing to say, rather than the em dash the rest of this
 * screen writes: a layer with no playhead has no bar for a label to sit beside.
 */
export function playheadLine(reading: PlayheadReading): string {
  if (reading.state !== 'live') {
    return '';
  }
  const { positionMs, durationMs } = reading.value;
  if (durationMs >= MINUTE_MS) {
    return `${minutes(positionMs)} / ${minutes(durationMs)}`;
  }
  return `${seconds(positionMs)} / ${seconds(durationMs)} s`;
}

/** Seconds to one decimal, for a clip shorter than a minute. */
function seconds(ms: number): string {
  // Floored to the tenth rather than rounded, for the reason `minutes` floors:
  // a clip a hair short of its end reading `4.0 / 4.0` says it has finished
  // while it is still on the wall.
  return (Math.floor(ms / 100) / 10).toFixed(1);
}

/** `1:05`: where the playhead is, and how long the clip is. */
function minutes(ms: number): string {
  // Floored rather than rounded, so a position never reads as a second the clip
  // has not reached and a length never reads as longer than it is.
  const total = Math.floor(ms / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`;
}

/**
 * What the bar is saying, in words, for its tooltip.
 *
 * It says that the position is predicted, because between samples it is this
 * client's arithmetic on the server's numbers, and an operator reading a
 * position off a screen is entitled to know which of the two they are looking
 * at. A held clip and one running backwards say so as well: a bar that stood
 * still would otherwise look like a client that had stopped hearing, and one
 * emptying looks like a fault until it is read as a reversed slot.
 *
 * Deliberately without the position itself, which is written beside the bar and
 * changes ten times a second. A tooltip rewritten at that rate is an
 * accessibility tree rebuilt at that rate, for a sentence nobody can read while
 * it moves.
 */
export function playheadNote(reading: PlayheadReading): string {
  if (reading.state !== 'live') {
    return 'The server has not reported a playhead for this layer. A layer that is not playing a clip has none.';
  }

  const { speedMsPerSecond: speed, frame, frameCount } = reading.value;
  const moving =
    speed === 0
      ? 'The server reports this clip as held, so the bar does not move.'
      : speed < 0
        ? 'It is running backwards, which is a reversed slot or the return leg of a bounce.'
        : 'Predicted from the last sample the server sent, and corrected by the next.';
  return `${moving} Frame ${frame} of ${frameCount}, counting from zero, when the server last spoke.`;
}
