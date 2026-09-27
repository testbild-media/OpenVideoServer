// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * EffectInstanceId (ULID, 26 characters)
 */
export type EffectInstanceId = string;

/**
 * Result of adding or removing an effect.
 */
export interface AddEffectResponse {
  /**
   * The instance added, or the one removed.
   *
   * The identity is the server's: it lives in the same document as the fixture patch that may point at it, so a client that invented one would be naming something the stored show does not have.
   */
  effect_instance_id: EffectInstanceId;
  /**
   * When the server recorded the edit.
   */
  updated_at: number;
}
