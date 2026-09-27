//! Shared server state.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use ovs_db::Database;
use ovs_proto::realtime::ServerMessage;
use ovs_types::telemetry::OutputTelemetry;
use tokio::sync::broadcast;

use crate::host::{HostMonitor, HostSample};
use crate::media_store::MediaStore;
use crate::state::{AuthoritativeState, SequencedChange};
use crate::uploads::Uploads;
use crate::Sessions;

/// How many updates a slow client may fall behind before it is disconnected.
///
/// A disconnected client resynchronises and catches up. Buffering without limit
/// would let one stalled laptop grow the server's memory until something more
/// important fails, which is exactly the dependency the failure philosophy
/// forbids (house rule §17).
const UPDATE_BUFFER: usize = 256;

/// The latest picture of one output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Which output it is a picture of.
    pub output_id: ovs_types::id::OutputId,
    /// Its operator-visible name.
    pub name: String,
    /// The picture, as PNG bytes.
    pub png: Vec<u8>,
    /// When it arrived.
    pub received_at: ovs_types::time::Timestamp,
}

/// One encoded picture on its way to whoever is watching.
#[derive(Debug, Clone)]
pub struct StreamChunk {
    /// What the surface is being encoded at.
    pub size: ovs_types::geometry::SizePx,
    /// Whether a watcher can start here.
    pub keyframe: bool,
    /// Microseconds since that stream started.
    pub timestamp_us: u64,
    /// The bytes, H.264 Annex-B.
    pub data: Vec<u8>,
}

/// What a watcher is sent.
///
/// One channel rather than two, so that a watcher cannot be told a surface is
/// software-encoded at fifteen frames a second after it has already been
/// shown a second of pictures and drawn its own conclusion.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// One encoded picture.
    Chunk(Arc<StreamChunk>),
    /// What this surface turned out to be encoded with.
    Encoding(Arc<ovs_types::telemetry::StreamEncoding>),
}

/// What a new watcher is handed.
pub struct Watching {
    /// Everything sent from now on.
    pub events: broadcast::Receiver<StreamEvent>,
    /// The last frame it can start from, so a preview opened between keyframes
    /// shows a picture now rather than in two seconds.
    pub keyframe: Option<Arc<StreamChunk>>,
    /// Why this surface cannot be streamed, if it cannot.
    pub failure: Option<String>,
    /// What it is being encoded with, when that is already known.
    pub encoding: Option<Arc<ovs_types::telemetry::StreamEncoding>>,
}

/// One surface being watched.
struct Stream {
    /// Where chunks go. Slow watchers miss frames rather than delaying fast
    /// ones, which for video is the only sane answer: the picture they want is
    /// the newest one, not the one they missed.
    chunks: broadcast::Sender<StreamEvent>,
    /// The last frame a watcher can start from.
    ///
    /// Kept so that somebody opening a preview sees a picture immediately
    /// rather than at the next keyframe, which is up to two seconds away.
    keyframe: Option<Arc<StreamChunk>>,
    /// How many are watching. Nobody watching means nothing is encoded.
    watchers: usize,
    /// Why this surface cannot be streamed, if it cannot.
    failure: Option<String>,
    /// What it turned out to be encoded with, once the renderer has said.
    ///
    /// Kept as well as broadcast, because a second watcher arriving later is
    /// owed the same answer and the renderer says it once (Q13).
    encoding: Option<Arc<ovs_types::telemetry::StreamEncoding>>,
}

