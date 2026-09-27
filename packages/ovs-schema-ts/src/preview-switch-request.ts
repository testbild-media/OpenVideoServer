// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Whether the renderer should produce previews.
 */
export interface PreviewSwitchRequest {
  /**
   * True to produce them, false to stop.
   */
  enabled: boolean;
}
