//! The render loop.
//!
//! The rules it exists to keep (house rule §5, §17):
//!
//! - No filesystem, network or database access on this path. Decoding happens
//!   on the media loader's thread; the loop only ever collects what is already
//!   finished and copies it, which is cheap.
//! - No allocation per frame after initialisation.
//! - A failing output degrades; it never stops the loop.
//! - The loop runs whether or not the core is connected, and whether or not a
//!   show is loaded. A renderer with no show presents black, because
//!   stopping would make "no show" and "renderer broken" look the same from
//!   the auditorium.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ovs_types::config::Show;
use ovs_types::id::{MediaItemId, OutputId};
use ovs_types::telemetry::OutputTelemetry;

use crate::backend::{Compositor, CompositorFactory, FrameRequest, OutputBackend};
use crate::buses::BusGains;
use crate::frame_clock::FrameClock;
use crate::media::{MediaLoader, MediaReport, Residency};
use crate::pieces::OutputMapping;
use crate::playback::{Playback, PlaybackReport};
use crate::scene::{compose_test_pattern, compose_whole_composition, Scene, TextureKey};
use crate::show_state::{SharedShowState, ShowState};
use ovs_types::config::TestPatternMode;
use ovs_types::geometry::FrameRate;
// Only the streaming path names a size directly; everything else works in
// whatever the surface reports. The encoder is Unix-only because ffmpeg is.
#[cfg(unix)]
use ovs_types::geometry::SizePx;

/// What an output is, which decides who may replace it and what it draws.
///
/// **Not two bools.** This was `on_the_network`, and two things turned on it:
/// which outputs the planner may hand a connector to, and which ones may be
/// replaced without rebuilding a panel. A System screen needs both answers and
/// neither of them is the network's, so a second bool would have made four
/// states of which one — on the network *and* the console — is nonsense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A connector carrying part of the show.
    ///
    /// The planner may give it an output of the show, it draws that output's
    /// pieces of the composition, and it is replaced when the show's list of
    /// connectors changes.
    Panel,
    /// A feed published on the network.
    ///
    /// Has no connector and takes none. Replaced when the show's list of network
    /// outputs changes, and by nothing else — a panel refusing a mode must not
    /// take the streams down with it (house rule §17).
    Network,
    /// The connector the System screen is on.
    ///
    /// Takes a socket like a panel, and like a panel that stops it being a media
    /// output — but the show says nothing about it and never should: which
    /// connector the operator's own screen is on is machine wiring, and moving a
    /// show between servers must not move it. It draws one picture, sent by the
    /// process that hosts the browser, and no part of the composition.
    Console,
}

/// One output the loop is driving.
struct DrivenOutput {
    backend: Box<dyn OutputBackend>,
    clock: FrameClock,
    /// What this output is.
    kind: Kind,
    /// Which rectangles of the composition land where on this output.
    ///
    /// Rebuilt each pass from the show. It is a handful of multiplications
    /// per piece and the alternative is cache invalidation, which is where the
    /// bugs are: a mapping that failed to notice an edit would put content on
    /// the wrong wall until the next restart.
    mapping: OutputMapping,
    /// This output's pieces as quads, reused between frames.
    pieces: Scene,
    /// This output's own test pattern, while it is showing one.
    ///
    /// Per output, because every one of them says something different: its own
    /// size, its own rate and its own name.
    pattern: Option<crate::pattern::ServicePattern>,
    /// The pieces that land on this output, drawn on black.
    ///
    /// Beside the service pattern rather than sharing its slot: they are
    /// different pictures answering different questions, and a machine
    /// switching between the two should not pay for a rebuild each time.
    pieces_pattern: Option<crate::pattern::pieces::PiecePattern>,
    /// What this output was last successfully bound to sample.
    ///
    /// An output still pointing at a composition that has been replaced would
    /// be sampling freed memory, so the binding is recorded rather than assumed.
    bound: crate::backend::CompositionSource,
    /// Set when the output reported itself permanently gone.
    ///
    /// Its pieces stay dark and the loop stops presenting to it, but the other
    /// outputs carry on. Never silently reassigned to another connector.
    faulted: Option<String>,
    /// Transient present failures since the last success, for diagnostics.
    consecutive_failures: u32,
    /// What the show calls this output, while a show names it.
    ///
    /// The caption on its test pattern, because the owner asked for it
    /// (2026-09-26: "Er soll den Output Namen anzeigen nicht eine generische
    /// Nummer des Outputs"). It overrules what stood here before — the position
    /// in the list — and the reasoning for that is still true and is no longer
    /// the operator's problem: a socket's own label is whatever is plugged into
    /// it, and this is the name they gave the wall.
    ///
    /// `None` when no show names it, and then the position is used: an engineer
    /// lining up a panel nobody has patched has nothing else to call it.
    configured_name: Option<String>,
    /// Whether [`mapping`](Self::mapping) is what this output is really drawing.
    ///
    /// False for an output the show says nothing about, which shows the
    /// whole composition instead. Recorded rather than inferred from the
    /// mapping being empty, because the mapping is left as it was in that case
    /// and would otherwise still describe a show that has since been closed.
    mapped: bool,
}

/// How many presents in a row the System screen may fail before it is left dark.
///
/// Small on purpose. A wall's failures are worth retrying and a browser's are
/// not — see the comment where this is used. Three is enough to step over a
/// swapchain being rebuilt, which is the one transient failure that is expected,
/// and few enough that a connector the driver cannot present on is let go of
/// within a twentieth of a second.
const CONSOLE_GIVES_UP_AFTER: u32 = 3;

/// What the pattern calls the composition.
///
/// The same word the System screen uses for it, because an operator standing in
/// front of both should not have to translate.
const COMPOSITION_TITLE: &str = "COMPOSITION";

/// One surface being encoded for somebody watching.
#[cfg(unix)]
struct Streaming {
    /// The encoder, and the thread it runs on.
    worker: crate::stream::StreamWorker,
    /// What it was opened for. A surface that changes mode gets a new encoder,
    /// because a decoder told one size will not take another mid-stream.
    size: SizePx,
    /// Likewise the rate, which is in the stream's own headers.
    rate: FrameRate,
    /// Whether the watcher has been told what this opened as.
    ///
    /// Once, not per frame: the answer does not change while the encoder lives,
    /// and a surface that changes mode gets a new `Streaming` anyway.
    announced: bool,
    /// When this surface may next be handed a picture.
    ///
    /// The loop runs faster than any surface presents — it wakes, decides
    /// whether anything is due, and sleeps again — so a stream fed once per
    /// pass is fed several times per frame. Measured on the test machine before
    /// this existed: a composition running at 59.94 was encoded at 149 pictures
    /// a second, and a stream asked for six megabits carried fifteen. The
    /// encoder was doing exactly what it was told; it was being told two and a
    /// half times too often.
    next_due: Instant,
}

/// How many bits a surface is worth, by how big it is.
///
/// Measured on the test machine with a moving picture: 720p60 lands near six
/// megabits and looks like the wall; 360p60 near one and a half. A preview is
/// judged on whether an operator can see what is happening, not on grain, so
/// this is deliberately at the low end of what looks right.
#[cfg(unix)]
fn stream_bitrate(size: SizePx) -> u32 {
    let pixels = u64::from(size.width) * u64::from(size.height);
    // Six megabits at 1280x720, in proportion either side of it, and never so
    // little that a picture falls apart.
    let bits = pixels * 6_000_000 / (1280 * 720);
    bits.clamp(500_000, 20_000_000) as u32
}

/// Drives a set of outputs at their configured rates.
pub struct RenderLoop {
    outputs: Vec<DrivenOutput>,
    /// Telemetry, one entry per output and in the same order.
    ///
    /// Kept beside the outputs rather than inside them so that reporting can
    /// hand back a contiguous slice without building a list per call.
    telemetry: Vec<OutputTelemetry>,
    /// Where each playing layer's playhead is, refilled when reporting asks.
    ///
    /// Kept here for the same reason as the per-output telemetry above it: a
    /// report happens about once a second and reuses this list, so nothing on
    /// the render path ever builds one.
    playheads: Vec<ovs_types::telemetry::LayerPlayhead>,
    state: Arc<SharedShowState>,
    /// The canvas every output samples.
    ///
    /// A frame is two passes: the layers are drawn once, here, and each output
    /// then draws its pieces sampling the result. Drawing the
    /// layers once per output instead would be the same picture only by
    /// coincidence, and would cost as many passes as there are walls.
    compositor: Box<dyn Compositor>,
    /// Makes a new composition when the show asks for a different size.
    ///
    /// Rebuilt rather than resized: the composition changes size only when a
    /// show is loaded, which is already a discontinuity — the phase and the
    /// timelines restart there too — and a rebuild is the same code
    /// path as starting up rather than a second one that has to agree with it.
    make_compositor: CompositorFactory,
    /// Composed once per pass and reused between them, so composing allocates
    /// nothing after the first few frames.
    scene: Scene,
    /// Decodes stills, on its own thread.
    loader: MediaLoader,
    /// What has a texture, what is on its way, and what could not be decoded.
    media: Residency,
    /// Which layer is playing what, and which frame of it is on the GPU.
    ///
    /// Stills and clips are deliberately not the same mechanism. A still is the
    /// same pixels for every layer showing it, so the residency shares one
    /// texture; a clip is at a different frame on every layer playing it, so it
    /// gets a player and a texture of its own.
    playback: Playback,
    /// Where the last pass's time went.
    ///
    /// Kept on the loop rather than returned in the report, because what an
    /// operator reads is the last frame rather than the last pass — a pass in
    /// which no output was due still did the media and the composition.
    timings: ovs_types::telemetry::FrameTimings,
    /// The media the current state refers to, refilled rather than rebuilt each
    /// pass so the render path allocates nothing.
    wanted: Vec<MediaItemId>,
    frame_index: u64,
    /// How many compositions have been built, including the first.
    compositions_built: u64,
    /// Configured outputs with no hardware driving them.
    ///
    /// Reported rather than worked around: §18's rule is that a missing output
    /// is a fault, and the operator needs to know which wall is dark.
    missing_outputs: Vec<crate::outputs::Missing>,
    /// Whether the log has already said that outputs are matched by order.
    warned_about_order: bool,
    /// Which missing outputs have already been reported.
    ///
    /// A missing wall is missing on every pass; saying so sixty times a second
    /// would bury the line that said it first.
    reported_missing: std::collections::BTreeSet<OutputId>,

    /// Whether anything has ever been drawn into the composition.
    ///
    /// Freeze holds what is there, and until the first pass what is there is
    /// undefined.
    composed_once: bool,
    /// The card's own management library, where the machine has one.
    nvml: Option<crate::nvml::Nvml>,
    /// The size the test pattern was last generated at, while it is on.
    ///
    /// `None` means there is none on the GPU: either it is off, or the
    /// composition changed size and the old one is the wrong shape.
    /// The composition's own test pattern, while it is showing one.
    test_pattern: Option<crate::pattern::ServicePattern>,
    /// One encoder per surface somebody is watching.
    ///
    /// Empty on a machine nobody has a client open on, which is the ordinary
    /// state of a server in a rack: an encoder that nobody watches is a cost
    /// with no reader.
    #[cfg(unix)]
    streams: std::collections::BTreeMap<ovs_types::telemetry::StreamSurface, Streaming>,
    /// Surfaces an encoder could not be opened for.
    ///
    /// Kept so the attempt is made once rather than every pass: a machine with
    /// no encoder will not grow one while the show runs, and whoever is
    /// watching has already been told why.
    #[cfg(unix)]
    refused: std::collections::BTreeSet<ovs_types::telemetry::StreamSurface>,
    /// Pictures the encoders have finished, reused between passes.
    #[cfg(unix)]
    chunks: Vec<crate::stream::Chunk>,
    /// Which parts of the composition the show's mapping takes.
    ///
    /// Worked out where the show is to hand rather than where the mask is
    /// wanted: this is read once per streamed frame per watcher, and the
    /// answer only changes when the show does.
    mask_regions: Vec<crate::mask::Region>,
    /// Whether the surfaces are currently copying their frames out.
    ///
    /// Tracked so the surfaces are told when it changes rather than every
    /// frame.
    readback: bool,
    /// Where the System screen's pictures come from, while one is being shown.
    ///
    /// Held here rather than in the binary, so the pull happens where the
    /// drawing does — the same place the test pattern is drawn — and the render
    /// thread never waits on a socket to find out whether anything changed.
    #[cfg(unix)]
    console: Option<crate::console::ConsoleSource>,
    /// Which of the console's frames has been put on the card.
    ///
    /// The cheap half. A System screen nobody is touching paints nothing for
    /// minutes, and comparing this against the source's count is the whole
    /// per-frame cost of having one.
    #[cfg(unix)]
    console_uploaded: u64,
}

/// Whether this build should stand in for video with a generated clip.
///
/// A development aid and never a fallback: until a decoder is linked, it is the
/// only way to drive the whole path — trigger, playhead, player, upload — on a
/// machine with no codec. A server that quietly did this without being asked
/// would put a colour cycle where an operator expected their clip.
fn synthetic_video() -> bool {
    std::env::var("OVS_RENDERER_SYNTHETIC_VIDEO")
        .is_ok_and(|value| matches!(value.trim(), "1" | "on" | "true" | "yes"))
}

/// A small picture of what one output is showing.
#[derive(Debug, Clone)]
pub struct Preview {
    /// Which output this is a picture of.
    pub output_id: ovs_types::id::OutputId,
    /// Its operator-visible name.
    pub name: String,
    /// The picture's size.
    pub size: ovs_types::geometry::SizePx,
    /// Tightly packed RGBA.
    pub pixels: Vec<u8>,
}

/// Microseconds since `began`, saturating rather than wrapping.
///
/// A pass that somehow took more than an hour is a pass nobody is reading a
/// microsecond figure for, and saturating is the honest answer for it.
fn micros_since(began: Instant) -> u32 {
    u32::try_from(Instant::now().saturating_duration_since(began).as_micros()).unwrap_or(u32::MAX)
}

/// What one pass of the loop did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TickReport {
    /// Outputs that presented a frame this pass.
    pub presented: usize,
    /// Outputs that failed transiently this pass.
    pub failed: usize,
    /// Outputs that became permanently faulted this pass.
    pub faulted: usize,
    /// Outputs that were not due yet.
    pub skipped: usize,
    /// Whether the composition pass drew this frame.
    pub composed: bool,
    /// Whether the frame was held because the show is frozen.
    pub held: bool,
    /// What the media pass did.
    pub media: MediaReport,
    /// What the playback pass did.
    pub playback: PlaybackReport,
}

impl RenderLoop {
    /// Builds a loop driving `outputs`, starting its clocks at `epoch`.
    ///
    /// # Errors
    ///
    /// Returns the name of the first output with an unusable refresh rate, or
    /// the reason the media loader could not be started, rather than dividing by
    /// zero or decoding on the render thread somewhere deeper.
    pub fn new(
        make_compositor: CompositorFactory,
        outputs: Vec<Box<dyn OutputBackend>>,
        state: Arc<SharedShowState>,
        epoch: Instant,
    ) -> Result<Self, String> {
        let (mut driven, telemetry) = Self::prepare(outputs, epoch)?;

        let loader = MediaLoader::spawn()
            .map_err(|error| format!("cannot start the media loader: {error}"))?;

        // Something to draw into before any show has said how big it should
        // be. The first show that says replaces it.
        let compositor = make_compositor(DEFAULT_COMPOSITION)
            .map_err(|error| format!("cannot build a composition: {error}"))?;
        let source = compositor.source();
        for output in &mut driven {
            match output.backend.bind_composition(source) {
                Ok(()) => output.bound = source,
                Err(error) => tracing::warn!("{error}"),
            }
        }

        Ok(Self {
            outputs: driven,
            telemetry,
            playheads: Vec::new(),
            state,
            compositor,
            make_compositor,
            scene: Scene::new(),
            loader,
            media: Residency::new(),
            playback: Playback::new(epoch, synthetic_video()),
            timings: ovs_types::telemetry::FrameTimings::default(),
            wanted: Vec::new(),
            frame_index: 0,
            compositions_built: 1,
            missing_outputs: Vec::new(),
            warned_about_order: false,
            reported_missing: std::collections::BTreeSet::new(),

            composed_once: false,
            nvml: None,
            test_pattern: None,
            #[cfg(unix)]
            streams: std::collections::BTreeMap::new(),
            #[cfg(unix)]
            refused: std::collections::BTreeSet::new(),
            // Nothing is open yet, so nothing is mapped. The first tick
            // replaces this with whatever the show says.
            mask_regions: vec![crate::mask::Region::WHOLE],
            #[cfg(unix)]
            chunks: Vec::new(),
            readback: true,
            #[cfg(unix)]
            console: None,
            #[cfg(unix)]
            console_uploaded: 0,
        })
    }

    /// Turns backends into outputs this loop can drive.
    fn prepare(
        outputs: Vec<Box<dyn OutputBackend>>,
        epoch: Instant,
    ) -> Result<(Vec<DrivenOutput>, Vec<OutputTelemetry>), String> {
        let mut driven = Vec::with_capacity(outputs.len());
        let mut telemetry = Vec::with_capacity(outputs.len());

        for backend in outputs {
            let mode = backend.mode();
            let clock = FrameClock::new(mode.refresh, epoch)
                .ok_or_else(|| format!("output {} has an unusable refresh rate", backend.name()))?;

            // Where this one physically is, for a console to print under the
            // output's own name. Absent on a backend that is not a panel — a
            // headless one in a test, an offscreen composition — which is the
            // honest answer rather than a blank socket.
            let identity = backend.identity();
            telemetry.push(OutputTelemetry {
                output_id: backend.id(),
                name: backend.name().to_owned(),
                active: true,
                size: mode.size,
                refresh: mode.refresh,
                actual_fps: 0.0,
                frame_render_time_us: 0,
                dropped_frames: clock.dropped_frames(),
                display: identity
                    .as_ref()
                    .and_then(|identity| identity.model.clone()),
                connector: identity
                    .as_ref()
                    .and_then(|identity| identity.connector.clone()),
                // Asked now and asked again every pass. A panel has no
                // swapchain yet at this point and answers `Unknown` for the
                // cable, which is the truth until it is brought up.
                composition_depth: backend.composition_depth(),
                output_depth: backend.output_depth(),
                // Nothing is mapped until a show is loaded and the planner has
                // matched this socket to one of its outputs. Refreshed every
                // pass below, where the answer is known.
                showing_pieces: false,
            });
            driven.push(DrivenOutput {
                backend,
                clock,
                // Set by `adopt_network` for the ones it builds. False here
                // because every other path through this function is building an
                // output that drives something.
                kind: Kind::Panel,
                mapping: OutputMapping::new(),
                pieces: Scene::new(),
                mapped: false,
                bound: crate::backend::CompositionSource::Nothing,
                faulted: None,
                consecutive_failures: 0,
                pattern: None,
                pieces_pattern: None,
                // Filled by `rebuild_mappings`, which runs before anything draws.
                configured_name: None,
            });
        }

        Ok((driven, telemetry))
    }

