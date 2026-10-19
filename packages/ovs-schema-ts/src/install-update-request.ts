// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Which release to install.
 */
export interface InstallUpdateRequest {
  /**
   * The version, such as `0.2.46`. Only a newer one than is installed.
   */
  version: string;
}
