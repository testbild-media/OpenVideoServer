// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What a tap did to the tempo.
 */
export interface TapTempoResponse {
  /**
   * The master BPM after this tap.
   */
  bpm: number;
  /**
   * Whether this tap changed it.
   *
   * The first tap of a run never does: one tap is not an interval. A client showing "tapping…" until this turns true is showing the truth.
   */
  changed: boolean;
}
