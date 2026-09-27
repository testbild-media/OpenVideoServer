//! The control thread.
//!
//! Runs beside the render loop, never inside it. It blocks on the network; the
//! render thread must not, so the two only meet through
//! [`SharedShowState`], which is wait-free to read.
//!
//! The renderer **listens** and the core connects to it. When the core stops,
//! this thread goes back to waiting for a connection and the render loop does
//! not notice.

use std::io::{BufReader, BufWriter};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ovs_ipc::{
    read_frame, write_frame, ControlMessage, IpcError, RendererMessage, IPC_PROTOCOL_VERSION,
};

use crate::show_state::{SharedShowState, ShowState};

/// How often the renderer reports what it measured.
///
/// Telemetry is monitoring, so it is deliberately slow and deliberately
/// unreliable: a report that does not arrive is a monitoring problem and never
/// a rendering one.
const TELEMETRY_INTERVAL: Duration = Duration::from_secs(1);

/// How often a picture of each output is sent.
///
/// Faster than telemetry because a preview is watched rather than monitored: a
/// number that is a second old is fine and a picture that is a second old looks
/// broken. Slow enough that the encoding stays a background cost — a thumbnail
/// PNG is a few milliseconds, on a thread that is doing nothing else.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(200);

/// The write half of a control connection.
///
/// Behind a mutex because two threads write to it: the reader thread when it
/// has to report a status, and the telemetry thread once a second. Without it,
/// two length-prefixed frames would interleave and corrupt each other.
type SharedWriter = Arc<Mutex<BufWriter<TcpStream>>>;

/// Writes one frame, ignoring a poisoned lock.
///
/// A poisoned mutex here means a writer thread panicked mid-frame. The stream is
/// then suspect, so the write is attempted and its failure ends the connection,
/// rather than the renderer panicking in sympathy.
fn send(writer: &SharedWriter, message: &RendererMessage) -> Result<(), IpcError> {
    let mut guard = writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    write_frame(&mut *guard, message)
}

/// Encodes one preview as a base64 PNG.
///
/// Returns `None` when the picture cannot be encoded, which is not a fault: a
/// preview nobody can encode is a preview an operator does not see, and the wall
/// is unaffected either way.
fn encode_preview(preview: &crate::render_loop::Preview) -> Option<String> {
    encode_picture(preview.size, &preview.pixels)
}

/// Encodes raw RGBA as a base64 PNG.
fn encode_picture(size: ovs_types::geometry::SizePx, pixels: &[u8]) -> Option<String> {
    use base64::Engine as _;

    let image = image::RgbaImage::from_raw(size.width, size.height, pixels.to_vec())?;

    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;

    Some(base64::engine::general_purpose::STANDARD.encode(&png))
}

/// Why the control thread stopped serving one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disconnected {
    /// The core closed the channel, or the connection broke.
    ///
    /// Ordinary. The renderer keeps rendering and waits for the core to come
    /// back.
    CoreWentAway,
    /// The core asked the renderer to stop.
    ///
    /// The only thing that ends a show on purpose.
    ShutdownRequested,
}

/// Accepts core connections and applies what they send, forever.
///
/// Returns only when a [`ControlMessage::Shutdown`] arrives or `running` is
/// cleared.
pub fn serve(
    listener: &TcpListener,
    state: &Arc<SharedShowState>,
    running: &AtomicBool,
) -> Result<(), IpcError> {
    for incoming in listener.incoming() {
        if !running.load(Ordering::Relaxed) {
            return Ok(());
        }

        let stream = match incoming {
            Ok(stream) => stream,
            Err(error) => {
                // A failed accept is a control problem, never a rendering one.
                tracing::warn!("could not accept a control connection: {error}");
                continue;
            }
        };

        tracing::info!("core connected");
        state.set_connected(true);

        match serve_connection(stream, state) {
            Ok(Disconnected::ShutdownRequested) => {
                tracing::info!("the core asked the renderer to stop");
                state.set_connected(false);
                return Ok(());
            }
            Ok(Disconnected::CoreWentAway) => {
                tracing::warn!("the core went away; rendering the last known state");
            }
            Err(error) => {
                tracing::warn!("control channel failed: {error}; rendering the last known state");
            }
        }

        state.set_connected(false);
    }

    Ok(())
}