/// Everything a request handler needs.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    /// The database is behind a plain mutex because every operation on it is a
    /// short, synchronous SQLite call. An async mutex would add a scheduling hop
    /// for no benefit.
    database: Mutex<Database>,
    state: RwLock<AuthoritativeState>,
    sessions: RwLock<Sessions>,
    updates: broadcast::Sender<ServerMessage>,
    /// What the renderer last reported about its outputs.
    ///
    /// Empty until a renderer connects and reports. Empty is honest: it means
    /// nothing has said anything about any output, which is different from an
    /// output running at zero.
    output_telemetry: RwLock<Vec<OutputTelemetry>>,
    /// What the renderer's device last reported about itself.
    ///
    /// Empty until a renderer connects and says. Empty is honest: it means
    /// nothing has said anything about any device.
    gpu_telemetry: RwLock<Option<ovs_types::telemetry::GpuTelemetry>>,
    /// Whether the renderer should produce previews.
    ///
    /// Mirrored here from the database so the link thread can read it without
    /// touching SQLite on every pass. The database is where it survives a
    /// restart; this is where it is read sixty times a minute.
    previews_enabled: AtomicBool,
    /// What the renderer last said this machine is wired to.
    ///
    /// Held in memory rather than stored: it describes the hardware as it is
    /// right now, and the renderer is the only thing that can know. What *is*
    /// stored is what each connector is for, which is a decision.
    connectors: RwLock<Vec<ovs_ipc::DiscoveredConnector>>,
    /// The NDI sources the renderer last reported seeing.
    ///
    /// Kept as well as broadcast, so a client connecting between two changes is
    /// answered from here rather than waiting for a sender to be switched on.
    ndi_sources: RwLock<Vec<String>>,
    local_account: ovs_types::id::AccountId,
    /// Set when a connector's role changed and the renderer has not been told.
    connector_roles_changed: AtomicBool,
    test_pattern: RwLock<ovs_types::config::TestPatternMode>,
    /// The piece the pieces pattern draws as the chosen one.
    ///
    /// Not persisted, for the same reason the mode is not: it is a thing
    /// on a wall, and the next person to start this server is entitled to
    /// assume the wall shows the show.
    highlighted_piece: RwLock<Option<ovs_types::id::PieceId>>,
    /// Set when somebody has asked for the renderer to be restarted.
    ///
    /// A flag rather than a channel because the link thread is already looping
    /// on a timeout, and because the request is idempotent: two operators both
    /// pressing the button want one restart, not two.
    restart_renderer: AtomicBool,
    /// Permissions to watch one surface, minted and not yet spent.
    stream_tickets: Mutex<crate::stream_tickets::StreamTickets>,
    /// The surfaces somebody is watching, and the last thing each sent.
    streams: RwLock<BTreeMap<ovs_types::telemetry::StreamSurface, Stream>>,
    /// Whether the set of watched surfaces has changed since the renderer was
    /// last told.
    streams_changed: AtomicBool,
    /// Which counters an operator has asked to clear, if any.
    reset_counters: RwLock<Option<ovs_types::telemetry::CounterGroup>>,
    /// The last picture of the whole composition, as a PNG.
    ///
    /// Not persisted and not part of the state stream, exactly like an output's
    /// preview: it is what the show looked like a moment ago, which is only
    /// ever interesting now.
    composition_preview: RwLock<Option<Vec<u8>>>,
    /// Configured outputs the renderer says nothing is driving.
    missing_outputs: RwLock<Vec<ovs_types::telemetry::MissingOutput>>,
    /// What the renderer's players last reported.
    ///
    /// `None` until a renderer says. Nothing here invents it: the core does not
    /// decode, so it cannot know.
    playback_telemetry: RwLock<Option<ovs_types::telemetry::PlaybackTelemetry>>,
    frame_timings: RwLock<Option<ovs_types::telemetry::FrameTimings>>,
    /// Where the renderer last said each playing layer's playhead was.
    ///
    /// Beside the counts rather than on them: a playhead moves continuously and
    /// nobody edits it, so it belongs with the measurements and not in the
    /// authoritative live state — the same reasoning as the master clock's
    /// position. A state stream that published either would be a metronome.
    playheads: RwLock<Vec<ovs_types::telemetry::LayerPlayhead>>,
    /// What is arriving on the lighting network.
    ///
    /// `None` until the DMX link says, which it only does when it is listening.
    /// Absent therefore means "not listening", and an empty universe list means
    /// "listening and nothing has arrived" — two different things an operator
    /// standing in front of a dark wall very much needs to tell apart.
    dmx_telemetry: RwLock<Option<ovs_types::telemetry::DmxTelemetry>>,
    /// What the machine itself is doing, sampled in the background.
    host: HostMonitor,
    /// The latest picture of each output, as a PNG.
    ///
    /// Not persisted and not part of the state stream: a preview is what an
    /// output looked like a moment ago, which is only ever interesting now.
    previews: RwLock<BTreeMap<ovs_types::id::OutputId, Preview>>,
    /// The live snapshot last written, and which show it belongs to.
    ///
    /// Only so that a periodic save can tell "nothing changed" from "something
    /// did" without reading the database.
    persisted_snapshot: RwLock<Option<(ovs_types::id::ShowId, ovs_types::live::LiveSnapshot)>>,
    /// Uploads this server is currently expecting bytes for.
    ///
    /// In memory and not persisted: an upload interrupted by a restart starts
    /// again, which is what a client would have to do anyway.
    uploads: RwLock<Uploads>,
    /// Where media files live, when this server has somewhere to put them.
    ///
    /// `None` is a server that cannot resolve media to files. A test harness is
    /// the ordinary case; the effect is that every layer draws its placeholder,
    /// which is visibly "no media here" rather than a wrong file.
    media_store: Option<MediaStore>,
    server_version: String,
    started_at: Instant,
}

impl AppState {
    /// Builds server state around an opened database, with no media store.
    ///
    /// Media resolves to nothing, so a show's layers draw their placeholder
    /// colours. For a server that actually shows media, see
    /// [`with_media_root`](AppState::with_media_root).
    #[must_use]
    pub fn new(database: Database, server_version: impl Into<String>) -> Self {
        Self::build(database, server_version, None)
    }

    /// Builds server state that resolves media to files under `media_root`.
    #[must_use]
    pub fn with_media_root(
        database: Database,
        server_version: impl Into<String>,
        media_root: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::build(
            database,
            server_version,
            Some(MediaStore::new(media_root.into())),
        )
    }

    fn build(
        database: Database,
        server_version: impl Into<String>,
        media_store: Option<MediaStore>,
    ) -> Self {
        let (updates, _) = broadcast::channel(UPDATE_BUFFER);
        Self {
            inner: Arc::new(Inner {
                database: Mutex::new(database),
                state: RwLock::new(AuthoritativeState::new()),
                sessions: RwLock::new(Sessions::new()),
                updates,
                output_telemetry: RwLock::new(Vec::new()),
                gpu_telemetry: RwLock::new(None),
                dmx_telemetry: RwLock::new(None),
                playback_telemetry: RwLock::new(None),
                frame_timings: RwLock::new(None),
                playheads: RwLock::new(Vec::new()),
                missing_outputs: RwLock::new(Vec::new()),
                composition_preview: RwLock::new(None),
                restart_renderer: AtomicBool::new(false),
                reset_counters: RwLock::new(None),
                stream_tickets: Mutex::new(crate::stream_tickets::StreamTickets::new()),
                streams: RwLock::new(BTreeMap::new()),
                streams_changed: AtomicBool::new(false),
                previews_enabled: AtomicBool::new(true),
                connectors: RwLock::new(Vec::new()),
                ndi_sources: RwLock::new(Vec::new()),
                // Who a request from the machine itself is acting as. Generated
                // per process and never stored: it is an identity for the logs,
                // not an account somebody can sign in to.
                local_account: ovs_types::id::AccountId::new(),
                connector_roles_changed: AtomicBool::new(false),
                // Never stored and never restored: a server that came back from
                // a reboot with a test pattern on the wall would be a dark show
                // that looks like a working one.
                test_pattern: RwLock::new(ovs_types::config::TestPatternMode::Off),
                highlighted_piece: RwLock::new(None),
                host: HostMonitor::new(),
                previews: RwLock::new(BTreeMap::new()),
                persisted_snapshot: RwLock::new(None),
                uploads: RwLock::new(Uploads::new()),
                media_store,
                server_version: server_version.into(),
                started_at: Instant::now(),
            }),
        }
    }

