// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The answer to a change on trial.
 */
export interface NetworkTrialAnswer {
  /**
   * True to keep the new settings, false to put the previous ones back now.
   */
  keep: boolean;
}