/// Serves one core connection until it ends.
fn serve_connection(
    stream: TcpStream,
    state: &Arc<SharedShowState>,
) -> Result<Disconnected, IpcError> {
    // Nagle would hold a small state update back waiting for company. A slot
    // change must reach the renderer now, not in 40 milliseconds.
    if let Err(error) = stream.set_nodelay(true) {
        tracing::warn!("could not disable Nagle on the control channel: {error}");
    }

    let closer = stream.try_clone()?;
    let writer: SharedWriter = Arc::new(Mutex::new(BufWriter::new(stream.try_clone()?)));
    let mut reader = BufReader::new(stream);

    send(
        &writer,
        &RendererMessage::Hello {
            protocol_version: IPC_PROTOCOL_VERSION,
            renderer_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    )?;

    // Straight after the hello, so the core knows what is happening before it
    // has to answer a status request about it. The renderer is the only thing
    // that actually knows this.
    send(&writer, &RendererMessage::Status(state.status()))?;

    // Telemetry goes out on its own thread so that reporting never sits in the
    // path of an incoming state update.
    let reporting = Arc::new(AtomicBool::new(true));
    let telemetry_thread = spawn_telemetry(state, &writer, &reporting)?;

    let outcome = read_control(&mut reader, state, &writer);

    reporting.store(false, Ordering::Relaxed);
    let _ = closer.shutdown(Shutdown::Both);
    let _ = telemetry_thread.join();

    outcome
}

/// Reads control messages until the connection ends.
fn read_control(
    reader: &mut BufReader<TcpStream>,
    state: &Arc<SharedShowState>,
    writer: &SharedWriter,
) -> Result<Disconnected, IpcError> {
    loop {
        let message: ControlMessage = match read_frame(reader) {
            Ok(message) => message,
            Err(IpcError::Closed) => return Ok(Disconnected::CoreWentAway),
            Err(error) => return Err(error),
        };

        match message {
            ControlMessage::Snapshot(snapshot) => {
                state.replace(ShowState::from_snapshot(*snapshot));
            }
            ControlMessage::Update { sequence, change } => {
                if !state.apply(sequence, &change) {
                    // The renderer missed something. Rendering on would put
                    // state on stage that the core never asked for, so say so
                    // and wait for a fresh snapshot.
                    tracing::warn!(
                        received = sequence.0,
                        "control update did not follow on; awaiting a snapshot"
                    );
                    send(writer, &RendererMessage::Status(state.status()))?;
                }
            }
            ControlMessage::Media { entries } => {
                tracing::info!(items = entries.len(), "media catalogue received");
                state.set_media(entries);
            }
            ControlMessage::SetPreviews { enabled } => {
                if enabled != state.previews_enabled() {
                    tracing::info!(enabled, "previews switched");
                }
                state.set_previews_enabled(enabled);
            }
            ControlMessage::SetConnectorRoles {
                roles,
                console_mode,
            } => {
                state.set_connector_roles(roles, console_mode);
            }
            ControlMessage::SetStreams { surfaces } => {
                // Said out loud because it is the difference between a machine
                // encoding nothing and one encoding four walls.
                tracing::info!(count = surfaces.len(), "the watched surfaces changed");
                state.set_wanted_streams(surfaces);
            }
            ControlMessage::ResetCounters { group } => {
                tracing::info!(?group, "an operator cleared a counter");
                state.request_counter_reset(group);
            }
            ControlMessage::SetTestPattern { mode, highlighted } => {
                if mode != state.test_pattern() {
                    // Said out loud because the walls have stopped showing the
                    // show, and a log that does not mention it leaves the next
                    // person to work out why on their own.
                    tracing::warn!(?mode, "the test pattern was switched");
                }
                state.set_test_pattern(mode, highlighted);
            }
            ControlMessage::Shutdown => return Ok(Disconnected::ShutdownRequested),
        }
    }
}

/// Sends whatever the render thread has encoded, and says whether the far end
/// is still there.
fn send_stream_chunks(state: &Arc<SharedShowState>, writer: &SharedWriter) -> bool {
    use base64::Engine as _;

    for (surface, reason) in state.take_stream_failures() {
        let message = RendererMessage::StreamFailed { surface, reason };
        if send(writer, &message).is_err() {
            return false;
        }
    }

    for (surface, encoding) in state.take_stream_encodings() {
        let message = RendererMessage::StreamEncoding { surface, encoding };
        if send(writer, &message).is_err() {
            return false;
        }
    }

    for chunk in state.take_stream_chunks() {
        let message = RendererMessage::StreamChunk {
            surface: chunk.surface,
            size: chunk.size,
            keyframe: chunk.keyframe,
            timestamp_us: chunk.timestamp_us,
            data_base64: base64::engine::general_purpose::STANDARD.encode(&chunk.data),
        };
        if send(writer, &message).is_err() {
            return false;
        }
    }

    true
}

/// Starts the thread that reports measurements to the core.
fn spawn_telemetry(
    state: &Arc<SharedShowState>,
    writer: &SharedWriter,
    reporting: &Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>, IpcError> {
    let state = Arc::clone(state);
    let writer = Arc::clone(writer);
    let reporting = Arc::clone(reporting);

    std::thread::Builder::new()
        .name("ovs-telemetry".to_owned())
        .spawn(move || {
            let mut since_telemetry = Duration::ZERO;
            // What was last sent, so a list that has not moved is not sent again.
            // It used to be a flag set once and never cleared, which meant a
            // monitor changed after the renderer started never reached the core
            // — the renderer knew and did not say (owner, 2026-09-25).
            let mut reported_connectors: Option<Vec<ovs_ipc::DiscoveredConnector>> = None;
            // What was last sent, so a list that has not moved is not sent
            // again. `None` until the first one goes, which is why an empty
            // list on a quiet network is still sent once.
            let mut reported_ndi: Option<Vec<String>> = None;
            while reporting.load(Ordering::Relaxed) {
                // Slept in slices so ending a connection does not wait out a
                // whole interval.
                let mut waited = Duration::ZERO;
                while waited < PREVIEW_INTERVAL && reporting.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                    waited += Duration::from_millis(5);

                    // Streams are drained in the slices rather than at the end
                    // of them: a picture held for a fifth of a second to be
                    // sent with the next batch is a fifth of a second of
                    // latency, which is most of what a preview is judged on.
                    if !send_stream_chunks(&state, &writer) {
                        return;
                    }
                }
                if !reporting.load(Ordering::Relaxed) {
                    return;
                }
                since_telemetry += waited;

                if since_telemetry >= TELEMETRY_INTERVAL {
                    since_telemetry = Duration::ZERO;

                    // What this machine is wired to, once per connection and
                    // before anything else that is not the status: a core that
                    // has just connected cannot answer a client's question about
                    // the outputs until it has this.
                    let connectors = state.connectors();
                    // Empty is not sent, and not because it is uninteresting: it
                    // is what a renderer has before it has looked, and a core
                    // told "no sockets" would offer an operator none.
                    if !connectors.is_empty() && reported_connectors.as_ref() != Some(&*connectors)
                    {
                        if send(
                            &writer,
                            &RendererMessage::Connectors {
                                connectors: (*connectors).clone(),
                            },
                        )
                        .is_err()
                        {
                            return;
                        }
                        reported_connectors = Some((*connectors).clone());
                    }

                    // And what is on the network, whenever it changes. Senders
                    // are switched on and off all through a rig check, so this
                    // is not a once-per-connection fact the way the connectors
                    // are — a socket is where it was all evening.
                    // What is worth offering, which is everything on the network
                    // except this renderer's own outputs.
                    let sources = state.ndi_offered();
                    if reported_ndi.as_ref() != Some(&sources) {
                        if send(
                            &writer,
                            &RendererMessage::NdiSources {
                                sources: sources.clone(),
                            },
                        )
                        .is_err()
                        {
                            return;
                        }
                        reported_ndi = Some(sources);
                    }

                    // Status first: it is the smaller and more important of the
                    // two, and it is what the Service Console leads with.
                    if send(&writer, &RendererMessage::Status(state.status())).is_err() {
                        // The core went away. The reader thread will notice too;
                        // this one just stops reporting.
                        return;
                    }

                    // Sent whether or not there are outputs. A renderer with
                    // none is not a renderer with nothing to say: it still holds
                    // the GPU, still composes, and what it reports about video
                    // memory, decoders and the walls it *cannot* find is exactly
                    // what somebody trying to work out why there are no outputs
                    // needs to see. Sending only when the list was non-empty
                    // left the Service Console blank in the one case where it
                    // matters most.
                    let outputs = state.telemetry();
                    if send(
                        &writer,
                        &RendererMessage::Telemetry {
                            outputs: outputs.as_ref().clone(),
                            gpu: state.gpu(),
                            playback: Some(state.playback()),
                            playheads: state.playheads().as_ref().clone(),
                            missing_outputs: state.missing_outputs().as_ref().clone(),
                            frame: state.frame_timings(),
                        },
                    )
                    .is_err()
                    {
                        return;
                    }
                }

                // Encoded here rather than on the render thread. A PNG of a
                // thumbnail is a few milliseconds of work, which is several
                // frames, and a picture for somebody to look at must never be
                // able to make a frame late.
                // The show itself, before it is cut into pieces. Sent
                // beside the per-output pictures rather than instead of them:
                // on a rig with six walls they are six answers and this is one.
                if let Some((size, pixels)) = state.composition_preview().as_ref() {
                    if let Some(png_base64) = encode_picture(*size, pixels) {
                        if send(&writer, &RendererMessage::CompositionPreview { png_base64 })
                            .is_err()
                        {
                            return;
                        }
                    }
                }

                for preview in state.previews().iter() {
                    let Some(png_base64) = encode_preview(preview) else {
                        continue;
                    };
                    if send(
                        &writer,
                        &RendererMessage::Preview {
                            output_id: preview.output_id,
                            name: preview.name.clone(),
                            png_base64,
                        },
                    )
                    .is_err()
                    {
                        return;
                    }
                }
            }
        })
        .map_err(IpcError::Io)
}

#[cfg(test)]
mod tests {
    use std::net::TcpStream;
    use std::sync::atomic::AtomicBool;
    use std::thread;

    use ovs_proto::realtime::{Sequence, StateChange, StateSnapshot};
    use ovs_types::config::Show;
    use ovs_types::live::MasterLive;
    use ovs_types::telemetry::RendererStatus;

    use super::*;

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

    /// Starts a control server on an ephemeral port.
    fn start() -> (
        String,
        Arc<SharedShowState>,
        Arc<AtomicBool>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
        let address = listener.local_addr().expect("has an address").to_string();
        let state = Arc::new(SharedShowState::new());
        let running = Arc::new(AtomicBool::new(true));

        let thread_state = Arc::clone(&state);
        let thread_running = Arc::clone(&running);
        let handle = thread::spawn(move || {
            let _ = serve(&listener, &thread_state, &thread_running);
        });

        (address, state, running, handle)
    }

    /// The core's end of the control channel, in a test.
    ///
    /// Holds both halves so that dropping it closes the socket completely. A
    /// test that drops only the writer leaves the read half open, the renderer
    /// never sees EOF, and the join at the end of the test hangs forever.
    struct CoreEnd {
        writer: BufWriter<TcpStream>,
        /// Held, not read. Its only job is to keep the read half of the socket
        /// open until the whole `CoreEnd` is dropped.
        #[allow(dead_code, reason = "held to own the read half of the socket")]
        reader: BufReader<TcpStream>,
    }

    impl CoreEnd {
        fn send(&mut self, message: &ControlMessage) {
            write_frame(&mut self.writer, message).expect("sends");
        }
    }

    /// Connects as the core and reads the renderer's hello.
    fn connect_as_core(address: &str) -> CoreEnd {
        let stream = TcpStream::connect(address).expect("connects");
        let writer = BufWriter::new(stream.try_clone().expect("clones"));
        let mut reader = BufReader::new(stream);

        let hello: RendererMessage = read_frame(&mut reader).expect("hello");
        assert!(matches!(hello, RendererMessage::Hello { .. }));
        CoreEnd { writer, reader }
    }

    /// Closes the core end and stops the control thread.
    fn stop(core: CoreEnd, address: &str, running: &AtomicBool, handle: thread::JoinHandle<()>) {
        running.store(false, Ordering::Relaxed);
        drop(core);
        // Unblock the accept loop so it can notice `running` is clear.
        let _ = TcpStream::connect(address);
        handle.join().expect("the control thread ends");
    }

    /// Waits until `check` holds, failing the test rather than hanging forever.
    fn until(check: impl Fn() -> bool) {
        for _ in 0..2_000 {
            if check() {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("condition was never met");
    }

    #[test]
    fn a_snapshot_reaches_the_render_state() {
        let (address, state, running, handle) = start();
        let mut core = connect_as_core(&address);

        core.send(&ControlMessage::Snapshot(Box::new(snapshot(10))));

        until(|| state.load().sequence == Sequence(10));
        assert!(state.load().has_show());
        assert!(state.is_connected());

        stop(core, &address, &running, handle);
    }

    #[test]
    fn the_core_can_switch_previews_off() {
        // A preview costs a readback and an encode. On a machine at its limit
        // an operator takes that back, and the renderer has to be the one that
        // stops doing the work — a core that simply ignored the pictures would
        // save nothing at all.
        let (address, state, running, handle) = start();
        let mut core = connect_as_core(&address);

        assert!(
            state.previews_enabled(),
            "on until the core says otherwise, so a renderer coming up during              setup is not blank"
        );

        core.send(&ControlMessage::SetPreviews { enabled: false });
        until(|| !state.previews_enabled());

        core.send(&ControlMessage::SetPreviews { enabled: true });
        until(|| state.previews_enabled());

        stop(core, &address, &running, handle);
    }

    #[test]
    fn a_media_catalogue_reaches_the_render_state() {
        let (address, state, running, handle) = start();
        let mut core = connect_as_core(&address);
        let clip = ovs_types::id::MediaItemId::new();

        core.send(&ControlMessage::Snapshot(Box::new(snapshot(10))));
        core.send(&ControlMessage::Media {
            entries: vec![ovs_ipc::MediaLocation {
                media_item_id: clip,
                kind: ovs_types::media::MediaKind::Image,
                path: "/srv/ovs/media/ab/clip.png".into(),
                dimensions: None,
                duration_ms: None,
                frame_rate: None,
            }],
        });

        until(|| state.media().contains_key(&clip));
        assert_eq!(state.media().len(), 1);

        stop(core, &address, &running, handle);
    }

    #[test]
    fn updates_apply_after_the_snapshot() {
        let (address, state, running, handle) = start();
        let mut core = connect_as_core(&address);

        core.send(&ControlMessage::Snapshot(Box::new(snapshot(1))));
        until(|| state.load().sequence == Sequence(1));

        let master = MasterLive {
            bpm: 174.0,
            ..MasterLive::default()
        };
        core.send(&ControlMessage::Update {
            sequence: Sequence(2),
            change: StateChange::MasterChanged { master },
        });

        until(|| state.load().master.bpm == 174.0);

        stop(core, &address, &running, handle);
    }

    #[test]
    fn the_core_going_away_leaves_the_state_intact_and_the_renderer_degraded() {
        // The invariant this whole design exists for.
        let (address, state, running, handle) = start();
        let mut core = connect_as_core(&address);

        core.send(&ControlMessage::Snapshot(Box::new(snapshot(42))));
        until(|| state.load().sequence == Sequence(42));

        // The core process dies: the whole socket closes, both halves.
        drop(core);

        until(|| !state.is_connected());
        assert_eq!(
            state.load().sequence,
            Sequence(42),
            "the state must survive"
        );
        assert!(state.load().has_show());
        assert!(
            state.status().is_rendering(),
            "losing the core degrades control, never output"
        );

        // And the core can come back.
        let mut core = connect_as_core(&address);
        core.send(&ControlMessage::Snapshot(Box::new(snapshot(100))));
        until(|| state.load().sequence == Sequence(100));
        assert!(state.is_connected());

        stop(core, &address, &running, handle);
    }

    #[test]
    fn only_an_explicit_shutdown_ends_the_control_thread() {
        // `running` is never cleared here: the thread must end because it was
        // asked to, not because the test told it to stop.
        let (address, state, _running, handle) = start();
        let mut core = connect_as_core(&address);

        core.send(&ControlMessage::Snapshot(Box::new(snapshot(1))));
        until(|| state.load().sequence == Sequence(1));

        core.send(&ControlMessage::Shutdown);
        handle.join().expect("the control thread ends on shutdown");
        assert!(!state.is_connected());
    }
}
