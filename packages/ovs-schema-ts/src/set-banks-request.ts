// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Sets how many banks of twelve a layer's matrix shows.
 *
 * Its own request because an operator asks for a bank in order to put something in it, and a count worked out from the slots would collapse back to what is filled on the next redraw (owner, 2026-09-23).
 */
export interface SetBanksRequest {
  /**
   * How many banks to show, between one and `Layer::MAX_BANKS`.
   */
  banks: number;
}
