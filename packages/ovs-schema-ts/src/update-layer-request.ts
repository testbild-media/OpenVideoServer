// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * How a layer composites onto the bus below it.
 */
export type BlendMode = 'normal' | 'add' | 'multiply' | 'screen' | 'overlay' | 'lighten' | 'darken' | 'difference';
/**
 * Which bus a layer feeds.
 *
 * Exactly one of three, never a blend of them. `A` and `B` feed the global crossfader; `M` bypasses it and goes straight to the master path.
 */
export type Route = 'A' | 'B' | 'M';
/**
 * What a layer's playhead is doing.
 *
 * Four things an operator can mean, and they must not be conflated (owner decision Q16). Selecting **slot 0** puts the layer on Black, which is a source; these are about the source it already has.
 *
 * - [`Transport::Playing`] runs against the master clock, which is where every layer starts. - [`Transport::Paused`] freezes the playhead on the frame that is on screen. Going back to `Playing` carries on from exactly there, which is the one thing slot 0 cannot do: coming back from black restarts the clip. - [`Transport::Stopped`] stops and rewinds — the active slot's playhead goes back to its beginning and the first frame is what is shown.
 *
 * A held frame is a picture, so a paused or stopped layer is still on stage and still composited. Taking a layer off stage is slot 0, or [`LayerLive::enabled`].
 */
export type Transport = 'playing' | 'paused' | 'stopped';

/**
 * Changes part of one layer's live state.
 *
 * Optional for the same reason as [`UpdateMasterRequest`].
 */
export interface UpdateLayerRequest {
  /**
   * How this layer composites onto its bus.
   *
   * Live, not configuration, for the same reason the grid above it is: an operator changes it while looking at the wall, and the show keeps its own default for the next time it opens (owner 2026-09-23).
   */
  blend_mode?: BlendMode | null;
  /**
   * Whether the layer is in the composition at all.
   *
   * Live, beside the show's own [`Layer::enabled`], which is where it starts. Changing it here never writes into the show: the layer keeps its slots, its transform and its chain.
   *
   * [`Layer::enabled`]: ovs_types::config::Layer::enabled
   */
  enabled?: boolean | null;
  /**
   * Live opacity in `0.0..=1.0`.
   */
  opacity?: number | null;
  /**
   * Which bus this layer feeds.
   */
  route?: Route | null;
  /**
   * Live transform.
   */
  transform?: Transform2D | null;
  /**
   * What the playhead is doing: playing, paused or stopped.
   *
   * Separate from selecting slot 0, which is Black and a different thing (owner decision Q16). Pausing holds the frame that is on screen and resuming carries on from it; stopping rewinds to the start of the slot.
   */
  transport?: Transport | null;
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
