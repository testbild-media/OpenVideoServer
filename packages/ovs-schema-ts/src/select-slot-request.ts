// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Selects a slot on a layer.
 */
export interface SelectSlotRequest {
  /**
   * The slot to select. `0` is the permanently black slot, which is always selectable on any layer without being configured anywhere.
   */
  slot_index: number;
}
