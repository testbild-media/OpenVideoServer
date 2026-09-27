// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * MediaAssetId (ULID, 26 characters)
 */
export type MediaAssetId = string;
/**
 * MediaItemId (ULID, 26 characters)
 */
export type MediaItemId = string;
/**
 * Whether a physical asset has been verified against its recorded hash.
 */
export type AssetValidation =
  | {
      state: 'pending';
    }
  | {
      /**
       * When verification succeeded.
       */
      at: number;
      state: 'verified';
    }
  | {
      /**
       * Why it failed, for the diagnostics view.
       */
      reason: string;
      state: 'failed';
    };

/**
 * One physical representation of a media item.
 *
 * A media item can have several assets, and in practice has one: the file that was uploaded for it. Nothing rewrites an asset, so a show keeps playing exactly the bytes it was built with.
 */
export interface MediaAsset {
  /**
   * Size on disk in bytes.
   */
  byte_size: number;
  /**
   * Codec name, as the server read it out of the file.
   *
   * `h264`, `hevc`, `av1`, `prores`, `hap`, `png`. This is what the file holds, not what an upload claimed it held: the server probes every video it accepts, and a declaration that disagrees with the file loses.
   */
  codec: string;
  /**
   * Container format, as the server read it out of the file.
   *
   * FFmpeg's own name for the demuxer, which is a list where one demuxer answers to several: `mov,mp4,m4a,3gp,3g2,mj2`.
   */
  container: string;
  /**
   * When the asset was registered.
   */
  created_at: number;
  /**
   * Stable identifier.
   */
  id: MediaAssetId;
  /**
   * The logical item this asset represents.
   */
  media_item_id: MediaItemId;
  /**
   * Content-addressed storage key, for example `ab/abcdef....mov`.
   *
   * Server-internal. Never sent to the client, never shown to an operator and never stored in a show.
   */
  object_key: string;
  /**
   * Lowercase hex SHA-256 of the file, used for verification and deduplication.
   */
  sha256: string;
  /**
   * Verification state.
   */
  validation: AssetValidation;
}
