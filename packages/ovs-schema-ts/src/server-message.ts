// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * A message from the server.
 */
export type ServerMessage =
  | {
      /**
       * What a control source has done to the effects since the show opened.
       *
       * Here rather than only in the updates that follow, because a client that joins mid-show has to see the effects that are already moved. Without it a late client would draw every effect control at its configured value while the wall showed something else.
       *
       * Only the instances somebody has touched. An instance that is absent is one running at exactly what the show configured, which is where every effect starts.
       */
      effects?: {
        [k: string]: {
          [k: string]: ParameterValue;
        };
      };
      /**
       * What a control source has done to the generators since the show opened.
       *
       * The same as [`effects`](Self::effects), for the other kind of module.
       */
      generators?: {
        [k: string]: {
          [k: string]: ParameterValue;
        };
      };
      /**
       * Live state of each layer in the loaded show.
       */
      layers: LayerLive[];
      /**
       * Live master state.
       */
      master: MasterLive;
      /**
       * What the renderer is doing.
       */
      renderer: RendererStatus;
      /**
       * The sequence this snapshot is current as of.
       */
      sequence: number;
      /**
       * The loaded show, or `None` when the server has none open.
       */
      show?: Show | null;
      type: 'snapshot';
    }
  | {
      /**
       * What changed.
       */
      change: StateChange;
      /**
       * Position of this update in the stream.
       */
      sequence: number;
      type: 'update';
    }
  | {
      /**
       * Decoders currently producing frames.
       */
      active_decoders: number;
      /**
       * Where the master clock is.
       *
       * Absent when no show is open, because there is no show for a beat to belong to — which is different from a beat at zero.
       */
      clock?: ClockTelemetry | null;
      /**
       * Total CPU utilisation, 0 to 100.
       */
      cpu_percent: number;
      /**
       * CPU package temperature in degrees Celsius, when readable.
       */
      cpu_temperature_c?: number | null;
      /**
       * Currently loaded show, if any.
       */
      current_show?: ShowId | null;
      /**
       * Name of the currently loaded show, for display.
       */
      current_show_name?: string | null;
      /**
       * Decode errors since the renderer started.
       */
      decode_errors: number;
      /**
       * Media storage total, in bytes.
       */
      disk_total_bytes: number;
      /**
       * Media storage used, in bytes.
       */
      disk_used_bytes: number;
      /**
       * What is arriving on the lighting network.
       *
       * Absent when this build is not listening at all, which is different from listening and hearing nothing — the first is a decision, the second is a problem somebody needs to see.
       */
      dmx?: DmxTelemetry | null;
      /**
       * Where the renderer's last frame went.
       *
       * Absent when the renderer has not reported one, which is every server whose renderer is not up yet.
       */
      frame?: FrameTimings | null;
      /**
       * GPU utilisation, 0 to 100, when readable.
       */
      gpu_percent?: number | null;
      /**
       * GPU temperature in degrees Celsius, when readable.
       */
      gpu_temperature_c?: number | null;
      /**
       * Primary IP address.
       */
      ip_address: string;
      /**
       * Frames held because a decoder was late, since the renderer started.
       */
      late_frames?: number;
      /**
       * Configured outputs that nothing is driving.
       *
       * Empty is the ordinary state. A name in here is a wall that is dark and an operator who needs to know before an audience does.
       */
      missing_outputs?: MissingOutput[];
      /**
       * Per-output measurements.
       */
      outputs: OutputTelemetry[];
      /**
       * Where each playing layer's playhead is.
       *
       * Beside [`ServerTelemetry::active_decoders`] rather than inside the playback figures it is counted in, because the two are different measurements: those are cumulative and describe the machine, this is instantaneous and describes one layer. Keeping them apart is also what leaves [`PlaybackTelemetry`] a `Copy` struct that costs nothing to read.
       *
       * Empty is the ordinary state of a server with nothing on stage, and of one whose renderer has not reported yet. A layer that is not playing a clip is absent rather than present at zero.
       */
      playheads?: LayerPlayhead[];
      /**
       * Memory utilisation, 0 to 100.
       */
      ram_percent: number;
      /**
       * Memory this machine has, in bytes.
       */
      ram_total_bytes?: number;
      /**
       * Memory in use, in bytes.
       *
       * Beside the percentage rather than instead of it. "How much is left" and "how hard is it working" are different questions, and the Service Console asks the first one: 40% of a number nobody stated answers neither.
       */
      ram_used_bytes?: number;
      /**
       * Renderer state.
       */
      renderer: RendererStatus;
      /**
       * Which surfaces are showing the test pattern instead of the show.
       *
       * On the telemetry rather than only in the console that switched it, because the wall is not showing the show and every client is entitled to know why — and to know *which* pattern, since one of them still goes through the mapping and the other does not.
       */
      test_pattern?: TestPatternMode & string;
      type: 'telemetry';
      /**
       * Seconds since the server started.
       */
      uptime_s: number;
      /**
       * OpenVideoServer version string.
       */
      version: string;
      /**
       * Video memory total, in bytes.
       */
      vram_total_bytes?: number | null;
      /**
       * Video memory used, in bytes.
       */
      vram_used_bytes?: number | null;
    }
  | {
      /**
       * One entry per socket, with what an operator has decided it is for.
       */
      connectors: ConnectorInfo[];
      type: 'connectors';
    }
  | {
      /**
       * Full source names, sorted, shaped `MACHINE (Source Name)`.
       */
      sources: string[];
      type: 'ndi_sources';
    }
  | {
      /**
       * The item being worked on.
       */
      media_item_id: MediaItemId;
      /**
       * Completion between 0.0 and 1.0.
       */
      progress: number;
      /**
       * What is happening, for the operator.
       */
      stage: string;
      type: 'media_progress';
    }
  | {
      /**
       * Short machine-readable code.
       */
      code: string;
      /**
       * Human-readable description.
       */
      message: string;
      type: 'fault';
    }
  | {
      /**
       * The nonce from the ping.
       */
      nonce: number;
      type: 'pong';
    };
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
 * What the renderer is doing.
 */
