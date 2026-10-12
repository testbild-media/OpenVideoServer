// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * ShowId (ULID, 26 characters)
 */
export type ShowId = string;
/**
 * Which protocol a universe is arriving on.
 */
export type DmxProtocol = 'art_net' | 'sacn';
/**
 * OutputId (ULID, 26 characters)
 */
export type OutputId = string;
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
 * LayerId (ULID, 26 characters)
 */
export type LayerId = string;
/**
 * MediaItemId (ULID, 26 characters)
 */
export type MediaItemId = string;
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
 * Measured state of the server as a whole.
 *
 * Every field the Service Console must show is present here, so the console is a pure view over one message rather than a collection of probes.
 */
export interface ServerTelemetry {
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
   * Whether this output is drawing the pieces a show cut for it.
   *
   * False means it is showing the whole composition instead, which is what an output the show says nothing about falls back to — right for a panel nobody has patched yet, and wrong in a way nothing announced for one that was patched and has quietly lost its assignment.
   *
   * It decides the masked preview too: an unmapped output puts the whole surface into the mask, so the "Pieces" picture becomes the plain composition. An operator seeing that had no way to tell which of the two had happened (owner, 2026-10-12: "Wann das passiert und weggefallen ist kann ich nicht sagen"), because this was reported nowhere.
   */
  showing_pieces?: boolean;
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
