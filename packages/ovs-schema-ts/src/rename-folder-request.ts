// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Renames a folder, which moves everything filed under it.
 */
export interface RenameFolderRequest {
  /**
   * The path as it is now.
   */
  from: string;
  /**
   * The path it is to have. Moving a folder inside itself is refused.
   */
  to: string;
}
