// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Result of locking or unlocking.
 */
export interface SetLockResponse {
  /**
   * When the server recorded the edit.
   */
  updated_at: number;
}
