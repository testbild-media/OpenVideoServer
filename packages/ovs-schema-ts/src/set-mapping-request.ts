// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * OutputId (ULID, 26 characters)
 */
export type OutputId = string;
/**
 * PieceId (ULID, 26 characters)
 */
export type PieceId = string;
/**
 * Which picture a piece takes its rectangle out of.
 *
 * The composition is the show as it leaves the server, and is what every piece took until the owner asked for a choice (2026-09-23). The three buses are what it is made of: A and B are the two sides of the crossfade, and M is the master path that goes over them.
 *
 * A wall fed from one bus is a monitor — it shows the look coming up rather than the look going out, which is the thing an operator is judging a crossfade against and cannot see on the wall itself.
 *
 * **One layer's own picture is not here yet.** A layer composites straight into its bus, so there is no image of one on its own to sample, and giving this a variant the renderer cannot honour would be a control that does nothing.
 */
export type PieceSource =
  | {
      kind: 'composition';
    }
  | {
      kind: 'bus';
      /**
       * Which one: A and B are the crossfade's sides, M is the master path.
       */
      route: Route;
    };
/**
 * Which bus a layer feeds.
 *
 * Exactly one of three, never a blend of them. `A` and `B` feed the global crossfader; `M` bypasses it and goes straight to the master path.
 */
export type Route = 'A' | 'B' | 'M';

/**
 * Replaces a show's mapping: its outputs and the pieces that draw to them.
 *
 * The two together, because they are one thing and refer to each other: a piece names the output it lands on, and sending the pieces alone would mean a moment in which one names an output that is not there yet.
 *
 * Whole rather than one piece at a time, because a mapping is dragged as a group — several pieces move together and must land together, in one frame rather than four (owner, 2026-09-23).
 *
 * It is a *document* edit made with a show running, like putting a source in a slot: storing the whole show would reload the show, and lining a wall up is the one job that has to be done while looking at it.
 */
export interface SetMappingRequest {
  /**
   * Every output this show drives.
   */
  outputs: OutputConfig[];
  /**
   * Every piece, in the order the document lists them.
   */
  pieces: Piece[];
}
/**
 * A configured output.
 */
export interface OutputConfig {
  /**
   * Whether this output is driven at all.
   */
  enabled: boolean;
  /**
   * Stable logical identifier, persisted and referenced by pieces.
   */
  id: OutputId;
  /**
   * Hardware identity used to find this output again after a reboot.
   *
   * Empty on an output that goes on the network, and refused as non-empty there: an output cannot be a socket and a stream at once.
   */
  identity: OutputIdentity;
  /**
   * The mode this output is configured to run at.
   *
   * On a network output this is what the stream declares itself as rather than a mode set on a panel: the size the composition is sampled into and the rate the frames are stamped with.
   */
  mode: DisplayMode;
  /**
   * Operator-visible name, for example `LED Wall Left`.
   */
  name: string;
  /**
   * Published on the network instead of driven out of a socket.
   *
   * The stream carries this output's own [`name`](Self::name), so there is one name and not two. **What the operator typed is the part in brackets**: the runtime puts the machine in front of it, so `Wall Left` on a server called `OPENVIDEOSERVER` is announced as `OPENVIDEOSERVER (Wall Left)` — measured on the reference machine rather than assumed. Storing the announced name would rename every stream in a show the day it moved to another machine.
   *
   * A flag rather than a second name, because a second name would have to be kept in step with the first and the rule about which one wins is where the fault would live.
   *
   * Not decided once and for all: an output can be moved between a socket and the network either way, which is what happens when a wall is unplugged for an evening and its feed has to go somewhere.
   */
  on_the_network?: boolean;
}
/**
 * Which connector an output is forced onto, and what was found there.
 *
 * **The GPU and the connector decide; nothing else does.** An output is wired to a socket on the back of a machine and stays there, so a show names that socket and the renderer drives it — whatever is plugged in, and whether or not the driver thinks anything is. Outputs are not hotplugged.
 *
 * The EDID fields are what was discovered there, kept for the patch page and for support: "DP-3 says it is a Novastar" is worth being able to read. They are never matched against, because the hardware this product drives cannot be relied on to identify itself — LED processors commonly report no EDID at all, or the same serial on every unit in the rack — and because a processor that is powered off after the server boots would otherwise take its wall dark for the night.
 */
export interface OutputIdentity {
  /**
   * Connector name as reported by the driver, for example `DP-1`.
   */
  connector?: string | null;
  /**
   * Hash of the EDID identification block.
   */
  edid_hash?: string | null;
  /**
   * PCI address of the GPU, for example `0000:01:00.0`.
   */
  gpu_pci_address?: string | null;
  /**
   * EDID manufacturer name, when reported.
   */
  manufacturer?: string | null;
  /**
   * EDID model name, when reported.
   */
  model?: string | null;
  /**
   * EDID serial number, when reported and non-empty.
   */
  serial?: string | null;
}
/**
 * A display mode: resolution plus exact refresh rate.
 */