    /// Replaces the outputs this loop drives.
    ///
    /// For when the configuration changes what the hardware should be doing: a
    /// show that asks for 1920x1080 at 60 on a connector currently running at
    /// something else needs the mode set again, and setting a mode means building
    /// the output again.
    ///
    /// The old outputs are dropped, which is what releases their displays. A
    /// wall goes dark for the second or so the new mode takes to come up — which
    /// is why this is only ever called when the answer has actually changed, and
    /// never for a show that asks for what is already running.
    ///
    /// # Errors
    ///
    /// Returns a message when an output's refresh rate is unusable.
    pub fn adopt(
        &mut self,
        outputs: Vec<Box<dyn OutputBackend>>,
        at: Instant,
    ) -> Result<(), String> {
        let (driven, telemetry) = Self::prepare(outputs, at)?;

        // **The streams are not this call's to replace.** They were, and it
        // meant a panel refusing a mode destroyed every NDI output on the
        // machine — and they never came back, because what decides whether a
        // stream is rebuilt is whether the show's list differs from the list
        // last built, and that list still said they were running. One output's
        // trouble became every output's, which is house rule §17 backwards
        // (owner, 2026-09-25: "es sterben alle ausgänge wenn einer stirbt").
        //
        // So this is the mirror of `adopt_network`: each replaces its own kind
        // and leaves the other where it is.
        let kept = std::mem::take(&mut self.outputs);
        let kept_telemetry = std::mem::take(&mut self.telemetry);
        let was = kept.len();
        let mut streams = Vec::new();
        let mut stream_telemetry = Vec::new();
        let blank = crate::scene::Scene::new();
        for (mut output, telemetry) in kept.into_iter().zip(kept_telemetry) {
            // The streams and the System screen both stay. A panel refusing a
            // mode must not take the network feeds down with it, and it must not
            // take the operator's own screen down either — that screen is how
            // they would find out what went wrong.
            if output.kind != Kind::Panel {
                streams.push(output);
                stream_telemetry.push(telemetry);
                continue;
            }

            // **Blanked on the way out.** Releasing a display destroys its
            // swapchain and its surface, and nothing in Vulkan says "turn this
            // connector off" — the panel goes on scanning out whatever was last
            // presented to it for as long as the mode stays set. A wall removed
            // from the show would otherwise keep showing the last frame of a show
            // it is no longer part of (owner, 2026-09-25: "gerade HDMI-0 entfernt
            // und screen ist immer noch aktiv").
            //
            // An empty scene draws nothing and the canvas clears to black, which
            // is what the panel holds afterwards.
            //
            // A connector that is only being rebuilt — a mode change — is blanked
            // too, and costs nothing by it: setting a mode blanks the panel
            // anyway.
            if let Err(error) = output.backend.present(
                FrameRequest {
                    index: self.frame_index,
                },
                &blank,
            ) {
                // Warned about and stepped over. It is being let go of either
                // way, and a renderer that refused to rebuild its walls because
                // one would not take a last frame would be worse than a stale
                // picture on a socket nobody is using.
                tracing::warn!(
                    output = %output.backend.name(),
                    "could not blank this output before releasing it: {error}"
                );
            }
        }

        tracing::info!(
            was,
            now = driven.len() + streams.len(),
            streams = streams.len(),
            "the outputs this renderer drives have changed"
        );

        // The rebuilt ones first, so a stream stays at the end of the list
        // where `adopt_network` also puts it.
        self.outputs = driven;
        self.telemetry = telemetry;
        let physical = self.outputs.len();
        self.outputs.extend(streams);
        self.telemetry.extend(stream_telemetry);

        // Everything downstream of a *rebuilt* output has to be built again:
        // what it samples, and which pieces of the show it draws. The streams
        // were not rebuilt and are already bound to this composition.
        let source = self.compositor.source();
        for output in self.outputs.iter_mut().take(physical) {
            match output.backend.bind_composition(source) {
                Ok(()) => output.bound = source,
                Err(error) => tracing::warn!("{error}"),
            }
        }
        // A wall that was missing before this may not be now, and one that is
        // missing now has not been said out loud yet.
        self.reported_missing.clear();
        self.warned_about_order = false;

        Ok(())
    }

    /// Lets go of every display output, so new ones can be built with none live.
    ///
    /// **The half that has to happen first.** Creating a display output while
    /// another is presenting moves that other one's picture onto the new panel —
    /// measured on two monitors on 2026-09-26, and the only arrangement out of
    /// six that failed. Every one that built all the outputs before presenting
    /// anything was correct.
    ///
    /// So the caller releases, then builds, then adopts. Between the release and
    /// the adoption the panels are dark, which is a fraction of a second and the
    /// price of them being right afterwards.
    ///
    /// Each output is blanked on its way out. Releasing a display destroys its
    /// swapchain and its surface, and nothing in Vulkan says "turn this
    /// connector off" — a panel goes on scanning out whatever was last presented
    /// to it for as long as the mode stays set.
    ///
    /// The network outputs are untouched: they have no connector and none of
    /// this is about them.
    pub fn release_displays(&mut self) {
        let blank = crate::scene::Scene::new();
        let kept = std::mem::take(&mut self.outputs);
        let kept_telemetry = std::mem::take(&mut self.telemetry);
        let mut released = 0_usize;

        for (mut output, telemetry) in kept.into_iter().zip(kept_telemetry) {
            if output.kind == Kind::Network {
                self.outputs.push(output);
                self.telemetry.push(telemetry);
                continue;
            }

            if let Err(error) = output.backend.present(
                FrameRequest {
                    index: self.frame_index,
                },
                &blank,
            ) {
                // Warned about and stepped over: it is being let go of either
                // way, and refusing to rebuild the walls because one would not
                // take a last frame is worse than a stale picture on a socket
                // nobody is using.
                tracing::warn!(
                    output = %output.backend.name(),
                    "could not blank this output before releasing it: {error}"
                );
            }
            released += 1;
        }

        if released > 0 {
            tracing::info!(released, "every display output was let go of");
        }

        // The console's source goes with its output. A new one is made when the
        // group is built again, because it carries the panel's size.
        #[cfg(unix)]
        {
            self.console = None;
            self.console_uploaded = 0;
        }
    }

    /// Installs a freshly built group of display outputs.
    ///
    /// Every one of them must have been built *after* [`Self::release_displays`]
    /// and before anything presented — that is the whole rule, and the reason
    /// these are two calls rather than one.
    ///
    /// # Errors
    ///
    /// Returns a message when one of the new outputs has an unusable refresh
    /// rate. Nothing is installed in that case, which leaves the panels dark
    /// until the next pass rather than half a group running.
    pub fn adopt_displays(
        &mut self,
        panels: Vec<Box<dyn OutputBackend>>,
        #[cfg(unix)] console: Option<(Box<dyn OutputBackend>, crate::console::ConsoleSource)>,
        at: Instant,
    ) -> Result<(), String> {
        let (panels, panel_telemetry) = Self::prepare(panels, at)?;

        #[cfg(unix)]
        let (console_output, console_telemetry, source) = match console {
            Some((backend, source)) => {
                let (mut driven, telemetry) = Self::prepare(vec![backend], at)?;
                for output in &mut driven {
                    output.kind = Kind::Console;
                    // Its scene is set when its first picture arrives. Until
                    // then it draws nothing, which is black — and black is the
                    // honest picture of a console that has not painted yet.
                    output.pieces.clear();
                }
                (driven, telemetry, Some(source))
            }
            None => (Vec::new(), Vec::new(), None),
        };

        // The fresh ones go in front of the network outputs, which is where
        // `adopt_network` also expects to find them.
        let streams = std::mem::take(&mut self.outputs);
        let stream_telemetry = std::mem::take(&mut self.telemetry);

        self.outputs = panels;
        self.telemetry = panel_telemetry;
        #[cfg(unix)]
        {
            self.outputs.extend(console_output);
            self.telemetry.extend(console_telemetry);
            self.console = source;
            // A new source has painted nothing yet and counts from zero again.
            self.console_uploaded = 0;
        }

        let fresh = self.outputs.len();
        self.outputs.extend(streams);
        self.telemetry.extend(stream_telemetry);

        // Everything just built has to be told what to sample. The streams were
        // not rebuilt and are already bound to this composition.
        let source = self.compositor.source();
        for output in self.outputs.iter_mut().take(fresh) {
            match output.backend.bind_composition(source) {
                Ok(()) => output.bound = source,
                Err(error) => tracing::warn!("{error}"),
            }
        }

        tracing::info!(
            displays = fresh,
            streams = self.outputs.len() - fresh,
            "the display outputs were built as one group"
        );

        // A wall that was missing before this may not be now, and one that is
        // missing now has not been said out loud yet.
        self.reported_missing.clear();
        self.warned_about_order = false;
        Ok(())
    }

    /// Replaces only the outputs that publish on the network.
    ///
    /// The physical outputs are left exactly as they are — their clocks, their
    /// mappings and every frame they have counted. Rebuilding one means setting
    /// a mode, which takes a panel dark for about a second, and an operator
    /// adding or removing a network feed has not asked for that to happen to
    /// their walls.
    ///
    /// The old network outputs are dropped, which is what stops their senders
    /// and takes their names off the network.
    ///
    /// # Errors
    ///
    /// Returns a message when one of the new outputs has an unusable refresh
    /// rate. The outputs already running are untouched in that case.
    pub fn adopt_network(
        &mut self,
        outputs: Vec<Box<dyn OutputBackend>>,
        at: Instant,
    ) -> Result<(), String> {
        // Prepared before anything is dropped, so a bad rate leaves the
        // renderer exactly as it was rather than halfway.
        let (mut fresh, fresh_telemetry) = Self::prepare(outputs, at)?;
        for output in &mut fresh {
            output.kind = Kind::Network;
        }

        let was = self.outputs.len();
        let kept = std::mem::take(&mut self.outputs);
        let kept_telemetry = std::mem::take(&mut self.telemetry);
        for (output, telemetry) in kept.into_iter().zip(kept_telemetry) {
            if output.kind != Kind::Network {
                self.outputs.push(output);
                self.telemetry.push(telemetry);
            }
        }
        let physical = self.outputs.len();
        self.outputs.extend(fresh);
        self.telemetry.extend(fresh_telemetry);

        if was != self.outputs.len() || physical != self.outputs.len() {
            tracing::info!(
                was,
                now = self.outputs.len(),
                streams = self.outputs.len() - physical,
                "the outputs this renderer publishes on the network have changed"
            );
        }

        // Only the new ones. The kept outputs are already bound to this
        // composition, and rebinding them would be work for nothing.
        let source = self.compositor.source();
        for output in self.outputs.iter_mut().skip(physical) {
            match output.backend.bind_composition(source) {
                Ok(()) => output.bound = source,
                Err(error) => tracing::warn!("{error}"),
            }
        }

        Ok(())
    }

    /// The names this renderer is publishing on the network.
    ///
    /// For subtracting from what discovery found, so a server is never offered
    /// its own output as a source. Empty on a renderer with no network outputs,
    /// and short of one entry for a sender that has only just been made.
    #[must_use]
    pub fn published_ndi_names(&self) -> Vec<String> {
        self.outputs
            .iter()
            .filter_map(|output| output.backend.published_as())
            .collect()
    }

    /// The names of the outputs that go on the network.
    ///
    /// For the tests that hold the two adoptions apart. `published_ndi_names`
    /// answers from the senders, which a headless stand-in does not have.
    #[cfg(test)]
    #[must_use]
    pub fn published_names_for_test(&self) -> Vec<String> {
        self.outputs
            .iter()
            .filter(|output| output.kind == Kind::Network)
            .map(|output| output.backend.name().to_owned())
            .collect()
    }

    /// Returns how many outputs the loop is driving, faulted ones included.
    ///
    /// Zero is an ordinary state and not a fault: a machine whose only panel is
    /// the System screen has no show outputs, and neither has one waiting for a
    /// show to name a connector. The renderer still composes, and the
    /// previews it produces are what every client is looking at.
    #[must_use]
    pub fn output_count(&self) -> usize {
        self.outputs.len()
    }

    /// Returns true when at least one output is still being driven.
    #[must_use]
    pub fn is_rendering(&self) -> bool {
        self.outputs.iter().any(|output| output.faulted.is_none())
    }

    /// Returns the earliest moment any output is next due.
    ///
    /// The loop sleeps until this rather than spinning.
    #[must_use]
    pub fn next_deadline(&self, now: Instant) -> Instant {
        self.outputs
            .iter()
            .filter(|output| output.faulted.is_none())
            .map(|output| output.clock.next_deadline())
            .min()
            .unwrap_or_else(|| now + Duration::from_millis(100))
    }

    /// Runs one pass: presents to every output that is due.
    pub fn tick(&mut self, now: Instant) -> TickReport {
        // One wait-free read per pass, not per output, so every output in a pass
        // composites the same state. Composition itself is Phase 4; what this
        // proves is that the state is reachable from here without blocking.
        let state = self.state.load();
        self.match_composition_to(&state);

        // Media first, so an image that finished decoding since the last pass is
        // on the GPU before the frame that wants it is composed. The decoding
        // happened on another thread; what is left is a copy into a buffer that
        // is already mapped — which is not free, and is timed because it was
        // once assumed to be (see `FrameTimings`).
        let media_began = Instant::now();
        let mut report = TickReport {
            media: self.update_media(&state, now),
            ..TickReport::default()
        };
        self.timings.media_us = micros_since(media_began);

        // Then the clips. Each playing layer's frame goes to the GPU before the
        // frame that shows it is composed, for the same reason the stills do.
        let playback_began = Instant::now();
        let catalogue = self.state.media();
        report.playback = self
            .playback
            .update(&state, &catalogue, self.compositor.as_mut(), now);
        self.timings.playback_us = micros_since(playback_began);

        // What an operator pressed, before anything is measured against it.
        if let Some(group) = self.state.take_counter_reset() {
            self.reset_counters(group);
        }

        // Whether anybody is watching. Told to the surfaces rather than checked
        // by them, and only when it changes: with previews off the frame is
        // never copied off the card, which is a megabyte per megapixel per
        // frame that nobody was going to look at.
        let watched = self.state.previews_enabled();
        if watched != self.readback {
            self.readback = watched;
            self.compositor.set_readback(watched);
            for output in &mut self.outputs {
                output.backend.set_readback(watched);
            }
        }

        // The composition's test pattern replaces the show rather than covering
        // it: while it is on it *is* the composition, so the pieces still cut it
        // up and each wall shows its own region of it. That is what makes it a
        // test of the mapping and not only of the cabling.
        //
        // The other pattern is drawn per output, further down, and deliberately
        // does not come through here: it is not a composition anything is
        // mapped from.
        let pattern = self.state.test_pattern();
        if pattern == TestPatternMode::Composition {
            self.draw_composition_pattern(&state, now);
            compose_test_pattern(&mut self.scene);
        } else {
            if self.test_pattern.take().is_some() {
                self.compositor.drop_texture(TextureKey::TestPattern);
            }

            self.scene.compose(&state, self.playback.showing());

            // How the three buses combine. Carried on the scene rather than
            // applied to its quads: each bus is composited on its own and the
            // crossfader mixes the results, so a layer's own opacity and the
            // crossfader are two different things now.
            //
            // The curve is show configuration, so a scene with no show
            // loaded keeps the default gains and has nothing to draw anyway.
            if let Some(show) = state.show.as_deref() {
                self.scene.set_gains(BusGains::from_master(
                    &state.master,
                    show.master.crossfader_curve,
                ));
            }
        }

        let index = self.frame_index;

        // Freeze holds the visible output and nothing else.
        // The loop still runs, the clock still runs, state still arrives and
        // the outputs still present — they simply present the composition that
        // is already there. That is what makes a show resume coherently rather
        // than jumping to wherever it would have been.
        //
        // The first frame is always composed, whatever the state says. Until
        // the composition has been drawn once its contents are undefined, and
        // holding undefined contents on a wall is not freezing, it is a fault
        // that looks like one.
        let frozen = state.master.freeze && self.composed_once;

        if frozen {
            report.held = true;
            // A frozen frame is not a cheap frame, it is a frame that was not
            // drawn. Reporting the last one's cost would make a frozen show look
            // like a fast one.
            self.timings.compose_us = 0;
        } else {
            // A failure here leaves the previous composition on the GPU, so the
            // outputs present a held frame rather than a black one — the right
            // way round for a transient fault.
            // **Free-running, and the same for every pass in this frame.** An
            // effect that moves is moving against a clock, and two layers
            // carrying one effect have to move together. Nothing about the
            // show reaches it -- not the trigger, not the transport, not the
            // master speed -- for the reason the test pattern's own clock is
            // free: a glitch is a look, not a cue.
            //
            // The wall clock, folded into a day so that the float keeps its
            // precision. At a day's worth of seconds an f32 still resolves
            // about a hundredth of a second, which is finer than anything this
            // drives; a float carrying seconds since 1970 would be quantised
            // to several seconds and nothing would move at all.
            self.compositor.set_effect_time(effect_seconds());

            let compose_began = Instant::now();
            match self.compositor.compose(FrameRequest { index }, &self.scene) {
                Ok(()) => {
                    report.composed = true;
                    self.composed_once = true;
                }
                Err(error) => tracing::warn!("the composition pass failed: {error}"),
            }
            self.timings.compose_us = micros_since(compose_began);
        }

        self.update_streams(&state);

        self.rebuild_mappings(state.show.as_deref());

        // And the other pattern: one per output, at that output's own mode and
        // with that output's own name on it, in place of whatever the mapping
        // would have put there.
        if pattern == TestPatternMode::Outputs {
            self.draw_output_patterns(now);
        } else {
            self.forget_output_patterns();
        }

        if pattern == TestPatternMode::Pieces {
            self.draw_piece_patterns();
        } else {
            self.forget_piece_patterns();
        }

        // After the mappings and the patterns, because both of those walk every
        // output and neither has any business with this one: the System screen
        // shows what the browser painted and no part of the show.
        #[cfg(unix)]
        self.update_console();

        for output in &mut self.outputs {
            if output.faulted.is_some() {
                continue;
            }
            if now < output.clock.next_deadline() {
                report.skipped += 1;
                continue;
            }

            let began = Instant::now();
            let result = output
                .backend
                .present(FrameRequest { index }, &output.pieces);
            let render_time = Instant::now().saturating_duration_since(began);

            match result {
                Ok(()) => {
                    // A backend that had hardware to bring up says so once, and
                    // its accounting starts from there: the second a panel
                    // spends setting a mode is not a second of dropped frames.
                    if output.backend.just_started() {
                        output.clock.begin(now);
                    }
                    output.clock.record_present(now, render_time);
                    output.consecutive_failures = 0;
                    report.presented += 1;
                }
                Err(error) if error.is_permanent() => {
                    // Dark, and stays dark until an operator deals with it. The
                    // other outputs are untouched.
                    tracing::error!(output = output.backend.name(), "{error}");
                    output.faulted = Some(error.to_string());
                    report.faulted += 1;
                }
                Err(error) => {
                    output.consecutive_failures = output.consecutive_failures.saturating_add(1);
                    // The clock is deliberately not advanced: an interval passed
                    // with nothing shown, which the next successful present will
                    // correctly count as a dropped frame.
                    tracing::warn!(
                        output = output.backend.name(),
                        failures = output.consecutive_failures,
                        "{error}"
                    );
                    report.failed += 1;

                    // **The System screen gives up where a wall would not.**
                    //
                    // A panel carrying a show retries for ever, because retrying
                    // is the only way it comes back and a dark wall is the thing
                    // this renderer exists to avoid. The console is not worth
                    // that: on 2026-09-26 its output failed its first present
                    // with `ERROR_UNKNOWN` four times in a second, and two
                    // seconds later every fence on the device was timing out —
                    // including the composition's. One browser took two panels
                    // and a card with it.
                    //
                    // So it is faulted after a handful of failures. It stays
                    // dark, it says so once, and `follow_console` leaves it
                    // alone until the connector or the mode changes — which is
                    // an operator deciding something, which is when it should be
                    // tried again.
                    if output.kind == Kind::Console
                        && output.consecutive_failures >= CONSOLE_GIVES_UP_AFTER
                    {
                        tracing::error!(
                            output = output.backend.name(),
                            failures = output.consecutive_failures,
                            "the System screen cannot be presented on this connector; leaving it \
                             dark so the show is not taken with it"
                        );
                        output.faulted = Some(error.to_string());
                        report.faulted += 1;
                    }
                }
            }
        }

        self.frame_index = self.frame_index.saturating_add(1);
        report
    }

