//! The authoritative state as the renderer holds it.
//!
//! Two properties matter here, and they are the same property seen from two
//! sides:
//!
//! - **The render thread never blocks to read state.** A lock held by the
//!   control thread while the core sends a large show would otherwise stall
//!   the render loop, which is the one thing that must not happen
//!   (house rule §5). Reads go through [`arc_swap`], which is wait-free.
//! - **Losing the core changes nothing.** The renderer holds the last state it
//!   was given and keeps rendering from it. There is deliberately no "state is
//!   stale" flag that the render path consults, because there is no action it
//!   could usefully take.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use ovs_ipc::MediaLocation;
use ovs_proto::realtime::{Sequence, StateChange, StateSnapshot};
use ovs_types::config::{ConnectorRole, TestPatternMode};
use ovs_types::id::{LayerId, MediaItemId, PieceId};
use ovs_types::live::{EffectOverrides, GeneratorOverrides, LayerLive, MasterLive};
use ovs_types::telemetry::{OutputTelemetry, RendererStatus};

/// The show, as opposed to a pattern.
const OFF: u8 = 0;
/// The composition is the pattern; the pieces still map it onto the walls.
const COMPOSITION: u8 = 1;
/// Every output is its own pattern, at its own mode.
const OUTPUTS: u8 = 2;
/// Each output draws the pieces that land on it.
const PIECES: u8 = 3;

/// One encoded picture on its way to the core.
#[derive(Debug, Clone)]
pub struct PendingChunk {
    /// Which surface it is a picture of.
    pub surface: ovs_types::telemetry::StreamSurface,
    /// What that surface is being encoded at.
    pub size: ovs_types::geometry::SizePx,
    /// Whether a watcher can start here.
    pub keyframe: bool,
    /// Microseconds since that stream started.
    pub timestamp_us: u64,
    /// The bytes, H.264 Annex-B.
    pub data: Vec<u8>,
}

/// Where the media a show refers to lives, by item.
pub type MediaCatalogue = BTreeMap<MediaItemId, MediaLocation>;

/// The state the renderer is rendering from.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowState {
    /// Position in the core's update stream this state reflects.
    pub sequence: Sequence,
    /// The loaded show, or `None` when there is none.
    pub show: Option<Box<ovs_types::config::Show>>,
    /// Live master state.
    pub master: MasterLive,
    /// Live state per layer.
    pub layers: Vec<LayerLive>,
    /// What a control source has done to the effects, by instance.
    ///
    /// Part of the state the scene is composed from, because it decides what is
    /// drawn: the show says what an effect was configured with and this says
    /// what it is running at now.
    ///
    /// Behind an `Arc` because every incremental update clones this whole struct
    /// and a show driven from a console produces one many times a second. It is
    /// also the reason the scene builder can read it for nothing: the render
    /// thread holds a pointer to a map somebody else built, not a map it has to
    /// assemble per frame (house rule §5).
    pub effects: Arc<EffectOverrides>,
    /// What a control source has done to the generators, by instance.
    ///
    /// The same thing `effects` is, for the other kind of module: an effect and
    /// a generator are both plugins with declared parameters and one parameter
    /// model (owner, 2026-09-23).
    pub generators: Arc<GeneratorOverrides>,
}

impl Default for ShowState {
    fn default() -> Self {
        Self {
            sequence: Sequence::START,
            show: None,
            master: MasterLive::default(),
            layers: Vec::new(),
            effects: Arc::new(EffectOverrides::new()),
            generators: Arc::new(GeneratorOverrides::new()),
        }
    }
}

impl ShowState {
    /// Builds renderer state from a snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: StateSnapshot) -> Self {
        Self {
            sequence: snapshot.sequence,
            show: snapshot.show.map(Box::new),
            master: snapshot.master,
            layers: snapshot.layers,
            effects: Arc::new(snapshot.effects),
            generators: Arc::new(snapshot.generators),
        }
    }

    /// Returns the live values on one effect instance, if it has any.
    ///
    /// `None` for an effect running at exactly what the show configured,
    /// which is most of them for most of a show.
    #[must_use]
    pub fn effect(
        &self,
        effect_instance_id: ovs_types::id::EffectInstanceId,
    ) -> Option<&ovs_types::config::ParameterMap> {
        self.effects.get(&effect_instance_id)
    }

    /// Returns the live state of one layer.
    #[must_use]
    pub fn layer(&self, layer_id: LayerId) -> Option<&LayerLive> {
        self.layers.iter().find(|live| live.layer_id == layer_id)
    }

    /// Returns true when there is something to draw.
    ///
    /// A renderer with no show still runs its loop and still presents; it
    /// presents black. Stopping would make "no show" and "renderer broken"
    /// look identical on stage.
    #[must_use]
    pub fn has_show(&self) -> bool {
        self.show.is_some()
    }
}

