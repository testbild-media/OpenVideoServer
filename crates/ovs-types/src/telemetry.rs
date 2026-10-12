//! Telemetry.
//!
//! Real measurements only. There is deliberately no aggregate "health score":
//! a single number hides exactly the problem an operator needs to see
//! (house rule §16).

use serde::{Deserialize, Serialize};

use crate::geometry::{FrameRate, SizePx};
use crate::id::{LayerId, MediaItemId, OutputId, ShowId};

/// What the renderer is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RendererStatus {
    /// Not running.
    Stopped,
    /// Initialising the GPU and outputs.
    Starting,
    /// Rendering.
    Running,
    /// Running in a degraded state; the show continues.
    ///
    /// This is the normal outcome of a non-critical failure such as a dead
    /// preview encoder (house rule §17).
    Degraded {
        /// What is degraded.
        reason: String,
    },
    /// Not rendering. Something essential failed.
    Faulted {
        /// What failed.
        reason: String,
    },
    /// The renderer cannot be reached, so its state is not known.
    ///
    /// Deliberately distinct from [`RendererStatus::Stopped`]. A renderer the
    /// core has lost contact with is usually still rendering perfectly well;
    /// reporting it as stopped would send somebody to the rack for nothing, and
    /// reporting it as running would hide a renderer that really had died.
    Unreachable {
        /// Why contact was lost.
        reason: String,
    },
}

impl RendererStatus {
    /// Returns true when frames are known to be reaching the outputs.
    ///
    /// [`RendererStatus::Unreachable`] is false here, not because the renderer
    /// has stopped but because nothing can currently say that it has not.
    #[must_use]
    pub const fn is_rendering(&self) -> bool {
        matches!(self, Self::Running | Self::Degraded { .. })
    }

    /// Returns true when the renderer's state is actually known.
    ///
    /// The difference between "not rendering" and "we cannot tell", which is
    /// what a Service Console has to show differently.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        !matches!(self, Self::Unreachable { .. })
    }
}

/// What a surface turned out to be encoded with.
///
/// The answer to the last clause of owner decision Q13: a preview whose rate has
/// been reduced must say so, because a picture arriving at a quarter speed and
/// explaining nothing is indistinguishable from a fault — and the operator who
/// concludes the show is stuttering when only the preview is has been misled by
/// silence.
///
/// Known only once the encoder is up, which is a fifth of a second after a
/// watcher asks. Nothing waits for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StreamEncoding {
    /// The encoder that was taken, by FFmpeg's name for it.
    ///
    /// A name rather than a category, because "libx264" is what a log says and
    /// what somebody diagnosing this will search for.
    pub encoder: String,
    /// Whether that encoder is on the card.
    ///
    /// Decided where the encoder list is, rather than by a client matching on
    /// the name: a list that grows a VAAPI entry should not need every watcher
    /// to be taught about it.
    pub hardware: bool,
    /// Frames a second this surface is really encoded at.
    ///
    /// The show's own rate unless a software encoder has capped it. Compare
    /// with the rate the surface runs at to know whether anything was given up.
    pub fps: f64,
}

/// One surface somebody can watch.
///
/// The composition is what the show *is*; an output is what one wall gets after
/// the mapping has cut it up. Both are worth watching and they answer different
/// questions, which is why a client picks rather than being given one.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StreamSurface {
    /// The show before it is cut into pieces.
    Composition,
    /// The show with everything no piece takes blacked out.
    ///
    /// The same picture as [`Composition`](Self::Composition), masked by the
    /// piece mapping, so that what is left is exactly the part of the
    /// composition that reaches a wall. A composition is authored at a
    /// convenient size and the walls are whatever the venue has, so the two
    /// are rarely the same thing; the difference is content nobody in the room
    /// will ever see.
    ///
    /// Scoped by the owner on 2026-09-23 as the second of three surfaces, after
    /// the composition and before the outputs. There is no per-piece and no
    /// per-layer surface: each one costs a readback and an encode on a machine
    /// that has to hold 60 fps first.
    CompositionMasked,
    /// One output, as that wall sees it.
    Output {
        /// Which one.
        output_id: OutputId,
    },
}

