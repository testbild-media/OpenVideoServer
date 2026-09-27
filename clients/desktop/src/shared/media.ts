// The media library, as something to put in front of an operator.
//
// A database view rather than a filesystem listing: no path, no
// storage key, nothing about a codec. Everything here is read off what `/media`
// sent and nothing else, because a library that filled a gap with a plausible
// number would be telling an operator something nobody measured.
//
// One thing this page deliberately does not show, because the server does not
// send it: **the playback profile**. It lives on `MediaAsset`, which is
// server-internal, and it is hidden on purpose — the operator never chooses a
// codec, and there is a Rust test that fails if one ever appears in
// a `MediaItem` (`a_media_item_never_reveals_which_profile_was_chosen`).
//
// How much disc an item takes is sent, on `MediaListItem` beside the item.
// That is a different kind of fact: it says nothing about codecs or paths, and
// it is what an operator uses to decide what to delete before a show (owner,
// 2026-09-23).

import type { FrameRate, MediaItem, MediaListItem, MediaState, SizePx } from '@ovs/schema';

import { bytesLabel } from './ingest.js';

/** One thing the server said about an item, ready to be drawn. */
export interface Fact {
  /** What it is, for the operator. */
  label: string;
  /** What the server said, in words. */
  value: string;
  /** The exact form, where rounding for the eye lost something. */
  title?: string;
}

/** What the states of the ingest pipeline are called on screen. */
const STATE_LABELS: Record<MediaState, string> = {
  importing: 'Importing',
  processing: 'Processing',
  uploading: 'Uploading',
  verifying: 'Verifying',
  ready: 'Ready',
  failed: 'Failed',
};

/** What an item's state is called on screen. */
export function stateLabel(state: MediaState): string {
  return STATE_LABELS[state] ?? state;
}

/** Whether an item may be put in a slot. `MediaState::is_usable` in Rust. */
export function isUsable(item: MediaItem): boolean {
  return item.state === 'ready';
}

/**
 * The pixel size, as `1920 × 1080`.
 *
 * `null` for an item the server has not sent dimensions for, which is every
 * item that has not finished being verified.
 */
export function sizeLabel(dimensions: SizePx | null | undefined): string | null {
  if (!dimensions) {
    return null;
  }
  return `${dimensions.width} × ${dimensions.height}`;
}

/**
 * A duration, as an operator reads one off a cue sheet.
 *
 * Tenths, because a clip's length matters to a tenth and not to a millisecond,
 * and the millisecond the server sent is kept for the tooltip so nothing is
 * lost by the rounding.
 */
