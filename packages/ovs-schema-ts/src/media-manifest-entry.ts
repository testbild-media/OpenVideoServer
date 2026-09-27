// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The kind of thing a media item is.
 */
export type MediaKind = 'video' | 'image';
/**
 * MediaItemId (ULID, 26 characters)
 */
export type MediaItemId = string;

/**
 * One media item a package needs, and everything required to ask for it.
 *
 * No physical path and no storage key: those belong to the server that holds the file, and a package moves between servers.
 */
export interface MediaManifestEntry {
  /**
   * Size of the asset in bytes.
   */
  byte_size: number;
  /**
   * Codec name, as the exporting server read it.
   *
   * Carried so an importing server can say what a package needs before a byte of it moves — a package built around H.264 on a machine that has no H.264 decoder is worth knowing about at import rather than at showtime.
   */
  codec: string;
  /**
   * Container format, as the exporting server read it.
   */
  container: string;
  /**
   * Pixel dimensions.
   */
  dimensions: SizePx;
  /**
   * Name to show in the library.
   */
  display_name: string;
  /**
   * Duration in milliseconds. `None` for a still.
   */
  duration_ms?: number | null;
  /**
   * Frame rate. `None` for a still.
   */
  frame_rate?: FrameRate | null;
  /**
   * Whether it carries an alpha channel.
   */
  has_alpha: boolean;
  /**
   * Video or image.
   */
  kind: MediaKind;
  /**
   * The item as the exporting server knew it.
   */
  media_item_id: MediaItemId;
  /**
   * The filename it was imported from, for the operator's orientation.
   */
  original_filename: string;
  /**
   * SHA-256 of the playable asset, lowercase hex.
   *
   * This is what identifies the content. An importing server that already holds these bytes needs none of them sent again.
   */
  sha256: string;
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
