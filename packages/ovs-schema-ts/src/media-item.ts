// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * MediaItemId (ULID, 26 characters)
 */
export type MediaItemId = string;
/**
 * The kind of thing a media item is.
 */
export type MediaKind = 'video' | 'image';
/**
 * Where a media item is in the ingest pipeline.
 *
 * These are the states the operator is shown. They deliberately say nothing about codecs, because the operator never chooses one.
 */
export type MediaState = 'importing' | 'processing' | 'uploading' | 'verifying' | 'ready' | 'failed';

/**
 * A logical media item: the only media identity a show ever stores.
 */
export interface MediaItem {
  /**
   * When the item was first imported.
   */
  created_at: number;
  /**
   * Pixel dimensions, once known.
   */
  dimensions?: SizePx | null;
  /**
   * Name shown in the content browser. Freely renameable without breaking shows.
   */
  display_name: string;
  /**
   * Duration in milliseconds. `None` for stills.
   */
  duration_ms?: number | null;
  /**
   * Why the item failed, when it did.
   */
  failure_reason?: string | null;
  /**
   * Where this item is filed in the library.
   *
   * A slash-separated path, for example `Intros/2026`, and empty for the root. **Virtual, always**: it is a name the item is filed under and nothing more. The file on disk does not move, its object key does not change, and content addressing goes on working (owner, 2026-09-23).
   *
   * Never a path on any filesystem, and never used to find anything.
   */
  folder?: string;
  /**
   * Frame rate, once known. `None` for stills.
   */
  frame_rate?: FrameRate | null;
  /**
   * Whether this item carries an alpha channel.
   */
  has_alpha: boolean;
  /**
   * Stable identifier.
   */
  id: MediaItemId;
  /**
   * Video or image.
   */
  kind: MediaKind;
  /**
   * The filename this item was imported from, kept for the operator's orientation only.
   *
   * Never used as identity and never used to find the file on disk.
   */
  original_filename: string;
  /**
   * Pipeline state.
   */
  state: MediaState;
}
/**
 * A size in whole pixels.
 */
export interface SizePx {
  /**
   * Height in pixels.
   */
  height: number;
  /**
   * Width in pixels.
   */
  width: number;
}
/**
 * An exact frame rate expressed as a rational number of frames per second.
 *
 * Rational rather than floating point so that 60000/1001 stays exact and the scheduler cannot accumulate drift.
 */
export interface FrameRate {
  /**
   * Denominator, for example `1001`.
   */
  denominator: number;
  /**
   * Numerator, for example `60000`.
   */
  numerator: number;
}