/// The renderer's view of the world, shared between the control thread and the
/// render thread.
#[derive(Debug)]
pub struct SharedShowState {
    current: ArcSwap<ShowState>,
    /// What the render thread last measured.
    ///
    /// Published from the render thread and read by the control thread, through
    /// the same wait-free swap as the show state: reporting must never be able
    /// to make a frame late.
    telemetry: ArcSwap<Vec<OutputTelemetry>>,
    /// Where the media of the loaded show lives.
    ///
    /// Held beside the show state rather than inside it, and deliberately.
    /// [`ShowState`] is replaced wholesale by every snapshot, so a catalogue
    /// stored in it would be thrown away each time the core resynchronised and
    /// every layer would drop to its placeholder for as long as it took the
    /// next catalogue to arrive. It is also not sequenced state: it cannot
    /// change what is on stage, only where the pixels come from.
    media: ArcSwap<MediaCatalogue>,
    /// What the device reports about itself.
    ///
    /// Published by whoever opened the device — nothing else in the process can
    /// ask — and read by the control thread when it reports.
    gpu: ArcSwap<Option<ovs_types::telemetry::GpuTelemetry>>,
    /// The piece the pieces pattern draws as the chosen one.
    ///
    /// Beside the mode rather than inside it, because the mode is an atomic
    /// number the render thread reads every frame and an identifier is not.
    highlighted_piece: ArcSwap<Option<PieceId>>,
    /// What the players are doing, read by the control thread.
    playback: ArcSwap<ovs_types::telemetry::PlaybackTelemetry>,
    /// Where each playing layer's playhead is, read by the control thread.
    ///
    /// Beside the counts rather than on them, so
    /// [`ovs_types::telemetry::PlaybackTelemetry`] stays a `Copy` struct that
    /// the reader above dereferences for nothing. A list on it would turn that
    /// read into a clone, here and again in the core.
    playheads: ArcSwap<Vec<ovs_types::telemetry::LayerPlayhead>>,
    /// Configured outputs nothing is driving.
    missing_outputs: ArcSwap<Vec<ovs_types::telemetry::MissingOutput>>,
    /// Where the last frame's time went, once one has been drawn.
    frame: ArcSwap<Option<ovs_types::telemetry::FrameTimings>>,
    /// Whether previews should be produced at all.
    previews_enabled: AtomicBool,
    /// The surfaces somebody is watching as a stream.
    ///
    /// Sent whole by the core rather than as changes, so this is the whole
    /// answer and never needs reconstructing.
    wanted_streams: ArcSwap<Vec<ovs_types::telemetry::StreamSurface>>,
    /// Surfaces that could not be encoded, and why, waiting to be sent.
    stream_failures: std::sync::Mutex<Vec<(ovs_types::telemetry::StreamSurface, String)>>,
    /// What each surface turned out to be encoded with, waiting to be sent.
    ///
    /// A map rather than a list: one answer per surface, and the newest is the
    /// true one (owner decision Q13).
    stream_encodings: std::sync::Mutex<
        std::collections::HashMap<
            ovs_types::telemetry::StreamSurface,
            ovs_types::telemetry::StreamEncoding,
        >,
    >,
    /// Encoded pictures waiting to be sent, oldest first.
    ///
    /// Filled by the render thread and drained by the one that talks to the
    /// core: encoding is the render thread's business only in the sense that it
    /// hands frames over, and a socket write has no place on a path that owes a
    /// frame every sixteen milliseconds.
    stream_chunks: std::sync::Mutex<Vec<PendingChunk>>,
    /// Which counters an operator has asked to clear, if any.
    ///
    /// Read and cleared by the render thread, which is the only thing that can
    /// act on it.
    reset_counters: std::sync::Mutex<Option<ovs_types::telemetry::CounterGroup>>,
    /// Which surfaces draw the test pattern.
    ///
    /// A number rather than a lock: the render thread reads it once a frame and
    /// the control thread writes it when somebody presses a button.
    test_pattern: std::sync::atomic::AtomicU8,
    /// What each connector on this machine is for, as the core last said.
    connector_roles: ArcSwap<std::collections::BTreeMap<String, ConnectorRole>>,
    /// The mode the System screen's connector should be driven at, if chosen.
    ///
    /// Beside the roles because it arrives with them, in the same message: an
    /// operator moving the screen to another socket picks a mode for it in the
    /// same breath, and two messages can interleave where one cannot.
    console_mode: ArcSwap<Option<ovs_types::config::DisplayMode>>,
    /// What this machine is wired to, as the renderer found it.
    connectors: ArcSwap<Vec<ovs_ipc::DiscoveredConnector>>,
    /// Every NDI source on the network, as discovery last saw it.
    ///
    /// Beside the connectors because it is the same kind of fact — what this
    /// machine can reach — and it reaches a client the same way.
    ///
    /// Everything found, including this renderer's own outputs. What is offered
    /// to a client is [`SharedShowState::ndi_offered`]; this is what is on the
    /// network, which is what a log line about the network should say.
    ndi_sources: ArcSwap<Vec<String>>,
    /// The names this renderer is publishing, as the runtime announces them.
    ndi_published: ArcSwap<Vec<String>>,
    /// The last picture of the whole composition, as raw RGBA.
    ///
    /// Encoded by the control thread rather than here: a PNG is a few
    /// milliseconds of work, which is several frames, and a picture for
    /// somebody to look at must never be able to make one late.
    /// The last picture of the whole composition, with the shape it was taken
    /// at: the composition's own, not the thumbnail box's.
    composition_preview: ArcSwap<Option<(ovs_types::geometry::SizePx, Vec<u8>)>>,
    /// The last preview taken of each output.
    ///
    /// Published from the render thread and read by the control thread, through
    /// the same wait-free swap as everything else that crosses between them: a
    /// picture for somebody to look at must never be able to make a frame late.
    previews: ArcSwap<Vec<crate::render_loop::Preview>>,
    /// Set once the core has been connected at least once.
    ///
    /// Only used for reporting; the render path never consults it.
    ever_connected: AtomicBool,
    connected: AtomicBool,
}

