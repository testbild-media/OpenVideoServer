// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * EffectInstanceId (ULID, 26 characters)
 */
export type EffectInstanceId = string;

/**
 * Sets the order of a layer's or a slot's chain.
 *
 * **The whole order, not a move.** A move is a pair of positions a client works out from a list it may already be behind on, and two clients moving different effects at once would each apply theirs to the other's result. An order is one statement of what the chain is: it either matches what is there or it is refused, and sending it twice changes nothing.
 *
 * Every instance currently in the chain must appear exactly once. That is what stops this from being a way to remove one by leaving it out — removing is `DELETE`, where it says so.
 */
export interface OrderEffectsRequest {
  /**
   * Every instance in the chain, in the order it is to run.
   */
  order: EffectInstanceId[];
}