    /// Rebuilds the composition when the show asks for a different size.
    ///
    /// The composition is the show's canvas, and everything downstream of it
    /// is normalised, so a mismatch costs resolution rather than correctness —
    /// a 4K composition drawn into a 1080p image is the same picture with half
    /// the detail. On an LED wall that is the difference an operator notices
    /// and cannot explain.
    ///
    /// Media goes with the old composition, because the textures lived on it.
    /// That costs a decode, at the one moment where it costs nothing else: a
    /// show load is already a discontinuity, and the media the new show
    /// wants is usually not the media the old one had.
    /// Draws the composition's own test pattern, for this frame.
    ///
    /// Generated rather than shipped as a file, and redrawn every frame rather
    /// than uploaded once. Both for the same reason: a pattern that was scaled
    /// to fit would hide the mis-set mode it exists to reveal, and a pattern
    /// that did not move would not tell a live output from a frozen one.
    fn draw_composition_pattern(&mut self, state: &ShowState, _now: Instant) {
        let size = self.compositor.size();
        // What the composition is actually being rendered at, which is what the
        // caption has to say — never a number somebody assumed.
        let rate = state
            .show
            .as_deref()
            .map_or(FrameRate::FPS_60, |show| show.composition.render_rate);

        let mut built = false;
        if !self
            .test_pattern
            .as_ref()
            .is_some_and(|pattern| pattern.matches(size, rate, COMPOSITION_TITLE))
        {
            self.test_pattern = Some(crate::pattern::ServicePattern::new(
                size,
                rate,
                COMPOSITION_TITLE,
            ));
            built = true;
            tracing::info!(
                width = size.width,
                height = size.height,
                "the composition is showing the test pattern"
            );
        }

        // The composition's own frames, at the composition's own rate: the
        // pattern is bound to that and to nothing else (owner, 2026-10-16).
        let seconds = pattern_seconds(self.frame_index, rate);
        let Some(pattern) = self.test_pattern.as_mut() else {
            return;
        };

        if let Err(error) = self
            .compositor
            .upload_texture(TextureKey::TestPattern, pattern.draw(seconds))
        {
            // Once, when the pattern was built: a refusal now will be a refusal
            // next frame, and sixty of these a second would bury the one line
            // that explained it. The composition keeps whatever it had, which
            // is the show.
            if built {
                tracing::error!("the test pattern could not be drawn: {error}");
            }
        }
    }

    /// Replaces the System screen's output, and the source feeding it.
    ///
    /// Its own adoption for the same reason the network outputs have one: what
    /// happens to a panel must not happen to this. A wall refusing a mode is a
    /// wall; the same event taking the operator's console with it would leave
    /// them with no way to see what had gone wrong (house rule §17).
    ///
    /// `None` takes it away — the connector was given back, or there is no
    /// System screen configured. The output is blanked on the way out, because
    /// releasing a Vulkan display does not turn a connector off and a panel left
    /// scanning out the last frame of a console nobody is serving is worse than
    /// a black one.
    ///
    /// # Errors
    ///
    /// Returns a message when the new output has an unusable refresh rate. The
    /// outputs already running are untouched in that case.
    #[cfg(unix)]
    pub fn adopt_console(
        &mut self,
        output: Option<(Box<dyn OutputBackend>, crate::console::ConsoleSource)>,
        at: Instant,
    ) -> Result<(), String> {
        // Prepared before anything is dropped, so a bad rate leaves the renderer
        // exactly as it was rather than halfway.
        let (fresh, fresh_telemetry, source) = match output {
            Some((backend, source)) => {
                let (mut driven, telemetry) = Self::prepare(vec![backend], at)?;
                for output in &mut driven {
                    output.kind = Kind::Console;
                    // Its scene is set when its first picture arrives. Until
                    // then it draws nothing, which is black — and black is the
                    // honest picture of a console that has not painted yet.
                    output.pieces.clear();
                }
                (driven, telemetry, Some(source))
            }
            None => (Vec::new(), Vec::new(), None),
        };

        let kept = std::mem::take(&mut self.outputs);
        let kept_telemetry = std::mem::take(&mut self.telemetry);
        let blank = crate::scene::Scene::new();
        for (mut output, telemetry) in kept.into_iter().zip(kept_telemetry) {
            if output.kind != Kind::Console {
                self.outputs.push(output);
                self.telemetry.push(telemetry);
                continue;
            }

            if let Err(error) = output.backend.present(
                FrameRequest {
                    index: self.frame_index,
                },
                &blank,
            ) {
                tracing::warn!(
                    output = %output.backend.name(),
                    "could not blank the System screen before releasing it: {error}"
                );
            }
        }

        let before = self.outputs.len();
        self.outputs.extend(fresh);
        self.telemetry.extend(fresh_telemetry);
        self.console = source;
        // A new source has painted nothing yet, and the count it reports starts
        // again from zero. Anything else would skip its first picture.
        self.console_uploaded = 0;

        // Bound like any other output, although it will never sample the
        // composition: the binding is what its canvas needs to be complete, and
        // an output that had never been bound is one whose first present is a
        // different code path from all the others.
        let source = self.compositor.source();
        for output in self.outputs.iter_mut().skip(before) {
            match output.backend.bind_composition(source) {
                Ok(()) => output.bound = source,
                Err(error) => tracing::warn!("{error}"),
            }
        }

        tracing::info!(
            showing = self.console.is_some(),
            "the System screen's output has changed"
        );
        Ok(())
    }

    /// Puts the newest System screen picture on its output, if there is one.
    ///
    /// Costs one atomic load when nothing has changed, which is the ordinary
    /// case: the page paints on damage, so a screen nobody is touching produces
    /// no frames at all.
    #[cfg(unix)]
    fn update_console(&mut self) {
        let Some(source) = self.console.as_ref() else {
            return;
        };
        let arrived = source.arrived();
        if arrived == self.console_uploaded {
            return;
        }
        let Some(frame) = source.frame() else {
            return;
        };

        let Some(output) = self
            .outputs
            .iter_mut()
            .find(|output| output.kind == Kind::Console && output.faulted.is_none())
        else {
            return;
        };

        match output
            .backend
            .upload_texture(TextureKey::Console, frame.as_ref())
        {
            Ok(()) => {
                // Set every time rather than once, because an output that has
                // just been rebuilt has an empty scene and no other code path
                // fills this one's.
                crate::scene::compose_console(&mut output.pieces);
                // Only on success. Leaving it alone means the next pass tries
                // again with whatever is newest, which is what a transient
                // upload failure should cost: one frame, not the screen.
                self.console_uploaded = arrived;
            }
            Err(error) => {
                tracing::warn!("the System screen's picture could not be uploaded: {error}");
            }
        }
    }

    /// Draws each output's own test pattern, for this frame.
    ///
    /// Every one of them is a different picture: its own size, its own rate and
    /// its own name, so an engineer at the back of the room reads which wall
    /// they are looking at off the wall itself. None of it goes
    /// through the composition, so the mapping is bypassed entirely — this
    /// pattern answers "is this cable reaching this wall, in this mode" and
    /// nothing else.
    fn draw_output_patterns(&mut self, _now: Instant) {
        // **Counted in frames, each wall at its own rate** (owner, 2026-10-16:
        // "an nichts gebunden außer die Bildrate des Ausgangs"). A pass is a
        // frame for every output it presents to, so the index of the pass is
        // the frame number here; dividing by the wall's own rate is what keeps
        // the picture the same speed on a 50 Hz panel as on a 60 Hz one.
        let frames = self.frame_index;

        for (index, output) in self.outputs.iter_mut().enumerate() {
            if output.faulted.is_some() {
                continue;
            }

            // Not on the System screen. A commissioning pattern belongs on a
            // wall an engineer is lining up, and putting one on the operator's
            // own console would take away the screen they turn it off from.
            if output.kind == Kind::Console {
                continue;
            }

            let mode = output.backend.mode();
            // **What the operator called this wall.** It used to be its position
            // in the list, on the reasoning that a socket's label is whatever is
            // plugged into it — which is true, and is why the panel's own name is
            // not used here either. The owner overruled the conclusion
            // (2026-09-26): the show's name for an output is the one an engineer
            // on a ladder is given over the radio.
            //
            // The position remains for an output no show names, because then
            // there is nothing else to call it.
            let title = output
                .configured_name
                .clone()
                .unwrap_or_else(|| format!("OUTPUT {}", index + 1));

            let mut built = false;
            if !output
                .pattern
                .as_ref()
                .is_some_and(|pattern| pattern.matches(mode.size, mode.refresh, &title))
            {
                output.pattern = Some(crate::pattern::ServicePattern::new(
                    mode.size,
                    mode.refresh,
                    &title,
                ));
                built = true;
                tracing::info!(
                    output = output.backend.name(),
                    title,
                    width = mode.size.width,
                    height = mode.size.height,
                    "this output is showing the test pattern"
                );
            }

            let Some(pattern) = output.pattern.as_mut() else {
                continue;
            };

            let seconds = pattern_seconds(frames, mode.refresh);
            match output
                .backend
                .upload_texture(TextureKey::TestPattern, pattern.draw(seconds))
            {
                // In place of the mapping, not over it.
                Ok(()) => compose_test_pattern(&mut output.pieces),
                Err(error) => {
                    if built {
                        tracing::error!(
                            output = output.backend.name(),
                            "this output cannot draw the test pattern: {error}"
                        );
                    }
                }
            }
        }
    }

    /// Keeps one encoder per watched surface, and feeds them.
    ///
    /// Called every pass because a watcher can arrive or leave at any moment,
    /// and doing nothing is the common case: with nobody watching this is one
    /// look at an empty list.
    #[cfg(unix)]
    fn update_streams(&mut self, state: &ShowState) {
        let wanted = self.state.wanted_streams();

        // Surfaces nobody is watching any more. Dropping the encoder closes the
        // session on the card, which is what a machine with a session limit
        // needs it to do — and forgetting a refusal means the next watcher gets
        // a fresh attempt rather than inheriting an old one's answer.
        self.streams.retain(|surface, _| wanted.contains(surface));
        self.refused.retain(|surface| wanted.contains(surface));

        for surface in wanted.iter().copied() {
            // What this surface is and what it is running at. An output that is
            // not being driven cannot be watched: there is nothing to encode.
            let Some((size, rate)) = self.surface_shape(surface, state) else {
                continue;
            };

            // A surface that changed mode needs a new encoder: H.264 carries
            // its size in the stream, and a decoder that was told 1280x720 is
            // not going to accept 1920x1080 in the middle of it.
            if self
                .streams
                .get(&surface)
                .is_some_and(|streaming| streaming.size != size || streaming.rate != rate)
            {
                tracing::info!(%surface, "this surface changed mode; starting its stream again");
                self.streams.remove(&surface);
                self.refused.remove(&surface);
            }

            if !self.streams.contains_key(&surface) {
                if self.refused.contains(&surface) {
                    continue;
                }
                match crate::stream::StreamWorker::start(size, rate, stream_bitrate(size)) {
                    Ok(worker) => {
                        // What it opened is not known yet — the encoder comes up
                        // on its own thread, because taking a session on the
                        // card costs the wall frames when it is done here.
                        tracing::info!(
                            %surface,
                            width = size.width,
                            height = size.height,
                            "encoding this surface for somebody watching"
                        );
                        self.streams.insert(
                            surface,
                            Streaming {
                                worker,
                                size,
                                rate,
                                announced: false,
                                // Due at once, so a watcher gets a picture as
                                // soon as the encoder is up rather than a frame
                                // later.
                                next_due: Instant::now(),
                            },
                        );
                    }
                    Err(error) => {
                        // Said to the core rather than only to the log: a client
                        // waiting for pictures is entitled to know that none are
                        // coming, and why.
                        self.state
                            .publish_stream_failure(surface, error.to_string());
                        self.refused.insert(surface);
                        continue;
                    }
                }
            }

            // At the surface's own rate. Reading the picture back is not free
            // either, so this skips the copy as well as the encode.
            let now = Instant::now();
            if self
                .streams
                .get(&surface)
                .is_some_and(|streaming| now < streaming.next_due)
            {
                continue;
            }

            let Some(picture) = self.snapshot_surface(surface) else {
                continue;
            };

            let Some(streaming) = self.streams.get_mut(&surface) else {
                continue;
            };

            // From when it was due, so the average is the surface's own rate
            // rather than that rate minus however long a pass took to come
            // round: reckoned from now, a wall running at 59.94 was encoded at
            // 53.8 pictures a second. A loop that fell more than a frame behind
            // starts again from now instead, so it cannot then catch up by
            // encoding a burst.
            let period =
                Duration::from_nanos(streaming.rate.frame_duration_nanos().unwrap_or(16_666_667));
            streaming.next_due = if now.duration_since(streaming.next_due) < period {
                streaming.next_due + period
            } else {
                now + period
            };
            streaming.worker.submit(picture);
            self.chunks.clear();
            streaming.worker.collect(&mut self.chunks);

            // The encoder is opened on its own thread, so this is where finding
            // out it could not be opened happens.
            if let Some(reason) = streaming.worker.failure() {
                self.state.publish_stream_failure(surface, reason);
                self.refused.insert(surface);
                self.streams.remove(&surface);
                continue;
            }

            // Said once, on the pass the encoder comes up. A watcher is
            // entitled to know that a preview at a quarter speed is a decision
            // rather than a fault (owner decision Q13), and nobody outside this
            // file used to be able to find out.
            if !streaming.announced {
                if let Some(encoding) = streaming.worker.encoding() {
                    tracing::info!(
                        %surface,
                        encoder = %encoding.encoder,
                        hardware = encoding.hardware,
                        fps = encoding.fps,
                        "this surface is being encoded"
                    );
                    self.state.publish_stream_encoding(surface, encoding);
                    streaming.announced = true;
                }
            }

            let size = streaming.worker.size();
            for chunk in self.chunks.drain(..) {
                self.state
                    .publish_stream_chunk(crate::show_state::PendingChunk {
                        surface,
                        size,
                        keyframe: chunk.keyframe,
                        timestamp_us: chunk.timestamp_us,
                        data: chunk.data,
                    });
            }
        }
    }

    /// Nothing to do where there is nothing to encode with.
    ///
    /// The renderer builds on Windows for development; its encoder does not,
    /// because it is ffmpeg. Such a machine simply never answers a watcher,
    /// which the core already copes with.
    #[cfg(not(unix))]
    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    fn update_streams(&mut self, state: &ShowState) {
        let _ = state;
    }

