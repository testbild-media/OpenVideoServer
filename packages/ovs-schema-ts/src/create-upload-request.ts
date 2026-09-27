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
 * Asks the server to accept an upload.
 *
 * The client has already probed, prepared and hashed the file, so the server can answer "I already have that" before a single byte crosses the network.
 */
export interface CreateUploadRequest {
  /**
   * Size of the prepared file in bytes.
   */
  byte_size: number;
  /**
   * Codec name the client read.
   *
   * Believed for nothing. It is here so a client that probed the file can say what it thought, and so a disagreement between the two readings is visible in the log rather than silent.
   */
  codec?: string;
  /**
   * Container format the client read.
   *
   * Believed only as far as naming the stored object. What the server records is what its own probe found.
   */
  container?: string;
  /**
   * Pixel dimensions the client measured.
   *
   * The client has already opened the file to prepare it, so it knows. The server checks what it can — for a still it decodes the file and compares — rather than taking this on trust, because a wrong size here is a wrong size in the library and in every show built on it.
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
   * Where in the library the new item is filed.
   *
   * Slash-separated, empty for the root. What the operator had open when they dropped the files: a clip imported while standing in a folder belongs in that folder, and appearing in All media instead is a move somebody then has to make by hand (owner, 2026-09-23).
   *
   * Made, with every folder above it, if it is not there yet — an upload is not the place to lose somebody's work over a folder deleted in another window. Ignored when `media_item_id` names an item that already exists, because that item is already filed somewhere.
   */
  folder?: string;
  /**
   * Frame rate. `None` for a still.
   */
  frame_rate?: FrameRate | null;
  /**
   * Whether the file carries an alpha channel.
   */
  has_alpha?: boolean;
  /**
   * Whether these bytes are a moving or a still image.
   *
   * Declared rather than inferred, because it decides which checks the server runs: a still is decoded and measured, a video is probed. A declaration that disagrees with the file is refused.
   */
  kind: MediaKind;
  /**
   * The item these bytes belong to, when there already is one.
   *
   * Set after importing a package: the item exists, empty, waiting for its content, and the show already refers to it. Left out for an ordinary import, where the server creates the item.
   */
  media_item_id?: MediaItemId | null;
  /**
   * The file the operator picked, kept for their orientation only.
   */
  original_filename: string;
  /**
   * SHA-256 of the prepared file, lowercase hex.
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
