// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Whether a change was started.
 */
export interface NetworkChangeResponse {
  /**
   * Whether the machine started it. How far it got is in [`NetworkStatus`].
   */
  accepted: boolean;
  /**
   * What was asked for, or why nothing was.
   */
  detail: string;
}
