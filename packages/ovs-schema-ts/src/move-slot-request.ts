// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Moves a slot to another pad of the same layer, with everything it carries.
 *
 * A pad dragged onto another on the live matrix (owner, 2026-10-21: "Slot 1 per Drag and Drop auf Slot 10 schieben"). A pad that is taken swaps with the one that moves, rather than being overwritten.
 */
export interface MoveSlotRequest {
  /**
   * The pad it goes to, between 1 and `Layer::MAX_SLOT_INDEX`.
   */
  to: number;
}
