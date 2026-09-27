// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What a surface turned out to be encoded with.
 *
 * The answer to the last clause of owner decision Q13: a preview whose rate has been reduced must say so, because a picture arriving at a quarter speed and explaining nothing is indistinguishable from a fault — and the operator who concludes the show is stuttering when only the preview is has been misled by silence.
 *
 * Known only once the encoder is up, which is a fifth of a second after a watcher asks. Nothing waits for it.
 */
export interface StreamEncoding {
  /**
   * The encoder that was taken, by FFmpeg's name for it.
   *
   * A name rather than a category, because "libx264" is what a log says and what somebody diagnosing this will search for.
   */
  encoder: string;
  /**
   * Frames a second this surface is really encoded at.
   *
   * The show's own rate unless a software encoder has capped it. Compare with the rate the surface runs at to know whether anything was given up.
   */
  fps: number;
  /**
   * Whether that encoder is on the card.
   *
   * Decided where the encoder list is, rather than by a client matching on the name: a list that grows a VAAPI entry should not need every watcher to be taught about it.
   */
  hardware: boolean;
}
