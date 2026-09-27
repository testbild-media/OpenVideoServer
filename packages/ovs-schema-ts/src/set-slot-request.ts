// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The direction a source's timeline advances in.
 */
export type PlaybackDirection = 'forward' | 'reverse';
/**
 * What happens when a source reaches the end of its timeline.
 */
export type PlaybackMode = 'loop' | 'once_freeze' | 'once_black' | 'bounce';
/**
 * Whether a slot runs at its own rate or to the show's beat.
 *
 * Free run is the default and stays the default: a clip dropped into a slot plays at the speed it was made at, whatever the tempo is doing, until somebody decides otherwise (owner, 2026-09-23). That is what an operator expects of a file they just imported, and it is the only setting under which a clip is certain to be playable — a beat-locked clip runs at whatever multiple of its own rate the tempo asks for, and a decoder has a ceiling.
 */
export type SlotSync =
  | {
      kind: 'FREE_RUN';
    }
  | {
      /**
       * How many beats one pass takes. At least one.
       */
      beats: number;
      kind: 'BEAT_SYNC';
    };

/**
 * Changes one slot's settings.
 *
 * Every field is optional and an absent one means "leave this alone", so the control an operator moved is the only thing that travels. It is a *document* edit made with a show running, like putting a source in a slot: storing the whole show would reload the show, and deciding that a clip should run to the beat, or nudging where it sits, is something done while watching it.
 *
 * What it does not carry is the source. A slot's content is changed by dragging something onto the pad, which is its own endpoint.
 */
export interface SetSlotRequest {
  /**
   * Which way the timeline runs.
   */
  direction?: PlaybackDirection | null;
  /**
   * What happens when the clip reaches the end.
   */
  playback_mode?: PlaybackMode | null;
  /**
   * How fast it runs through one pass, as a ratio.
   *
   * Both kinds of sync have one (owner, 2026-09-23): "pro Slot Scale sollt aber verfügbar sein für Free Run und BPM Sync." A free-running slot has no other rate — the master's scale deliberately does not reach it — and a beat-locked one is scaled on top of the beats it was given.
   */
  speed?: number | null;
  /**
   * Whether it runs at its own rate or to the show's beat, and over how many.
   */
  sync?: SlotSync | null;
  /**
   * Where this source sits, before the layer's own transform.
   *
   * The slot's, not the layer's: two clips in one layer are rarely framed the same way, and this is where an operator puts one of them right without moving the other (owner, 2026-09-23).
   */
  transform?: Transform2D | null;
}
/**
 * A 2D transform applied to a slot source or to a whole layer.
 *
 * `x` and `y` are an offset in composition pixels from the centre of the composition, chosen over normalised units because LED operators work in pixels and because a non-square composition makes normalised offsets behave differently on each axis.
 *
 * The layer transform is intended for live Art-Net/sACN control; the slot transform is configuration only. Both use this type, but they live in different places in the model.
 */
export interface Transform2D {
  /**
   * Clockwise rotation in degrees about the transform anchor.
   */
  rotation_deg: number;
  /**
   * Horizontal scale multiplier, `1.0` meaning unscaled.
   */
  scale_x: number;
  /**
   * Vertical scale multiplier, `1.0` meaning unscaled.
   */
  scale_y: number;
  /**
   * Horizontal offset from the composition centre, in pixels.
   */
  x: number;
  /**
   * Vertical offset from the composition centre, in pixels.
   */
  y: number;
}