impl std::fmt::Display for StreamSurface {
    /// The name this goes by in a URL and in a log.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Composition => formatter.write_str("composition"),
            Self::CompositionMasked => formatter.write_str("composition-masked"),
            Self::Output { output_id } => write!(formatter, "{output_id}"),
        }
    }
}

/// Which of the operator-visible counters to put back to zero.
///
/// The three numbers on a System screen that only mean anything relative to
/// when somebody last looked. They are separate because they are tapped
/// separately: an operator clearing the dropped-frame count after fixing a
/// cable has not decided that the evening's decoder errors never happened.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CounterGroup {
    /// Every one of them.
    #[default]
    All,
    /// Frames the outputs did not present in time.
    DroppedFrames,
    /// Frames a decoder could not produce in time.
    LateFrames,
    /// Media that could not be decoded.
    DecodeErrors,
}

/// Two dropped-frame counters for one output.
///
/// The operator resets the first one to watch a specific moment. The second is
/// never resettable, so a genuine long-run problem cannot be hidden by pressing
/// a button.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct DroppedFrames {
    /// Resettable counter shown on the Service Console.
    pub since_reset: u64,
    /// Lifetime counter since the renderer started. Never reset by an operator.
    pub lifetime: u64,
}

impl DroppedFrames {
    /// Records dropped frames in both counters.
    pub fn record(&mut self, count: u64) {
        self.since_reset = self.since_reset.saturating_add(count);
        self.lifetime = self.lifetime.saturating_add(count);
    }

    /// Resets only the operator-visible counter.
    pub fn reset_operator_counter(&mut self) {
        self.since_reset = 0;
    }
}

/// A configured output that nothing is driving.
///
/// A missing output is an explicit fault, never a silent remap.
/// This is that fault, on its way to the screen an operator is looking at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MissingOutput {
    /// The configured output.
    pub output_id: OutputId,
    /// What the operator calls it.
    pub name: String,
    /// Why nothing is driving it.
    pub reason: String,
}

/// What the renderer's players are doing.
///
/// Reported by the renderer because it is the process that decodes. A core with
/// no renderer connected reports nothing here rather than zeroes, because
/// "nothing is playing" and "nobody has said" are different answers
/// (house rule §16).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct PlaybackTelemetry {
    /// Layers showing a frame of a clip right now.
    pub playing: u32,
    /// Frames held because the decoder had not produced the next one in time.
    ///
    /// Not a dropped frame: the show kept its rate and one frame was shown
    /// twice. Climbing steadily is a decoder that cannot keep up, which is the
    /// number worth watching on a machine that is close to its limit.
    pub late_frames: u64,
    /// Sources that could not be played, since the renderer started.
    pub errors: u64,
}