export type RendererStatus =
  | {
      state: 'stopped';
    }
  | {
      state: 'starting';
    }
  | {
      state: 'running';
    }
  | {
      /**
       * What is degraded.
       */
      reason: string;
      state: 'degraded';
    }
  | {
      /**
       * What failed.
       */
      reason: string;
      state: 'faulted';
    }
  | {
      /**
       * Why contact was lost.
       */
      reason: string;
      state: 'unreachable';
    };
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
 * MediaItemId (ULID, 26 characters)
 */
export type MediaItemId = string;
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
 * An incremental change to the authoritative state.
 */
export type StateChange =
  | {
      change: 'show_loaded';
      /**
       * What a control source has done to the effects of the show now loaded.
       *
       * Empty, always, because opening a show returns every effect to the values it was configured with. It is sent rather than assumed for the same reason `master` and `layers` are: the restart policy lives in the core, and a peer that cleared its own copy would be a second implementation of it that has to agree with the server's forever.
       */
      effects?: {
        [k: string]: {
          [k: string]: ParameterValue;
        };
      };
      /**
       * What a control source has done to the generators of the show now open.
       */
      generators: {
        [k: string]: {
          [k: string]: ParameterValue;
        };
      };
      /**
       * Live state of every layer in the show now loaded.
       *
       * Empty when the show was closed.
       */
      layers: LayerLive[];
      /**
       * Live master state as it now stands.
       */
      master: MasterLive;
      /**
       * The show now in effect, or `None` when it was closed.
       */
      show?: Show | null;
    }
  | {
      change: 'master_changed';
      /**
       * The new master state.
       */
      master: MasterLive;
    }
  | {
      change: 'layer_changed';
      /**
       * The new layer state.
       */
      layer: LayerLive;
    }
  | {
      change: 'effect_changed';
      /**
       * Which instance moved.
       */
      effect_instance_id: EffectInstanceId;
      /**
       * Every live value now on it, whole rather than as a difference.
       *
       * Empty when the last live value was cleared, which is how "this effect is back at what the show configured" is said. A peer replaces what it holds for this instance with this, so applying one needs no history and a client cannot end up holding a value nobody sent.
       */
      overrides?: {
        [k: string]: ParameterValue;
      };
    }
  | {
      change: 'generator_changed';
      /**
       * Which instance moved.
       */
      generator_instance_id: GeneratorInstanceId;
      /**
       * Every live value now on it, whole rather than as a difference.
       */
      overrides?: {
        [k: string]: ParameterValue;
      };
    }
  | {
      change: 'renderer_changed';
      /**
       * The new status.
       */
      renderer: RendererStatus;
    };
/**
 * Which protocol a universe is arriving on.
 */
export type DmxProtocol = 'art_net' | 'sacn';
/**
 * How many bits a colour channel carries inside the compositor.
 *
 * What the layer buses and the composition surface are made of, which is what decides whether blending twenty layers bands. **This is the stage that quantises**, and it is eight bits: the owner's decision of 2026-09-23 is to leave it there until banding is measured as a real problem, because widening it doubles the bandwidth and memory of every bus of every output.
 *
 * It is not a function of the material. A Main 10 clip is decoded at ten bits and composited into this, and this still says eight.
 */