    /// What one surface is, if it exists.
    #[cfg(unix)]
    fn surface_shape(
        &self,
        surface: ovs_types::telemetry::StreamSurface,
        state: &ShowState,
    ) -> Option<(SizePx, FrameRate)> {
        match surface {
            ovs_types::telemetry::StreamSurface::Composition
            | ovs_types::telemetry::StreamSurface::CompositionMasked => {
                let rate = state
                    .show
                    .as_deref()
                    .map_or(FrameRate::FPS_60, |show| show.composition.render_rate);
                Some((stream_size(self.compositor.size()), rate))
            }
            ovs_types::telemetry::StreamSurface::Output { output_id } => {
                let output = self
                    .outputs
                    .iter()
                    .find(|output| output.backend.id() == output_id)?;
                if output.faulted.is_some() {
                    return None;
                }
                let mode = output.backend.mode();
                Some((stream_size(mode.size), mode.refresh))
            }
        }
    }

    /// Copies the picture the card already shrank, for one surface.
    #[cfg(unix)]
    fn snapshot_surface(
        &mut self,
        surface: ovs_types::telemetry::StreamSurface,
    ) -> Option<crate::media::DecodedImage> {
        let mut pixels = Vec::new();
        let size = match surface {
            ovs_types::telemetry::StreamSurface::Composition => {
                let size = stream_size(self.compositor.size());
                self.compositor.snapshot(size, &mut pixels).then_some(size)
            }
            // The same readback, masked afterwards on the preview-sized buffer:
            // one pass over at most 1280x720 rather than a second render pass,
            // a second framebuffer and a second readback for a picture nobody
            // is watching most of the time.
            ovs_types::telemetry::StreamSurface::CompositionMasked => {
                let size = stream_size(self.compositor.size());
                if !self.compositor.snapshot(size, &mut pixels) {
                    return None;
                }
                crate::mask::keep_only(&mut pixels, size, &self.mapped_regions());
                Some(size)
            }
            ovs_types::telemetry::StreamSurface::Output { output_id } => {
                let output = self
                    .outputs
                    .iter()
                    .find(|output| output.backend.id() == output_id)?;
                let size = stream_size(output.backend.mode().size);
                output.backend.snapshot(size, &mut pixels).then_some(size)
            }
        }?;

        crate::media::DecodedImage::new(pixels, size)
    }

    /// Takes the per-output patterns off the walls.
    fn forget_output_patterns(&mut self) {
        for output in &mut self.outputs {
            if output.pattern.take().is_some() {
                output.backend.drop_texture(TextureKey::TestPattern);
            }
        }
    }

    /// Draws each wall's pieces on black, at the pixels they were given.
    ///
    /// What the operator drew, on the wall itself, with nothing scaled: the
    /// question this answers is whether a rectangle is where they put it, and
    /// an answer that had been resampled to fit would not be one.
    ///
    /// Rebuilt only when the wall's mode or its pieces change. Standing still
    /// costs a texture upload a frame and no drawing at all.
    fn draw_piece_patterns(&mut self) {
        let state = self.state.load();
        let chosen = self.state.highlighted_piece();
        let Some(show) = state.show.as_deref() else {
            self.forget_piece_patterns();
            return;
        };

        for output in &mut self.outputs {
            if output.faulted.is_some() {
                continue;
            }

            // By the assignment the planner made, never by name. The renderer
            // calls a wall after its connector and the show calls it whatever
            // the operator typed -- "HDMI-0" against "Sony LMD-2110W" on the
            // reference machine -- so matching names draws nothing at all.
            let Some(output_id) = output.mapping.output_id() else {
                // A wall the show says nothing about has no pieces to draw, and
                // black is the honest picture of that.
                output.pieces_pattern = None;
                continue;
            };

            // In draw order, so the piece on top here is the piece on top on
            // the wall. A switched-off piece is not drawn at all: this mode is
            // about what reaches the wall.
            let mut pieces: Vec<ovs_types::config::Piece> = show
                .pieces
                .iter()
                .filter(|piece| piece.output_id == output_id && piece.enabled)
                .cloned()
                .collect();
            pieces.sort_by_key(|piece| piece.order);

            let drawn: Vec<crate::pattern::pieces::DrawnPiece> = pieces
                .iter()
                .map(|piece| crate::pattern::pieces::DrawnPiece {
                    rect: piece.dest_rect,
                    name: piece.name.clone(),
                    chosen: chosen.is_some_and(|id| id == piece.id),
                })
                .collect();

            let mode = output.backend.mode();
            if !output
                .pieces_pattern
                .as_ref()
                .is_some_and(|pattern| pattern.matches(mode.size, &drawn))
            {
                output.pieces_pattern =
                    Some(crate::pattern::pieces::PiecePattern::new(mode.size, drawn));
                tracing::info!(
                    output = output.backend.name(),
                    pieces = pieces.len(),
                    "this output is showing its pieces"
                );
            }

            let Some(pattern) = output.pieces_pattern.as_ref() else {
                continue;
            };
            if output
                .backend
                .upload_texture(TextureKey::TestPattern, pattern.frame())
                .is_ok()
            {
                compose_test_pattern(&mut output.pieces);
            }
        }
    }

    fn forget_piece_patterns(&mut self) {
        for output in &mut self.outputs {
            if output.pieces_pattern.take().is_some() {
                output.backend.drop_texture(TextureKey::TestPattern);
            }
        }
    }

    /// Rebuilds each output's pieces from the show.
    ///
    /// An output the show says nothing about shows the whole composition.
    /// That is the ordinary state of a single-wall setup nobody has mapped, and
    /// showing everything is more useful than showing nothing — an operator who
    /// has not reached the mapping screen yet still sees their show.
    ///
    /// Which physical output is which show output is decided by position for
    /// now. Real identity — connector, EDID, PCI address — is the display
    /// backend's to resolve and the Service Console's to assign
    ///; until then, the first output takes the first configured
    /// one.
    fn rebuild_mappings(&mut self, show: Option<&Show>) {
        // The mask the Pieces preview is cut with. Here because this is where
        // the show is, and because the mapping is the show's rather than the
        // hardware's — see `mask_regions`.
        self.mask_regions = Self::mask_regions(show);

        // Which physical output drives which configured one. Worked out from
        // hardware identity where there is any, because "Display 2" is whatever
        // the driver enumerated second this morning and a wall that came
        // unplugged must not hand its content to the next one along.
        //
        // Only the outputs that take a connector. A network output would
        // otherwise be counted as a spare socket — its slot is `Unused`, and
        // `Unused` slots are what a show output with no identity is matched into
        // in order — so a wall's pieces would have been drawn onto a stream and
        // the wall left showing the whole composition instead.
        let physical: Vec<usize> = (0..self.outputs.len())
            // Panels only. A stream has no connector to be planned onto, and
            // the System screen's connector is not the show's to plan with.
            .filter(|&index| self.outputs[index].kind == Kind::Panel)
            .collect();
        let discovered: Vec<Option<ovs_types::config::OutputIdentity>> = physical
            .iter()
            .map(|&index| self.outputs[index].backend.identity())
            .collect();
        let plan = crate::outputs::plan(show, &discovered);

        if plan.matched_by_order && !self.warned_about_order {
            self.warned_about_order = true;
            tracing::info!(
                "this show's outputs carry no hardware identity, so they are matched in order"
            );
        }
        for missing in &plan.missing {
            if self.reported_missing.insert(missing.output_id) {
                tracing::error!(
                    output = %missing.name,
                    "{}; its pieces stay dark rather than moving to another output",
                    missing.reason
                );
            }
        }
        self.missing_outputs = plan.missing;

        // Which configured output each driven one is showing. The planner
        // answers for the sockets; a network output answers for itself, because
        // it was built from a configured output and carries its id.
        let mut configured: Vec<Option<OutputId>> = vec![None; self.outputs.len()];
        for (slot, &index) in physical.iter().enumerate() {
            configured[index] = plan
                .assignments
                .get(slot)
                .and_then(crate::outputs::Assignment::output_id);
        }
        for (index, output) in self.outputs.iter().enumerate() {
            if output.kind == Kind::Network {
                configured[index] = Some(output.backend.id());
            }
        }

        // What the show calls each of them, for the caption on a test pattern.
        // Taken here because this is where an output is already matched to a
        // configured one, and working it out anywhere else would be a second
        // answer that could disagree with this one.
        let named: Vec<Option<String>> = configured
            .iter()
            .map(|wanted| {
                let wanted = (*wanted)?;
                show?
                    .outputs
                    .iter()
                    .find(|configured| configured.id == wanted)
                    .map(|configured| configured.name.clone())
            })
            .collect();

        for (index, output) in self.outputs.iter_mut().enumerate() {
            output.configured_name = named[index].clone();
            if output.faulted.is_some() {
                continue;
            }

            // **The System screen is never mapped and never falls through.** The
            // arm below hands an output the show says nothing about the whole
            // composition, which is right for a panel nobody has patched yet and
            // would put the show on the operator's own console — the one screen
            // they would use to put it back. Its scene is set where its picture
            // is uploaded and nowhere else.
            if output.kind == Kind::Console {
                continue;
            }

            let size = output.backend.mode().size;
            let configured = configured[index];

            match (show, configured) {
                (Some(show), Some(output_id)) => {
                    output.mapping.build(show, output_id, size);
                    output.mapping.fill_scene(&mut output.pieces);
                    output.mapped = true;
                }
                // No show, or no configuration for this output: show all of
                // whatever the composition holds, which with no show is
                // black.
                _ => {
                    compose_whole_composition(&mut output.pieces);
                    output.mapped = false;
                }
            }
        }
    }

    /// Which rectangles of the composition currently reach a wall.
    ///
    /// Read off what the renderer actually composed rather than off the show
    /// a second time: a mask built from its own reading of the pieces could
    /// disagree with the mapping, and an operator using this surface to decide
    /// whether their content is being cut off would be shown the wrong answer
    /// with nothing to compare it against.
    ///
    /// A faulted output contributes nothing, because its pieces stay dark
    ///. An output with no mapping contributes the whole
    /// composition, because that is what it shows.
    /// **Unix, or a test.** Its one caller outside the tests is the masked
    /// preview surface, which lives in the `cfg(unix)` half of `update_streams`
    /// because it needs the encoder. Without this the Windows build carries a
    /// method nothing calls, and `-D warnings` makes that an error rather than
    /// a warning — which is how the workspace came to stop compiling there.
    #[cfg(any(unix, test))]
    fn mapped_regions(&self) -> Vec<crate::mask::Region> {
        self.mask_regions.clone()
    }

    /// Which parts of the composition the show's mapping takes.
    ///
    /// **From the show, not from what is plugged in** (owner, 2026-10-13: "das
    /// mapping existiert aber für den ausgang... damit kann man offline weiter
    /// bauen"). This used to walk the outputs the renderer was driving, so a
    /// wall that was not connected contributed nothing and the preview went
    /// black — on exactly the machine where somebody is most likely to be
    /// building a show, the one with no wall on it. The pieces are in the
    /// document either way, and the document is what this surface is a picture
    /// of.
    ///
    /// It also takes the System screen out of the question for good. That was
    /// one of the driven outputs, it is deliberately never mapped, and unmapped
    /// meant "takes everything" — so from the day the renderer took over that
    /// connector the mask let the whole composition through. A show has no such
    /// output to confuse it with.
    ///
    /// Two cases answer with the whole composition, because that is what a wall
    /// would be showing: no show at all, and a show that maps nothing. A show
    /// with outputs but no pieces on them answers with nothing, and nothing is
    /// black — which is also what those walls would be drawing.
    fn mask_regions(show: Option<&Show>) -> Vec<crate::mask::Region> {
        let Some(show) = show else {
            return vec![crate::mask::Region::WHOLE];
        };
        if show.outputs.is_empty() {
            return vec![crate::mask::Region::WHOLE];
        }

        let mut regions = Vec::new();
        let mut mapping = crate::pieces::OutputMapping::new();
        for output in &show.outputs {
            if !output.enabled {
                continue;
            }
            // The size the show configures rather than a panel's: there may be
            // no panel, and where there is one it is driven at this mode.
            mapping.build(show, output.id, output.mode.size);
            for piece in mapping.pieces() {
                if piece.is_invisible() || piece.is_off_screen() {
                    continue;
                }
                // The turn as well as the rectangle. A piece whose input end is
                // taken at an angle takes a diamond out of the composition, and
                // a mask that kept the upright box it came from would show
                // horizontal bars where the mapping has diagonal ones — which
                // is the one thing this surface exists to be trusted about
                // (owner, 2026-09-23). The same matrix the shader samples
                // through, so the two agree by construction.
                regions.push(crate::mask::Region::turned(
                    piece.source_left,
                    piece.source_top,
                    piece.source_right,
                    piece.source_bottom,
                    piece.source_turn.matrix,
                    piece.source_turn.pivot,
                ));
            }
        }
        regions
    }

    /// Whether the surfaces are copying their frames out to be looked at.
    #[cfg(test)]
    const fn readback_for_test(&self) -> bool {
        self.readback
    }

    /// What the composition's own pattern was drawn at, if it has one.
    #[cfg(test)]
    fn composition_pattern(&self) -> Option<ovs_types::geometry::SizePx> {
        self.test_pattern
            .as_ref()
            .map(crate::pattern::ServicePattern::size)
    }

    /// What each output's own pattern calls it, if it has one.
    #[cfg(test)]
    fn output_pattern_titles(&self) -> Vec<Option<String>> {
        self.outputs
            .iter()
            .map(|output| {
                output
                    .pattern
                    .as_ref()
                    .map(|pattern| pattern.title().to_owned())
            })
            .collect()
    }

    /// What each output's own pattern was drawn at, if it has one.
    #[cfg(test)]
    fn output_patterns(&self) -> Vec<Option<ovs_types::geometry::SizePx>> {
        self.outputs
            .iter()
            .map(|output| {
                output
                    .pattern
                    .as_ref()
                    .map(crate::pattern::ServicePattern::size)
            })
            .collect()
    }

    /// Returns how many quads the last composed frame contained.
    #[must_use]
    pub fn composed_pieces(&self) -> usize {
        self.outputs.iter().map(|output| output.pieces.len()).sum()
    }

    fn match_composition_to(&mut self, state: &ShowState) {
        let Some(wanted) = state.show.as_deref().map(|show| show.composition.size) else {
            return;
        };
        if wanted == self.compositor.size() || wanted.width == 0 || wanted.height == 0 {
            return;
        }

        let replacement = match (self.make_compositor)(wanted) {
            Ok(replacement) => replacement,
            Err(error) => {
                // Keeping the composition it has is the safe direction: the
                // show carries on at the wrong resolution rather than stopping.
                tracing::error!(
                    width = wanted.width,
                    height = wanted.height,
                    "could not build a composition this size: {error}; keeping the current one"
                );
                return;
            }
        };

        tracing::info!(
            width = wanted.width,
            height = wanted.height,
            "composition resized for the loaded show"
        );

        self.compositor = replacement;
        self.compositions_built = self.compositions_built.saturating_add(1);
        self.media.forget_everything();
        // The players go too. Their textures belong to the composition that has
        // just been replaced, and the passes they were playing were timed
        // against a show that is no longer loaded.
        self.playback.forget_everything(self.compositor.as_mut());
        self.composed_once = false;

        let source = self.compositor.source();
        for output in &mut self.outputs {
            match output.backend.bind_composition(source) {
                Ok(()) => output.bound = source,
                Err(error) => tracing::warn!("{error}"),
            }
        }
    }

    /// Keeps the outputs stocked with the media the state refers to.
    ///
    /// What is wanted comes from the show state rather than from the composed
    /// scene, and deliberately: a layer faded to nothing composes no quad, so
    /// taking the answer from the scene would free its texture and force a
    /// decode the moment an operator faded it back up — which is exactly when a
    /// stutter is least welcome.
    fn update_media(&mut self, state: &ShowState, now: Instant) -> MediaReport {
        let catalogue = self.state.media();
        fill_wanted(state, &catalogue, &mut self.wanted);
        self.media.update(
            &self.loader,
            &catalogue,
            &self.wanted,
            self.compositor.as_mut(),
            now,
        )
    }

    /// Copies a preview of each output that can offer one.
    ///
    /// Called a few times a second from outside the render path, never per
    /// frame: the readback and the scaling are cheap but not free, and a
    /// preview is for whoever is watching rather than for the wall.
    pub fn take_previews(&mut self) -> Vec<Preview> {
        let mut previews = Vec::new();

        for output in &self.outputs {
            if output.faulted.is_some() {
                continue;
            }

            let mut pixels = Vec::new();
            // The shape of the wall, not the shape of the box it will be
            // shown in: a readback stretches whatever it is given to whatever
            // size it is asked for, so asking for the wrong shape is how a
            // preview comes out squashed.
            let size = preview_size(output.backend.mode().size);
            if output.backend.snapshot(size, &mut pixels) {
                previews.push(Preview {
                    output_id: output.backend.id(),
                    name: output.backend.name().to_owned(),
                    size,
                    pixels,
                });
            }
        }

        previews
    }

    /// Returns the configured outputs that nothing is driving.
    #[must_use]
    pub fn missing_outputs(&self) -> &[crate::outputs::Missing] {
        &self.missing_outputs
    }

    /// Returns what the players are doing, for the core's status.
    #[must_use]
    pub fn playback_telemetry(&self) -> ovs_types::telemetry::PlaybackTelemetry {
        self.playback.telemetry()
    }

    /// Returns where the last frame's time went.
    #[must_use]
    pub const fn frame_timings(&self) -> ovs_types::telemetry::FrameTimings {
        self.timings
    }

    /// Returns where each playing layer's playhead is, for the core's status.
    ///
    /// Beside the counts rather than part of them: those are cumulative and
    /// describe the machine, these are instantaneous and describe one layer
    /// each. The scheduler recorded them as it drew; this only collects them,
    /// into a list it keeps rather than one it builds.
    pub fn playheads(&mut self) -> &[ovs_types::telemetry::LayerPlayhead] {
        self.playback.playheads(&mut self.playheads);
        &self.playheads
    }

