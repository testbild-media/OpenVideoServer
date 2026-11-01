// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Hides a layer from the Live screen's matrix, or shows it again.
 *
 * **Administrators only, and the only way it changes** (owner, 2026-11-01: "Ein Admin sollte Layer verstecken können"). Only what the screen shows: the layer renders and answers its desk as before. A show sent whole carries the setting it already has, as it carries a lock.
 */
export interface SetLayerHiddenRequest {
  /**
   * Hidden, or not.
   */
  hidden: boolean;
}