export type CompositionDepth = 'eight' | 'ten';
/**
 * How many bits a colour channel carries on the cable.
 *
 * What the display engine is scanning out, read off the format the swapchain was actually created with rather than off anything that was asked for. A driver that offers no ten-bit surface format gets [`Self::Eight`] however the show is configured, and that is the number reported.
 *
 * [`Self::Unknown`] is not a failure. A backend that is not a panel — a headless one in a test, a composition rendered for a stream — has no cable and no honest answer, and saying so is better than defaulting to eight and being believed.
 */
export type OutputDepth = 'eight' | 'ten' | 'unknown';
/**
 * How many bits a sample of the material being decoded carries.
 *
 * A property of the clip and its decoder, reported per playing layer, because that is where it is a fact: two layers of one show can be playing an eight-bit clip and a Main 10 clip at the same time and both are right.
 *
 * Where the ten bits *sit* inside their element is a separate matter again and does not belong on a wire — a card's P010 and a processor's `yuv420p10le` are sixty-four times apart and the renderer's own `SampleDepth` keeps them apart. An operator asking what is playing is asking how many bits, not which end of a `u16` they are at.
 */
export type DecodeDepth = 'eight' | 'ten';
/**
 * Which surfaces are showing the service test pattern.
 *
 * Two different questions, which is why this is not a switch.
 *
 * **Composition** asks "is the show reaching the walls, and is my mapping right?" The pattern becomes the composition, so the pieces cut it up and each wall shows its own region of it: a wall showing the middle of the pattern is mapped to the middle of the composition.
 *
 * **Outputs** asks "is this cable reaching this wall, and in what mode?" Each output draws its own pattern at its own size and rate, with its own name on it, and the mapping is bypassed entirely. An engineer reads the answer off the wall.
 *
 * Doing both at once would answer neither, so this is one setting with three values rather than two switches.
 */
export type TestPatternMode = 'OFF' | 'COMPOSITION' | 'OUTPUTS' | 'PIECES';
/**
 * What a GPU connector is used for.
 *
 * This is server configuration, not show data: it describes the machine, and it must never be inferred from what happens to be plugged in. The reference server has no integrated GPU, so the single-GPU case where a service display and media outputs share one card is normal, not exceptional.
 */
export type ConnectorRole = 'SERVICE_DISPLAY' | 'MEDIA_OUTPUT' | 'UNUSED';

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
}
/**
 * Where the master clock is, as a measurement rather than as state.
 *
 * The beat moves sixty times a second and nobody edits it, so it is not part of the authoritative live state — publishing a change every frame would make the state stream a metronome. It is reported with the rest of the telemetry and a client animates between samples from the tempo it already has, which is what a beat strip is: a prediction corrected whenever the server speaks.
 */
export interface ClockTelemetry {
  /**
   * Bars since the last reset.
   */
  bar: number;
  /**
   * Which beat of the bar, counting from zero.
   */
  beat_in_bar: number;
  /**
   * Beats in a bar, so a client knows how many marks to draw.
   */
  beats_per_bar: number;
  /**
   * Beats a second, tempo and master speed together.
   *
   * What a client animates with between samples. Sent rather than left to be worked out, because the two numbers it comes from are in different messages and a screen that multiplied the wrong pair would drift.
   */
  beats_per_second: number;
  /**
   * How far through that beat, in `0.0..1.0`.
   */
  phase: number;
}
/**
 * What is arriving on the lighting network.
 *
 * Counters rather than a log: a receiver that wrote a line per packet would produce a hundred and fifty thousand an hour for one universe, and the question an operator is actually asking — "is the console reaching it?" — is answered by a number that goes up.
 */
export interface DmxTelemetry {
  /**
   * Art-Net packets applied since the server started.
   */
  artnet_packets: number;
  /**
   * Packets understood but not applied: outranked, contended, late or preview.
   */
  refused_packets: number;
  /**
   * sACN packets applied since the server started.
   */
  sacn_packets: number;
  /**
   * Every universe this server has heard from, lowest first.
   */
  universes: UniverseTelemetry[];
  /**
   * Datagrams that were not a data packet of either protocol.
   *
   * Never zero on a shared network, and not a fault: consoles poll for nodes, and a venue plugs in whatever it likes.
   */
  unusable_packets: number;
}
/**
 * What one universe is receiving.
 */
