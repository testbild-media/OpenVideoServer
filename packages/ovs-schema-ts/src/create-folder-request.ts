// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Makes a folder in the library.
 *
 * Every folder above it that is not there yet is made with it, because a tree with a hole in it cannot be drawn. Making one that already exists changes nothing and is not an error.
 */
export interface CreateFolderRequest {
  /**
   * The whole path, slash-separated, for example `Intros/2026`.
   */
  path: string;
}
