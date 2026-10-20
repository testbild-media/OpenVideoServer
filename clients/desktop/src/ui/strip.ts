// A clip's moving preview: ten frames side by side in one picture.
//
// **A strip rather than an animated image** (owner, 2026-09-23). What was asked
// for is a preview that plays while the pointer is over a clip and shows its
// first frame otherwise, and an animated image cannot be stopped on a frame: a
// browser plays one whenever it is on screen. Ten frames in one picture can —
// the box shows a tenth of it at a time, and stepping through those tenths is
// what playing means here. Whether it is playing is the stylesheet's business
// (`.strip` in style.css), so one picture serves a pad that plays on hover, a
// layer's picture that plays always, and a library tile that plays on hover.
//
// It is a background rather than an `img` for the same reason: which part of a
// picture is showing is a thing CSS can say and animate, and a frame is a whole
// number of steps rather than a position that drifts.

import { make } from './dom.js';

/**
 * How many frames a strip holds. The server always sends exactly this many,
 * repeating the last where a clip was too short (`STRIP_FRAMES` in
 * upload_routes.rs), so a tenth is always one whole frame.
 */
export const STRIP_FRAMES = 10;

/**
 * Each strip's frame shape, width over height, once its picture has been read.
 *
 * Keyed by the picture itself, which is content-addressed on the server and so
 * never changes under the same key. `null` while it is being read, and
 * `UNREADABLE` for a picture that would not say its size.
 */
const shapes = new Map<string, number | null>();

/** A strip whose shape could not be read, which keeps the box's own shape. */
const UNREADABLE = -1;

/** Who is waiting for a strip's shape, keyed as `shapes` is. */
const waiting = new Map<string, Array<(shape: number) => void>>();

/**
 * Draws a clip's moving preview into a box, showing its first frame.
 *
 * **At the clip's own shape, as its still is.** The server takes each frame
 * at the clip's proportions with nothing around it, and the boxes are 16:9: a
 * strip stretched to fill one showed a 4:3 or a square clip pulled sideways,
 * beside a still of the same clip that was not. The frame is sized to fit the
 * box the way `object-fit: contain` fits the still, by the container units in
 * `.strip`, and the shape comes from the picture once it has been read.
 *
 * Reuses what is already in the box where it can, so a redraw does not restart
 * a preview somebody is watching.
 */
export function paintStrip(target: HTMLElement, strip: string): void {
  const url = `url("${strip}")`;
  const existing = target.firstElementChild;
  let frame: HTMLElement;
  if (existing instanceof HTMLElement && existing.classList.contains('strip-frame')) {
    frame = existing;
  } else {
    frame = make('span', 'strip-frame');
    frame.append(make('span', 'strip'));
    target.replaceChildren(frame);
  }
  const picture = frame.firstElementChild as HTMLElement;
  if (picture.style.backgroundImage !== url) {
    picture.style.backgroundImage = url;
  }
  whenShaped(strip, (shape) => {
    // Still this strip: a box redrawn with another clip while this one was
    // being read keeps the other clip's shape.
    if (picture.style.backgroundImage === url) {
      frame.style.setProperty('--frame', String(shape));
    }
  });
}

/** Calls `then` with a strip's frame shape, reading the picture once if need be. */
function whenShaped(strip: string, then: (shape: number) => void): void {
  const known = shapes.get(strip);
  if (known === UNREADABLE) {
    return;
  }
  if (typeof known === 'number') {
    then(known);
    return;
  }
  const queue = waiting.get(strip) ?? [];
  queue.push(then);
  waiting.set(strip, queue);
  if (known === null) {
    return;
  }
  shapes.set(strip, null);
  // A picture that will not say its size keeps the box's own shape, which is
  // what every strip had before this.
  const unreadable = () => {
    shapes.set(strip, UNREADABLE);
    waiting.delete(strip);
  };
  const reading = new Image();
  reading.addEventListener('load', () => {
    const shape =
      reading.naturalHeight > 0 ? reading.naturalWidth / STRIP_FRAMES / reading.naturalHeight : 0;
    if (!(shape > 0)) {
      unreadable();
      return;
    }
    shapes.set(strip, shape);
    for (const each of waiting.get(strip) ?? []) {
      each(shape);
    }
    waiting.delete(strip);
  });
  reading.addEventListener('error', unreadable);
  reading.src = strip;
}