export interface UniverseTelemetry {
  /**
   * How many channels it is sending.
   */
  channels: number;
  /**
   * The priority it claims.
   */
  priority?: number | null;
  /**
   * Which protocol that source is using.
   */
  protocol?: DmxProtocol | null;
  /**
   * What the driving source calls itself, or its address for Art-Net.
   *
   * Absent when nothing is driving it. The channels are still whatever the last source left, because loss holds the look rather than blacking it out — so "no source" and "no data" are deliberately different things.
   */
  source?: string | null;
  /**
   * Universe number.
   */
  universe: number;
}
/**
 * How long each part of one frame took, in microseconds.
 *
 * # Why this exists
 *
 * A wall running below its mode is the most important thing a renderer can report and the least useful on its own: it says a frame did not fit without saying what did not fit in it. This server ran at forty frames a second with three clips on stage while the only timing it reported — how long an output took to present — sat unmoved at three milliseconds of the sixteen and a half a frame has. Every microsecond of the difference was in parts nothing was measuring.
 *
 * So the pass is timed where it is spent: getting stills onto the card, getting each playing layer's frame onto the card, and drawing the composition. Presenting is already measured per output, because each output has its own mode and its own deadline, and belongs on [`OutputTelemetry::frame_render_time_us`] rather than here.
 *
 * These are the last frame's figures, not an average. A mean over a second hides exactly the frame that missed, and an operator chasing a stutter wants the one that missed.
 */
export interface FrameTimings {
  /**
   * Drawing the three buses and combining them into the composition.
   */
  compose_us: number;
  /**
   * Getting stills and generators onto the card.
   */
  media_us: number;
  /**
   * Getting each playing layer's decoded frame onto the card.
   *
   * This is where a clip costs what it costs. The decoding happened on another thread; what is counted here is what the render thread does with the result, which is a copy of every layer's picture into mapped memory.
   */
  playback_us: number;
}
/**
 * A configured output that nothing is driving.
 *
 * A missing output is an explicit fault, never a silent remap. This is that fault, on its way to the screen an operator is looking at.
 */
export interface MissingOutput {
  /**
   * What the operator calls it.
   */
  name: string;
  /**
   * The configured output.
   */
  output_id: OutputId;
  /**
   * Why nothing is driving it.
   */
  reason: string;
}
/**
 * Measured state of one physical output.
 */
export interface OutputTelemetry {
  /**
   * Whether this output is currently being driven.
   */
  active: boolean;
  /**
   * Frames per second actually presented, averaged over the last second.
   */
  actual_fps: number;
  /**
   * How many bits a channel carries inside the compositor.
   *
   * Read off the surfaces the renderer actually created. The stage that quantises, and the one that must never be confused with either of the others: material decoded at ten bits is composited into this, and this still says what it is. A different type from the other two so they can never be filled from each other — see [`crate::depth`].
   */
  composition_depth?: CompositionDepth & string;
  /**
   * The socket it is forced onto: `HDMI-0`.
   */
  connector?: string | null;
  /**
   * What is plugged into the socket, when the driver says: `Sony LMD-2110W`.
   *
   * To read, not to patch by. An operator names an output by where it is in the list — OUTPUT 1, OUTPUT 2 — because that is what a show is built against and it does not change when a monitor is swapped. The panel and the socket belong underneath it, where somebody tracing a cable looks.
   */
  display?: string | null;
  /**
   * Dropped frame counters.
   */
  dropped_frames: DroppedFrames;
  /**
   * Time spent rendering the last frame, in microseconds.
   */
  frame_render_time_us: number;
  /**
   * Operator-visible name.
   */
  name: string;
  /**
   * How many bits a channel carries on this output's cable.
   *
   * Read off the swapchain format the driver actually gave, and `unknown` on a backend that has no cable. **Never** borrowed from `composition_depth` or from what is playing: a show decoding Main 10 into an eight-bit socket reports eight here, and an operator reading this can believe it.
   */
  output_depth?: OutputDepth & string;
  /**
   * Which output this describes.
   */
  output_id: OutputId;
  /**
   * Refresh rate actually in use.
   */
  refresh: FrameRate;
  /**
   * Resolution actually in use.
   */
  size: SizePx;
}
/**
 * Two dropped-frame counters for one output.
 *
 * The operator resets the first one to watch a specific moment. The second is never resettable, so a genuine long-run problem cannot be hidden by pressing a button.
 */
