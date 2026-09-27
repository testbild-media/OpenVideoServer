// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Changes part of the live master state.
 *
 * Every field is optional and means "leave this alone". A console sending one fader must not have to send the others, and a client that sent a default for a field it was not thinking about would move something on stage.
 */
export interface UpdateMasterRequest {
  /**
   * Master BPM. There is exactly one of these in the whole system.
   */
  bpm?: number | null;
  /**
   * Crossfader position: `0.0` is fully A, `1.0` is fully B.
   */
  crossfader?: number | null;
  /**
   * Master dimmer in `0.0..=1.0`.
   */
  dimmer?: number | null;
  /**
   * Whether incoming DMX reaches this show at all.
   *
   * The operator taking the show off the console by hand. Not something a console can ask for: a desk that could switch itself off could not switch itself back on, and the whole point of the switch is that it is reached for when the desk is the problem.
   */
  dmx_enabled?: boolean | null;
  /**
   * Whether the final output is frozen.
   */
  freeze?: boolean | null;
  /**
   * Global speed scale.
   */
  speed_scale?: number | null;
}
