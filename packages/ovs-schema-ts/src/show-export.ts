// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The kind of thing a media item is.
 */
export type MediaKind = 'video' | 'image';
/**
 * MediaItemId (ULID, 26 characters)
 */
export type MediaItemId = string;
/**
 * FixtureInstanceId (ULID, 26 characters)
 */
export type FixtureInstanceId = string;
/**
 * What a patched fixture controls.
 */
export type FixtureTarget =
  | {
      kind: 'master';
    }
  | {
      kind: 'layer';
      /**
       * The layer this fixture drives.
       */
      layer_id: LayerId;
    }
  | {
      /**
       * The instance this fixture drives.
       */
      effect_instance_id: EffectInstanceId;
      kind: 'effect';
    }
  | {
      /**
       * The instance this fixture drives.
       */
      generator_instance_id: GeneratorInstanceId;
      kind: 'generator';
    };
/**
 * LayerId (ULID, 26 characters)
 */
export type LayerId = string;
/**
 * EffectInstanceId (ULID, 26 characters)
 */
export type EffectInstanceId = string;
/**
 * GeneratorInstanceId (ULID, 26 characters)
 */
export type GeneratorInstanceId = string;
/**
 * ShowId (ULID, 26 characters)
 */
export type ShowId = string;
/**
 * How a layer composites onto the bus below it.
 */
export type BlendMode = 'normal' | 'add' | 'multiply' | 'screen' | 'overlay' | 'lighten' | 'darken' | 'difference';
/**
 * A typed parameter value for an effect or generator instance.
 */
export type ParameterValue =
  | {
      type: 'bool';
      value: boolean;
    }
  | {
      type: 'int';
      value: number;
    }
  | {
      type: 'float';
      value: number;
    }
  | {
      type: 'text';
      value: string;
    }
  | {
      type: 'color';
      /**
       * @minItems 4
       * @maxItems 4
       */
      value: [number, number, number, number];
    }
  | {
      type: 'choice';
      value: string;
    };
/**
 * Which bus a layer feeds.
 *
 * Exactly one of three, never a blend of them. `A` and `B` feed the global crossfader; `M` bypasses it and goes straight to the master path.
 */
export type Route = 'A' | 'B' | 'M';
/**
 * The direction a source's timeline advances in.
 */
export type PlaybackDirection = 'forward' | 'reverse';
/**
 * What happens when a source reaches the end of its timeline.
 */
export type PlaybackMode = 'loop' | 'once_freeze' | 'once_black' | 'bounce';
/**
 * What a slot plays.
 *
 * New source types are added as variants here (capture, DeckLink, NDI and HTML as expected future types). Everything downstream of a source treats them identically, so adding one must not require changes to the layer, composition, piece or output model.
 */
export type SourceRef =
  | {
      kind: 'media';
      /**
       * Logical media identity.
       */
      media_item_id: MediaItemId;
    }
  | {
      /**
       * This instance's own identity.
       *
       * What a patched fixture points at and what a live value is keyed by, the same way an effect instance's is.
       *
       * Optional on the wire, so that a client need not invent one and a show written before generators had one still opens. The server gives it a real identity **when the show is saved**, because a patch entry points at this instance and lives in the same document: an identity that existed only in the loaded state would be minted again on the next open, and every fixture patched against the previous one would be pointing at a generator that no longer exists. A document already on disk without one gains it when it is opened (`Show::assign_missing_ids`).
       *
       * Nil rather than a fresh one as the `serde` default, because a default is written into the published schema and a schema that changes on every build can never be checked against a committed one — and left out of the wire form entirely when it is nil, so that "no identity" never reads as "the identity of twenty-six zeroes".
       */
      instance_id?: GeneratorInstanceId;
      kind: 'generator';
      /**
       * Generator parameters as defined by its manifest.
       */
      parameters?: {
        [k: string]: ParameterValue;
      };
      /**
       * Stable generator type identifier, for example `ovs.solid_colour`.
       */
      type_id: string;
      /**
       * Manifest version of that generator.
       */
      version: number;
    }
  | {
      kind: 'ndi';
      /**
       * The full NDI source name, as the network announces it.
       *
       * `MACHINE (Source Name)` is the shape NDI uses, and it is carried whole rather than split: the two halves are one identifier and a client that took them apart would have to put them back together exactly to find the source again.
       */
      name: string;
    };
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
 * How the A/B crossfader converts its position into bus contributions.
 */