/// Where one layer's clip has got to.
///
/// One entry per layer that is actually playing a clip. A layer showing a
/// generator has no playhead — a generator draws itself and has no timeline to
/// be a position in — and neither has a layer showing nothing. Both are absent
/// rather than reported at zero, because "at the beginning" and "there is no
/// clip" are different pictures and only one of them gets a progress bar
/// (house rule §16).
///
/// # Both units, because two things are reading
///
/// An operator cueing the next clip reads seconds: "eight of twelve" is the
/// question being asked while a hand hovers over the next pad. A scheduler, and
/// anything that has to land on a boundary, works in frames — and a frame index
/// cannot be recovered from a millisecond position without the clip's own rate,
/// which nothing else on the wire carries. Sending both costs a handful of bytes
/// and saves every reader from deriving the half it was not given.
///
/// # Why a speed comes with the position
///
/// The same reason [`ClockTelemetry`] carries `beats_per_second`. This is
/// published about once a second and a progress bar moves sixty times between
/// samples, so the bar is a prediction corrected whenever the server speaks.
/// What it is predicted from is [`LayerPlayhead::speed_ms_per_second`], which is
/// the whole rate already multiplied out: the layer's speed ratio, the master
/// speed and the tempo the clip is running against. Left to be worked out, those
/// come from three different messages, and a screen that combined the wrong pair
/// would drift away from the wall it is describing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LayerPlayhead {
    /// Which layer this is the playhead of.
    pub layer_id: LayerId,
    /// Which slot of that layer is playing.
    ///
    /// Beside the item and not instead of it: a sample can be most of a second
    /// old, and a layer that has changed pad since it was taken must not have
    /// the old bar drawn on the new clip. The slot alone answers that, because a
    /// show may hold the same item in two slots.
    pub slot: u16,
    /// What is playing.
    pub media_item_id: MediaItemId,
    /// The frame the playhead is on, counting from zero.
    ///
    /// Where the *playhead* is, which is not always what reached the GPU: a
    /// decoder that was late leaves the previous frame on the wall, and that is
    /// what `late_frames` on [`PlaybackTelemetry`] counts. A bar drawn from the
    /// uploaded frame instead would stall whenever a decoder did, which would
    /// report a storage problem as a timing one.
    pub frame: u32,
    /// How many frames one pass of the clip holds.
    pub frame_count: u32,
    /// How many bits a sample of this clip carries.
    ///
    /// Per layer, because that is where it is a fact: two layers of one show can
    /// be playing an eight-bit clip and a Main 10 clip at once. Says nothing
    /// about what the compositor or the cable does with it — those are
    /// `OutputTelemetry`'s two fields and they are two other types.
    #[serde(default)]
    pub decode_depth: crate::depth::DecodeDepth,
    /// How far through the clip the playhead is, in milliseconds.
    pub position_ms: u64,
    /// How long one pass of the clip is, in milliseconds, at its own rate.
    ///
    /// The clip's own length, not how long this pass will take on the wall
    /// clock. Those differ whenever the layer is off unity or the tempo has
    /// moved since the trigger, and the difference between them is precisely
    /// [`LayerPlayhead::speed_ms_per_second`].
    pub duration_ms: u64,
    /// How fast the playhead is travelling, in clip milliseconds per second.
    ///
    /// A thousand is real time. Signed: negative while the timeline is running
    /// backwards, which is a reversed slot or the return leg of a bounce, and
    /// zero while a frozen clip holds its last frame — a bar that kept moving
    /// there would be an animation of something that has stopped.
    pub speed_ms_per_second: f64,
}

impl LayerPlayhead {
    /// Returns how far through the clip this is, from `0.0` to `1.0`.
    ///
    /// `None` for a clip with no length, which is the one case nothing can draw
    /// a bar for.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        if self.duration_ms == 0 {
            return None;
        }
        let through = self.position_ms as f64 / self.duration_ms as f64;
        Some(through.clamp(0.0, 1.0) as f32)
    }
}

/// Which protocol a universe is arriving on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DmxProtocol {
    /// Art-Net.
    ArtNet,
    /// ANSI E1.31, sACN.
    Sacn,
}

/// What one universe is receiving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UniverseTelemetry {
    /// Universe number.
    pub universe: u16,
    /// What the driving source calls itself, or its address for Art-Net.
    ///
    /// Absent when nothing is driving it. The channels are still whatever the
    /// last source left, because loss holds the look rather than blacking it
    /// out — so "no source" and "no data" are deliberately different things.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Which protocol that source is using.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<DmxProtocol>,
    /// The priority it claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    /// How many channels it is sending.
    pub channels: u16,
}

