// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * How a layer composites onto the bus below it.
 */
export type BlendMode = 'normal' | 'add' | 'multiply' | 'screen' | 'overlay' | 'lighten' | 'darken' | 'difference';
/**
 * LayerId (ULID, 26 characters)
 */
export type LayerId = string;
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
 * The live state of one layer.
 */
export interface LayerLive {
  /**
   * The slot currently selected. `0` is the permanently black slot.
   */
  active_slot: number;
  /**
   * How this layer currently composites onto its bus.
   *
   * Live because it is something an operator changes while looking at the wall (owner, 2026-09-23), and configuration because a show has to open somewhere: [`Layer::blend_mode`] is where it starts, exactly as the grid and the opacity do.
   *
   * [`Layer::blend_mode`]: crate::config::Layer::blend_mode
   */
  blend_mode?: BlendMode & string;
  /**
   * Whether the layer is in the composition at all.
   *
   * Live, beside the configured [`Layer::enabled`] it starts from. A client or a console may switch a layer off during a show and that **never** writes back into the show: the layer keeps its slots, its transform and its chain, and comes back exactly as it was.
   *
   * Distinct from opacity at zero, which is a layer that is composited and contributes nothing: this one is not composited, so its decoder stops and its cost goes with it.
   */
  enabled?: boolean;
  /**
   * Which layer this state belongs to.
   */
  layer_id: LayerId;
  /**
   * Live opacity in `0.0..=1.0`, usually driven by the Layer Fixture.
   */
  opacity: number;
  /**
   * Which bus the layer currently feeds.
   *
   * A/B/M assignment is a live client-side function and is deliberately not part of the standard Layer Fixture.
   */
  route: Route;
  /**
   * Live transform, usually driven by the Layer Fixture.
   */
  transform: Transform2D;
  /**
   * What the playhead is doing.
   *
   * Live only. A show has no stopped layers in it — a show opens playing — so there is nothing here to restore and nothing to save.
   */
  transport?: Transport & string;
  /**
   * How many times a slot has been selected on this layer.
   *
   * The number itself means nothing; what means something is that it changes. Re-selecting the slot that is already running is a *retrigger* — an operator hitting the same pad to restart the clip — and without this there is nothing in the state that says it happened, because the slot index did not move. The renderer restarts a source when this changes, so a retrigger reaches the stage rather than being lost between the console and the wall.
   */
  trigger: number;
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
