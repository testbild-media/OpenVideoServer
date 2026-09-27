// The conversion between what the model stores and what a picker speaks.
//
// Worth testing rather than trusting because it is the one piece of arithmetic
// in this client where being *nearly* right looks exactly like being right: a
// gamma-2.2 approximation draws a picture that is plausible everywhere and
// wrong by a couple of percent in the shadows, and nobody finds that by eye.

import { describe, expect, it } from 'vitest';

import {
  hexFromLinear,
  linearFromHex,
  linearFromSrgb,
  srgbFromLinear,
} from '../src/shared/colour.js';

describe('sRGB and linear light', () => {
  it('leaves the two ends where they are', () => {
    // Black is black and white is white in both spaces. If these move, the
    // curve has been written with a sign or a scale wrong and everything else
    // here would still pass.
    expect(linearFromSrgb(0)).toBe(0);
    expect(linearFromSrgb(1)).toBeCloseTo(1, 12);
    expect(srgbFromLinear(0)).toBe(0);
    expect(srgbFromLinear(1)).toBeCloseTo(1, 12);
  });

  it('puts mid grey where the standard puts it, not where gamma 2.2 does', () => {
    // sRGB 0.5 is 0.2140 of the light. The 2.2 shorthand says 0.2176, which is
    // the error this test exists to keep out.
    expect(linearFromSrgb(0.5)).toBeCloseTo(0.2140, 4);
    expect(linearFromSrgb(0.5)).not.toBeCloseTo(0.5 ** 2.2, 4);
    expect(srgbFromLinear(0.2140)).toBeCloseTo(0.5, 3);
  });

  it('is a straight line below the knee and joins up at it', () => {
    // The piecewise part. A curve that used the power law all the way down
    // would be visibly wrong here and correct everywhere a casual check looks.
    expect(linearFromSrgb(0.04)).toBeCloseTo(0.04 / 12.92, 12);
    // The two halves meet: approached from either side of the knee, within the
    // tolerance the standard's own rounded constants allow.
    expect(linearFromSrgb(0.04045)).toBeCloseTo(linearFromSrgb(0.0405), 4);
  });

  it('comes back to where it started', () => {
    for (const channel of [0, 0.01, 0.04, 0.2, 0.5, 0.75, 1]) {
      expect(linearFromSrgb(srgbFromLinear(channel))).toBeCloseTo(channel, 10);
      expect(srgbFromLinear(linearFromSrgb(channel))).toBeCloseTo(channel, 10);
    }
  });

  it('keeps a channel in range however it is asked not to', () => {
    // These arrive from a DOM element and from the server, and neither is this
    // client's to trust. A NaN reaching the renderer as a colour is a black
    // frame nobody can explain.
    expect(linearFromSrgb(-1)).toBe(0);
    expect(linearFromSrgb(2)).toBeCloseTo(1, 12);
    expect(srgbFromLinear(Number.NaN)).toBe(0);
  });
});

describe('the hex a picker takes', () => {
  it('writes and reads the same colour', () => {
    const colour: [number, number, number] = [0.2140, 0.0513, 0.7231];
    const back = linearFromHex(hexFromLinear(colour));
    // Eight bits per channel is what the element stores, so the round trip is
    // exact to about a 255th in sRGB — not to the linear value it came from.
    for (const [index, channel] of colour.entries()) {
      expect(srgbFromLinear(back[index] ?? 0)).toBeCloseTo(srgbFromLinear(channel), 2);
    }
  });

  it('writes the two ends as the two ends', () => {
    expect(hexFromLinear([0, 0, 0, 1])).toBe('#000000');
    expect(hexFromLinear([1, 1, 1, 1])).toBe('#ffffff');
    // Pure red stays pure red: a conversion applied to the wrong channel shows
    // up here and nowhere else.
    expect(hexFromLinear([1, 0, 0, 1])).toBe('#ff0000');
    expect(hexFromLinear([0, 1, 0, 1])).toBe('#00ff00');
    expect(hexFromLinear([0, 0, 1, 1])).toBe('#0000ff');
  });

  it('ignores alpha, because the element has none', () => {
    expect(hexFromLinear([1, 0, 0, 0])).toBe(hexFromLinear([1, 0, 0, 1]));
  });

  it('answers black for anything that is not a colour', () => {
    // Read off a DOM element, so this is a real input rather than a defensive
    // gesture: an element handed something else must not take the panel down.
    expect(linearFromHex('')).toEqual([0, 0, 0]);
    expect(linearFromHex('rebeccapurple')).toEqual([0, 0, 0]);
    expect(linearFromHex('#12345')).toEqual([0, 0, 0]);
  });

  it('takes the hex with or without its hash, in either case', () => {
    expect(linearFromHex('#FF0000')).toEqual(linearFromHex('ff0000'));
  });
});