/// What is arriving on the lighting network.
///
/// Counters rather than a log: a receiver that wrote a line per packet would
/// produce a hundred and fifty thousand an hour for one universe, and the
/// question an operator is actually asking — "is the console reaching it?" —
/// is answered by a number that goes up.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DmxTelemetry {
    /// Art-Net packets applied since the server started.
    pub artnet_packets: u64,
    /// sACN packets applied since the server started.
    pub sacn_packets: u64,
    /// Datagrams that were not a data packet of either protocol.
    ///
    /// Never zero on a shared network, and not a fault: consoles poll for
    /// nodes, and a venue plugs in whatever it likes.
    pub unusable_packets: u64,
    /// Packets understood but not applied: outranked, contended, late or preview.
    pub refused_packets: u64,
    /// Every universe this server has heard from, lowest first.
    pub universes: Vec<UniverseTelemetry>,
}

/// Where the master clock is, as a measurement rather than as state.
///
/// The beat moves sixty times a second and nobody edits it, so it is not part of
/// the authoritative live state — publishing a change every frame would make the
/// state stream a metronome. It is reported with the rest of the telemetry and a
/// client animates between samples from the tempo it already has, which is what
/// a beat strip is: a prediction corrected whenever the server speaks.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClockTelemetry {
    /// Bars since the last reset.
    pub bar: u64,
    /// Which beat of the bar, counting from zero.
    pub beat_in_bar: u32,
    /// How far through that beat, in `0.0..1.0`.
    pub phase: f64,
    /// Beats in a bar, so a client knows how many marks to draw.
    pub beats_per_bar: u32,
    /// Beats a second, tempo and master speed together.
    ///
    /// What a client animates with between samples. Sent rather than left to be
    /// worked out, because the two numbers it comes from are in different
    /// messages and a screen that multiplied the wrong pair would drift.
    pub beats_per_second: f64,
}

/// Measured state of one physical output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OutputTelemetry {
    /// Which output this describes.
    pub output_id: OutputId,
    /// Operator-visible name.
    pub name: String,
    /// Whether this output is currently being driven.
    pub active: bool,
    /// Resolution actually in use.
    pub size: SizePx,
    /// Refresh rate actually in use.
    pub refresh: FrameRate,
    /// Frames per second actually presented, averaged over the last second.
    pub actual_fps: f32,
    /// Time spent rendering the last frame, in microseconds.
    pub frame_render_time_us: u32,
    /// What is plugged into the socket, when the driver says: `Sony LMD-2110W`.
    ///
    /// To read, not to patch by. An operator names an output by where it is in
    /// the list — OUTPUT 1, OUTPUT 2 — because that is what a show is built
    /// against and it does not change when a monitor is swapped. The panel and
    /// the socket belong underneath it, where somebody tracing a cable looks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// The socket it is forced onto: `HDMI-0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    /// Whether this output is drawing the pieces a show cut for it.
    ///
    /// False means it is showing the whole composition instead, which is what
    /// an output the show says nothing about falls back to — right for a panel
    /// nobody has patched yet, and wrong in a way nothing announced for one
    /// that was patched and has quietly lost its assignment.
    ///
    /// It decides the masked preview too: an unmapped output puts the whole
    /// surface into the mask, so the "Pieces" picture becomes the plain
    /// composition. An operator seeing that had no way to tell which of the
    /// two had happened (owner, 2026-10-12: "Wann das passiert und weggefallen
    /// ist kann ich nicht sagen"), because this was reported nowhere.
    #[serde(default)]
    pub showing_pieces: bool,
    /// Dropped frame counters.
    pub dropped_frames: DroppedFrames,
    /// How many bits a channel carries inside the compositor.
    ///
    /// Read off the surfaces the renderer actually created. The stage that
    /// quantises, and the one that must never be confused with either of the
    /// others: material decoded at ten bits is composited into this, and this
    /// still says what it is. A different type from the other two so they can
    /// never be filled from each other — see [`crate::depth`].
    #[serde(default)]
    pub composition_depth: crate::depth::CompositionDepth,
    /// How many bits a channel carries on this output's cable.
    ///
    /// Read off the swapchain format the driver actually gave, and `unknown` on
    /// a backend that has no cable. **Never** borrowed from `composition_depth`
    /// or from what is playing: a show decoding Main 10 into an eight-bit socket
    /// reports eight here, and an operator reading this can believe it.
    #[serde(default)]
    pub output_depth: crate::depth::OutputDepth,
}

