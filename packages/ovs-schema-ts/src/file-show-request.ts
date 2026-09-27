// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Files one show in a folder. The document is untouched.
 */
export interface FileShowRequest {
  /**
   * Where it is to be filed. Empty is the root.
   */
  folder: string;
}