impl Default for SharedShowState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedShowState {
    /// Creates empty state: no show, not connected.
    #[must_use]
    pub fn new() -> Self {
        Self {
            current: ArcSwap::from_pointee(ShowState::default()),
            telemetry: ArcSwap::from_pointee(Vec::new()),
            previews: ArcSwap::from_pointee(Vec::new()),
            gpu: ArcSwap::from_pointee(None),
            highlighted_piece: ArcSwap::from_pointee(None),
            playback: ArcSwap::from_pointee(ovs_types::telemetry::PlaybackTelemetry::default()),
            playheads: ArcSwap::from_pointee(Vec::new()),
            missing_outputs: ArcSwap::from_pointee(Vec::new()),
            frame: ArcSwap::from_pointee(None),
            composition_preview: ArcSwap::from_pointee(None),
            previews_enabled: AtomicBool::new(true),
            // Off, always, on every start. A test pattern that survived a
            // restart would be a wall showing a grid to an audience.
            reset_counters: std::sync::Mutex::new(None),
            wanted_streams: ArcSwap::from_pointee(Vec::new()),
            stream_chunks: std::sync::Mutex::new(Vec::new()),
            stream_failures: std::sync::Mutex::new(Vec::new()),
            stream_encodings: std::sync::Mutex::new(std::collections::HashMap::new()),
            test_pattern: std::sync::atomic::AtomicU8::new(OFF),
            connector_roles: ArcSwap::from_pointee(std::collections::BTreeMap::new()),
            console_mode: ArcSwap::from_pointee(None),
            connectors: ArcSwap::from_pointee(Vec::new()),
            ndi_sources: ArcSwap::from_pointee(Vec::new()),
            ndi_published: ArcSwap::from_pointee(Vec::new()),
            media: ArcSwap::from_pointee(MediaCatalogue::new()),
            ever_connected: AtomicBool::new(false),
            connected: AtomicBool::new(false),
        }
    }

    /// Reads the current state without blocking.
    ///
    /// Called once per frame from the render thread. It cannot block and it
    /// cannot fail.
    #[must_use]
    pub fn load(&self) -> Arc<ShowState> {
        self.current.load_full()
    }

    /// Replaces the state wholesale, as on a snapshot.
    pub fn replace(&self, state: ShowState) {
        self.current.store(Arc::new(state));
    }

    /// Applies an incremental change.
    ///
    /// Returns false when the update did not follow on from the state held, in
    /// which case the caller must ask the core for a fresh snapshot. Applying it
    /// anyway would put something on stage that the core never asked for.
    pub fn apply(&self, sequence: Sequence, change: &StateChange) -> bool {
        let current = self.current.load();
        if sequence != current.sequence.next() {
            return false;
        }

        let mut next = ShowState::clone(&current);
        next.sequence = sequence;

        match change {
            StateChange::ShowLoaded {
                show,
                master,
                layers,
                effects,
                generators,
            } => {
                // Everything at once, because a show load replaces
                // everything. The live state travels with the show rather
                // than being worked out from it: the restart policy lives in the
                // core, and a renderer that reimplemented it would
                // put its own idea of the opening state on stage.
                next.show = show.clone();
                next.master = *master;
                next.layers = layers.clone();
                next.effects = Arc::new(effects.clone());
                next.generators = Arc::new(generators.clone());
            }
            StateChange::MasterChanged { master } => next.master = *master,
            StateChange::EffectChanged {
                effect_instance_id,
                overrides,
            } => {
                // The whole of that instance's live values, so this replaces
                // rather than merges: an operator clearing one sends the rest,
                // and a value left behind here would be an effect still running
                // at something nobody asked for.
                let mut effects = EffectOverrides::clone(&next.effects);
                if overrides.is_empty() {
                    effects.remove(effect_instance_id);
                } else {
                    effects.insert(*effect_instance_id, overrides.clone());
                }
                next.effects = Arc::new(effects);
            }
            StateChange::GeneratorChanged {
                generator_instance_id,
                overrides,
            } => {
                // The same replace-rather-than-merge an effect's values get,
                // for the same reason.
                let mut generators = GeneratorOverrides::clone(&next.generators);
                if overrides.is_empty() {
                    generators.remove(generator_instance_id);
                } else {
                    generators.insert(*generator_instance_id, overrides.clone());
                }
                next.generators = Arc::new(generators);
            }
            StateChange::LayerChanged { layer } => {
                if let Some(existing) = next
                    .layers
                    .iter_mut()
                    .find(|live| live.layer_id == layer.layer_id)
                {
                    *existing = *layer;
                } else {
                    next.layers.push(*layer);
                }
            }
            // The renderer's own status comes from the renderer. Accepting the
            // core's opinion of it would let a stale message overwrite what is
            // actually happening.
            StateChange::RendererChanged { .. } => {}
        }

        self.current.store(Arc::new(next));
        true
    }

    /// Replaces the media catalogue.
    ///
    /// The core always sends the complete set, so this replaces rather than
    /// merges: an item that has gone is one the renderer must stop drawing,
    /// and a merge would keep it forever.
    pub fn set_media(&self, entries: Vec<MediaLocation>) {
        let catalogue: MediaCatalogue = entries
            .into_iter()
            .map(|entry| (entry.media_item_id, entry))
            .collect();
        self.media.store(Arc::new(catalogue));
    }

    /// Reads the media catalogue without blocking.
    #[must_use]
    pub fn media(&self) -> Arc<MediaCatalogue> {
        self.media.load_full()
    }

    /// Publishes what the render thread measured.
    ///
    /// Called about once a second from the render thread, never per frame.
    pub fn publish_telemetry(&self, outputs: Vec<OutputTelemetry>) {
        self.telemetry.store(Arc::new(outputs));
    }