/// What the device the renderer is using reports about itself.
///
/// Only the renderer can answer this: it is the process with the device open,
/// and nothing in the core can invent it. A field the driver does not offer is
/// absent rather than zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GpuTelemetry {
    /// The device as the driver names it.
    pub device_name: String,
    /// Device-local memory in use, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_used_bytes: Option<u64>,
    /// Device-local memory this process may use, in bytes.
    ///
    /// A budget rather than a capacity: a driver may hand out less than the
    /// card holds when something else is using it, and what an operator needs
    /// to know is how much is left for the show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_total_bytes: Option<u64>,
    /// How hard the card is working, 0 to 100.
    ///
    /// Absent unless the driver's own management library is installed: Vulkan
    /// does not report it, and a figure worked out from frame times would be a
    /// measure of this renderer rather than of the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilisation_percent: Option<u32>,
    /// Core temperature in degrees Celsius, from the same place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature_c: Option<u32>,
}

/// Measured state of the server as a whole.
///
/// Every field the Service Console must show is present here, so the
/// console is a pure view over one message rather than a collection of probes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerTelemetry {
    /// OpenVideoServer version string.
    pub version: String,
    /// Seconds since the server started.
    pub uptime_s: u64,
    /// Primary IP address.
    pub ip_address: String,
    /// Currently loaded show, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_show: Option<ShowId>,
    /// Name of the currently loaded show, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_show_name: Option<String>,
    /// Renderer state.
    pub renderer: RendererStatus,
    /// Total CPU utilisation, 0 to 100.
    pub cpu_percent: f32,
    /// CPU package temperature in degrees Celsius, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_temperature_c: Option<f32>,
    /// Memory utilisation, 0 to 100.
    pub ram_percent: f32,
    /// Memory in use, in bytes.
    ///
    /// Beside the percentage rather than instead of it. "How much is left" and
    /// "how hard is it working" are different questions, and the Service
    /// Console asks the first one: 40% of a number nobody stated answers
    /// neither.
    #[serde(default)]
    pub ram_used_bytes: u64,
    /// Memory this machine has, in bytes.
    #[serde(default)]
    pub ram_total_bytes: u64,
    /// GPU utilisation, 0 to 100, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_percent: Option<f32>,
    /// GPU temperature in degrees Celsius, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_temperature_c: Option<f32>,
    /// Video memory used, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_used_bytes: Option<u64>,
    /// Video memory total, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_total_bytes: Option<u64>,
    /// Media storage used, in bytes.
    pub disk_used_bytes: u64,
    /// Media storage total, in bytes.
    pub disk_total_bytes: u64,
    /// Decoders currently producing frames.
    pub active_decoders: u32,
    /// Decode errors since the renderer started.
    pub decode_errors: u64,
    /// Frames held because a decoder was late, since the renderer started.
    #[serde(default)]
    pub late_frames: u64,
    /// Which surfaces are showing the test pattern instead of the show.
    ///
    /// On the telemetry rather than only in the console that switched it,
    /// because the wall is not showing the show and every client is entitled to
    /// know why — and to know *which* pattern, since one of them still goes
    /// through the mapping and the other does not.
    #[serde(default)]
    pub test_pattern: crate::config::TestPatternMode,
    /// Per-output measurements.
    pub outputs: Vec<OutputTelemetry>,
    /// Configured outputs that nothing is driving.
    ///
    /// Empty is the ordinary state. A name in here is a wall that is dark and
    /// an operator who needs to know before an audience does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_outputs: Vec<MissingOutput>,
    /// What is arriving on the lighting network.
    ///
    /// Absent when this build is not listening at all, which is different from
    /// listening and hearing nothing — the first is a decision, the second is a
    /// problem somebody needs to see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmx: Option<DmxTelemetry>,
    /// Where the master clock is.
    ///
    /// Absent when no show is open, because there is no show for a beat to
    /// belong to — which is different from a beat at zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<ClockTelemetry>,
    /// Where each playing layer's playhead is.
    ///
    /// Beside [`ServerTelemetry::active_decoders`] rather than inside the
    /// playback figures it is counted in, because the two are different
    /// measurements: those are cumulative and describe the machine, this is
    /// instantaneous and describes one layer. Keeping them apart is also what
    /// leaves [`PlaybackTelemetry`] a `Copy` struct that costs nothing to read.
    ///
    /// Empty is the ordinary state of a server with nothing on stage, and of one
    /// whose renderer has not reported yet. A layer that is not playing a clip
    /// is absent rather than present at zero.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub playheads: Vec<LayerPlayhead>,
    /// Where the renderer's last frame went.
    ///
    /// Absent when the renderer has not reported one, which is every server
    /// whose renderer is not up yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<FrameTimings>,
}

