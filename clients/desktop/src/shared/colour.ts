// Between the colour the model stores and the colour a picker speaks.
//
// The model stores **linear** RGBA in 0..=1, because that is the space a
// compositor multiplies in: half brightness over black is 0.5, and two layers
// at half opacity are a half of a half. A colour picker — every one of them,
// including the operating system's — speaks **sRGB**, where 0.5 is the grey
// that *looks* halfway and is 0.2140 of the light.
//
// Those are not the same number, and a picker wired straight to the model is a
// picker that lies about what it picked. So the conversion is here, once,
// written out rather than approximated: the piecewise curve of IEC 61966-2-1,
// not the gamma-2.2 shorthand, which is off by up to 1.5% in the shadows where
// a stage picture mostly lives.
//
// The owner asked for the picker on 2026-09-27 — "für alle parameter mit Farbe
// nehme doch bitte einen Colorpicker, damit es einfacher wird". The four number
// fields it replaces carried a note saying a picker would need a conversion
// this client had no business choosing on an operator's behalf. That was about
// choosing one silently. This one is named, standard, and tested against its
// own anchors.

/** Where the sRGB curve stops being a straight line, going up. */
const SRGB_KNEE = 0.04045;

/** The same knee from the other side, in linear light. */
const LINEAR_KNEE = 0.0031308;

/** Keeps a channel inside the only range the model has for one. */
function clamped(channel: number): number {
  if (!Number.isFinite(channel)) {
    return 0;
  }
  return Math.min(1, Math.max(0, channel));
}

/** One sRGB channel in 0..=1, as linear light. */
export function linearFromSrgb(channel: number): number {
  const value = clamped(channel);
  return value <= SRGB_KNEE ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

/** One channel of linear light in 0..=1, as sRGB. */
export function srgbFromLinear(channel: number): number {
  const value = clamped(channel);
  return value <= LINEAR_KNEE ? value * 12.92 : 1.055 * value ** (1 / 2.4) - 0.055;
}

/**
 * The model's colour as `#rrggbb`, which is all `input[type=color]` accepts.
 *
 * Alpha is not in it. The element has no alpha channel at all, which is why the
 * controls that use this keep a field for it beside the swatch rather than
 * pretending a colour is three numbers.
 */
export function hexFromLinear(rgba: readonly number[]): string {
  const hex = [0, 1, 2]
    .map((index) => Math.round(srgbFromLinear(rgba[index] ?? 0) * 255))
    .map((channel) => channel.toString(16).padStart(2, '0'))
    .join('');
  return `#${hex}`;
}

/**
 * `#rrggbb` back to three channels of linear light.
 *
 * Anything else answers black rather than throwing: this reads a value out of a
 * DOM element, and an element that has been handed something else is not a
 * reason to take the panel down.
 */
export function linearFromHex(hex: string): [number, number, number] {
  const match = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (match === null) {
    return [0, 0, 0];
  }
  const digits = match[1] ?? '000000';
  const channel = (at: number): number =>
    linearFromSrgb(Number.parseInt(digits.slice(at, at + 2), 16) / 255);
  return [channel(0), channel(2), channel(4)];
}
