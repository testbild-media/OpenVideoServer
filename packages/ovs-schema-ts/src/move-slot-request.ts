// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * LayerId (ULID, 26 characters)
 */
export type LayerId = string;

/**
 * Moves a slot to another pad, of the same layer or another, with everything it carries.
 *
 * A pad dragged onto another on the live matrix (owner, 2026-10-21: "Slot 1 per Drag and Drop auf Slot 10 schieben", and then "von einem Layer auf einen anderen Layer"). A pad that is taken swaps with the one that moves, rather than being overwritten.
 */
export interface MoveSlotRequest {
  /**
   * The pad it goes to, between 1 and `Layer::MAX_SLOT_INDEX`.
   */
  to: number;
  /**
   * The layer it goes to. Absent is the layer it is on.
   */
  to_layer?: LayerId | null;
}
