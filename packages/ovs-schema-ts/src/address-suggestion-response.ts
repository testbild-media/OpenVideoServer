// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Where a fixture fits.
 */
export interface AddressSuggestionResponse {
  /**
   * The lowest address it fits at, or `None` when there is no room.
   *
   * No room is a real answer: a universe has 512 channels and a show can want more than that.
   */
  start_address?: number | null;
  /**
   * The universe that was asked about.
   */
  universe: number;
}
