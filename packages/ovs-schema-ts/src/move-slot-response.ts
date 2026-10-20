// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Result of moving a slot.
 */
export interface MoveSlotResponse {
  /**
   * The pad it went to had a slot of its own, which is now where this one was.
   */
  swapped: boolean;
  /**
   * When the server recorded the edit.
   */
  updated_at: number;
}