export interface DroppedFrames {
  /**
   * Lifetime counter since the renderer started. Never reset by an operator.
   */
  lifetime: number;
  /**
   * Resettable counter shown on the Service Console.
   */
  since_reset: number;
}
/**
 * Where one layer's clip has got to.
 *
 * One entry per layer that is actually playing a clip. A layer showing a generator has no playhead — a generator draws itself and has no timeline to be a position in — and neither has a layer showing nothing. Both are absent rather than reported at zero, because "at the beginning" and "there is no clip" are different pictures and only one of them gets a progress bar (house rule §16).
 *
 * # Both units, because two things are reading
 *
 * An operator cueing the next clip reads seconds: "eight of twelve" is the question being asked while a hand hovers over the next pad. A scheduler, and anything that has to land on a boundary, works in frames — and a frame index cannot be recovered from a millisecond position without the clip's own rate, which nothing else on the wire carries. Sending both costs a handful of bytes and saves every reader from deriving the half it was not given.
 *
 * # Why a speed comes with the position
 *
 * The same reason [`ClockTelemetry`] carries `beats_per_second`. This is published about once a second and a progress bar moves sixty times between samples, so the bar is a prediction corrected whenever the server speaks. What it is predicted from is [`LayerPlayhead::speed_ms_per_second`], which is the whole rate already multiplied out: the layer's speed ratio, the master speed and the tempo the clip is running against. Left to be worked out, those come from three different messages, and a screen that combined the wrong pair would drift away from the wall it is describing.
 */
export interface LayerPlayhead {
  /**
   * How many bits a sample of this clip carries.
   *
   * Per layer, because that is where it is a fact: two layers of one show can be playing an eight-bit clip and a Main 10 clip at once. Says nothing about what the compositor or the cable does with it — those are `OutputTelemetry`'s two fields and they are two other types.
   */
  decode_depth?: DecodeDepth & string;
  /**
   * How long one pass of the clip is, in milliseconds, at its own rate.
   *
   * The clip's own length, not how long this pass will take on the wall clock. Those differ whenever the layer is off unity or the tempo has moved since the trigger, and the difference between them is precisely [`LayerPlayhead::speed_ms_per_second`].
   */
  duration_ms: number;
  /**
   * The frame the playhead is on, counting from zero.
   *
   * Where the *playhead* is, which is not always what reached the GPU: a decoder that was late leaves the previous frame on the wall, and that is what `late_frames` on [`PlaybackTelemetry`] counts. A bar drawn from the uploaded frame instead would stall whenever a decoder did, which would report a storage problem as a timing one.
   */
  frame: number;
  /**
   * How many frames one pass of the clip holds.
   */
  frame_count: number;
  /**
   * Which layer this is the playhead of.
   */
  layer_id: LayerId;
  /**
   * What is playing.
   */
  media_item_id: MediaItemId;
  /**
   * How far through the clip the playhead is, in milliseconds.
   */
  position_ms: number;
  /**
   * Which slot of that layer is playing.
   *
   * Beside the item and not instead of it: a sample can be most of a second old, and a layer that has changed pad since it was taken must not have the old bar drawn on the new clip. The slot alone answers that, because a show may hold the same item in two slots.
   */
  slot: number;
  /**
   * How fast the playhead is travelling, in clip milliseconds per second.
   *
   * A thousand is real time. Signed: negative while the timeline is running backwards, which is a reversed slot or the return leg of a bounce, and zero while a frozen clip holds its last frame — a bar that kept moving there would be an animation of something that has stopped.
   */
  speed_ms_per_second: number;
}
/**
 * A GPU connector as discovered on the server.
 */
export interface ConnectorInfo {
  /**
   * Whether something is currently plugged in.
   */
  connected: boolean;
  /**
   * Connector name as the driver reports it, for example `DP-1`.
   */
  connector: string;
  /**
   * PCI address of the GPU driving it.
   */
  gpu_pci_address: string;
  /**
   * EDID model name, when the attached device reports one.
   *
   * For recognising hardware in the client and for support calls. It decides nothing: outputs are forced onto connectors.
   */
  model?: string | null;
  /**
   * The modes this connector offers, as the driver reports them.
   *
   * Empty until a renderer has said. An operator choosing a mode for a wall needs the list, and inventing one would offer modes the panel cannot do.
   */
  modes?: ConnectorModeInfo[];
  /**
   * What this connector is configured for.
   */
  role: ConnectorRole;
}
/**
 * One mode a connector offers.
 */
export interface ConnectorModeInfo {
  /**
   * Refresh rate in millihertz, as the driver reports it.
   *
   * Not a float: 59 940 is 60000/1001 exactly, and the two rates a panel actually has must not round into one.
   */
  refresh_millihertz: number;
  /**
   * Resolution in pixels.
   */
  size: SizePx;
}