/// How long each part of one frame took, in microseconds.
///
/// # Why this exists
///
/// A wall running below its mode is the most important thing a renderer can
/// report and the least useful on its own: it says a frame did not fit without
/// saying what did not fit in it. This server ran at forty frames a second with
/// three clips on stage while the only timing it reported — how long an output
/// took to present — sat unmoved at three milliseconds of the sixteen and a
/// half a frame has. Every microsecond of the difference was in parts nothing
/// was measuring.
///
/// So the pass is timed where it is spent: getting stills onto the card,
/// getting each playing layer's frame onto the card, and drawing the
/// composition. Presenting is already measured per output, because each output
/// has its own mode and its own deadline, and belongs on
/// [`OutputTelemetry::frame_render_time_us`] rather than here.
///
/// These are the last frame's figures, not an average. A mean over a second
/// hides exactly the frame that missed, and an operator chasing a stutter wants
/// the one that missed.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct FrameTimings {
    /// Getting stills and generators onto the card.
    pub media_us: u32,
    /// Getting each playing layer's decoded frame onto the card.
    ///
    /// This is where a clip costs what it costs. The decoding happened on
    /// another thread; what is counted here is what the render thread does with
    /// the result, which is a copy of every layer's picture into mapped memory.
    pub playback_us: u32,
    /// Drawing the three buses and combining them into the composition.
    pub compose_us: u32,
}

impl FrameTimings {
    /// What the three parts cost together.
    #[must_use]
    pub const fn total_us(&self) -> u32 {
        self.media_us
            .saturating_add(self.playback_us)
            .saturating_add(self.compose_us)
    }

    /// How much of a frame at `rate` this leaves for everything else.
    ///
    /// `None` when the rate has no frame duration to measure against. Negative
    /// is not representable and is not wanted: a pass that took longer than its
    /// frame has nothing left, which is what zero says.
    #[must_use]
    pub fn headroom_us(&self, rate: FrameRate) -> Option<u32> {
        let frame = u32::try_from(rate.frame_duration_nanos()? / 1_000).ok()?;
        Some(frame.saturating_sub(self.total_us()))
    }
}

impl ServerTelemetry {
    /// Returns disk usage as a percentage, or `None` when the total is unknown.
    #[must_use]
    pub fn disk_percent(&self) -> Option<f32> {
        if self.disk_total_bytes == 0 {
            return None;
        }
        Some((self.disk_used_bytes as f64 / self.disk_total_bytes as f64 * 100.0) as f32)
    }