export type CrossfaderCurve = 'linear' | 'equal_power';
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
 * The most a clip may be prepared at before it is sent to this show.
 *
 * **A property of the wall, not of the laptop that happens to be uploading.** What reaches a panel is never the clip: it is the composition, cut into pieces. On the owner's own wall the largest piece is 840x280, taken from a 1366x444 region of a 1080p composition — so a 720-line clip already puts more than one source pixel behind every pixel out there, while a 2160-line one carries 8.3 megapixels across the bus to show 0.235 of them.
 *
 * That waste is paid every frame, for ever: a decoded frame is copied into a staging buffer and then onto the card, and the copy at 2160 is four times the copy at 1080 — about 8.7 ms a layer at 1080p, measured 2026-10-14, on a render thread that has 16.7 ms for everything. One house wants 1080 and another is a wall of small panels where 720 is plenty, so the number lives with the show rather than being guessed per file.
 *
 * A ceiling, never a target: nothing is ever made bigger, and a clip that is already under it is left exactly as it is.
 */
export type UploadCeiling = 'SOURCE' | '2160P' | '1080P' | '720P';

/**
 * A show and the inventory of what it needs to play.
 *
 * The media itself is not in here. A package is a document an operator can read and a server can validate before a byte of video moves; the files come afterwards, through the same upload path as any other import, and only the ones the receiving server does not already hold.
 */
export interface ShowExport {
  /**
   * When it was written.
   */
  exported_at: number;
  /**
   * Version of the server that wrote it, for diagnostics.
   */
  exported_by: string;
  /**
   * Every media item the show refers to.
   */
  media: MediaManifestEntry[];
  /**
   * Format version of this package.
   */
  package_version: number;
  /**
   * The show itself.
   */
  show: Show;
}
/**
 * One media item a package needs, and everything required to ask for it.
 *
 * No physical path and no storage key: those belong to the server that holds the file, and a package moves between servers.
 */