    /// Records whether previews should be produced.
    pub fn set_previews_enabled(&self, enabled: bool) {
        self.previews_enabled.store(enabled, Ordering::Relaxed);
    }

    /// Returns whether previews should be produced.
    ///
    /// On unless the core has said otherwise. A renderer that came up with them
    /// off because nobody had told it yet would leave an operator looking at an
    /// empty console during setup.
    #[must_use]
    pub fn previews_enabled(&self) -> bool {
        self.previews_enabled.load(Ordering::Relaxed)
    }

    /// Records what this machine is wired to.
    ///
    /// The renderer is the only process that can know: it is the one holding the
    /// GPU. A client cannot offer an operator a list of sockets to assign
    /// without this.
    pub fn set_connectors(&self, connectors: Vec<ovs_ipc::DiscoveredConnector>) {
        self.connectors.store(Arc::new(connectors));
    }

    /// Returns what this machine is wired to.
    #[must_use]
    pub fn connectors(&self) -> Arc<Vec<ovs_ipc::DiscoveredConnector>> {
        self.connectors.load_full()
    }

    /// Records the NDI sources discovery can see.
    pub fn set_ndi_sources(&self, sources: Vec<String>) {
        self.ndi_sources.store(Arc::new(sources));
    }

    /// Returns the NDI sources discovery last saw, this renderer's own included.
    #[must_use]
    pub fn ndi_sources(&self) -> Arc<Vec<String>> {
        self.ndi_sources.load_full()
    }

    /// Records what this renderer is publishing on the network.
    pub fn set_published_ndi(&self, names: Vec<String>) {
        self.ndi_published.store(Arc::new(names));
    }

    /// The NDI sources worth offering an operator.
    ///
    /// Everything discovery found except this renderer's own outputs (owner,
    /// 2026-09-25: "kein loopback schaffen"). Putting a server's own output on
    /// one of its own layers is a feedback loop, and nothing in a list of names
    /// would tell an operator which entry does that.
    ///
    /// Subtracted by the whole announced name. Matching the part in brackets
    /// would take a second machine's identically named source out with ours,
    /// which is a worse fault than the one being avoided.
    #[must_use]
    pub fn ndi_offered(&self) -> Vec<String> {
        let mine = self.ndi_published.load();
        self.ndi_sources
            .load()
            .iter()
            .filter(|name| !mine.contains(name))
            .cloned()
            .collect()
    }

    /// Records what each connector on this machine is for.
    pub fn set_connector_roles(
        &self,
        roles: std::collections::BTreeMap<String, ConnectorRole>,
        console_mode: Option<ovs_types::config::DisplayMode>,
    ) {
        tracing::info!(
            count = roles.len(),
            ?console_mode,
            "connector roles received"
        );
        self.connector_roles.store(Arc::new(roles));
        self.console_mode.store(Arc::new(console_mode));
    }

    /// The mode the System screen's connector should be driven at.
    ///
    /// `None` means the panel's own preference, which is both the default and a
    /// deliberate answer: a panel that advertises a mode it cannot show is why
    /// an operator can say otherwise.
    #[must_use]
    pub fn console_mode(&self) -> Option<ovs_types::config::DisplayMode> {
        *self.console_mode.load().as_ref()
    }

    /// Returns what each connector is for.
    ///
    /// Empty until the core has said. A connector that is not in the map has had
    /// no decision made about it, which the caller must read as "not a show
    /// output" — an unassigned socket stays dark rather than having a show put
    /// on it.
    #[must_use]
    pub fn connector_roles(&self) -> Arc<std::collections::BTreeMap<String, ConnectorRole>> {
        self.connector_roles.load_full()
    }

    /// Records which surfaces somebody is watching.
    pub fn set_wanted_streams(&self, surfaces: Vec<ovs_types::telemetry::StreamSurface>) {
        self.wanted_streams.store(Arc::new(surfaces));
    }

    /// Which surfaces somebody is watching.
    #[must_use]
    pub fn wanted_streams(&self) -> Arc<Vec<ovs_types::telemetry::StreamSurface>> {
        self.wanted_streams.load_full()
    }

    /// Queues an encoded picture for the core.
    ///
    /// Dropped rather than queued without limit when the far end is not
    /// keeping up: what a watcher wants is the newest picture, and a queue is
    /// a promise to show them an old one.
    pub fn publish_stream_chunk(&self, chunk: PendingChunk) {
        /// About a third of a second at sixty frames a second.
        const WAITING: usize = 20;

        let mut chunks = self
            .stream_chunks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if chunks.len() >= WAITING {
            chunks.remove(0);
        }
        chunks.push(chunk);
    }

    /// Records that a surface cannot be encoded, and why.
    ///
    /// Said once per attempt rather than per frame: the render loop remembers
    /// which surfaces it has already failed on, because a machine with no
    /// encoder will not grow one while the show runs.
    pub fn publish_stream_failure(
        &self,
        surface: ovs_types::telemetry::StreamSurface,
        reason: String,
    ) {
        self.stream_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((surface, reason));
    }

    /// Records what a surface turned out to be encoded with.
    ///
    /// Overwrites rather than queues: a surface has one answer at a time, and
    /// a restart during a slow poll should send the new one rather than both.
    pub fn publish_stream_encoding(
        &self,
        surface: ovs_types::telemetry::StreamSurface,
        encoding: ovs_types::telemetry::StreamEncoding,
    ) {
        self.stream_encodings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(surface, encoding);
    }