    /// Returns true when any active output is dropping frames since its last reset.
    #[must_use]
    pub fn is_dropping_frames(&self) -> bool {
        self.outputs
            .iter()
            .any(|output| output.active && output.dropped_frames.since_reset > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(active: bool, dropped: u64) -> OutputTelemetry {
        let mut frames = DroppedFrames::default();
        frames.record(dropped);
        OutputTelemetry {
            output_id: OutputId::new(),
            name: "Wall".to_owned(),
            active,
            size: SizePx::new(1920, 1080),
            refresh: FrameRate::FPS_60,
            actual_fps: 60.0,
            frame_render_time_us: 4_000,
            dropped_frames: frames,
            display: None,
            connector: None,
            showing_pieces: true,
            // A stand-in output composes through nothing and is plugged
            // into nothing, so it reports the default pipeline and no
            // depth at all on the cable.
            composition_depth: crate::depth::CompositionDepth::Eight,
            output_depth: crate::depth::OutputDepth::Unknown,
        }
    }

    fn telemetry(outputs: Vec<OutputTelemetry>) -> ServerTelemetry {
        ServerTelemetry {
            version: "0.0.1".to_owned(),
            uptime_s: 60,
            ip_address: "192.168.1.10".to_owned(),
            current_show: None,
            current_show_name: None,
            renderer: RendererStatus::Running,
            cpu_percent: 30.0,
            cpu_temperature_c: Some(55.0),
            ram_percent: 40.0,
            ram_used_bytes: 13 * 1_000_000_000,
            ram_total_bytes: 32 * 1_000_000_000,
            gpu_percent: Some(70.0),
            gpu_temperature_c: Some(62.0),
            vram_used_bytes: Some(2 << 30),
            vram_total_bytes: Some(8 << 30),
            disk_used_bytes: 287 * 1_000_000_000,
            disk_total_bytes: 500 * 1_000_000_000,
            active_decoders: 3,
            decode_errors: 0,
            late_frames: 0,
            test_pattern: crate::config::TestPatternMode::Off,
            outputs,
            missing_outputs: Vec::new(),
            dmx: None,
            clock: None,
            playheads: Vec::new(),
            frame: None,
        }
    }

    #[test]
    fn resetting_the_operator_counter_leaves_the_lifetime_counter_alone() {
        let mut frames = DroppedFrames::default();
        frames.record(5);
        frames.record(3);
        assert_eq!(
            frames,
            DroppedFrames {
                since_reset: 8,
                lifetime: 8
            }
        );

        frames.reset_operator_counter();
        assert_eq!(frames.since_reset, 0);
        assert_eq!(frames.lifetime, 8, "a long-run problem must stay visible");

        frames.record(1);
        assert_eq!(
            frames,
            DroppedFrames {
                since_reset: 1,
                lifetime: 9
            }
        );
    }

    #[test]
    fn counters_saturate_rather_than_wrapping() {
        let mut frames = DroppedFrames {
            since_reset: u64::MAX,
            lifetime: u64::MAX,
        };
        frames.record(10);
        assert_eq!(frames.since_reset, u64::MAX);
        assert_eq!(frames.lifetime, u64::MAX);
    }

    #[test]
    fn an_unreachable_renderer_is_not_the_same_as_a_stopped_one() {
        let unreachable = RendererStatus::Unreachable {
            reason: "the control channel dropped".to_owned(),
        };
        assert!(!unreachable.is_known(), "nothing can say what it is doing");
        assert!(
            !unreachable.is_rendering(),
            "and nothing can claim it is rendering"
        );
        assert_ne!(unreachable, RendererStatus::Stopped);

        assert!(
            RendererStatus::Stopped.is_known(),
            "stopped is a fact, not a guess"
        );
        assert!(RendererStatus::Running.is_known());
    }

    #[test]
    fn a_degraded_renderer_is_still_rendering() {
        assert!(RendererStatus::Running.is_rendering());
        assert!(RendererStatus::Degraded {
            reason: "preview encoder restarted".to_owned()
        }
        .is_rendering());
        assert!(!RendererStatus::Stopped.is_rendering());
        assert!(!RendererStatus::Faulted {
            reason: "no output".to_owned()
        }
        .is_rendering());
    }

    #[test]
    fn a_frame_says_what_it_cost_and_what_is_left_of_its_budget() {
        // The figures a console puts side by side: what the pass cost, and how
        // much of a frame at this mode it left for presenting.
        let frame = FrameTimings {
            media_us: 120,
            playback_us: 7_400,
            compose_us: 2_100,
        };
        assert_eq!(frame.total_us(), 9_620);
        assert_eq!(frame.headroom_us(FrameRate::FPS_60), Some(16_666 - 9_620));
        assert_eq!(frame.headroom_us(FrameRate::FPS_50), Some(20_000 - 9_620));
    }

    #[test]
    fn a_pass_that_overran_its_frame_has_no_headroom_rather_than_a_negative_one() {
        // Which is the case worth reporting honestly: a pass that took longer
        // than the frame it was for has nothing left, and wrapping round to a
        // huge number would read as the healthiest frame on the machine.
        let frame = FrameTimings {
            media_us: 0,
            playback_us: 30_000,
            compose_us: 0,
        };
        assert_eq!(frame.headroom_us(FrameRate::FPS_60), Some(0));
    }

    #[test]
    fn a_frame_that_cost_nothing_has_the_whole_budget() {
        assert_eq!(
            FrameTimings::default().headroom_us(FrameRate::FPS_60),
            Some(16_666)
        );
    }

    #[test]
    fn disk_usage_matches_the_service_console_readout() {
        // Shown as "287 GB / 500 GB".
        let percent = telemetry(Vec::new()).disk_percent().expect("known total");
        assert!((percent - 57.4).abs() < 0.05, "got {percent}");
    }

    #[test]
    fn disk_usage_is_unknown_rather_than_zero_when_the_total_is_unknown() {
        let unknown = ServerTelemetry {
            disk_total_bytes: 0,
            ..telemetry(Vec::new())
        };
        assert_eq!(unknown.disk_percent(), None);
    }

    fn playhead(position_ms: u64, duration_ms: u64) -> LayerPlayhead {
        LayerPlayhead {
            layer_id: LayerId::new(),
            slot: 1,
            media_item_id: MediaItemId::new(),
            frame: 25,
            frame_count: 100,
            // A stand-in clip: eight bits, which is what most material is.
            decode_depth: crate::depth::DecodeDepth::Eight,
            position_ms,
            duration_ms,
            speed_ms_per_second: 1_000.0,
        }
    }

    #[test]
    fn a_playhead_says_how_far_through_the_clip_it_is() {
        let head = playhead(1_000, 4_000);
        assert!((head.fraction().expect("a length") - 0.25).abs() < 1e-6);
        assert!((playhead(0, 4_000).fraction().expect("a length")).abs() < 1e-6);
        assert!((playhead(4_000, 4_000).fraction().expect("a length") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_clip_with_no_length_has_no_bar_to_draw_rather_than_an_empty_one() {
        // Nothing can say how far through a clip of no length something is, and
        // a bar at zero would claim it had just started.
        assert_eq!(playhead(0, 0).fraction(), None);
    }

    #[test]
    fn a_layer_with_no_playhead_is_left_out_of_the_status_rather_than_zeroed() {
        // "Nothing is playing on that layer" and "that layer has just started"
        // must not encode the same way, so an empty list is absent from the JSON
        // altogether.
        let quiet = telemetry(Vec::new());
        let json = serde_json::to_value(&quiet).expect("serialises");
        assert!(json.get("playheads").is_none(), "{json}");

        let busy = ServerTelemetry {
            playheads: vec![playhead(1_000, 4_000)],
            ..telemetry(Vec::new())
        };
        let json = serde_json::to_value(&busy).expect("serialises");
        assert_eq!(
            json["playheads"][0]["position_ms"], 1_000,
            "and a layer that is playing says where it is: {json}"
        );
        assert_eq!(
            json["playheads"][0]["frame"], 25,
            "in frames as well as in milliseconds: {json}"
        );
    }

    #[test]
    fn drops_on_an_inactive_output_do_not_raise_an_alarm() {
        assert!(telemetry(vec![output(true, 3)]).is_dropping_frames());
        assert!(!telemetry(vec![output(false, 3)]).is_dropping_frames());
        assert!(!telemetry(vec![output(true, 0)]).is_dropping_frames());
    }
}