    /// Copies a preview of the composition, if the compositor can give one.
    ///
    /// The show before it is cut into pieces: what a client's programme view
    /// shows, and the only preview that means the same thing on a rig with six
    /// walls as on one with a single screen.
    pub fn take_composition_preview(&mut self) -> Option<(ovs_types::geometry::SizePx, Vec<u8>)> {
        let mut pixels = Vec::new();
        let size = preview_size(self.compositor.size());
        self.compositor
            .snapshot(size, &mut pixels)
            .then_some((size, pixels))
    }

    /// Returns what the device reports about itself, if it can say.
    ///
    /// Asked of the compositor because that is what holds the device open. An
    /// output could answer too; asking one of several would mean picking one,
    /// and they are all the same device.
    #[must_use]
    pub fn gpu_telemetry(&self) -> Option<ovs_types::telemetry::GpuTelemetry> {
        let mut telemetry = self.compositor.gpu_telemetry()?;

        // What the driver's own library says, where there is one. Vulkan reports
        // memory and nothing else, and utilisation and temperature are two of
        // the numbers an operator looks at first.
        if let Some(nvml) = &self.nvml {
            let load = nvml.sample();
            telemetry.utilisation_percent = load.utilisation_percent;
            telemetry.temperature_c = load.temperature_c;

            // NVML knows the whole card; Vulkan knows this process's budget.
            // The card's own figure is the one an operator means by "how full
            // is it", so it wins where both exist.
            if load.memory_total_bytes.is_some() {
                telemetry.vram_used_bytes = load.memory_used_bytes;
                telemetry.vram_total_bytes = load.memory_total_bytes;
            }
        }

        Some(telemetry)
    }

    /// Starts reading the card's own numbers, if its driver will say.
    ///
    /// Called once, with the PCI address of the device the renderer opened, so
    /// that a machine with two cards reports the one carrying the show.
    pub fn watch_gpu(&mut self, pci_address: Option<&str>) {
        self.nvml = crate::nvml::Nvml::open(pci_address);
        match &self.nvml {
            Some(nvml) => tracing::info!(
                driver = nvml.driver_version().unwrap_or_default(),
                "reading utilisation and temperature from the GPU driver"
            ),
            None => tracing::info!(
                "no GPU management library on this machine: utilisation and temperature are \
                 not reported, which is a dash on a console rather than a zero"
            ),
        }
    }

    /// Returns the size the composition is being drawn at.
    #[must_use]
    pub fn composition_size(&self) -> ovs_types::geometry::SizePx {
        self.compositor.size()
    }

    /// Returns how many compositions have been built, including the first.
    ///
    /// For tests and for diagnostics: a number that climbs during a show means
    /// something is rebuilding the canvas when it should not be.
    #[must_use]
    pub const fn compositions_built(&self) -> u64 {
        self.compositions_built
    }

    /// Returns how many outputs are bound to the current composition.
    #[must_use]
    pub fn bound_outputs(&self) -> usize {
        let source = self.compositor.source();
        self.outputs
            .iter()
            .filter(|output| output.bound == source)
            .count()
    }

    /// Returns how many media items currently hold textures.
    #[must_use]
    pub fn resident_media(&self) -> usize {
        self.media.resident_count()
    }

    /// Returns true when this item's pixels are on the outputs.
    #[must_use]
    pub fn media_is_resident(&self, media_item_id: MediaItemId) -> bool {
        self.media.is_resident(media_item_id)
    }

    /// Returns current telemetry for every output.
    #[must_use]
    pub fn telemetry(&mut self) -> Vec<OutputTelemetry> {
        for (entry, output) in self.telemetry.iter_mut().zip(&self.outputs) {
            entry.actual_fps = output.clock.actual_fps();
            entry.frame_render_time_us = output.clock.last_render_time_us();
            entry.dropped_frames = output.clock.dropped_frames();
            entry.active = output.faulted.is_none();
            // Every pass rather than once at bring-up: a panel's swapchain is
            // created when the output first presents and rebuilt whenever the
            // mode changes under it, so the depth on the cable is not known at
            // the moment this list is built and can change afterwards.
            entry.composition_depth = output.backend.composition_depth();
            entry.output_depth = output.backend.output_depth();
            // Every pass, because a show being opened or closed changes it and
            // nothing else announces that it has.
            entry.showing_pieces = output.mapped;
        }

        // **The System screen is not one of the show's outputs.** It was
        // reported as one: `OUTPUT 1` on its own channel list, a row in the
        // client's output patch, and one more in the count a show is driving. It
        // carries no piece of the composition and no mapping names it, and an
        // operator offered it as somewhere to put a wall is being offered the
        // screen they are standing at.
        //
        // The entry is kept and updated — the loop needs it to pace the panel —
        // and simply not reported. A `Vec` rather than a slice because of it,
        // which is one allocation a second and nothing on this path.
        self.outputs
            .iter()
            .zip(&self.telemetry)
            .filter(|(output, _)| output.kind != Kind::Console)
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    /// Returns how many quads the last composed frame contained.
    #[must_use]
    pub fn composed_quads(&self) -> usize {
        self.scene.len()
    }

    /// Returns how the buses combine in the frame last composed.
    #[must_use]
    pub fn composed_gains(&self) -> BusGains {
        self.scene.gains()
    }

    /// Returns the fault reason for each faulted output, by name.
    #[must_use]
    pub fn faults(&self) -> Vec<(&str, &str)> {
        self.outputs
            .iter()
            .filter_map(|output| {
                output
                    .faulted
                    .as_deref()
                    .map(|reason| (output.backend.name(), reason))
            })
            .collect()
    }

    /// Resets the operator-visible dropped-frame counter for one output.
    ///
    /// Returns true when an output with that name was found.
    /// Puts some of the operator-visible counters back to zero.
    ///
    /// The lifetime figures underneath are untouched: "has this machine ever
    /// dropped a frame" is a different question from "has it dropped one since
    /// I looked", and only the second is an operator's to clear.
    pub fn reset_counters(&mut self, group: ovs_types::telemetry::CounterGroup) {
        use ovs_types::telemetry::CounterGroup;

        if matches!(group, CounterGroup::All | CounterGroup::DroppedFrames) {
            for output in &mut self.outputs {
                output.clock.reset_operator_counter();
            }
        }
        self.playback.reset_counters(group);
    }

    /// Puts one output's dropped-frame counter back to zero.
    pub fn reset_dropped_frames(&mut self, output_name: &str) -> bool {
        for output in &mut self.outputs {
            if output.backend.name() == output_name {
                output.clock.reset_operator_counter();
                return true;
            }
        }
        false
    }
}

/// The size one surface is streamed at.
///
/// The same ceiling as the card's own small copy, so a readback at this size is
/// a copy rather than a second scaling, and the same proportions as the surface
/// so nothing comes back squashed. Even sides, because every video encoder
/// downstream of this wants them.
#[must_use]
pub fn stream_size(surface: ovs_types::geometry::SizePx) -> ovs_types::geometry::SizePx {
    /// What a preview stream is worth at most. Above this the bandwidth buys
    /// detail nobody watching a preview is looking for.
    const CEILING: ovs_types::geometry::SizePx = ovs_types::geometry::SizePx::new(1280, 720);

    let width = u64::from(surface.width.max(1));
    let height = u64::from(surface.height.max(1));
    if width <= u64::from(CEILING.width) && height <= u64::from(CEILING.height) {
        return ovs_types::geometry::SizePx::new(
            surface.width.max(2) & !1,
            surface.height.max(2) & !1,
        );
    }

    // Whichever side runs out of room first decides the scale, in integers so
    // the answer cannot depend on how a float rounded.
    if width * u64::from(CEILING.height) >= height * u64::from(CEILING.width) {
        let scaled = (height * u64::from(CEILING.width) / width).max(2) as u32;
        ovs_types::geometry::SizePx::new(CEILING.width, scaled & !1)
    } else {
        let scaled = (width * u64::from(CEILING.height) / height).max(2) as u32;
        ovs_types::geometry::SizePx::new(scaled & !1, CEILING.height)
    }
}

/// How large a preview picture is.
///
/// Small on purpose. A preview answers "what is on that wall", which is a
/// question a thumbnail answers; anything larger costs readback, encoding and
/// bandwidth for detail nobody is looking for.
pub const PREVIEW_SIZE: ovs_types::geometry::SizePx = ovs_types::geometry::SizePx::new(480, 270);

/// The size to take a preview of one surface at.
///
/// [`PREVIEW_SIZE`] is a ceiling rather than a shape: a readback stretches
/// whatever it is given to whatever size it is asked for, so a 4:3 wall asked
/// for a 16:9 thumbnail comes back squashed — and a console cannot undo that,
/// because by then the picture is the wrong shape rather than the wrong size.
/// This keeps the surface's own proportions and fits them inside the ceiling.
#[must_use]
pub fn preview_size(surface: ovs_types::geometry::SizePx) -> ovs_types::geometry::SizePx {
    let width = u64::from(surface.width.max(1));
    let height = u64::from(surface.height.max(1));
    let ceiling_width = u64::from(PREVIEW_SIZE.width);
    let ceiling_height = u64::from(PREVIEW_SIZE.height);

    // Whichever side runs out of room first decides the scale, in integers so
    // that the answer does not depend on how a float rounded.
    if width * ceiling_height >= height * ceiling_width {
        let scaled = (height * ceiling_width / width).max(1);
        ovs_types::geometry::SizePx::new(PREVIEW_SIZE.width, scaled as u32)
    } else {
        let scaled = (width * ceiling_height / height).max(1);
        ovs_types::geometry::SizePx::new(scaled as u32, PREVIEW_SIZE.height)
    }
}

/// The composition size a renderer uses before any show has said.
///
/// Nothing is drawn into it until a show is loaded, so this is only what the
/// outputs sample in the meantime, which is black.
const DEFAULT_COMPOSITION: ovs_types::geometry::SizePx =
    ovs_types::geometry::SizePx::new(1920, 1080);

/// Collects the media every layer's active slot refers to, without allocating.
///
/// Deduplicated by scanning rather than by a set: a composition has tens of
/// layers, not thousands, and a linear scan over that beats allocating a set per
/// frame on a path that must not allocate at all.
fn fill_wanted(
    state: &ShowState,
    catalogue: &crate::show_state::MediaCatalogue,
    into: &mut Vec<MediaItemId>,
) {
    into.clear();

    let Some(show) = state.show.as_deref() else {
        return;
    };

    for live in &state.layers {
        let Some(layer) = show.layer(live.layer_id) else {
            continue;
        };

        // The pictures the layer's effects name, and the active slot's. A mask
        // is a still like any other and comes through the same residency; what
        // is different is that nothing in the slot points at it, so it has to
        // be collected from the chains.
        crate::effects::media_wanted(&layer.effects, &state.effects, into);
        if let Some(slot) = layer.slot(live.active_slot) {
            crate::effects::media_wanted(&slot.effects, &state.effects, into);
        }

        let Some(media_item_id) = layer
            .slot(live.active_slot)
            .and_then(ovs_types::config::Slot::media_item_id)
        else {
            continue;
        };

        if !into.contains(&media_item_id) {
            into.push(media_item_id);
        }
    }

    // Stills only, whichever way they got here. A clip belongs to the
    // scheduler, and handing one to the still loader would mean trying to open
    // a video file as an image — once per clip, and every one of them failing.
    // A mask naming a clip is the same mistake by a different route, so the
    // rule is applied once, to the whole list, rather than at each place that
    // can add to it.
    into.retain(|media_item_id| {
        catalogue
            .get(media_item_id)
            .is_none_or(|location| location.kind == ovs_types::media::MediaKind::Image)
    });
}

/// How far into its own run a service pattern is, from a count of frames.
///
/// **The pattern is bound to the surface's frame rate and to nothing else**
/// (owner, 2026-10-16: "testpattern läuft frei, also an nichts gebunden außer
/// die Bildrate des Ausgangs. nicht an der systemzeit oder sonst was"). Its
/// speeds are written in pixels a second, so the rate is what turns a frame
/// count into the seconds they are measured against: the picture then runs at
/// the same speed on a 50 Hz wall as on a 60 Hz one, and the faster wall gets
/// more of it rather than a faster version of it.
///
/// `f64` throughout. Single precision stops counting whole frames at sixteen
/// million, which is three days at sixty a second — and a wall that has been up
/// for a week would start showing the same picture for several frames running,
/// which is the fault this replaced.
fn pattern_seconds(frames: u64, rate: FrameRate) -> f64 {
    let hertz = rate.as_f64();
    if hertz <= 0.0 {
        return 0.0;
    }
    frames as f64 / hertz
}

/// The clock an effect pass reads, in seconds within a day.
///
/// Folded into a day on purpose. An `f32` has twenty-four bits of mantissa, so
/// seconds since 1970 -- about 1.8 billion of them -- would quantise to
/// hundreds of seconds and every moving effect would stand perfectly still. A
/// day's worth still resolves about a hundredth of a second.
///
/// The fold shows once a day as a jump in whatever is moving. In a glitch that
/// is indistinguishable from the effect, and nothing else reads this.
fn effect_seconds() -> f32 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    const A_DAY: u64 = 24 * 60 * 60;
    let seconds = since_epoch.as_secs() % A_DAY;
    seconds as f32 + since_epoch.subsec_nanos() as f32 / 1_000_000_000.0
}

#[cfg(test)]
mod tests {
    use ovs_proto::realtime::{Sequence, StateSnapshot};
    use ovs_types::config::{DisplayMode, Show};
    use ovs_types::geometry::{FrameRate, SizePx};
    use ovs_types::live::MasterLive;
    use ovs_types::telemetry::RendererStatus;

    use crate::backend::{HeadlessBackend, HeadlessCompositor};
    use crate::show_state::ShowState;

    use super::*;