export interface DisplayMode {
  /**
   * Refresh rate as an exact rational value.
   */
  refresh: FrameRate;
  /**
   * Resolution in pixels.
   */
  size: SizePx;
}
/**
 * An exact frame rate expressed as a rational number of frames per second.
 *
 * Rational rather than floating point so that 60000/1001 stays exact and the scheduler cannot accumulate drift.
 */
export interface FrameRate {
  /**
   * Denominator, for example `1001`.
   */
  denominator: number;
  /**
   * Numerator, for example `60000`.
   */
  numerator: number;
}
/**
 * A size in whole pixels.
 */
export interface SizePx {
  /**
   * Height in pixels.
   */
  height: number;
  /**
   * Width in pixels.
   */
  width: number;
}
/**
 * One rectangular mapping from the composition to an output.
 */
export interface Piece {
  /**
   * The point in `dest_rect` the rotation happens about.
   *
   * The centre unless somebody says otherwise, which is what every document written before this field existed reads as — so nothing already mapped moves.
   */
  anchor?: AnchorPoint;
  /**
   * Region of the output this piece fills, in output pixels.
   *
   * Scaling is the relationship between `source_rect` and this rectangle, so there is no separate scale factor that could disagree with the geometry.
   */
  dest_rect: RectPx;
  /**
   * Whether this piece is drawn.
   */
  enabled: boolean;
  /**
   * Stable identifier.
   */
  id: PieceId;
  /**
   * Operator-visible name, for example `Wall Left Upper`.
   */
  name: string;
  /**
   * Draw order within its output; lower values draw first.
   */
  order: number;
  /**
   * Which output this piece draws to.
   */
  output_id: OutputId;
  /**
   * Clockwise rotation of the *placed* rectangle, in degrees.
   *
   * About [`anchor`](Self::anchor), in output pixels.
   */
  rotation_deg: number;
  /**
   * Which picture this piece takes its rectangle out of.
   *
   * The composition unless somebody says otherwise, which is what every document written before this field existed reads as — so nothing already mapped changes when it is loaded.
   *
   * The owner asked for a choice here (2026-09-23): a wall showing the incoming look rather than the outgoing one is a monitor, and a wall showing one bus is what a crossfade is judged against.
   */
  source?: PieceSource;
  /**
   * The point in `source_rect` that rotation happens about.
   *
   * The centre unless somebody says otherwise, for the same reason [`anchor`](Self::anchor) is.
   */
  source_anchor?: AnchorPoint;
  /**
   * Region of the composition this piece takes, in composition pixels.
   */
  source_rect: RectPx;
  /**
   * Clockwise rotation of the *taken* region, in degrees.
   *
   * About [`source_anchor`](Self::source_anchor), and in composition pixels rather than in texture coordinates — a forty-five degree turn of a 16:9 region is a turn and not a shear.
   *
   * Both ends of a piece turn (owner, 2026-09-23): the output rotation puts a rectangle on a wall at an angle, and this one takes a rectangle out of the composition at an angle, which is what a wall hung on the diagonal needs fed to it.
   *
   * Zero unless somebody says otherwise, which is what every document written before this field existed reads as.
   */
  source_rotation_deg?: number;
}
/**
 * Where in a rectangle a turn happens.
 *
 * A fraction of the rectangle's own size, measured from its top-left corner: `(0.0, 0.0)` is that corner, `(0.5, 0.5)` the centre, `(1.0, 1.0)` the bottom-right. Fractions rather than pixels so that resizing the rectangle does not move the point the operator picked.
 *
 * The owner asked for an anchor point per piece (2026-09-23). Rotation used to be about the centre and nothing else, which is right for a wall hung square and wrong for one hinged at a corner: turning it about its centre moves the hinge, and the operator then has to put the rectangle back by hand.
 *
 * The centre is the default, and the default is what every document written before this field existed reads as — so nothing already mapped moves.
 */
export interface AnchorPoint {
  /**
   * Across, as a fraction of the width from the left edge.
   */
  x: number;
  /**
   * Down, as a fraction of the height from the top edge.
   */
  y: number;
}
/**
 * An axis-aligned rectangle in whole pixels.
 *
 * The origin is the top-left corner of the surrounding surface, with `x` growing right and `y` growing down. Offsets are signed so a piece may be positioned partly outside its output, which is a legitimate mapping choice.
 */
export interface RectPx {
  /**
   * Height in pixels.
   */
  height: number;
  /**
   * Width in pixels.
   */
  width: number;
  /**
   * Left edge in pixels, may be negative.
   */
  x: number;
  /**
   * Top edge in pixels, may be negative.
   */
  y: number;
}