    /// Runs an operation against the uploads in flight.
    ///
    /// A closure rather than a guard, so the lock cannot be held across an
    /// await point by accident — which on this path would mean holding it for
    /// the length of a network read.
    pub fn with_uploads<T>(&self, operation: impl FnOnce(&mut Uploads) -> T) -> T {
        let mut uploads = self
            .inner
            .uploads
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut uploads)
    }

    /// Records what the renderer's device reported.
    pub fn set_gpu_telemetry(&self, gpu: Option<ovs_types::telemetry::GpuTelemetry>) {
        let mut held = self
            .inner
            .gpu_telemetry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = gpu;
    }

    /// Returns what the renderer's device last reported.
    #[must_use]
    pub fn gpu_telemetry(&self) -> Option<ovs_types::telemetry::GpuTelemetry> {
        self.inner
            .gpu_telemetry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Records what the renderer says this machine is wired to.
    pub fn set_connectors(&self, connectors: Vec<ovs_ipc::DiscoveredConnector>) {
        tracing::info!(
            count = connectors.len(),
            "the renderer reported its connectors"
        );
        *self
            .inner
            .connectors
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = connectors;

        // Told, not left to be asked. A client's chooser was filled in when it
        // signed in, so a monitor changed afterwards left it describing the one
        // that had been there before (owner, 2026-09-25).
        self.publish(ServerMessage::Connectors {
            connectors: self.connector_infos(),
        });
    }

    /// What this machine is wired to, in the shape a client reads.
    ///
    /// The renderer reports sockets and modes; what each one is *for* is the
    /// operator's decision and lives in this server's own database. Joined in
    /// one place, because two joins are two answers and the one that runs less
    /// often is the one that drifts.
    #[must_use]
    pub fn connector_infos(&self) -> Vec<ovs_proto::http::ConnectorInfo> {
        let roles = self.connector_roles().unwrap_or_default();
        self.connectors()
            .into_iter()
            .map(|found| ovs_proto::http::ConnectorInfo {
                role: roles
                    .get(&ovs_db::connector_key(
                        &found.gpu_pci_address,
                        &found.connector,
                    ))
                    .copied()
                    .unwrap_or_default(),
                gpu_pci_address: found.gpu_pci_address,
                connector: found.connector,
                connected: found.connected,
                model: found.model,
                modes: found
                    .modes
                    .into_iter()
                    .map(|mode| ovs_proto::http::ConnectorModeInfo {
                        size: mode.size,
                        refresh_millihertz: mode.refresh_millihertz,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Returns the sockets on the cards the renderer does not render with.
    ///
    /// For the one setting that is chosen by card rather than by connector: the
    /// System screen. See [`crate::machine_connectors`] for why they cannot
    /// simply be added to the list above, and why they are named by the kernel
    /// rather than by Vulkan.
    ///
    /// Which card the renderer is on comes from what it reported: every
    /// connector it sends carries the address of the device it rendered with.
    /// Before it has said anything there is no card to exclude, and every socket
    /// on the machine comes back — which is the right answer while somebody is
    /// commissioning a rack with the renderer not yet running.
    #[must_use]
    pub fn connectors_elsewhere(&self) -> Vec<ovs_proto::http::ConnectorInfo> {
        let roles = self.connector_roles().unwrap_or_default();
        let renderer_gpu = self
            .connectors()
            .first()
            .map(|found| found.gpu_pci_address.clone())
            .unwrap_or_default();

        crate::machine_connectors::elsewhere(&renderer_gpu)
            .into_iter()
            .map(|found| ovs_proto::http::ConnectorInfo {
                role: roles
                    .get(&ovs_db::connector_key(
                        &found.gpu_pci_address,
                        &found.connector,
                    ))
                    .copied()
                    .unwrap_or_default(),
                gpu_pci_address: found.gpu_pci_address,
                connector: found.connector,
                connected: found.connected,
                // Neither is knowable without a driver open on the card, and a
                // socket that can only carry the System screen needs no mode
                // list: the compositor asks the panel itself.
                model: None,
                modes: Vec::new(),
            })
            .collect()
    }

    /// Returns what this machine is wired to, as the renderer last reported it.
    ///
    /// Empty until a renderer has said, which is different from "this machine
    /// has no connectors": a client offering an operator a list of sockets to
    /// assign has to be able to tell those apart.
    #[must_use]
    pub fn connectors(&self) -> Vec<ovs_ipc::DiscoveredConnector> {
        self.inner
            .connectors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Records the NDI sources the renderer can see, and tells every client.
    ///
    /// Told rather than left to be asked, because the operator's list has just
    /// become wrong and they are looking at it.
    pub fn set_ndi_sources(&self, sources: Vec<String>) {
        tracing::info!(
            count = sources.len(),
            "the renderer reported the NDI sources it can see"
        );
        *self
            .inner
            .ndi_sources
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = sources.clone();
        self.publish(ServerMessage::NdiSources { sources });
    }

    /// Returns the NDI sources the renderer last reported.
    ///
    /// Empty until a renderer has said, which is not the same as "there are
    /// none" — and not the same as this machine having no NDI runtime either.
    #[must_use]
    pub fn ndi_sources(&self) -> Vec<String> {
        self.inner
            .ndi_sources
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Returns what each connector is for.
    ///
    /// # Errors
    ///
    /// Returns the database error when the setting cannot be read.
    pub fn connector_roles(
        &self,
    ) -> Result<std::collections::BTreeMap<String, ovs_types::config::ConnectorRole>, ovs_db::DbError>
    {
        // **A stored row may withhold a socket; it may not claim one.** Rows
        // saying `MEDIA_OUTPUT` exist on machines where that was once the only
        // way to make an output work, and one of them outlived the output it was
        // written for: the show let the socket go and the row held it, which is
        // exactly what stops somebody freeing a monitor in order to put the
        // System screen on it (owner, 2026-09-25).
        //
        // What is left is a deliberate `UNUSED`, which the show overrides below
        // anyway — so nothing writes a row that does anything, and
        // `PUT /server/connectors/role` is a control that reaches nothing.
        let mut roles = self.with_database(|database| database.connector_roles())?;
        roles.retain(|_, role| !matches!(role, ovs_types::config::ConnectorRole::MediaOutput));

        // **A socket the show uses is a media output.** Owner, 2026-09-25:
        // "Unused ist = Free = kann für alles benutzt werden, bis er dann benutzt
        // wird."
        //
        // Derived from the show rather than stored beside it. Nothing in this
        // system ever set this role — the route exists and no control calls it —
        // so an operator who picked a free socket for an output got a show that
        // named a socket the renderer would not take. Storing it instead would be
        // a second answer to "what is this socket for", kept in step with the
        // show by hand, and the day the two disagreed the wall would be dark with
        // the database insisting otherwise.
        //
        // Disabled outputs count. An output switched off is one an operator is
        // coming back to, and letting the socket go free in the meantime would
        // hand it to something else between two cues.
        roles.extend(self.with_state(|current| {
            let Some(show) = current.show() else {
                return Vec::new();
            };
            show.outputs
                .iter()
                .filter_map(|output| {
                    let gpu = output.identity.gpu_pci_address.as_ref()?;
                    let connector = output.identity.connector.as_ref()?;
                    Some((
                        ovs_db::connector_key(gpu, connector),
                        ovs_types::config::ConnectorRole::MediaOutput,
                    ))
                })
                .collect::<Vec<_>>()
        }));

        // The System screen's connector comes from the machine's configuration
        // file and wins over both. It is decided once, when the rack is built,
        // and the one thing that has to be right before anything else can be: a
        // renderer that took that socket would put a show on the screen an
        // operator is looking at, and a row in a database is not where somebody
        // fixes that from (`crate::config_file`).
        roles.extend(crate::config_file::ServerConfig::load().roles());
        Ok(roles)
    }

    /// Sets what one connector is for, and tells the renderer.
    ///
    /// # Errors
    ///
    /// Returns the database error when it cannot be written down.
    pub fn set_connector_role(
        &self,
        gpu_pci_address: &str,
        connector: &str,
        role: ovs_types::config::ConnectorRole,
    ) -> Result<(), ovs_db::DbError> {
        tracing::info!(
            gpu = gpu_pci_address,
            connector,
            ?role,
            "a connector's role was set"
        );
        self.with_database(|database| {
            database.set_connector_role(gpu_pci_address, connector, role)
        })?;
        // The renderer picks the change up on its next pass through the link,
        // which is where the roles are sent from.
        self.inner
            .connector_roles_changed
            .store(true, Ordering::Relaxed);
        Ok(())
    }

    /// The mode the System screen's connector should be driven at, if one was
    /// chosen.
    ///
    /// From the machine's configuration file, beside the connector itself —
    /// which is where the Service Display has always been decided, and for the
    /// same reason: it is a fact about the rack, not about a show.
    #[must_use]
    pub fn console_mode(&self) -> Option<ovs_types::config::DisplayMode> {
        crate::config_file::ServerConfig::load()
            .service_display
            .and_then(|display| display.mode)
    }

    /// Says the connector roles have changed, so the renderer is told at once.
    ///
    /// For the machine configuration, which is a file rather than a row and
    /// therefore does not go through [`Self::set_connector_role`]. The System
    /// screen's connector lives there, and a change to it that waited for a
    /// restart would be a control that answers at the next trigger.
    pub fn connector_roles_changed(&self) {
        self.inner
            .connector_roles_changed
            .store(true, Ordering::Relaxed);
    }

    /// Returns true once when the connector roles have changed.
    #[must_use]
    pub fn take_connector_roles_changed(&self) -> bool {
        self.inner
            .connector_roles_changed
            .swap(false, Ordering::Relaxed)
    }

    /// The account a request from this machine acts as.
    #[must_use]
    pub fn local_account(&self) -> ovs_types::id::AccountId {
        self.inner.local_account
    }

    /// Mints a permission to watch one surface.
    ///
    /// # Errors
    ///
    /// Returns the reason one could not be minted, which is the random source
    /// failing or too many outstanding at once.
    pub fn mint_stream_ticket(
        &self,
        surface: ovs_types::telemetry::StreamSurface,
    ) -> Result<(String, ovs_types::time::Timestamp), crate::auth::AuthError> {
        self.inner
            .stream_tickets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mint(surface, ovs_types::time::Timestamp::now())
    }

    /// Spends a permission, and says whether it was good for that surface.
    pub fn spend_stream_ticket(
        &self,
        ticket: &str,
        surface: ovs_types::telemetry::StreamSurface,
    ) -> bool {
        self.inner
            .stream_tickets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .spend(ticket, surface, ovs_types::time::Timestamp::now())
    }

    /// Starts watching one surface, or joins whoever already is.
    ///
    /// The renderer is told to encode it when the first watcher arrives and to
    /// stop when the last one leaves — so a server nobody has a client open on
    /// encodes nothing at all.
    ///
    /// Returns the receiver, the last keyframe if there is one so the watcher
    /// sees a picture now rather than at the next one, and whatever is already
    /// known about the stream itself.
    pub fn watch_stream(&self, surface: ovs_types::telemetry::StreamSurface) -> Watching {
        /// How many chunks a watcher may fall behind before it starts missing
        /// them. Half a second at sixty frames a second.
        const BEHIND: usize = 30;

        let mut streams = self
            .inner
            .streams
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let stream = streams.entry(surface).or_insert_with(|| {
            let (chunks, _) = broadcast::channel(BEHIND);
            Stream {
                chunks,
                keyframe: None,
                watchers: 0,
                failure: None,
                encoding: None,
            }
        });

        stream.watchers += 1;
        if stream.watchers == 1 {
            self.inner.streams_changed.store(true, Ordering::Relaxed);
            tracing::info!(%surface, "somebody is watching this surface");
        }

        Watching {
            events: stream.chunks.subscribe(),
            keyframe: stream.keyframe.clone(),
            failure: stream.failure.clone(),
            encoding: stream.encoding.clone(),
        }
    }

    /// Stops watching one surface.
    pub fn unwatch_stream(&self, surface: ovs_types::telemetry::StreamSurface) {
        let mut streams = self
            .inner
            .streams
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let Some(stream) = streams.get_mut(&surface) else {
            return;
        };

        stream.watchers = stream.watchers.saturating_sub(1);
        if stream.watchers == 0 {
            // The surface is forgotten rather than kept: its keyframe is of no
            // use to the next watcher, who will get a fresh one, and a failure
            // is worth trying again for.
            streams.remove(&surface);
            self.inner.streams_changed.store(true, Ordering::Relaxed);
            tracing::info!(%surface, "nobody is watching this surface any more");
        }
    }

    /// Records one encoded picture and hands it to everybody watching.
    pub fn publish_stream_chunk(
        &self,
        surface: ovs_types::telemetry::StreamSurface,
        chunk: StreamChunk,
    ) {
        let mut streams = self
            .inner
            .streams
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let Some(stream) = streams.get_mut(&surface) else {
            // A chunk for a surface nobody is watching any more: the renderer
            // has not been told to stop yet, and the picture is dropped rather
            // than kept for somebody who is not there.
            return;
        };

        let chunk = Arc::new(chunk);
        if chunk.keyframe {
            stream.keyframe = Some(Arc::clone(&chunk));
        }
        stream.failure = None;
        // The error is "nobody is listening this instant", which is ordinary
        // between a watcher leaving and the renderer being told.
        let _ = stream.chunks.send(StreamEvent::Chunk(chunk));
    }

    /// Records what a surface turned out to be encoded with, and says so.
    ///
    /// The answer to the last clause of owner decision Q13. A preview whose
    /// rate has been reduced has to say so, or an operator reads a quarter-speed
    /// picture as a show that is stuttering.
    pub fn publish_stream_encoding(
        &self,
        surface: ovs_types::telemetry::StreamSurface,
        encoding: ovs_types::telemetry::StreamEncoding,
    ) {
        let mut streams = self
            .inner
            .streams
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let Some(stream) = streams.get_mut(&surface) else {
            return;
        };

        let encoding = Arc::new(encoding);
        stream.encoding = Some(Arc::clone(&encoding));
        let _ = stream.chunks.send(StreamEvent::Encoding(encoding));
    }

    /// Records that a surface cannot be streamed, and why.
    pub fn publish_stream_failure(
        &self,
        surface: ovs_types::telemetry::StreamSurface,
        reason: String,
    ) {
        let mut streams = self
            .inner
            .streams
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(stream) = streams.get_mut(&surface) {
            tracing::warn!(%surface, "this surface cannot be streamed: {reason}");
            stream.failure = Some(reason);
        }
    }

    /// The surfaces somebody is watching.
    #[must_use]
    pub fn watched_streams(&self) -> Vec<ovs_types::telemetry::StreamSurface> {
        self.inner
            .streams
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }

    /// Takes the "the watched set has changed" flag, if it is set.
    pub fn take_streams_changed(&self) -> bool {
        self.inner.streams_changed.swap(false, Ordering::Relaxed)
    }

    /// The piece the pieces pattern draws as the chosen one, if any.
    #[must_use]
    pub fn highlighted_piece(&self) -> Option<ovs_types::id::PieceId> {
        *self
            .inner
            .highlighted_piece
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Returns which surfaces are showing the test pattern.
    #[must_use]
    pub fn test_pattern(&self) -> ovs_types::config::TestPatternMode {
        *self
            .inner
            .test_pattern
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Puts the test pattern on the composition or on the outputs, or takes it
    /// off again.
    ///
    /// Deliberately not persisted, unlike the preview setting. Previews are a
    /// cost an operator chooses to stop paying; a test pattern is a thing on a
    /// wall, and the next person to start this server is entitled to assume the
    /// wall shows the show.
    pub fn set_test_pattern(
        &self,
        mode: ovs_types::config::TestPatternMode,
        highlighted: Option<ovs_types::id::PieceId>,
    ) {
        if mode != self.test_pattern() {
            tracing::warn!(?mode, "the test pattern was switched");
        }
        *self
            .inner
            .highlighted_piece
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = highlighted;
        *self
            .inner
            .test_pattern
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = mode;
    }

    /// Returns whether the renderer should produce previews.
    #[must_use]
    pub fn previews_enabled(&self) -> bool {
        self.inner.previews_enabled.load(Ordering::Relaxed)
    }

    /// Sets whether the renderer should produce previews, and remembers it.
    ///
    /// # Errors
    ///
    /// Returns the database error when it cannot be written down. The flag is
    /// still applied: an operator who switched previews off to get through a
    /// show should not have to wait for a disk to agree.
    pub fn set_previews_enabled(&self, enabled: bool) -> Result<(), ovs_db::DbError> {
        self.inner
            .previews_enabled
            .store(enabled, Ordering::Relaxed);

        if !enabled {
            // What is already held goes too. Keeping it would leave a console
            // showing a picture that has stopped moving, which looks exactly
            // like a show that has frozen — and an operator who has just
            // switched previews off is the one least able to afford that
            // confusion.
            self.inner
                .previews
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
            *self
                .inner
                .composition_preview
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }

        self.with_database(|database| database.set_previews_enabled(enabled))
    }

    /// Reads the stored preview setting into memory.
    ///
    /// Called once at startup, before anything reads the flag.
    pub fn load_previews_enabled(&self) {
        let stored = self.with_database(|database| database.previews_enabled());
        match stored {
            Ok(enabled) => self
                .inner
                .previews_enabled
                .store(enabled, Ordering::Relaxed),
            Err(error) => tracing::warn!("could not read the preview setting: {error}"),
        }
    }

    /// Asks for the renderer to be restarted.
    ///
    /// The core does not start processes: it tells the renderer to stop, and
    /// whatever supervises it starts it again. On the
    /// reference platform that is systemd's `Restart=always`, which is also
    /// what restarts a renderer that has crashed — so this is the same path an
    /// operator would get by other means, not a second one.
    pub fn request_renderer_restart(&self) {
        self.inner.restart_renderer.store(true, Ordering::Relaxed);
    }

    /// Asks the renderer to put some of the operator-visible counters back to
    /// zero.
    ///
    /// Not persisted and not acknowledged: the next telemetry says what the
    /// counters are, which is the only answer worth having.
    ///
    /// Two different groups tapped before either has been sent become "all of
    /// them" — which is what somebody tapping two counters in half a second
    /// meant anyway, and cheaper than a queue.
    pub fn request_counter_reset(&self, group: ovs_types::telemetry::CounterGroup) {
        let mut pending = self
            .inner
            .reset_counters
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *pending = Some(match *pending {
            Some(waiting) if waiting != group => ovs_types::telemetry::CounterGroup::All,
            _ => group,
        });
    }

    /// Takes the reset request, if there is one.
    pub fn take_counter_reset(&self) -> Option<ovs_types::telemetry::CounterGroup> {
        self.inner
            .reset_counters
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Takes the restart request, if there is one.
    ///
    /// Clears it, so a link that reconnects after the renderer went away does
    /// not stop it again for a request that has already been honoured.
    pub fn take_renderer_restart(&self) -> bool {
        self.inner.restart_renderer.swap(false, Ordering::Relaxed)
    }

    /// Records a picture of the whole composition.
    pub fn set_composition_preview(&self, png: Option<Vec<u8>>) {
        if png.is_some() && !self.previews_enabled() {
            return;
        }

        let mut held = self
            .inner
            .composition_preview
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = png;
    }

    /// Returns the last picture of the whole composition.
    #[must_use]
    pub fn composition_preview(&self) -> Option<Vec<u8>> {
        self.inner
            .composition_preview
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Records the outputs the renderer says nothing is driving.
    pub fn set_missing_outputs(&self, missing: Vec<ovs_types::telemetry::MissingOutput>) {
        let mut held = self
            .inner
            .missing_outputs
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = missing;
    }

    /// Returns the outputs the renderer says nothing is driving.
    #[must_use]
    pub fn missing_outputs(&self) -> Vec<ovs_types::telemetry::MissingOutput> {
        self.inner
            .missing_outputs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Records what the renderer's players reported.
    pub fn set_playback_telemetry(
        &self,
        playback: Option<ovs_types::telemetry::PlaybackTelemetry>,
    ) {
        let mut held = self
            .inner
            .playback_telemetry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = playback;
    }

    /// Returns what the renderer's players last reported.
    ///
    /// Dereferenced out of the lock rather than cloned, which is what keeps
    /// [`ovs_types::telemetry::PlaybackTelemetry`] a `Copy` struct and the
    /// per-layer playheads a separate field beside it.
    #[must_use]
    pub fn playback_telemetry(&self) -> Option<ovs_types::telemetry::PlaybackTelemetry> {
        *self
            .inner
            .playback_telemetry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records where the renderer says its last frame's time went.
    pub fn set_frame_timings(&self, frame: Option<ovs_types::telemetry::FrameTimings>) {
        let mut held = self
            .inner
            .frame_timings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = frame;
    }

    /// Returns where the renderer's last frame's time went.
    #[must_use]
    pub fn frame_timings(&self) -> Option<ovs_types::telemetry::FrameTimings> {
        *self
            .inner
            .frame_timings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records where the renderer says each playing layer's playhead is.
    pub fn set_playheads(&self, playheads: Vec<ovs_types::telemetry::LayerPlayhead>) {
        let mut held = self
            .inner
            .playheads
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = playheads;
    }

    /// Returns where each playing layer's playhead last was.
    ///
    /// Empty when no layer is playing a clip, and when no renderer has said
    /// anything yet. Both are a Live Mode screen with no bars to draw, which is
    /// why they are not distinguished here — unlike the playback counts beside
    /// them, where "nothing playing" and "nobody has said" are different answers
    /// to the same question and the Service Console shows them differently.
    #[must_use]
    pub fn playheads(&self) -> Vec<ovs_types::telemetry::LayerPlayhead> {
        self.inner
            .playheads
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Where the master clock is, right now.
    ///
    /// Read at the moment it is asked for rather than stored, because a beat is
    /// a function of the clock and the time: keeping a copy would mean keeping
    /// it fresh, which is a metronome nobody asked for.
    #[must_use]
    pub fn clock_telemetry(&self) -> Option<ovs_types::telemetry::ClockTelemetry> {
        self.with_state(|current| {
            current.show()?;
            let clock = current.clock();
            let position = clock.position_at(std::time::Instant::now());
            Some(ovs_types::telemetry::ClockTelemetry {
                bar: position.bar,
                beat_in_bar: position.beat_in_bar,
                phase: position.phase,
                beats_per_bar: clock.beats_per_bar(),
                beats_per_second: clock.beats_per_second(),
            })
        })
    }

    /// Records what the DMX link is hearing.
    pub fn set_dmx_telemetry(&self, dmx: Option<ovs_types::telemetry::DmxTelemetry>) {
        let mut held = self
            .inner
            .dmx_telemetry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = dmx;
    }

    /// Returns what the DMX link last heard.
    #[must_use]
    pub fn dmx_telemetry(&self) -> Option<ovs_types::telemetry::DmxTelemetry> {
        self.inner
            .dmx_telemetry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Takes a sample of the host.
    ///
    /// Called from a background task, never from a request: CPU utilisation is
    /// a difference between two readings, and a platform call has no business
    /// on a path that is supposed to be a database read.
    pub fn sample_host(&self) {
        let root = self.inner.media_store.as_ref().map(MediaStore::root);
        self.inner.host.refresh(root);
    }

    /// Returns the bytes still free where media is stored.
    ///
    /// `None` on a server with nowhere to store media, and on a machine whose
    /// filesystems cannot be read — in both cases the answer is that the space
    /// is unknown, which is not the same as there being none.
    pub fn media_space_available(&self) -> Option<u64> {
        let root = self.inner.media_store.as_ref().map(MediaStore::root);
        self.inner.host.available_bytes(root)
    }

    /// Returns the last sample of the host.
    #[must_use]
    pub fn host(&self) -> HostSample {
        self.inner.host.latest()
    }

    /// Records the latest picture of one output.
    pub fn set_preview(&self, preview: Preview) {
        if !self.previews_enabled() {
            // One that was already in flight when an operator switched them
            // off. Keeping it would put a picture back on a page that has just
            // been told there are none.
            return;
        }

        let mut previews = self
            .inner
            .previews
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        previews.insert(preview.output_id, preview);
    }

    /// Returns the latest picture of one output, if there is one.
    #[must_use]
    pub fn preview(&self, output_id: ovs_types::id::OutputId) -> Option<Preview> {
        let previews = self
            .inner
            .previews
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        previews.get(&output_id).cloned()
    }

    /// Returns every output there is a picture of, in a stable order.
    #[must_use]
    pub fn previews(&self) -> Vec<Preview> {
        let previews = self
            .inner
            .previews
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        previews.values().cloned().collect()
    }

    /// Writes what a restart should restore, if anything has changed.
    ///
    /// Returns true when something was written. The snapshot is narrow by
    /// design — the BPM value and the selected slot per layer, nothing a console
    /// drives — so it changes rarely, and writing it only when it has
    /// changed keeps a show that is running quietly from touching the disk at
    /// all.
    ///
    /// Called on a timer and at every point where the open show is about to
    /// stop being the open show. Writing it on every state change instead
    /// would mean a database write per DMX frame, forty-four times a second.
    pub fn persist_live_snapshot(&self) -> bool {
        let Some((show_id, snapshot)) = self.with_state(|current| {
            current
                .show_id()
                .map(|show_id| (show_id, current.live_snapshot()))
        }) else {
            return false;
        };

        {
            let last = self
                .inner
                .persisted_snapshot
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if last.as_ref() == Some(&(show_id, snapshot.clone())) {
                return false;
            }
        }

        let written =
            self.with_database(|database| database.save_live_snapshot(show_id, &snapshot));
        match written {
            Ok(()) => {
                let mut last = self
                    .inner
                    .persisted_snapshot
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *last = Some((show_id, snapshot));
                true
            }
            Err(error) => {
                // Not fatal. A show whose live state cannot be persisted still
                // runs; what is lost is where it would have come back to.
                tracing::warn!(%show_id, "could not save the live snapshot: {error}");
                false
            }
        }
    }

    /// Returns where media files live, if anywhere.
    #[must_use]
    pub fn media_store(&self) -> Option<&MediaStore> {
        self.inner.media_store.as_ref()
    }

    /// Runs an operation against the database.
    ///
    /// A closure rather than a guard, so a lock cannot be held across an await
    /// point by accident.
    pub fn with_database<T>(&self, operation: impl FnOnce(&mut Database) -> T) -> T {
        let mut database = self
            .inner
            .database
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut database)
    }

    /// Reads the authoritative state.
    pub fn with_state<T>(&self, operation: impl FnOnce(&AuthoritativeState) -> T) -> T {
        let state = self
            .inner
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&state)
    }

    /// Mutates the authoritative state and broadcasts whatever changed.
    ///
    /// The broadcast happens here rather than at each call site, so a change can
    /// be made without being published only by not going through this function.
    pub fn mutate_state<T>(
        &self,
        operation: impl FnOnce(&mut AuthoritativeState) -> (T, Option<SequencedChange>),
    ) -> T {
        let (result, change) = {
            let mut state = self
                .inner
                .state
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            operation(&mut state)
        };

        if let Some(change) = change {
            // A send with no subscribers is not an error: the show runs whether
            // anybody is watching.
            let _ = self.inner.updates.send(ServerMessage::Update {
                sequence: change.sequence,
                change: change.change,
            });
        }

        result
    }

    /// Mutates the state and broadcasts every change it made.
    ///
    /// For the handful of operations that change more than one thing at once:
    /// a grid boundary can land on six layers, and publishing them one
    /// mutation at a time would take the write lock six times for what is one
    /// moment in the show.
    pub fn mutate_state_each<T>(
        &self,
        operation: impl FnOnce(&mut AuthoritativeState) -> (T, Vec<SequencedChange>),
    ) -> T {
        let (result, changes) = {
            let mut state = self
                .inner
                .state
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            operation(&mut state)
        };

        for change in changes {
            let _ = self.inner.updates.send(ServerMessage::Update {
                sequence: change.sequence,
                change: change.change,
            });
        }

        result
    }

    /// Returns a copy of the authoritative state, for a snapshot.
    #[must_use]
    pub fn authoritative(&self) -> AuthoritativeState {
        self.with_state(Clone::clone)
    }

    /// Runs an operation against the session store.
    pub fn with_sessions<T>(&self, operation: impl FnOnce(&mut Sessions) -> T) -> T {
        let mut sessions = self
            .inner
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut sessions)
    }

    /// Subscribes to the update stream.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ServerMessage> {
        self.inner.updates.subscribe()
    }

    /// Publishes a message outside the sequenced stream, such as telemetry.
    pub fn publish(&self, message: ServerMessage) {
        let _ = self.inner.updates.send(message);
    }

    /// Records what the renderer reported about its outputs.
    ///
    /// Pictures of outputs that are no longer in the list go with it. A renderer
    /// that restarts builds its outputs again with new identifiers, so without
    /// this the last frame of every output the machine has ever had stays in
    /// memory and in `/preview` — a client listing them would offer an operator
    /// four cameras where the machine has one, three of them frozen.
    pub fn set_output_telemetry(&self, outputs: Vec<OutputTelemetry>) {
        let live: std::collections::BTreeSet<_> =
            outputs.iter().map(|output| output.output_id).collect();

        let mut telemetry = self
            .inner
            .output_telemetry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *telemetry = outputs;
        drop(telemetry);

        let mut previews = self
            .inner
            .previews
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        previews.retain(|output_id, _| live.contains(output_id));
    }

    /// Returns what the renderer last reported about its outputs.
    #[must_use]
    pub fn output_telemetry(&self) -> Vec<OutputTelemetry> {
        self.inner
            .output_telemetry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Returns this build's version string.
    #[must_use]
    pub fn server_version(&self) -> &str {
        &self.inner.server_version
    }

    /// Returns how long the server has been running, in seconds.
    #[must_use]
    pub fn uptime_s(&self) -> u64 {
        self.inner.started_at.elapsed().as_secs()
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::telemetry::RendererStatus;

    use super::*;

    fn state() -> AppState {
        AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test")
    }

    /// One output, as the renderer reports it.
    fn output(id: ovs_types::id::OutputId) -> OutputTelemetry {
        OutputTelemetry {
            output_id: id,
            name: "Wall".to_owned(),
            active: true,
            size: ovs_types::geometry::SizePx::new(1920, 1080),
            refresh: ovs_types::geometry::FrameRate::FPS_60,
            actual_fps: 60.0,
            frame_render_time_us: 1000,
            dropped_frames: ovs_types::telemetry::DroppedFrames::default(),
            display: Some("Sony LMD-2110W".to_owned()),
            // A stand-in output: an eight-bit pipeline, and nothing
            // claimed about a cable that does not exist.
            composition_depth: ovs_types::depth::CompositionDepth::Eight,
            output_depth: ovs_types::depth::OutputDepth::Unknown,
            connector: Some("HDMI-0".to_owned()),
        }
    }

    /// One encoding, as the renderer would report it.
    fn software() -> ovs_types::telemetry::StreamEncoding {
        ovs_types::telemetry::StreamEncoding {
            encoder: "libx264".to_owned(),
            hardware: false,
            fps: 15.0,
        }
    }

    #[tokio::test]
    async fn a_watcher_is_told_what_the_surface_is_being_encoded_with() {
        // The last clause of owner decision Q13. A software preview runs at a
        // quarter of the show's rate so that watching cannot cost the show, and
        // a watcher who is not told reads it as the show stuttering.
        let app = state();
        let surface = ovs_types::telemetry::StreamSurface::Composition;

        let mut watching = app.watch_stream(surface);
        assert!(
            watching.encoding.is_none(),
            "nothing is known until the encoder is up"
        );

        app.publish_stream_encoding(surface, software());

        let event = watching.events.try_recv().expect("the watcher is told");
        let StreamEvent::Encoding(encoding) = event else {
            panic!("an encoding, not a picture");
        };
        assert_eq!(encoding.encoder, "libx264");
        assert!((encoding.fps - 15.0).abs() < f64::EPSILON);
        assert!(!encoding.hardware);

        app.unwatch_stream(surface);
    }

    #[tokio::test]
    async fn somebody_who_arrives_later_is_owed_the_same_answer() {
        // The renderer says it once, when the encoder comes up. A second
        // watcher joining a surface that is already being encoded would
        // otherwise never find out, and a rig has more than one person in it.
        let app = state();
        let surface = ovs_types::telemetry::StreamSurface::Composition;

        let first = app.watch_stream(surface);
        app.publish_stream_encoding(surface, software());

        let second = app.watch_stream(surface);
        assert_eq!(
            second.encoding.as_deref(),
            Some(&software()),
            "a later watcher is told on arrival"
        );

        drop((first, second));
        app.unwatch_stream(surface);
        app.unwatch_stream(surface);
    }

    #[tokio::test]
    async fn what_a_surface_was_encoded_with_goes_when_the_last_watcher_does() {
        // Nobody watching means nothing is encoded, so the next watcher gets a
        // new encoder and may well get a different one. Keeping the old answer
        // would show them the last show's.
        let app = state();
        let surface = ovs_types::telemetry::StreamSurface::Composition;

        let watching = app.watch_stream(surface);
        app.publish_stream_encoding(surface, software());
        drop(watching);
        app.unwatch_stream(surface);

        let again = app.watch_stream(surface);
        assert!(again.encoding.is_none());
    }

    #[tokio::test]
    async fn a_picture_of_an_output_that_is_gone_goes_with_it() {
        // A renderer that restarts builds its outputs again under new
        // identifiers. Without this, the last frame of every output the machine
        // has ever driven stays in `/preview` for ever, and a client offers an
        // operator a row of frozen cameras.
        let app = state();
        let old = ovs_types::id::OutputId::new();
        let new = ovs_types::id::OutputId::new();

        for id in [old, new] {
            app.set_preview(Preview {
                output_id: id,
                name: "Wall".to_owned(),
                png: vec![0],
                received_at: ovs_types::time::Timestamp::now(),
            });
        }

        app.set_output_telemetry(vec![output(new)]);

        assert!(app.preview(new).is_some(), "the one being driven stays");
        assert!(app.preview(old).is_none(), "the one that is gone does not");
    }

    #[tokio::test]
    async fn a_state_change_reaches_every_subscriber() {
        let app = state();
        let mut first = app.subscribe();
        let mut second = app.subscribe();

        app.mutate_state(|state| {
            let change = state.set_renderer(RendererStatus::Running);
            ((), change)
        });

        for receiver in [&mut first, &mut second] {
            let message = receiver.try_recv().expect("delivered");
            assert!(matches!(message, ServerMessage::Update { .. }));
        }
    }

    #[tokio::test]
    async fn a_change_that_changes_nothing_publishes_nothing() {
        let app = state();
        let mut updates = app.subscribe();

        app.mutate_state(|state| {
            let change = state.set_renderer(RendererStatus::Stopped);
            ((), change)
        });

        assert!(
            updates.try_recv().is_err(),
            "an empty change must not reach the wire"
        );
    }

    #[tokio::test]
    async fn the_show_runs_with_nobody_watching() {
        // A broadcast with no subscribers must not be an error, or the server
        // would depend on a client being connected.
        let app = state();
        app.mutate_state(|state| {
            let change = state.set_renderer(RendererStatus::Running);
            ((), change)
        });
        assert!(app.with_state(|state| state.renderer().is_rendering()));
    }

    #[tokio::test]
    async fn a_slow_client_is_dropped_rather_than_buffered_without_limit() {
        let app = state();
        let mut slow = app.subscribe();

        // Overflow the buffer without reading.
        for index in 0..(UPDATE_BUFFER + 10) {
            app.publish(ServerMessage::Pong {
                nonce: index as u64,
            });
        }

        let error = slow.try_recv().expect_err("lagged");
        assert!(
            matches!(error, broadcast::error::TryRecvError::Lagged(_)),
            "a client that cannot keep up must lag, not grow the server"
        );
    }

    #[test]
    fn uptime_starts_at_zero_and_the_version_is_reported() {
        let app = state();
        assert_eq!(app.uptime_s(), 0);
        assert_eq!(app.server_version(), "0.0.1-test");
    }
}