    fn mode() -> DisplayMode {
        DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60)
    }

    /// A factory that makes headless compositions of whatever size is asked.
    fn headless_compositions() -> crate::backend::CompositorFactory {
        Box::new(|size| Ok(Box::new(HeadlessCompositor::sized(size)) as Box<dyn Compositor>))
    }

    fn loop_with(
        outputs: Vec<Box<dyn OutputBackend>>,
    ) -> (Instant, Arc<SharedShowState>, RenderLoop) {
        let epoch = Instant::now();
        let state = Arc::new(SharedShowState::new());
        let render = RenderLoop::new(headless_compositions(), outputs, Arc::clone(&state), epoch)
            .expect("valid rates");
        (epoch, state, render)
    }

    /// Advances to the centre of interval `index`.
    fn at(epoch: Instant, index: u64) -> Instant {
        let frame = Duration::from_nanos(
            FrameRate::FPS_60
                .frame_duration_nanos()
                .expect("valid rate"),
        );
        epoch + frame * u32::try_from(index).expect("small") + frame / 2
    }

    /// Rebuilding a wall does not take the streams with it.
    ///
    /// **The fault this is here for.** A panel refusing a mode rebuilds the
    /// walls; the rebuild used to replace every output, streams included; and
    /// the streams never came back, because what decides whether to rebuild one
    /// is whether the show's list differs from the list last built — and that
    /// list still said they were running. So a mode a panel could not take
    /// killed every NDI output on the machine until it was restarted (owner,
    /// 2026-09-25: "es sterben alle ausgänge wenn einer stirbt").
    #[test]
    fn rebuilding_a_wall_does_not_take_the_streams_with_it() {
        let (epoch, _state, mut render) = loop_with(vec![Box::new(HeadlessBackend::new(
            "Wall",
            DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
        ))]);

        render
            .adopt_network(
                vec![Box::new(HeadlessBackend::new(
                    "Programme feed",
                    DisplayMode::new(SizePx::new(1280, 720), FrameRate::FPS_60),
                ))],
                epoch,
            )
            .expect("valid rates");
        assert_eq!(render.output_count(), 2, "a wall and a stream");

        // What a mode change does: the walls are built again from scratch.
        render
            .adopt(
                vec![Box::new(HeadlessBackend::new(
                    "Wall",
                    DisplayMode::new(SizePx::new(1280, 720), FrameRate::FPS_60),
                ))],
                epoch,
            )
            .expect("valid rates");

        assert_eq!(
            render.published_names_for_test(),
            vec!["Programme feed".to_owned()],
            "the stream must still be there after the wall was rebuilt"
        );
        assert_eq!(
            render.output_count(),
            2,
            "the wall came back and the stream stayed"
        );
    }

    /// A fake console on a real socket, and the source that reads it.
    ///
    /// The wire is the wire, so this exercises the same bytes the real console
    /// sends; what it stands in for is the browser, not the transport.
    #[cfg(unix)]
    fn a_console_sending(
        shade: u8,
        size: SizePx,
    ) -> (crate::console::ConsoleSource, std::path::PathBuf) {
        use std::os::unix::net::UnixListener;

        let directory = std::env::temp_dir().join(format!(
            "ovs-render-console-{}-{}",
            std::process::id(),
            shade
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a directory");
        let path = directory.join("console.sock");
        let listener = UnixListener::bind(&path).expect("listens");

        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut head = [0_u8; 9];
            if std::io::Read::read_exact(&mut stream, &mut head).is_err() {
                return;
            }
            // Written until the renderer goes away, so the source stays
            // connected for as long as the test wants to look at it.
            while ovs_console_wire::write_frame(
                &mut stream,
                &ovs_console_wire::Frame {
                    width: size.width,
                    height: size.height,
                    pixels: vec![shade; (size.width as usize) * (size.height as usize) * 4],
                },
            )
            .is_ok()
            {
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        (
            crate::console::ConsoleSource::watching(&path, size),
            directory,
        )
    }

    /// Rebuilding a wall does not take the System screen with it.
    ///
    /// The same fault as the streams above, and worse: a panel refusing a mode
    /// rebuilds the walls, and if that took the console with it the operator
    /// would lose the one screen they could have used to see why (house rule
    /// §17).
    #[cfg(unix)]
    #[test]
    fn rebuilding_a_wall_does_not_take_the_system_screen_with_it() {
        let panel = DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60);
        let (epoch, _state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", panel))]);

        let small = SizePx::new(64, 32);
        let (source, directory) = a_console_sending(5, small);
        render
            .adopt_console(
                Some((
                    Box::new(HeadlessBackend::new(
                        "System screen",
                        DisplayMode::new(small, FrameRate::FPS_60),
                    )),
                    source,
                )),
                epoch,
            )
            .expect("valid rates");
        assert_eq!(render.output_count(), 2, "a wall and the System screen");

        // What a mode change does: the walls are built again from scratch.
        render
            .adopt(
                vec![Box::new(HeadlessBackend::new(
                    "Wall",
                    DisplayMode::new(SizePx::new(1280, 720), FrameRate::FPS_60),
                ))],
                epoch,
            )
            .expect("valid rates");

        assert_eq!(
            render.output_count(),
            2,
            "the wall came back and the System screen stayed"
        );
        assert!(
            render
                .outputs
                .iter()
                .any(|output| output.kind == Kind::Console),
            "the System screen was rebuilt away with the walls"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A System screen that cannot present is let go of; a wall is not.
    ///
    /// **The fault this is here for.** On 2026-09-26 the console's output failed
    /// its first present with `ERROR_UNKNOWN`, four times in a second, and two
    /// seconds later every fence on the device was timing out — including the
    /// composition's. One browser took two panels and a card with it. A wall is
    /// worth retrying for ever; a browser is not worth a show.
    #[cfg(unix)]
    #[test]
    fn a_system_screen_that_cannot_present_is_let_go_of_and_a_wall_is_not() {
        let mode = DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60);
        let (epoch, _state, mut render) = loop_with(vec![Box::new(
            HeadlessBackend::new("Wall", mode).failing_from(0),
        )]);

        let small = SizePx::new(64, 32);
        let (source, directory) = a_console_sending(4, small);
        render
            .adopt_console(
                Some((
                    Box::new(
                        HeadlessBackend::new(
                            "System screen",
                            DisplayMode::new(small, FrameRate::FPS_60),
                        )
                        .failing_from(0),
                    ),
                    source,
                )),
                epoch,
            )
            .expect("valid rates");

        // Well past the console's patience, and nowhere near a wall's, which is
        // that it has none.
        for index in 1..=10 {
            render.tick(at(epoch, index));
        }

        let console = render
            .outputs
            .iter()
            .find(|output| output.kind == Kind::Console)
            .expect("the System screen");
        assert!(
            console.faulted.is_some(),
            "the System screen went on failing instead of giving its connector up"
        );

        let wall = render
            .outputs
            .iter()
            .find(|output| output.kind == Kind::Panel)
            .expect("the wall");
        assert!(
            wall.faulted.is_none(),
            "a wall that cannot present must keep trying: retrying is the only way it comes back"
        );
        assert!(
            wall.consecutive_failures > CONSOLE_GIVES_UP_AFTER,
            "the wall should have failed more times than the console was allowed to"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// And the System screen is never handed the show.
    ///
    /// An output the show says nothing about gets the whole composition,
    /// fullscreen — which is right for a panel nobody has patched yet and would
    /// put the show on the operator's own console, over the buttons they would
    /// use to put it back.
    #[cfg(unix)]
    #[test]
    fn the_system_screen_is_never_given_the_show() {
        let small = SizePx::new(64, 32);
        let (epoch, _state, mut render) = loop_with(vec![Box::new(HeadlessBackend::new(
            "Wall",
            DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
        ))]);

        let (source, directory) = a_console_sending(9, small);
        render
            .adopt_console(
                Some((
                    Box::new(HeadlessBackend::new(
                        "System screen",
                        DisplayMode::new(small, FrameRate::FPS_60),
                    )),
                    source,
                )),
                epoch,
            )
            .expect("valid rates");

        // No show at all, which is the state in which the fallback fires.
        render.tick(at(epoch, 1));

        let wall = render
            .outputs
            .iter()
            .find(|output| output.kind == Kind::Panel)
            .expect("the wall");
        assert!(
            wall.pieces
                .quads()
                .iter()
                .any(|quad| quad.texture == crate::scene::TextureRef::Composition),
            "the wall should be showing the whole composition"
        );

        let console = render
            .outputs
            .iter()
            .find(|output| output.kind == Kind::Console)
            .expect("the System screen");
        assert!(
            !console
                .pieces
                .quads()
                .iter()
                .any(|quad| quad.texture == crate::scene::TextureRef::Composition),
            "the show was put on the operator's own screen"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The System screen draws what the console sent it, and nothing else.
    #[cfg(unix)]
    #[test]
    fn the_system_screen_draws_what_the_console_sent() {
        let small = SizePx::new(64, 32);
        let (epoch, _state, mut render) = loop_with(Vec::new());

        let (source, directory) = a_console_sending(7, small);
        render
            .adopt_console(
                Some((
                    Box::new(HeadlessBackend::new(
                        "System screen",
                        DisplayMode::new(small, FrameRate::FPS_60),
                    )),
                    source,
                )),
                epoch,
            )
            .expect("valid rates");

        // The first picture has to travel a socket, so this waits for it rather
        // than assuming one pass is enough.
        let mut index = 1;
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(5) {
            render.tick(at(epoch, index));
            index += 1;
            if render
                .outputs
                .iter()
                .any(|output| !output.pieces.quads().is_empty())
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let console = render
            .outputs
            .iter()
            .find(|output| output.kind == Kind::Console)
            .expect("the System screen");
        let quads = console.pieces.quads();
        assert_eq!(quads.len(), 1, "one quad, across the whole panel");
        assert_eq!(
            quads[0].texture,
            crate::scene::TextureRef::Console,
            "the System screen is drawing something other than the console"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// And a stream changing does not take the walls with it.
    ///
    /// The half that was written carefully first. Held anyway, because a
    /// symmetry nobody checks is a symmetry that lasts until the next edit.
    #[test]
    fn changing_the_streams_does_not_take_the_walls_with_it() {
        let (epoch, _state, mut render) = loop_with(vec![Box::new(HeadlessBackend::new(
            "Wall",
            DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
        ))]);

        render
            .adopt_network(
                vec![Box::new(HeadlessBackend::new(
                    "Programme feed",
                    DisplayMode::new(SizePx::new(1280, 720), FrameRate::FPS_60),
                ))],
                epoch,
            )
            .expect("valid rates");

        // Every stream taken away, which is what removing the last network
        // output from a show does.
        render
            .adopt_network(Vec::new(), epoch)
            .expect("valid rates");

        assert_eq!(render.output_count(), 1, "the wall is still driven");
        assert!(
            render.published_names_for_test().is_empty(),
            "the stream was asked to go and went"
        );
    }

    #[test]
    fn the_composition_and_the_cable_are_reported_as_two_separate_stages() {
        // The owner's rule of 2026-09-23, as a test: an eight-bit output must
        // never be shown as ten-bit merely because something upstream of it is
        // deeper. The two readings come from two different pieces of hardware
        // and the loop carries both of them out unchanged.
        use ovs_types::depth::{CompositionDepth, OutputDepth};

        let (_, _, mut render) = loop_with(vec![
            Box::new(HeadlessBackend::at_depths(
                "Wall",
                mode(),
                CompositionDepth::Ten,
                OutputDepth::Eight,
            )),
            // And the other way round, which is the configuration nobody asked
            // for and which must still read as what it is rather than as the
            // nearest sensible thing.
            Box::new(HeadlessBackend::at_depths(
                "Monitor",
                mode(),
                CompositionDepth::Eight,
                OutputDepth::Ten,
            )),
        ]);

        let telemetry = render.telemetry();
        assert_eq!(telemetry.len(), 2);
        assert_eq!(telemetry[0].composition_depth, CompositionDepth::Ten);
        assert_eq!(
            telemetry[0].output_depth,
            OutputDepth::Eight,
            "ten bits through the compositor says nothing about the cable"
        );
        assert_eq!(telemetry[1].composition_depth, CompositionDepth::Eight);
        assert_eq!(telemetry[1].output_depth, OutputDepth::Ten);
    }

    #[test]
    fn an_output_with_no_cable_does_not_borrow_another_stages_depth() {
        // A headless output is not plugged into anything, and the honest answer
        // is that nobody knows what leaves it — not eight, which would be a
        // claim, and not another stage's number wearing the output's label.
        let (_, _, mut render) = loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let telemetry = render.telemetry();
        assert_eq!(
            telemetry[0].composition_depth,
            ovs_types::depth::CompositionDepth::Eight
        );
        assert_eq!(
            telemetry[0].output_depth,
            ovs_types::depth::OutputDepth::Unknown
        );
    }

    #[test]
    fn the_test_pattern_replaces_the_show_and_gives_it_back() {
        // A wall being lined up shows the pattern; the show is not gone, it is
        // underneath, and the moment the pattern goes off it is back.
        let mut show = Show::new("A show");
        show.composition.size = SizePx::new(1920, 1080);
        let layer = ovs_types::config::Layer::new("Wall", 0);
        let live = ovs_types::live::LayerLive::on_restart(&layer, None);
        show.layers.push(layer);

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        state.replace(ShowState::from_snapshot(StateSnapshot {
            sequence: Sequence(1),
            show: Some(show),
            master: MasterLive::default(),
            layers: vec![live],
            effects: Default::default(),
            generators: Default::default(),
            renderer: RendererStatus::Running,
        }));

        render.tick(at(epoch, 0));
        let show_quads = render.composed_quads();

        state.set_test_pattern(TestPatternMode::Composition, None);
        render.tick(at(epoch, 1));
        assert_eq!(
            render.composed_quads(),
            1,
            "the pattern is the composition, not a layer over it"
        );

        state.set_test_pattern(TestPatternMode::Off, None);
        render.tick(at(epoch, 2));
        assert_eq!(render.composed_quads(), show_quads, "the show is back");
    }

    #[test]
    fn each_output_draws_its_own_pattern_at_its_own_mode() {
        // The other half of the question. The composition pattern tests the
        // mapping; this one tests the cable and the mode, so it never goes
        // through the composition and every wall says something different.
        let small = DisplayMode::new(SizePx::new(1280, 720), FrameRate::FPS_50);
        let large = DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_59_94);
        let (epoch, state, mut render) = loop_with(vec![
            Box::new(HeadlessBackend::new("Left", small)),
            Box::new(HeadlessBackend::new("Right", large)),
        ]);

        state.set_test_pattern(TestPatternMode::Outputs, None);
        render.tick(at(epoch, 0));

        // A picture per wall, each the size of that wall — which is what makes
        // the caption on it that wall's own mode rather than a guess.
        assert_eq!(
            render.output_patterns(),
            vec![Some(small.size), Some(large.size)]
        );
        assert_eq!(
            render.composed_pieces(),
            2,
            "one quad each: its own pattern, in place of the mapping"
        );
        assert_eq!(
            render.composition_pattern(),
            None,
            "and the composition is still composing the show underneath"
        );

        state.set_test_pattern(TestPatternMode::Off, None);
        render.tick(at(epoch, 1));

        assert_eq!(
            render.output_patterns(),
            vec![None, None],
            "the walls give their patterns back"
        );
    }

    #[test]
    fn an_output_pattern_is_captioned_with_what_the_show_calls_it() {
        // **The reported fault.** It said `OUTPUT 1` on a wall the operator had
        // called `Internal DP`, and the name they are given over the radio is the
        // one thing an engineer up a ladder needs to read off it.
        let mut show = Show::new("A show");
        show.outputs = vec![
            ovs_types::config::OutputConfig::new("Internal DP", mode()),
            ovs_types::config::OutputConfig::new("LED Wall Left", mode()),
        ];

        let (epoch, state, mut render) = loop_with(vec![
            Box::new(HeadlessBackend::new("first socket", mode())),
            Box::new(HeadlessBackend::new("second socket", mode())),
        ]);
        state.replace(ShowState::from_snapshot(StateSnapshot {
            sequence: Sequence(1),
            show: Some(show),
            master: MasterLive::default(),
            layers: Vec::new(),
            effects: Default::default(),
            generators: Default::default(),
            renderer: RendererStatus::Running,
        }));
        state.set_test_pattern(TestPatternMode::Outputs, None);
        render.tick(at(epoch, 0));

        assert_eq!(
            render.output_pattern_titles(),
            vec![
                Some("Internal DP".to_owned()),
                Some("LED Wall Left".to_owned())
            ],
            "the caption is the show's name for the output, not its position"
        );
    }

    #[test]
    fn an_output_no_show_names_is_captioned_by_its_position() {
        // The other half: a panel nobody has patched yet has no name to show, and
        // `OUTPUT 2` is at least true.
        let (epoch, state, mut render) = loop_with(vec![
            Box::new(HeadlessBackend::new("first socket", mode())),
            Box::new(HeadlessBackend::new("second socket", mode())),
        ]);
        state.set_test_pattern(TestPatternMode::Outputs, None);
        render.tick(at(epoch, 0));

        assert_eq!(
            render.output_pattern_titles(),
            vec![Some("OUTPUT 1".to_owned()), Some("OUTPUT 2".to_owned())]
        );
    }

    #[test]
    fn the_composition_pattern_is_drawn_at_the_composition_size() {
        // Not at an output's size and not scaled to fit: the pattern *is* the
        // composition, and the pieces cut it up exactly as they cut up a show.
        let mut show = Show::new("A show");
        show.composition.size = SizePx::new(1280, 720);

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        state.replace(ShowState::from_snapshot(StateSnapshot {
            sequence: Sequence(1),
            show: Some(show),
            master: MasterLive::default(),
            layers: Vec::new(),
            effects: Default::default(),
            generators: Default::default(),
            renderer: RendererStatus::Running,
        }));
        state.set_test_pattern(TestPatternMode::Composition, None);
        render.tick(at(epoch, 0));

        assert_eq!(render.composition_pattern(), Some(SizePx::new(1280, 720)));
        assert_eq!(
            render.output_patterns(),
            vec![None],
            "the wall holds nothing of its own: it is sampling the composition"
        );
    }

    #[test]
    fn the_test_pattern_ignores_the_master() {
        // It is a service tool. A dimmer at zero or a crossfader on the other
        // side would hide the thing an engineer is on a ladder to look at.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        let master = MasterLive {
            dimmer: 0.0,
            crossfader: 1.0,
            ..MasterLive::default()
        };
        state.replace(ShowState::from_snapshot(StateSnapshot {
            sequence: Sequence(1),
            show: Some(Show::new("A show")),
            master,
            layers: Vec::new(),
            effects: Default::default(),
            generators: Default::default(),
            renderer: RendererStatus::Running,
        }));
        state.set_test_pattern(TestPatternMode::Composition, None);

        let report = render.tick(at(epoch, 0));

        assert!(report.composed);
        assert_eq!(render.composed_quads(), 1);
    }

    #[test]
    fn an_unplugged_wall_leaves_its_pieces_dark_rather_than_moving_them() {
        // At the level where it matters: the show is built for
        // two walls, one of them is not plugged in, and the wall that is must
        // keep showing its own half rather than inheriting the missing one's.
        use ovs_types::config::{OutputConfig, OutputIdentity, Piece};
        use ovs_types::geometry::RectPx;

        fn identity(connector: &str) -> OutputIdentity {
            OutputIdentity {
                gpu_pci_address: Some("0000:01:00.0".to_owned()),
                connector: Some(connector.to_owned()),
                ..OutputIdentity::default()
            }
        }

        let mode = DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60);
        let mut show = Show::new("Two walls");
        show.composition.size = SizePx::new(3840, 1080);

        let left = OutputConfig {
            identity: identity("DP-1"),
            ..OutputConfig::new("Left", mode)
        };
        let right = OutputConfig {
            identity: identity("DP-2"),
            ..OutputConfig::new("Right", mode)
        };
        let (left_id, right_id) = (left.id, right.id);
        show.outputs = vec![left, right];
        show.pieces = vec![
            Piece::new(
                "Left half",
                left_id,
                RectPx::new(0, 0, 1920, 1080),
                RectPx::new(0, 0, 1920, 1080),
            ),
            Piece::new(
                "Right half",
                right_id,
                RectPx::new(1920, 0, 1920, 1080),
                RectPx::new(0, 0, 1920, 1080),
            ),
        ];

        let shared = Arc::new(SharedShowState::new());
        shared.replace(ShowState {
            show: Some(Box::new(show)),
            ..ShowState::default()
        });

        // Only the right-hand wall is connected.
        let outputs: Vec<Box<dyn OutputBackend>> = vec![Box::new(HeadlessBackend::identified(
            "Connected",
            mode,
            identity("DP-2"),
        ))];
        let epoch = Instant::now();
        let mut render = RenderLoop::new(
            Box::new(|size| Ok(Box::new(HeadlessCompositor::sized(size)) as Box<dyn Compositor>)),
            outputs,
            Arc::clone(&shared),
            epoch,
        )
        .expect("valid rates");

        render.tick(epoch + Duration::from_millis(20));

        let missing = render.missing_outputs();
        assert_eq!(missing.len(), 1, "one wall is unaccounted for");
        assert_eq!(missing[0].output_id, left_id);
        assert_eq!(missing[0].name, "Left");

        // The connected wall draws its own piece, and only its own.
        assert_eq!(render.composed_pieces(), 1);
    }

    #[test]
    fn an_unmapped_wall_puts_the_whole_composition_in_the_mask() {
        // The masked surface has to agree with what the renderer draws, and an
        // output the show says nothing about draws everything. Masking it
        // to nothing would show an operator a black preview beside a wall that
        // is showing their show.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let show = Show::new("Show");
        assert!(show.outputs.is_empty(), "nothing is mapped");
        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            ..ShowState::default()
        });
        render.tick(at(epoch, 0));

        assert_eq!(render.mapped_regions(), vec![crate::mask::Region::WHOLE]);
    }

    #[test]
    fn the_pieces_are_in_the_mask_with_no_wall_attached() {
        // **The case this exists for.** Somebody building a show at a desk has
        // no wall on the machine, and the mapping is in the document all the
        // same (owner, 2026-10-13: "das mapping existiert aber für den
        // ausgang... damit kann man offline weiter bauen"). Reading the mask
        // off the driven outputs made Pieces black exactly there.
        use ovs_types::config::{OutputConfig, Piece};
        use ovs_types::geometry::RectPx;

        // Nothing but the System screen: no wall is connected at all.
        let (epoch, state, mut render) = loop_with(vec![Box::new(HeadlessBackend::new(
            "System screen",
            mode(),
        ))]);
        render.outputs[0].kind = Kind::Console;

        let mut show = Show::new("Show");
        let output = OutputConfig::new("LED Wall", mode());
        let output_id = output.id;
        show.outputs.push(output);
        show.pieces.push(Piece::new(
            "Left half",
            output_id,
            RectPx::new(0, 0, 960, 1080),
            RectPx::new(0, 0, 960, 1080),
        ));
        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            ..ShowState::default()
        });
        render.tick(at(epoch, 0));

        let regions = render.mapped_regions();
        assert_eq!(regions.len(), 1, "the piece the show maps: {regions:?}");
        assert_ne!(
            regions[0],
            crate::mask::Region::WHOLE,
            "the left half, not the whole composition"
        );
    }

    #[test]
    fn the_system_screen_takes_none_of_the_composition() {
        // **What the Pieces preview is for**: showing which regions of the
        // composition a wall actually takes. The mask is built by asking every
        // output what it takes, and the System screen is one of them — it
        // takes nothing, because it shows the console rather than the show.
        //
        // It is deliberately never mapped, and "not mapped" was read as "takes
        // the whole composition". So on every machine with a System screen the
        // mask let everything through and Pieces was the plain composition
        // (owner, 2026-10-12: "es ist beides Composition, als würde bei Pieces
        // die Maske fehlen").
        use ovs_types::config::{OutputConfig, Piece};
        use ovs_types::geometry::RectPx;

        let (epoch, state, mut render) = loop_with(vec![
            Box::new(HeadlessBackend::new("Wall", mode())),
            Box::new(HeadlessBackend::new("System screen", mode())),
        ]);
        // What `adopt_console` does on a machine that has one. Set here
        // because that method is `cfg(unix)` and this has to run everywhere.
        render.outputs[1].kind = Kind::Console;

        let mut show = Show::new("Show");
        let output = OutputConfig::new("LED Wall", mode());
        let output_id = output.id;
        show.outputs.push(output);
        show.pieces.push(Piece::new(
            "Left half",
            output_id,
            RectPx::new(0, 0, 960, 1080),
            RectPx::new(0, 0, 960, 1080),
        ));
        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            ..ShowState::default()
        });
        render.tick(at(epoch, 0));

        let regions = render.mapped_regions();
        assert!(
            !regions.contains(&crate::mask::Region::WHOLE),
            "the System screen put the whole composition into the mask: {regions:?}"
        );
        assert_eq!(regions.len(), 1, "the wall's one piece, and nothing else");
    }

    #[test]
    fn a_show_that_closes_stops_claiming_the_mapping_it_had() {
        // The mapping is left as it was when a show goes, so "this output is
        // drawing its pieces" is recorded rather than inferred from the mapping
        // being non-empty. Inferred, this would keep masking to the pieces of a
        // show nobody has open.
        use ovs_types::config::{OutputConfig, Piece};
        use ovs_types::geometry::RectPx;

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let mut show = Show::new("Show");
        let output = OutputConfig::new("LED Wall", mode());
        let output_id = output.id;
        show.outputs.push(output);
        show.pieces.push(Piece::new(
            "Left half",
            output_id,
            RectPx::new(0, 0, 960, 1080),
            RectPx::new(0, 0, 960, 1080),
        ));
        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            ..ShowState::default()
        });
        render.tick(at(epoch, 0));
        assert_eq!(render.mapped_regions().len(), 1, "one piece");

        state.replace(ShowState::default());
        render.tick(at(epoch, 1));

        assert_eq!(
            render.mapped_regions(),
            vec![crate::mask::Region::WHOLE],
            "no show: the wall shows all of nothing, and so does the mask"
        );
    }

    #[test]
    fn a_configured_wall_puts_only_what_its_pieces_take_in_the_mask() {
        // The whole point of the surface: a composition wider than the walls,
        // and the mask says which part of it a wall actually receives.
        use ovs_types::config::{OutputConfig, Piece};
        use ovs_types::geometry::RectPx;

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let mut show = Show::new("Show");
        show.composition.size = SizePx::new(1920, 1080);
        let output = OutputConfig::new("LED Wall", mode());
        let output_id = output.id;
        show.outputs.push(output);
        // The left half of the composition, on the whole wall. The right half
        // reaches nothing.
        show.pieces.push(Piece::new(
            "Left half",
            output_id,
            RectPx::new(0, 0, 960, 1080),
            RectPx::new(0, 0, 1920, 1080),
        ));

        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            ..ShowState::default()
        });
        render.tick(at(epoch, 0));

        let regions = render.mapped_regions();
        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions[0],
            crate::mask::Region::rectangle(0.0, 0.0, 0.5, 1.0),
            "the left half, upright: {regions:?}"
        );
    }

    #[test]
    fn a_switched_off_piece_takes_its_region_out_of_the_mask() {
        // A piece that is not drawn is not receiving content either, and an
        // operator wondering why half the wall is dark is exactly who is
        // looking at this surface.
        use ovs_types::config::{OutputConfig, Piece};
        use ovs_types::geometry::RectPx;

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let mut show = Show::new("Show");
        show.composition.size = SizePx::new(1920, 1080);
        let output = OutputConfig::new("LED Wall", mode());
        let output_id = output.id;
        show.outputs.push(output);
        show.pieces.push(Piece::new(
            "Left half",
            output_id,
            RectPx::new(0, 0, 960, 1080),
            RectPx::new(0, 0, 960, 1080),
        ));
        let mut spare = Piece::new(
            "Right half",
            output_id,
            RectPx::new(960, 0, 960, 1080),
            RectPx::new(960, 0, 960, 1080),
        );
        spare.enabled = false;
        show.pieces.push(spare);

        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            ..ShowState::default()
        });
        render.tick(at(epoch, 0));

        let regions = render.mapped_regions();
        assert_eq!(
            regions.len(),
            1,
            "only the piece that is drawn: {regions:?}"
        );
        assert_eq!(
            regions[0],
            crate::mask::Region::rectangle(0.0, 0.0, 0.5, 1.0),
            "the half that is drawn: {regions:?}"
        );
    }

    #[test]
    fn a_clip_is_never_handed_to_the_still_loader() {
        // The loader opens files as images. Given a clip it would fail once per
        // item, mark it undecodable, and leave the layer looking like a broken
        // still rather than one the scheduler is playing.
        use ovs_types::media::MediaKind;

        let mut show = ovs_types::config::Show::new("Show");
        let mut layer = ovs_types::config::Layer::new("Layer 1", 0);
        let still = MediaItemId::new();
        let clip = MediaItemId::new();
        layer.slots.push(ovs_types::config::Slot::media(1, still));
        let still_layer = layer.id;
        show.layers.push(layer);

        let mut second = ovs_types::config::Layer::new("Layer 2", 1);
        second.slots.push(ovs_types::config::Slot::media(1, clip));
        let clip_layer = second.id;
        show.layers.push(second);

        let mut still_live = ovs_types::live::LayerLive::black(still_layer);
        still_live.active_slot = 1;
        let mut clip_live = ovs_types::live::LayerLive::black(clip_layer);
        clip_live.active_slot = 1;

        let state = ShowState {
            show: Some(Box::new(show)),
            layers: vec![still_live, clip_live],
            ..ShowState::default()
        };

        let mut catalogue = crate::show_state::MediaCatalogue::new();
        for (media_item_id, kind) in [(still, MediaKind::Image), (clip, MediaKind::Video)] {
            catalogue.insert(
                media_item_id,
                ovs_ipc::MediaLocation {
                    media_item_id,
                    kind,
                    path: "whatever".into(),
                    dimensions: None,
                    duration_ms: None,
                    frame_rate: None,
                },
            );
        }

        let mut wanted = Vec::new();
        fill_wanted(&state, &catalogue, &mut wanted);

        assert_eq!(wanted, vec![still], "only the still");
    }

    #[test]
    fn an_output_with_an_unusable_rate_is_refused_at_construction() {
        let broken = HeadlessBackend::new(
            "Broken",
            DisplayMode::new(SizePx::new(1920, 1080), FrameRate::new(0, 1)),
        );
        let error = RenderLoop::new(
            headless_compositions(),
            vec![Box::new(broken)],
            Arc::new(SharedShowState::new()),
            Instant::now(),
        )
        .map(|_| ())
        .expect_err("refused");
        assert!(error.contains("unusable refresh rate"));
    }

    #[test]
    fn the_loop_presents_once_per_interval() {
        let (epoch, _state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        for index in 0..10 {
            let report = render.tick(at(epoch, index));
            assert_eq!(report.presented, 1, "interval {index}");
        }
        assert_eq!(render.telemetry()[0].dropped_frames.lifetime, 0);
    }

    #[test]
    fn an_output_that_is_not_due_yet_is_skipped_rather_than_presented_twice() {
        let (epoch, _state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        assert_eq!(render.tick(at(epoch, 0)).presented, 1);
        let quarter = Duration::from_nanos(
            FrameRate::FPS_60
                .frame_duration_nanos()
                .expect("valid rate"),
        ) / 4;
        let report = render.tick(at(epoch, 0) + quarter);
        assert_eq!(report.presented, 0);
        assert_eq!(report.skipped, 1);
    }

    #[test]
    fn the_crossfader_reaches_the_quads_the_loop_draws() {
        // The bus pass is part of composing a frame, not something a caller has
        // to remember: a crossfader that only moved in the API would be a
        // crossfader that did nothing on the wall.
        use ovs_types::config::{Layer, Route, Slot};
        use ovs_types::id::MediaItemId;
        use ovs_types::live::LayerLive;

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let mut show = Show::new("Show");
        let mut on_a = Layer::new("A side", 0);
        on_a.route = Route::A;
        on_a.slots.push(Slot::media(1, MediaItemId::new()));
        let mut on_b = Layer::new("B side", 1);
        on_b.route = Route::B;
        on_b.slots.push(Slot::media(1, MediaItemId::new()));
        show.layers.push(on_a);
        show.layers.push(on_b);

        let layers = show
            .layers
            .iter()
            .map(|layer| LayerLive {
                active_slot: 1,
                ..LayerLive::black(layer.id)
            })
            .collect();

        // Crossfader hard over to A. Both layers are still composed — each
        // bus is its own picture now, and the crossfader mixes the pictures
        // rather than the layers.
        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            master: MasterLive {
                crossfader: 0.0,
                ..MasterLive::default()
            },
            layers,
            ..ShowState::default()
        });

        render.tick(at(epoch, 0));
        assert_eq!(render.composed_quads(), 2, "both layers are drawn");

        let gains = render.composed_gains();
        assert!((gains.a - 1.0).abs() < 1e-6, "A at full: {}", gains.a);
        assert!(gains.b.abs() < 1e-6, "and nothing of B: {}", gains.b);
        assert!((gains.m - 1.0).abs() < 1e-6, "M is not crossfaded");
    }

    /// Builds a one-layer show whose active slot holds `clip`.
    fn show_with(clip: ovs_types::id::MediaItemId) -> ShowState {
        use ovs_types::config::{Layer, Slot};
        use ovs_types::live::LayerLive;

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, clip));
        let layer_id = layer.id;
        show.layers.push(layer);

        ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: vec![LayerLive {
                active_slot: 1,
                ..LayerLive::black(layer_id)
            }],
            ..ShowState::default()
        }
    }

    /// Writes a one-colour PNG and returns its path.
    fn png(directory: &std::path::Path, name: &str, colour: [u8; 4]) -> std::path::PathBuf {
        let path = directory.join(name);
        image::RgbaImage::from_pixel(2, 2, image::Rgba(colour))
            .save(&path)
            .expect("writes a png");
        path
    }

    /// Ticks until `done`, and returns the interval it happened in.
    ///
    /// Time-bounded rather than tick-bounded: a decode happens on another
    /// thread, so how many passes it takes depends on the machine, while how
    /// long it may take does not.
    fn tick_until(
        render: &mut RenderLoop,
        epoch: Instant,
        done: impl Fn(&RenderLoop) -> bool,
    ) -> u64 {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut index = 0;
        loop {
            render.tick(at(epoch, index));
            if done(render) {
                return index;
            }
            assert!(Instant::now() < deadline, "the loop never got there");
            index += 1;
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_still_in_a_slot_ends_up_on_the_output() {
        // The whole path in one test: a slot names a media item, the catalogue
        // says where its file is, the loader decodes it off the render thread
        // and the loop uploads it. Nothing here opens a file on the render
        // thread, which is the property the frame budget depends on.
        use ovs_ipc::MediaLocation;
        use ovs_types::id::MediaItemId;

        let directory = tempfile::tempdir().expect("a temporary directory");
        let clip = MediaItemId::new();
        let path = png(directory.path(), "clip.png", [10, 20, 30, 255]);

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        state.replace(show_with(clip));
        state.set_media(vec![MediaLocation {
            media_item_id: clip,
            kind: ovs_types::media::MediaKind::Image,
            path,
            dimensions: None,
            duration_ms: None,
            frame_rate: None,
        }]);

        tick_until(&mut render, epoch, |render| render.media_is_resident(clip));

        assert_eq!(render.resident_media(), 1);
        assert_eq!(render.composed_quads(), 1, "and it is being drawn");
    }

    #[test]
    fn a_slot_with_no_catalogue_entry_still_draws_and_still_presents() {
        // Media arrives after the show that names it, every time: the core
        // sends the snapshot first. A renderer that waited for pixels before
        // presenting would go black for those frames.
        use ovs_types::id::MediaItemId;

        let clip = MediaItemId::new();
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        state.replace(show_with(clip));

        for index in 0..10 {
            let report = render.tick(at(epoch, index));
            assert_eq!(report.presented, 1);
            assert_eq!(report.media.requested, 0, "nothing to ask for yet");
        }
        assert_eq!(render.composed_quads(), 1, "drawn flat, not dropped");
        assert_eq!(render.resident_media(), 0);
    }

    #[test]
    fn a_layer_faded_to_nothing_keeps_its_texture() {
        // It composes no quad, so taking "what is wanted" from the scene would
        // free it here and decode it again the moment the operator faded back
        // up — a stutter at exactly the wrong moment.
        use ovs_ipc::MediaLocation;
        use ovs_types::id::MediaItemId;

        let directory = tempfile::tempdir().expect("a temporary directory");
        let clip = MediaItemId::new();
        let path = png(directory.path(), "clip.png", [1, 2, 3, 255]);

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        state.replace(show_with(clip));
        state.set_media(vec![MediaLocation {
            media_item_id: clip,
            kind: ovs_types::media::MediaKind::Image,
            path,
            dimensions: None,
            duration_ms: None,
            frame_rate: None,
        }]);

        let index = tick_until(&mut render, epoch, |render| render.media_is_resident(clip));

        // Fade the layer out entirely.
        let mut faded = ShowState::clone(&state.load());
        faded.layers[0].opacity = 0.0;
        state.replace(faded);

        for step in 1..60 {
            render.tick(at(epoch, index + step));
        }

        assert_eq!(render.composed_quads(), 0, "nothing on stage");
        assert!(
            render.media_is_resident(clip),
            "but the pixels are still on the GPU"
        );
    }

    #[test]
    fn a_still_the_show_stopped_referring_to_is_given_back() {
        use ovs_ipc::MediaLocation;
        use ovs_types::id::MediaItemId;

        let directory = tempfile::tempdir().expect("a temporary directory");
        let clip = MediaItemId::new();
        let path = png(directory.path(), "clip.png", [4, 5, 6, 255]);

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        state.replace(show_with(clip));
        state.set_media(vec![MediaLocation {
            media_item_id: clip,
            kind: ovs_types::media::MediaKind::Image,
            path,
            dimensions: None,
            duration_ms: None,
            frame_rate: None,
        }]);

        let index = tick_until(&mut render, epoch, |render| render.media_is_resident(clip));

        // Another show is loaded, and it uses none of this one's media.
        state.set_media(Vec::new());
        let report = render.tick(at(epoch, index + 1));

        assert_eq!(report.media.evicted, 1);
        assert_eq!(render.resident_media(), 0);
    }

    #[test]
    fn every_frame_is_composed_once_and_presented_once_per_output() {
        // The shape of a frame. Composing per output would be three passes over
        // the same layers for three walls, and they would agree only by
        // coincidence.
        let (epoch, _state, mut render) = loop_with(vec![
            Box::new(HeadlessBackend::new("Left", mode())),
            Box::new(HeadlessBackend::new("Right", mode())),
        ]);

        for index in 0..5 {
            let report = render.tick(at(epoch, index));
            assert!(report.composed, "interval {index}");
            assert_eq!(report.presented, 2, "interval {index}");
        }
    }

    #[test]
    fn an_output_the_show_says_nothing_about_shows_the_whole_composition() {
        // The ordinary state of a setup nobody has mapped yet. Showing nothing
        // would be correct by the letter of the configuration and useless to
        // the operator standing in front of the wall.
        use ovs_types::config::{Layer, Slot};
        use ovs_types::id::MediaItemId;
        use ovs_types::live::LayerLive;

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        let layer_id = layer.id;
        show.layers.push(layer);
        assert!(show.outputs.is_empty(), "nothing is mapped");

        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: vec![LayerLive {
                active_slot: 1,
                ..LayerLive::black(layer_id)
            }],
            ..ShowState::default()
        });

        render.tick(at(epoch, 0));

        assert_eq!(render.composed_quads(), 1, "one layer in the composition");
        assert_eq!(render.composed_pieces(), 1, "and one full-screen piece");
    }

    #[test]
    fn a_configured_output_draws_its_pieces_and_nothing_else() {
        use ovs_types::config::{OutputConfig, Piece};
        use ovs_types::geometry::RectPx;

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let mut show = Show::new("Show");
        let output = OutputConfig::new("LED Wall", mode());
        let output_id = output.id;
        show.outputs.push(output);

        // Two halves of the composition, side by side on the wall.
        show.pieces.push(Piece::new(
            "Left half",
            output_id,
            RectPx::new(0, 0, 960, 1080),
            RectPx::new(0, 0, 960, 1080),
        ));
        show.pieces.push(Piece::new(
            "Right half",
            output_id,
            RectPx::new(960, 0, 960, 1080),
            RectPx::new(960, 0, 960, 1080),
        ));

        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: Vec::new(),
            ..ShowState::default()
        });

        render.tick(at(epoch, 0));

        assert_eq!(render.composed_pieces(), 2, "one quad per piece");
    }

    #[test]
    fn an_output_whose_pieces_are_all_disabled_shows_nothing() {
        // Deliberately dark, which is different from unconfigured. A wall an
        // operator has switched off must stay off rather than helpfully
        // showing the whole composition.
        use ovs_types::config::{OutputConfig, Piece};
        use ovs_types::geometry::RectPx;

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let mut show = Show::new("Show");
        let output = OutputConfig::new("LED Wall", mode());
        let output_id = output.id;
        show.outputs.push(output);
        let mut piece = Piece::new(
            "Everything",
            output_id,
            RectPx::new(0, 0, 1920, 1080),
            RectPx::new(0, 0, 1920, 1080),
        );
        piece.enabled = false;
        show.pieces.push(piece);

        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: Vec::new(),
            ..ShowState::default()
        });

        render.tick(at(epoch, 0));

        assert_eq!(render.composed_pieces(), 0);
    }

    /// The show state with `freeze` set as given.
    fn frozen(freeze: bool) -> ShowState {
        ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(Show::new("Show"))),
            master: MasterLive {
                freeze,
                ..MasterLive::default()
            },
            layers: Vec::new(),
            ..ShowState::default()
        }
    }

    #[test]
    fn freezing_holds_the_composition_and_keeps_presenting() {
        // Freeze holds the visible output and nothing else. The
        // loop still runs, the clock still runs and the outputs still present —
        // they present what is already there. A freeze that stopped presenting
        // would be a freeze that shows an operator a stalled output.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        state.replace(frozen(false));
        let report = render.tick(at(epoch, 0));
        assert!(report.composed);
        assert_eq!(report.presented, 1);

        state.replace(frozen(true));
        for index in 1..10 {
            let report = render.tick(at(epoch, index));
            assert!(report.held, "interval {index}");
            assert!(!report.composed, "interval {index}");
            assert_eq!(report.presented, 1, "interval {index}");
        }
        assert_eq!(render.telemetry()[0].dropped_frames.lifetime, 0);
    }

    #[test]
    fn unfreezing_starts_composing_again() {
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        state.replace(frozen(false));
        render.tick(at(epoch, 0));
        state.replace(frozen(true));
        render.tick(at(epoch, 1));

        state.replace(frozen(false));
        let report = render.tick(at(epoch, 2));

        assert!(report.composed);
        assert!(!report.held);
    }

    #[test]
    fn the_first_frame_is_composed_even_when_the_show_arrives_frozen() {
        // Until the composition has been drawn once its contents are undefined,
        // and holding undefined contents on a wall is not a freeze, it is a
        // fault that looks like one.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        state.replace(frozen(true));

        let report = render.tick(at(epoch, 0));

        assert!(report.composed, "the first pass always runs");
        assert!(!report.held);

        // And from then on it is held, as asked.
        assert!(render.tick(at(epoch, 1)).held);
    }

    /// A show whose composition is the given size.
    fn show_sized(size: SizePx) -> ShowState {
        let mut show = Show::new("Show");
        show.composition.size = size;
        ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: Vec::new(),
            ..ShowState::default()
        }
    }

    #[test]
    fn the_composition_takes_the_size_the_show_asks_for() {
        // Everything downstream of the composition is normalised, so a
        // mismatch costs resolution rather than correctness — a 4K composition
        // drawn into a 1080p image is the same picture with half the detail.
        // On an LED wall that is the difference an operator notices and cannot
        // explain.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        assert_eq!(render.composition_size(), SizePx::new(1920, 1080));

        state.replace(show_sized(SizePx::new(3840, 1080)));
        render.tick(at(epoch, 0));

        assert_eq!(render.composition_size(), SizePx::new(3840, 1080));
    }

    #[test]
    fn a_show_of_the_same_size_does_not_rebuild_the_composition() {
        // Rebuilding costs the media on it. Doing that on every show load
        // regardless would turn "reopen the same show" into a reload of every
        // clip in it.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        state.replace(show_sized(SizePx::new(1920, 1080)));
        render.tick(at(epoch, 0));
        let rebuilds = render.compositions_built();

        state.replace(show_sized(SizePx::new(1920, 1080)));
        render.tick(at(epoch, 1));

        assert_eq!(render.compositions_built(), rebuilds);
    }

    #[test]
    fn a_composition_of_no_size_is_refused_rather_than_built() {
        // A show that says its canvas is zero pixels wide is a show that
        // has been edited by something that should not have.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        state.replace(show_sized(SizePx::new(0, 1080)));
        render.tick(at(epoch, 0));

        assert_eq!(
            render.composition_size(),
            SizePx::new(1920, 1080),
            "the one it had"
        );
    }

    #[test]
    fn rebuilding_the_composition_forgets_the_media_that_lived_on_it() {
        // The textures were on the old composition. Bookkeeping that still
        // claimed them would leave every layer drawing flat for the rest of the
        // show, with nothing saying why.
        use ovs_ipc::MediaLocation;
        use ovs_types::id::MediaItemId;

        let directory = tempfile::tempdir().expect("a temporary directory");
        let clip = MediaItemId::new();
        let path = png(directory.path(), "clip.png", [1, 2, 3, 255]);

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let mut show = show_with(clip);
        if let Some(show) = show.show.as_deref_mut() {
            show.composition.size = SizePx::new(1920, 1080);
        }
        state.replace(show);
        state.set_media(vec![MediaLocation {
            media_item_id: clip,
            kind: ovs_types::media::MediaKind::Image,
            path,
            dimensions: None,
            duration_ms: None,
            frame_rate: None,
        }]);

        let index = tick_until(&mut render, epoch, |render| render.media_is_resident(clip));
        assert_eq!(render.resident_media(), 1);

        // Another show, on a different canvas.
        let mut wider = ShowState::clone(&state.load());
        if let Some(show) = wider.show.as_deref_mut() {
            show.composition.size = SizePx::new(3840, 1080);
        }
        state.replace(wider);
        render.tick(at(epoch, index + 1));

        assert_eq!(render.composition_size(), SizePx::new(3840, 1080));
        assert_eq!(render.resident_media(), 0, "the textures went with it");

        // And it loads again onto the new one.
        tick_until(&mut render, epoch, |render| render.media_is_resident(clip));
    }

    #[test]
    fn every_output_is_bound_to_the_composition_it_should_sample() {
        // An output still pointing at a composition that has been replaced is
        // an output sampling freed memory, which is the kind of fault that
        // looks like a driver bug.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        state.replace(show_sized(SizePx::new(2560, 1440)));
        render.tick(at(epoch, 0));

        assert_eq!(render.bound_outputs(), 1);
    }

    #[test]
    fn a_blackout_leaves_the_loop_with_nothing_to_draw() {
        use ovs_types::config::{Layer, Slot};
        use ovs_types::id::MediaItemId;
        use ovs_types::live::LayerLive;

        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        show.layers.push(layer);
        let layers = vec![LayerLive {
            active_slot: 1,
            ..LayerLive::black(show.layers[0].id)
        }];

        state.replace(ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            master: MasterLive {
                dimmer: 0.0,
                ..MasterLive::default()
            },
            layers,
            ..ShowState::default()
        });

        // Still presents: a blackout is black output, not a stopped renderer.
        assert_eq!(render.tick(at(epoch, 0)).presented, 1);

        // The layer is still composed; what makes the wall black is the dimmer
        // on the way out, applied after the buses are combined rather than to
        // each layer on the way in.
        assert_eq!(render.composed_quads(), 1);
        let gains = render.composed_gains();
        assert!(gains.a.abs() < 1e-6);
        assert!(gains.b.abs() < 1e-6);
        assert!(gains.m.abs() < 1e-6, "and nothing survives on M either");
    }

    #[test]
    fn the_loop_runs_with_no_show_loaded() {
        // Stopping would make "no show" and "renderer broken" look the same
        // from the auditorium.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        assert!(!state.load().has_show());

        for index in 0..30 {
            assert_eq!(render.tick(at(epoch, index)).presented, 1);
        }
        assert!(render.is_rendering());
    }

    #[test]
    fn the_loop_keeps_running_after_the_core_disappears() {
        // The invariant, measured rather than asserted about.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        state.replace(ShowState::from_snapshot(StateSnapshot {
            sequence: Sequence(5),
            show: Some(Show::new("Show")),
            master: MasterLive::default(),
            layers: Vec::new(),
            effects: Default::default(),
            generators: Default::default(),
            renderer: RendererStatus::Running,
        }));
        state.set_connected(true);

        for index in 0..10 {
            assert_eq!(render.tick(at(epoch, index)).presented, 1);
        }

        // The core goes away mid-show.
        state.set_connected(false);

        for index in 10..120 {
            assert_eq!(
                render.tick(at(epoch, index)).presented,
                1,
                "the renderer must keep presenting without the core"
            );
        }

        assert!(render.is_rendering());
        assert_eq!(
            render.telemetry()[0].dropped_frames.lifetime,
            0,
            "and drop nothing"
        );
        assert!(
            state.load().has_show(),
            "still rendering the state it was given"
        );
        assert!(matches!(state.status(), RendererStatus::Degraded { .. }));
    }

    #[test]
    fn one_failing_output_does_not_stop_the_others() {
        let (epoch, _state, mut render) = loop_with(vec![
            Box::new(HeadlessBackend::new("Good", mode())),
            Box::new(HeadlessBackend::new("Bad", mode()).lost_from(3)),
        ]);

        for index in 0..10 {
            let report = render.tick(at(epoch, index));
            assert!(
                report.presented >= 1,
                "at least the good output must present"
            );
        }

        assert!(render.is_rendering());
        assert_eq!(render.faults().len(), 1);
        assert_eq!(render.faults()[0].0, "Bad");

        let telemetry = render.telemetry();
        assert!(
            telemetry
                .iter()
                .find(|entry| entry.name == "Good")
                .expect("present")
                .active
        );
        assert!(
            !telemetry
                .iter()
                .find(|entry| entry.name == "Bad")
                .expect("present")
                .active
        );
    }

    #[test]
    fn a_transient_failure_becomes_a_measured_dropped_frame() {
        // A present that fails shows nothing, so the interval it was for is a
        // dropped frame. The clock is not advanced on failure, so the next
        // successful present counts the gap.
        let (epoch, _state, mut render) = loop_with(vec![Box::new(
            // Frames 1 and 2 fail; the backend recovers for frame 3.
            HeadlessBackend::new("Wall", mode()).failing_between(1, 2),
        )]);

        assert_eq!(render.tick(at(epoch, 0)).presented, 1);
        assert_eq!(render.telemetry()[0].dropped_frames.lifetime, 0);

        assert_eq!(render.tick(at(epoch, 1)).failed, 1);
        assert_eq!(render.tick(at(epoch, 2)).failed, 1);
        assert!(
            render.is_rendering(),
            "a transient failure must not fault the output"
        );

        assert_eq!(render.tick(at(epoch, 3)).presented, 1);
        assert_eq!(
            render.telemetry()[0].dropped_frames.lifetime,
            2,
            "two intervals showed nothing, so two frames were dropped"
        );
    }

    #[test]
    fn a_failed_present_never_counts_as_a_presented_frame() {
        let (epoch, _state, mut render) = loop_with(vec![Box::new(
            HeadlessBackend::new("Wall", mode()).failing_from(0),
        )]);

        for index in 0..10 {
            let report = render.tick(at(epoch, index));
            assert_eq!(report.presented, 0);
            assert_eq!(report.failed, 1);
        }
        assert_eq!(
            render.telemetry()[0].actual_fps,
            0.0,
            "nothing reached the display"
        );
        assert!(render.is_rendering(), "the output is still being tried");
    }

    #[test]
    fn every_output_faulting_stops_rendering_and_says_so() {
        let (epoch, _state, mut render) = loop_with(vec![Box::new(
            HeadlessBackend::new("Only", mode()).lost_from(0),
        )]);

        render.tick(at(epoch, 0));
        assert!(!render.is_rendering());
        assert_eq!(render.faults().len(), 1);
        // Even with nothing to drive, the loop still has a deadline to wait on
        // rather than spinning.
        let now = at(epoch, 1);
        assert!(render.next_deadline(now) > now);
    }

    #[test]
    fn outputs_at_different_rates_are_driven_independently() {
        let fast = HeadlessBackend::new("Fast", mode());
        let slow = HeadlessBackend::new(
            "Slow",
            DisplayMode::new(SizePx::new(1920, 1080), FrameRate::new(30, 1)),
        );
        let (epoch, _state, mut render) = loop_with(vec![Box::new(fast), Box::new(slow)]);

        for index in 0..60 {
            render.tick(at(epoch, index));
        }

        let telemetry = render.telemetry();
        let fast_frames = telemetry
            .iter()
            .find(|entry| entry.name == "Fast")
            .expect("present");
        let slow_frames = telemetry
            .iter()
            .find(|entry| entry.name == "Slow")
            .expect("present");
        assert_eq!(fast_frames.refresh, FrameRate::FPS_60);
        assert_eq!(slow_frames.refresh, FrameRate::new(30, 1));
        assert_eq!(fast_frames.dropped_frames.lifetime, 0);
        assert_eq!(
            slow_frames.dropped_frames.lifetime, 0,
            "a 30 Hz output is not dropping"
        );
    }

    #[test]
    fn resetting_dropped_frames_finds_the_named_output() {
        let (epoch, _state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        render.tick(at(epoch, 0));
        render.tick(at(epoch, 5));
        assert_eq!(render.telemetry()[0].dropped_frames.since_reset, 4);

        assert!(render.reset_dropped_frames("Wall"));
        assert!(!render.reset_dropped_frames("Nonexistent"));

        let telemetry = render.telemetry();
        assert_eq!(telemetry[0].dropped_frames.since_reset, 0);
        assert_eq!(telemetry[0].dropped_frames.lifetime, 4);
    }

    #[test]
    fn switching_previews_off_stops_the_copy_as_well_as_the_encode() {
        // The promise an operator is given when they switch previews off is
        // that the cost stops. Half of it used to: the picture was still copied
        // off the card every frame and then thrown away, which on a 4K
        // composition is thirty-three megabytes a frame nobody was going to
        // look at.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        render.tick(at(epoch, 0));
        assert!(
            render.readback_for_test(),
            "somebody is watching by default"
        );

        state.set_previews_enabled(false);
        render.tick(at(epoch, 1));
        assert!(!render.readback_for_test(), "and now nobody is");

        state.set_previews_enabled(true);
        render.tick(at(epoch, 2));
        assert!(render.readback_for_test(), "and now somebody is again");
    }

    #[test]
    fn a_preview_is_the_shape_of_what_it_is_a_preview_of() {
        // A readback stretches whatever it is given to whatever size it is
        // asked for, so the shape has to be right here: by the time a picture
        // reaches a console it is the wrong shape rather than the wrong size,
        // and nothing downstream can undo that.
        use ovs_types::geometry::SizePx;

        assert_eq!(
            preview_size(SizePx::new(1920, 1080)),
            SizePx::new(480, 270),
            "the ordinary case fills the box exactly"
        );
        assert_eq!(
            preview_size(SizePx::new(1024, 768)),
            SizePx::new(360, 270),
            "a 4:3 wall is narrower, not squashed"
        );
        assert_eq!(
            preview_size(SizePx::new(3840, 1080)),
            SizePx::new(480, 135),
            "and a wide one is shorter"
        );
        assert_eq!(
            preview_size(SizePx::new(1080, 1920)),
            SizePx::new(151, 270),
            "a portrait wall is a portrait preview"
        );
        // A surface with no size is not a widescreen one, so this does not
        // pretend it is. What matters is that nothing is ever asked for at
        // zero, which is a readback of nothing.
        let nothing = preview_size(SizePx::new(0, 0));
        assert!(nothing.width >= 1 && nothing.height >= 1, "{nothing:?}");
        assert!(
            nothing.width <= PREVIEW_SIZE.width && nothing.height <= PREVIEW_SIZE.height,
            "{nothing:?}"
        );
    }

    #[test]
    fn an_operator_can_clear_every_counter_at_once() {
        // What the CLEAR button on the System screen does. An operator asking
        // "has anything gone wrong since I looked" is asking about a window
        // that starts when they press it — and the machine's own history is not
        // theirs to erase, so the lifetime count stays.
        let (epoch, state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);
        render.tick(at(epoch, 0));
        render.tick(at(epoch, 5));
        assert_eq!(render.telemetry()[0].dropped_frames.since_reset, 4);

        // Through the same door the core knocks on, rather than by calling the
        // method: what is being tested is that pressing the button reaches the
        // counters.
        state.request_counter_reset(ovs_types::telemetry::CounterGroup::All);
        render.tick(at(epoch, 6));

        let telemetry = render.telemetry();
        assert_eq!(telemetry[0].dropped_frames.since_reset, 0, "cleared");
        assert_eq!(
            telemetry[0].dropped_frames.lifetime, 4,
            "and the night's history is still there"
        );
    }

    #[test]
    fn telemetry_reports_a_measured_rate_not_the_target() {
        let (epoch, _state, mut render) =
            loop_with(vec![Box::new(HeadlessBackend::new("Wall", mode()))]);

        // Present into every second interval: 30 fps on a 60 Hz output.
        for index in 0..31 {
            render.tick(at(epoch, index * 2));
        }

        let measured = render.telemetry()[0].actual_fps;
        assert!(
            (measured - 30.0).abs() < 1.5,
            "expected about 30 fps, reported {measured}"
        );
        assert_eq!(
            render.telemetry()[0].refresh,
            FrameRate::FPS_60,
            "the target is unchanged"
        );
    }
}