    /// Takes whatever encodings are waiting to be sent.
    #[must_use]
    pub fn take_stream_encodings(
        &self,
    ) -> Vec<(
        ovs_types::telemetry::StreamSurface,
        ovs_types::telemetry::StreamEncoding,
    )> {
        std::mem::take(
            &mut *self
                .stream_encodings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .into_iter()
        .collect()
    }

    /// Takes whatever failures are waiting to be sent.
    #[must_use]
    pub fn take_stream_failures(&self) -> Vec<(ovs_types::telemetry::StreamSurface, String)> {
        std::mem::take(
            &mut *self
                .stream_failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Takes whatever is waiting to be sent.
    #[must_use]
    pub fn take_stream_chunks(&self) -> Vec<PendingChunk> {
        std::mem::take(
            &mut *self
                .stream_chunks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Asks the render loop to put some of the counters back to zero.
    pub fn request_counter_reset(&self, group: ovs_types::telemetry::CounterGroup) {
        let mut pending = self
            .reset_counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *pending = Some(match *pending {
            Some(waiting) if waiting != group => ovs_types::telemetry::CounterGroup::All,
            _ => group,
        });
    }

    /// Takes that request, if there is one.
    #[must_use]
    pub fn take_counter_reset(&self) -> Option<ovs_types::telemetry::CounterGroup> {
        self.reset_counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Puts the test pattern on the composition, the outputs or the pieces,
    /// or takes it off.
    ///
    /// The chosen piece travels with the mode because it is part of the same
    /// question, and because one store cannot be half-applied the way two can:
    /// a frame drawn between them would highlight the piece before last.
    pub fn set_test_pattern(&self, mode: TestPatternMode, highlighted: Option<PieceId>) {
        let value = match mode {
            TestPatternMode::Off => OFF,
            TestPatternMode::Composition => COMPOSITION,
            TestPatternMode::Outputs => OUTPUTS,
            TestPatternMode::Pieces => PIECES,
        };
        self.highlighted_piece.store(Arc::new(highlighted));
        self.test_pattern.store(value, Ordering::Relaxed);
    }

    /// The piece the pattern draws as the chosen one, if any.
    #[must_use]
    pub fn highlighted_piece(&self) -> Option<PieceId> {
        **self.highlighted_piece.load()
    }

    /// Which surfaces are drawing the test pattern instead of the show.
    #[must_use]
    pub fn test_pattern(&self) -> TestPatternMode {
        match self.test_pattern.load(Ordering::Relaxed) {
            COMPOSITION => TestPatternMode::Composition,
            OUTPUTS => TestPatternMode::Outputs,
            PIECES => TestPatternMode::Pieces,
            // Nothing else can be written by the setter above, and the show is
            // the right answer to a number nobody wrote.
            _ => TestPatternMode::Off,
        }
    }

    /// Publishes a picture of the whole composition.
    pub fn publish_composition_preview(
        &self,
        picture: Option<(ovs_types::geometry::SizePx, Vec<u8>)>,
    ) {
        self.composition_preview.store(Arc::new(picture));
    }

    /// Returns the last picture of the whole composition.
    #[must_use]
    pub fn composition_preview(&self) -> Arc<Option<(ovs_types::geometry::SizePx, Vec<u8>)>> {
        self.composition_preview.load_full()
    }

    /// Publishes the configured outputs nothing is driving.
    pub fn publish_missing_outputs(&self, missing: Vec<ovs_types::telemetry::MissingOutput>) {
        self.missing_outputs.store(Arc::new(missing));
    }

    /// Returns the configured outputs nothing is driving.
    #[must_use]
    pub fn missing_outputs(&self) -> Arc<Vec<ovs_types::telemetry::MissingOutput>> {
        self.missing_outputs.load_full()
    }

    /// Publishes where the last frame's time went.
    pub fn publish_frame_timings(&self, frame: ovs_types::telemetry::FrameTimings) {
        self.frame.store(Arc::new(Some(frame)));
    }

    /// Returns where the last frame's time went.
    ///
    /// `None` until a frame has been drawn: a renderer that has not started is
    /// not a renderer whose frames cost nothing.
    #[must_use]
    pub fn frame_timings(&self) -> Option<ovs_types::telemetry::FrameTimings> {
        *self.frame.load_full()
    }

    /// Publishes what the players are doing.
    pub fn publish_playback(&self, playback: ovs_types::telemetry::PlaybackTelemetry) {
        self.playback.store(Arc::new(playback));
    }

    /// Returns what the players last reported.
    #[must_use]
    pub fn playback(&self) -> ovs_types::telemetry::PlaybackTelemetry {
        *self.playback.load_full()
    }

    /// Publishes where each playing layer's playhead is.
    pub fn publish_playheads(&self, playheads: Vec<ovs_types::telemetry::LayerPlayhead>) {
        self.playheads.store(Arc::new(playheads));
    }

    /// Returns where each playing layer's playhead last was.
    ///
    /// Empty when nothing is playing a clip, which is the same answer as a
    /// renderer that has not drawn yet: neither has a bar to offer.
    #[must_use]
    pub fn playheads(&self) -> Arc<Vec<ovs_types::telemetry::LayerPlayhead>> {
        self.playheads.load_full()
    }

    /// Publishes what the device reports about itself.
    pub fn publish_gpu(&self, gpu: Option<ovs_types::telemetry::GpuTelemetry>) {
        self.gpu.store(Arc::new(gpu));
    }

    /// Returns what the device last reported about itself.
    #[must_use]
    pub fn gpu(&self) -> Option<ovs_types::telemetry::GpuTelemetry> {
        self.gpu.load_full().as_ref().clone()
    }

    /// Publishes the latest previews.
    pub fn publish_previews(&self, previews: Vec<crate::render_loop::Preview>) {
        self.previews.store(Arc::new(previews));
    }

    /// Returns the last published previews.
    #[must_use]
    pub fn previews(&self) -> Arc<Vec<crate::render_loop::Preview>> {
        self.previews.load_full()
    }

    /// Returns the last published telemetry.
    #[must_use]
    pub fn telemetry(&self) -> Arc<Vec<OutputTelemetry>> {
        self.telemetry.load_full()
    }

    /// Records that the core is connected.
    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Relaxed);
        if connected {
            self.ever_connected.store(true, Ordering::Relaxed);
        }
    }

    /// Returns true when the core is currently connected.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Returns the status to report, given that rendering is happening.
    ///
    /// A missing core is `Degraded`, never `Faulted`: output continues, only
    /// control does not (house rule §17).
    #[must_use]
    pub fn status(&self) -> RendererStatus {
        if self.is_connected() || !self.ever_connected.load(Ordering::Relaxed) {
            RendererStatus::Running
        } else {
            RendererStatus::Degraded {
                reason: "the core is not connected; rendering the last known state".to_owned(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::{Layer, Show};
    use ovs_types::media::MediaKind;

    use super::*;

    fn located(media_item_id: MediaItemId, path: &str) -> MediaLocation {
        MediaLocation {
            media_item_id,
            kind: MediaKind::Image,
            path: path.into(),
            dimensions: None,
            duration_ms: None,
            frame_rate: None,
        }
    }

    fn snapshot(at: u64) -> StateSnapshot {
        StateSnapshot {
            sequence: Sequence(at),
            show: Some(Show::new("Show")),
            master: MasterLive::default(),
            layers: Vec::new(),
            effects: Default::default(),
            generators: Default::default(),
            renderer: RendererStatus::Running,
        }
    }

    fn a_radius(radius: f64) -> ovs_types::config::ParameterMap {
        ovs_types::config::ParameterMap::from([(
            "radius_px".to_owned(),
            ovs_types::config::ParameterValue::Float(radius),
        )])
    }

    #[test]
    fn a_client_joining_mid_show_is_given_the_effects_that_are_already_moved() {
        // The renderer is one of those clients. Coming back to a show with an
        // effect pushed somewhere and drawing it at its configured value would
        // be a visible snap on the wall every time it resynchronised.
        let shared = SharedShowState::new();
        let instance = ovs_types::id::EffectInstanceId::new();

        let mut opening = snapshot(10);
        opening.effects = EffectOverrides::from([(instance, a_radius(40.0))]);
        shared.replace(ShowState::from_snapshot(opening));

        let state = shared.load();
        assert_eq!(
            state
                .effect(instance)
                .and_then(|live| live.get("radius_px")),
            Some(&ovs_types::config::ParameterValue::Float(40.0))
        );
    }

    #[test]
    fn an_effect_change_replaces_that_instances_values_and_leaves_the_others() {
        let shared = SharedShowState::new();
        let first = ovs_types::id::EffectInstanceId::new();
        let second = ovs_types::id::EffectInstanceId::new();
        shared.replace(ShowState::from_snapshot(snapshot(10)));

        assert!(shared.apply(
            Sequence(11),
            &StateChange::EffectChanged {
                effect_instance_id: first,
                overrides: a_radius(40.0),
            }
        ));
        assert!(shared.apply(
            Sequence(12),
            &StateChange::EffectChanged {
                effect_instance_id: second,
                overrides: a_radius(12.0),
            }
        ));

        let state = shared.load();
        assert!(state.effect(first).is_some(), "one instance at a time");
        assert!(state.effect(second).is_some());
    }

    #[test]
    fn an_effect_put_back_stops_being_applied() {
        // An empty set is how "this effect is at what the show configured"
        // is said. Holding an entry with nothing in it would be a second way of
        // saying the same thing, and the renderer would keep looking it up.
        let shared = SharedShowState::new();
        let instance = ovs_types::id::EffectInstanceId::new();
        shared.replace(ShowState::from_snapshot(snapshot(10)));

        shared.apply(
            Sequence(11),
            &StateChange::EffectChanged {
                effect_instance_id: instance,
                overrides: a_radius(40.0),
            },
        );
        shared.apply(
            Sequence(12),
            &StateChange::EffectChanged {
                effect_instance_id: instance,
                overrides: ovs_types::config::ParameterMap::new(),
            },
        );

        assert!(shared.load().effect(instance).is_none());
    }

    #[test]
    fn loading_a_show_returns_every_effect_to_what_it_was_configured_with() {
        // The restart policy lives in the core, so the change says what the
        // live effect state now is rather than leaving the renderer to work it
        // out.
        let shared = SharedShowState::new();
        let instance = ovs_types::id::EffectInstanceId::new();
        shared.replace(ShowState::from_snapshot(snapshot(10)));
        shared.apply(
            Sequence(11),
            &StateChange::EffectChanged {
                effect_instance_id: instance,
                overrides: a_radius(40.0),
            },
        );

        assert!(shared.apply(
            Sequence(12),
            &StateChange::ShowLoaded {
                show: Some(Box::new(Show::new("Other"))),
                master: MasterLive::default(),
                layers: Vec::new(),
                effects: EffectOverrides::new(),
                generators: Default::default(),
            }
        ));

        assert!(shared.load().effect(instance).is_none());
    }

    #[test]
    fn a_fresh_renderer_has_nothing_to_draw_but_is_still_running() {
        let shared = SharedShowState::new();
        assert!(!shared.load().has_show());
        assert_eq!(shared.status(), RendererStatus::Running);
    }

    #[test]
    fn a_snapshot_replaces_everything() {
        let shared = SharedShowState::new();
        shared.replace(ShowState::from_snapshot(snapshot(10)));

        let state = shared.load();
        assert_eq!(state.sequence, Sequence(10));
        assert!(state.has_show());
    }

    #[test]
    fn updates_apply_in_order() {
        let shared = SharedShowState::new();
        shared.replace(ShowState::from_snapshot(snapshot(10)));

        let master = MasterLive {
            bpm: 174.0,
            ..MasterLive::default()
        };
        assert!(shared.apply(Sequence(11), &StateChange::MasterChanged { master }));
        assert_eq!(shared.load().master.bpm, 174.0);
        assert_eq!(shared.load().sequence, Sequence(11));
    }

    #[test]
    fn an_update_that_does_not_follow_on_is_refused() {
        // Applying it would render something the core never asked for.
        let shared = SharedShowState::new();
        shared.replace(ShowState::from_snapshot(snapshot(10)));

        let master = MasterLive {
            bpm: 174.0,
            ..MasterLive::default()
        };
        assert!(!shared.apply(Sequence(14), &StateChange::MasterChanged { master }));
        assert_eq!(shared.load().master.bpm, 120.0, "state must not have moved");
        assert_eq!(shared.load().sequence, Sequence(10));
    }

    #[test]
    fn a_replayed_update_is_refused_rather_than_applied_twice() {
        let shared = SharedShowState::new();
        shared.replace(ShowState::from_snapshot(snapshot(10)));
        let master = MasterLive {
            bpm: 174.0,
            ..MasterLive::default()
        };
        assert!(shared.apply(Sequence(11), &StateChange::MasterChanged { master }));
        assert!(!shared.apply(Sequence(11), &StateChange::MasterChanged { master }));
    }

    #[test]
    fn a_layer_change_updates_in_place_or_appends() {
        let shared = SharedShowState::new();
        shared.replace(ShowState::from_snapshot(snapshot(0)));

        let layer_id = LayerId::new();
        let live = LayerLive {
            active_slot: 3,
            ..LayerLive::black(layer_id)
        };
        assert!(shared.apply(Sequence(1), &StateChange::LayerChanged { layer: live }));
        assert_eq!(
            shared.load().layer(layer_id).expect("present").active_slot,
            3
        );

        let moved = LayerLive {
            active_slot: 5,
            ..live
        };
        assert!(shared.apply(Sequence(2), &StateChange::LayerChanged { layer: moved }));
        assert_eq!(
            shared.load().layers.len(),
            1,
            "the layer must not be duplicated"
        );
        assert_eq!(
            shared.load().layer(layer_id).expect("present").active_slot,
            5
        );
    }

    #[test]
    fn loading_a_show_replaces_the_previous_live_state() {
        // Both halves matter. Keeping the old show's layers would composite
        // them over the new show; dropping the new show's layers without
        // putting anything in their place would leave every layer with no live
        // state at all, which composes to nothing — a show that loads and then
        // shows black.
        let shared = SharedShowState::new();
        shared.replace(ShowState::from_snapshot(snapshot(0)));
        let old_layer = LayerId::new();
        shared.apply(
            Sequence(1),
            &StateChange::LayerChanged {
                layer: LayerLive::black(old_layer),
            },
        );
        assert_eq!(shared.load().layers.len(), 1);

        let mut replacement = Show::new("Other");
        let layer = Layer::new("Layer 1", 0);
        let new_layer = layer.id;
        replacement.layers.push(layer);
        assert!(shared.apply(
            Sequence(2),
            &StateChange::ShowLoaded {
                show: Some(Box::new(replacement)),
                master: MasterLive {
                    dimmer: 0.5,
                    ..MasterLive::default()
                },
                layers: vec![LayerLive {
                    active_slot: 3,
                    ..LayerLive::black(new_layer)
                }],
                effects: EffectOverrides::new(),
                generators: Default::default(),
            }
        ));

        let state = shared.load();
        assert_eq!(state.layers.len(), 1);
        assert!(state.layer(old_layer).is_none(), "the old one is gone");
        assert_eq!(state.layer(new_layer).expect("present").active_slot, 3);
        assert!((state.master.dimmer - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn the_core_cannot_tell_the_renderer_what_its_own_status_is() {
        // The renderer knows whether it is rendering; the core only knows what
        // it was last told, which may be older than the truth.
        let shared = SharedShowState::new();
        shared.replace(ShowState::from_snapshot(snapshot(0)));
        shared.set_connected(true);

        assert!(shared.apply(
            Sequence(1),
            &StateChange::RendererChanged {
                renderer: RendererStatus::Faulted {
                    reason: "nonsense".to_owned()
                }
            }
        ));
        assert_eq!(shared.status(), RendererStatus::Running);
    }

    #[test]
    fn losing_the_core_degrades_rather_than_faults() {
        let shared = SharedShowState::new();
        shared.replace(ShowState::from_snapshot(snapshot(10)));
        shared.set_connected(true);
        assert_eq!(shared.status(), RendererStatus::Running);

        shared.set_connected(false);
        let status = shared.status();
        assert!(
            status.is_rendering(),
            "output continues when the core goes away"
        );
        assert!(matches!(status, RendererStatus::Degraded { .. }));

        // And the state it was given is still there to render.
        assert!(shared.load().has_show());
        assert_eq!(shared.load().sequence, Sequence(10));
    }

    #[test]
    fn a_renderer_that_has_never_seen_a_core_is_not_degraded() {
        // Starting before the core does is normal, not a fault.
        let shared = SharedShowState::new();
        assert_eq!(shared.status(), RendererStatus::Running);
    }

    #[test]
    fn telemetry_starts_empty_rather_than_invented() {
        // Empty means nothing has measured anything yet, which is different
        // from an output running at zero.
        let shared = SharedShowState::new();
        assert!(shared.telemetry().is_empty());
    }

    #[test]
    fn published_telemetry_is_what_comes_back() {
        let shared = SharedShowState::new();
        let entry = OutputTelemetry {
            output_id: ovs_types::id::OutputId::new(),
            name: "Wall".to_owned(),
            active: true,
            size: ovs_types::geometry::SizePx::new(1920, 1080),
            refresh: ovs_types::geometry::FrameRate::FPS_60,
            actual_fps: 59.94,
            frame_render_time_us: 4_000,
            dropped_frames: ovs_types::telemetry::DroppedFrames::default(),
            display: None,
            connector: None,
            // A stand-in output composes through nothing and is plugged
            // into nothing, so it reports the default pipeline and no
            // depth at all on the cable.
            composition_depth: ovs_types::depth::CompositionDepth::Eight,
            output_depth: ovs_types::depth::OutputDepth::Unknown,
            showing_pieces: true,
        };
        shared.publish_telemetry(vec![entry.clone()]);

        let reported = shared.telemetry();
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0], entry);
    }
    #[test]
    fn a_catalogue_survives_the_snapshots_that_follow_it() {
        // The property the whole placement rests on. A snapshot replaces the
        // show state, and the core sends snapshots whenever it resynchronises;
        // if that dropped the catalogue, every layer would fall back to its
        // placeholder on a reconnect that changed nothing.
        let shared = SharedShowState::new();
        let clip = MediaItemId::new();
        shared.set_media(vec![located(clip, "/srv/ovs/media/ab/clip.png")]);

        shared.replace(ShowState::from_snapshot(snapshot(2)));
        shared.replace(ShowState::from_snapshot(snapshot(3)));

        assert_eq!(shared.media().len(), 1);
        assert_eq!(
            shared.media().get(&clip).map(|entry| entry.path.clone()),
            Some("/srv/ovs/media/ab/clip.png".into())
        );
    }

    #[test]
    fn a_new_catalogue_replaces_the_old_one_entirely() {
        // Loading another show must not leave the previous show's clips
        // resolvable, or a slot pointing at a deleted item would keep playing.
        let shared = SharedShowState::new();
        let first = MediaItemId::new();
        let second = MediaItemId::new();
        shared.set_media(vec![located(first, "/media/first.png")]);
        shared.set_media(vec![located(second, "/media/second.png")]);

        assert_eq!(shared.media().len(), 1);
        assert!(shared.media().contains_key(&second));
        assert!(!shared.media().contains_key(&first));
    }

    #[test]
    fn an_empty_catalogue_means_nothing_resolves() {
        let shared = SharedShowState::new();
        shared.set_media(vec![located(MediaItemId::new(), "/media/clip.png")]);
        shared.set_media(Vec::new());

        assert!(shared.media().is_empty());
    }

    /// A server's own outputs are not offered back to it as sources.
    ///
    /// Owner, 2026-09-25: "kein loopback schaffen". Putting a server's own
    /// output on one of its own layers is a feedback loop, and nothing in a
    /// list of names would tell an operator which entry does that.
    #[test]
    fn a_renderer_is_not_offered_its_own_outputs() {
        let shared = SharedShowState::new();
        shared.set_ndi_sources(vec![
            "OTHER (Camera 1)".to_owned(),
            "OURS (Wall Left)".to_owned(),
            "OURS (Wall Right)".to_owned(),
        ]);
        shared.set_published_ndi(vec!["OURS (Wall Left)".to_owned()]);

        assert_eq!(
            shared.ndi_offered(),
            vec![
                "OTHER (Camera 1)".to_owned(),
                "OURS (Wall Right)".to_owned()
            ],
            "only the one this renderer publishes comes out"
        );
        assert_eq!(
            shared.ndi_sources().len(),
            3,
            "what is on the network is still all of it, for the log line that says so"
        );
    }

    /// Subtracted by the whole name, so somebody else's is not taken with ours.
    ///
    /// The fault this guards against is worse than the one being fixed: matching
    /// the part in brackets would take a second machine's identically named
    /// source out of the operator's list, and they would have no way to know it
    /// had ever been there.
    #[test]
    fn another_machine_sending_the_same_name_is_still_offered() {
        let shared = SharedShowState::new();
        shared.set_ndi_sources(vec![
            "OURS (Programme)".to_owned(),
            "STUDIO-2 (Programme)".to_owned(),
        ]);
        shared.set_published_ndi(vec!["OURS (Programme)".to_owned()]);

        assert_eq!(
            shared.ndi_offered(),
            vec!["STUDIO-2 (Programme)".to_owned()]
        );
    }

    /// A renderer publishing nothing offers everything.
    #[test]
    fn publishing_nothing_subtracts_nothing() {
        let shared = SharedShowState::new();
        shared.set_ndi_sources(vec!["OTHER (Camera 1)".to_owned()]);
        assert_eq!(shared.ndi_offered(), vec!["OTHER (Camera 1)".to_owned()]);
    }
}
