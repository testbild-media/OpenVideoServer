// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Whether the installer was started.
 */
export interface InstallUpdateResponse {
  /**
   * Whether the machine started it. Its progress is in [`UpdateStatus`].
   */
  accepted: boolean;
  /**
   * What was run, or why nothing was.
   */
  detail: string;
}
