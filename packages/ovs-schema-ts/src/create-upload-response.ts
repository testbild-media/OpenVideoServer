// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The server's answer to an upload request.
 */
export type CreateUploadResponse =
  | {
      /**
       * The item that already holds it.
       */
      media_item_id: MediaItemId;
      result: 'already_present';
    }
  | {
      /**
       * Total bytes expected.
       */
      byte_size: number;
      /**
       * Chunk size the server prefers, in bytes.
       */
      chunk_size: number;
      /**
       * The item the content will belong to once it is verified.
       */
      media_item_id: MediaItemId;
      /**
       * Bytes already received, contiguous from the start.
       *
       * A client resumes by sending from this offset. Contiguous rather than a set of ranges because a single offset is all a resumable PUT needs, and a range set would invite out-of-order writes that complicate verification for no operator-visible benefit.
       */
      received_bytes: number;
      result: 'accepted';
      /**
       * Identifies this upload.
       */
      upload_id: string;
    };
/**
 * MediaItemId (ULID, 26 characters)
 */
export type MediaItemId = string;