export function durationLabel(durationMs: number | null | undefined): string | null {
  if (durationMs === null || durationMs === undefined) {
    return null;
  }
  const totalTenths = Math.round(durationMs / 100);
  const tenths = totalTenths % 10;
  const totalSeconds = (totalTenths - tenths) / 10;
  const seconds = totalSeconds % 60;
  const minutes = (totalSeconds - seconds) / 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}.${tenths}`;
}

/**
 * A frame rate, as a decimal.
 *
 * The ratio is what the server sent and what the scheduler uses — 60000/1001 is
 * not 59.94 — so the decimal is for reading and the ratio goes in the tooltip.
 */
export function rateLabel(rate: FrameRate | null | undefined): string | null {
  if (!rate || rate.denominator === 0) {
    return null;
  }
  const fps = rate.numerator / rate.denominator;
  // Two places distinguish 59.94 from 60 and 23.98 from 24, which is the whole
  // reason anybody looks.
  const rounded = Number.isInteger(fps) ? String(fps) : fps.toFixed(2);
  return `${rounded} fps`;
}

/** The exact rate, for the tooltip. */
export function rateRatio(rate: FrameRate): string {
  return `${rate.numerator}/${rate.denominator}`;
}

/** When the item was imported, in the operator's own locale. */
export function importedLabel(createdAtMs: number): string {
  return new Date(createdAtMs).toLocaleString();
}

/**
 * Everything the server said about one item, in the order a row reads.
 *
 * A field the server left out is left out here. There is no "—" for an unknown
 * duration: a dash in a column an operator scans reads as "none", and a clip
 * whose length nobody has measured yet is not a clip of no length.
 */
export function facts(item: MediaItem | MediaListItem): Fact[] {
  const out: Fact[] = [];

  out.push({ label: 'Type', value: item.kind === 'video' ? 'Video' : 'Still' });

  const size = sizeLabel(item.dimensions);
  if (size !== null) {
    out.push({ label: 'Dimensions', value: size });
  }

  // Zero for an item whose content has not arrived — an imported show's
  // items look like that until they are filled — and absent rather than "0 B",
  // because nothing is on the disc for it yet. In whatever unit reads best,
  // with the exact count in the tooltip: "487.3 MB" is what somebody deciding
  // what to delete wants, and the byte count is what they want when two clips
  // read the same.
  const bytes = 'byte_size' in item ? item.byte_size : 0;
  if (bytes > 0) {
    out.push({
      label: 'Size',
      value: bytesLabel(bytes),
      title: `${bytes.toLocaleString('en-GB')} bytes`,
    });
  }

  const duration = durationLabel(item.duration_ms);
  if (duration !== null) {
    out.push({
      label: 'Duration',
      value: duration,
      title: `${item.duration_ms} ms`,
    });
  }

  const rate = rateLabel(item.frame_rate);
  if (rate !== null && item.frame_rate) {
    out.push({ label: 'Rate', value: rate, title: rateRatio(item.frame_rate) });
  }

  // Said only when it is true. "Alpha: no" on every opaque clip in the library
  // is a column nobody reads, and the ones that carry alpha are the ones an
  // operator is looking for.
  if (item.has_alpha) {
    out.push({ label: 'Alpha', value: 'Yes' });
  }

  out.push({ label: 'Imported', value: importedLabel(item.created_at) });

  return out;
}

/** What to draw in the picture's place. */
export type Picture =
  | { kind: 'picture'; src: string; alt: string }
  | { kind: 'name'; text: string };

/**
 * The item's picture, or its name when there is none.
 *
 * `GET /media/{id}/thumbnail` answers 404 for an item registered before
 * thumbnails existed and for one nothing could be read from, and the server
 * treats that as ordinary: "a library entry with no picture is a library entry
 * with a name on it, which is what every one of them was until now"
 * (`make_thumbnail` in upload_routes.rs). So a missing picture is not a broken
 * image and not a placeholder that looks like a fault — it is the name, which
 * is the thing the operator was going to read anyway.
 */
export function picture(item: MediaItem, thumbnail: string | null): Picture {
  if (thumbnail === null || thumbnail === '') {
    return { kind: 'name', text: item.display_name };
  }
  return { kind: 'picture', src: thumbnail, alt: item.display_name };
}

/**
 * What the usage endpoint said, as a sentence.
 *
 * An empty list is a real answer and says so plainly: it is the difference
 * between "nothing uses this" and "nobody has asked yet", and only one of those
 * makes deleting safe.
 */
export function usageLine(usedBy: readonly string[]): string {
  if (usedBy.length === 0) {
    return 'No show uses this.';
  }
  return `Used by ${listOf(usedBy)}.`;
}

/** A refusal, as the HTTP client reports one. */
export interface Refusal {
  code: string;
  message: string;
}

/**
 * Why a delete did not happen, naming the shows when that is the reason.
 *
 * The server refuses with `media_in_use` and its own sentence, which lists the
 * shows — but it lists them inside prose, and an operator has to be told
 * which shows they would have broken in a form the panel can put beside the
 * item. So the names come from `/media/{id}/usage`, which is the same database
 * answer in a shape nothing has to parse.
 *
 * When usage comes back empty against a refusal that says otherwise — a show
 * deleted between the two requests — the server's own words stand. Inventing a
 * reconciliation between two answers a second apart would be this client
 * deciding which of the server's statements it believes.
 */
export function refusedDeletion(refusal: Refusal, usedBy: readonly string[]): string {
  if (refusal.code !== 'media_in_use' || usedBy.length === 0) {
    return refusal.message;
  }
  const shows = usedBy.length === 1 ? 'that show' : 'those shows';
  return `Not deleted: ${listOf(usedBy)} still ${usedBy.length === 1 ? 'uses' : 'use'} it. Take it out of ${shows} first.`;
}

/** `a`, `a and b`, `a, b and c`. */
export function listOf(names: readonly string[]): string {
  if (names.length <= 1) {
    return names[0] ?? '';
  }
  const last = names[names.length - 1] ?? '';
  return `${names.slice(0, -1).join(', ')} and ${last}`;
}

/**
 * The library in the order a browser should show it.
 *
 * Newest first, which is the order `/media` already sends and the order an
 * operator who has just imported something is looking in. Sorted here anyway
 * rather than trusted, because the ordering is the list endpoint's to change
 * and a content browser that reorders itself after an unrelated server upgrade
 * is a puzzle nobody needs during a get-in.
 */
export function newestFirst(items: readonly MediaItem[]): MediaItem[] {
  return [...items].sort((a, b) => b.created_at - a.created_at);
}