export interface MediaManifestEntry {
  /**
   * Size of the asset in bytes.
   */
  byte_size: number;
  /**
   * Codec name, as the exporting server read it.
   *
   * Carried so an importing server can say what a package needs before a byte of it moves — a package built around H.264 on a machine that has no H.264 decoder is worth knowing about at import rather than at showtime.
   */
  codec: string;
  /**
   * Container format, as the exporting server read it.
   */
  container: string;
  /**
   * Pixel dimensions.
   */
  dimensions: SizePx;
  /**
   * Name to show in the library.
   */
  display_name: string;
  /**
   * Duration in milliseconds. `None` for a still.
   */
  duration_ms?: number | null;
  /**
   * Frame rate. `None` for a still.
   */
  frame_rate?: FrameRate | null;
  /**
   * Whether it carries an alpha channel.
   */
  has_alpha: boolean;
  /**
   * Video or image.
   */
  kind: MediaKind;
  /**
   * The item as the exporting server knew it.
   */
  media_item_id: MediaItemId;
  /**
   * The filename it was imported from, for the operator's orientation.
   */
  original_filename: string;
  /**
   * SHA-256 of the playable asset, lowercase hex.
   *
   * This is what identifies the content. An importing server that already holds these bytes needs none of them sent again.
   */
  sha256: string;
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
 * A complete show.
 */
export interface Show {
  /**
   * The composition canvas.
   */
  composition: Composition;
  /**
   * When the show was created.
   */
  created_at: number;
  /**
   * DMX-over-IP patch.
   */
  fixture_patch?: FixtureInstance[];
  /**
   * Stable identifier.
   */
  id: ShowId;
  /**
   * Compositing layers.
   */
  layers: Layer[];
  /**
   * Master settings this show starts from.
   */
  master: MasterConfig;
  /**
   * Operator-visible name.
   */
  name: string;
  /**
   * Configured physical outputs.
   */
  outputs: OutputConfig[];
  /**
   * Rectangular mappings from composition to outputs.
   */
  pieces: Piece[];
  /**
   * Schema version this show was written with.
   *
   * Always present and always checked on load, so a show written by a newer build is refused with an explanation rather than partially understood.
   */
  schema_version: number;
  /**
   * Show preferences.
   */
  settings: ShowSettings;
  /**
   * When the show was last written.
   */
  updated_at: number;
}
/**
 * The canvas everything composites onto before being cut into pieces.
 */
export interface Composition {
  /**
   * The rate the composition is rendered at.
   *
   * Outputs carry their own refresh rate. When they differ, this is the rate the compositor runs at and outputs present from.
   */
  render_rate: FrameRate;
  /**
   * Canvas size in pixels.
   */
  size: SizePx;
}
/**
 * A fixture definition bound to a target, universe and start address.
 */
export interface FixtureInstance {
  /**
   * Whether this patch entry is active.
   */
  enabled: boolean;
  /**
   * Number of channels this fixture occupies.
   */
  footprint: number;
  /**
   * Stable identifier of this patch entry.
   */
  id: FixtureInstanceId;
  /**
   * One-based DMX start address within the universe.
   */
  start_address: number;
  /**
   * What this fixture controls.
   */
  target: FixtureTarget;
  /**
   * Stable fixture type identifier, for example `ovs.master`.
   */
  type_id: string;
  /**
   * Universe number.
   */
  universe: number;
  /**
   * Definition version this patch was created against.
   *
   * Published fixture definitions are immutable, because a console has the channel layout physically programmed into its show file.
   */
  version: number;
}
/**
 * One compositing layer of a show.
 */
export interface Layer {
  /**
   * How many banks of twelve the matrix shows for this layer.
   *
   * Stored rather than worked out from the slots, because an operator adds a bank in order to put something in it: one derived from the slots would collapse back to what is filled the moment the screen redrew, and the empty bank they had just asked for would vanish (owner, 2026-09-23).
   *
   * One by default, which is the owner's default of 1–12 alone, and at most [`Layer::MAX_BANKS`]. A document written before this field existed reads as one bank and still shows every slot it has — see [`Layer::bank_count`].
   */
  banks?: number;
  /**
   * How this layer composites onto the bus.
   */
  blend_mode: BlendMode;
  /**
   * Opacity the layer starts from, in `0.0..=1.0`.
   *
   * Live opacity is DMX-driven and lives in [`crate::live`].
   */
  default_opacity: number;
  /**
   * Transform the layer starts from.
   *
   * Live transform is DMX-driven and lives in [`crate::live`].
   */
  default_transform: Transform2D;
  /**
   * Effects applied to the whole layer, in chain order.
   */
  effects: EffectInstance[];
  /**
   * Whether the layer contributes to its bus at all.
   */
  enabled: boolean;
  /**
   * Stable identifier.
   */
  id: LayerId;
  /**
   * Operator-visible name.
   */
  name: string;
  /**
   * Composite order; lower values composite first, so higher values are on top.
   */
  order: number;
  /**
   * Which bus this layer feeds.
   */
  route: Route;
  /**
   * Configured slots, never including index 0.
   *
   * Slot 0 is permanently Black and is implicit on every layer . It is not stored, cannot be assigned and needs no decoder.
   */
  slots: Slot[];
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
/**
 * One configured effect in a slot or layer effect chain.
 *
 * Effects deliberately have no generic "effect master". Each effect exposes its own parameters, and may optionally carry its own DMX fixture personality defined by its manifest.
 */
export interface EffectInstance {
  /**
   * Whether this effect contributes to the chain.
   */
  enabled: boolean;
  /**
   * Stable identifier of this instance.
   */
  id: EffectInstanceId;
  /**
   * Parameter values as declared by the manifest.
   */
  parameters?: {
    [k: string]: ParameterValue;
  };
  /**
   * Stable effect type identifier, for example `ovs.colour_correct`.
   */
  type_id: string;
  /**
   * Manifest version this instance was configured against.
   *
   * Published manifests are immutable, so an old show keeps loading the exact effect behaviour it was built with.
   */
  version: number;
}
/**
 * One configured slot on a layer.
 */
export interface Slot {
  /**
   * Timeline direction.
   */
  direction: PlaybackDirection;
  /**
   * Effects applied to this source, in chain order.
   */
  effects: EffectInstance[];
  /**
   * Slot number as shown in the Live Mode matrix. Never 0.
   */
  index: number;
  /**
   * What an operator calls this pad, where they have called it anything.
   *
   * Absent is the ordinary case and means the pad is named after what it plays: the clip's name in the library, or the generator's. A name here replaces that on the pad and nowhere else — the library keeps its own names, and the same clip in another slot is unaffected.
   *
   * Why it exists (owner, 2026-10-15): "Manchmal kann ja der selbe Inhalt sein, aber mit anderen Effekte drauf etc. für Dauerhafte installation wäre das noch gut." Two pads holding one clip with different effects on them read alike otherwise, and which is which is then remembered rather than read — once a week, in a venue, by whoever is standing there.
   */
  name?: string | null;
  /**
   * End-of-timeline behaviour.
   */
  playback_mode: PlaybackMode;
  /**
   * What this slot plays.
   */
  source: SourceRef;
  /**
   * How fast this slot runs through one pass, as a ratio.
   *
   * `1.0` is a pass exactly as long as [`sync`](Self::sync) says: the clip's own length when it free-runs, or the beats it was given when it is locked. `2.0` is twice as fast, `0.5` half.
   *
   * On **both** kinds of sync, and on the slot (owner, 2026-09-23): "pro Slot Scale sollt aber verfügbar sein für Free Run und BPM Sync." It is not the master's scale wearing a different hat — the master's does not reach a free-running clip at all, and must not, "weil 'Free' ist". This is the operator's hand on this one clip, and it is the only rate a free-running slot has.
   *
   * Between [`MIN_SPEED_SCALE`](crate::config::MIN_SPEED_SCALE) and [`MAX_SPEED_SCALE`](crate::config::MAX_SPEED_SCALE), the same ladder the master runs on, so an operator meets one range rather than two.
   *
   * What it cannot do is make a decoder faster. A pass shorter than the clip asks for frames at a multiple of the rate the file was written at, and past what the card can decode the picture stops advancing — see [`SlotSync::BeatSync`].
   */
  speed?: number;
  /**
   * Whether this slot follows the beat, and over how many beats.
   *
   * On the slot rather than on the layer, because it is a property of the clip: how long *this* piece of footage takes is a fact about the footage, and a layer that carried it would apply one answer to every slot on it (owner, 2026-09-23). A layer therefore has no rate of its own at all; what it shows is whatever the slot it is playing says.
   */
  sync?: SlotSync;
  /**
   * Source transform, applied before the layer transform.
   *
   * Configuration only. The standard Layer Fixture drives the layer transform instead.
   */
  transform: Transform2D;
}
/**
 * Show-level master settings.
 *
 * These are the values a show *starts* from. What the master is doing right now lives in [`crate::live::MasterLive`] and is never written back here.
 */
export interface MasterConfig {
  /**
   * Beats per bar, which is what the clock counts in.
   */
  beats_per_bar: number;
  /**
   * Crossfader response curve.
   */
  crossfader_curve: CrossfaderCurve;
  /**
   * BPM the master clock starts at.
   */
  default_bpm: number;
  /**
   * Crossfader position a restart returns to: `0.0` is fully A, `1.0` fully B.
   */
  default_crossfader: number;
  /**
   * Master dimmer a restart returns to, in `0.0..=1.0`.
   *
   * Live state is not restored after a restart, so this is the value the show sits at until a console sends one.
   */
  default_dimmer: number;
  /**
   * Master speed scale the show starts at.
   */
  default_speed_scale: number;
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
/**
 * Show-level preferences that are not part of the show itself.
 */
export interface ShowSettings {
  /**
   * Whether edits are saved automatically.
   */
  auto_save: boolean;
  /**
   * Free-form operator notes.
   */
  notes?: string;
  /**
   * The most a clip may be prepared at before it is uploaded here.
   *
   * Left out of the document when it is `SOURCE`, so a show nobody has decided this for reads exactly as it did before this existed.
   */
  upload_ceiling?: UploadCeiling;
}
