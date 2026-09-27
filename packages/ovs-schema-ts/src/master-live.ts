// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The live state of the master bus and the global clock.
 */
export interface MasterLive {
  /**
   * Current master BPM. There is exactly one of these in the whole system.
   */
  bpm: number;
  /**
   * Crossfader position: `0.0` is fully A, `1.0` is fully B.
   */
  crossfader: number;
  /**
   * Master dimmer in `0.0..=1.0`, applied after the buses are combined.
   */
  dimmer: number;
  /**
   * Whether incoming DMX reaches this show at all.
   *
   * On by default, because a show is normally driven from the desk and a server that came up ignoring it would be a server that looks broken.
   *
   * Off is the operator taking the show by hand: Art-Net and sACN keep arriving, the status keeps saying what is on the wire, and **nothing they carry changes anything**. The show holds whatever it had, which is the same behaviour as a desk that has stopped sending — loss is a hold, not a blackout — and Live Mode drives it instead (owner, 2026-09-23).
   *
   * Live rather than configuration: it is a thing somebody does in the middle of a night that has gone wrong, and a show that opened with its console ignored would be a worse surprise than the one it solved.
   */
  dmx_enabled?: boolean;
  /**
   * Whether the final output is frozen.
   *
   * Freezing holds the visible output but does not stop control or state processing, so the show resumes coherently.
   */
  freeze: boolean;
  /**
   * Global speed scale, roughly `0.5` to `4.0`.
   */
  speed_scale: number;
}
