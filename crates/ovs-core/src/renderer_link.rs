//! The core's end of the control channel.
//!
//! Runs on its own thread with blocking I/O. The core is async everywhere else,
//! but the renderer has no async runtime and the channel is a single stream of
//! small messages, so a thread is both simpler and one less thing between a slot
//! change and the stage.
//!
//! The rule it exists to keep is the same one the client side keeps: **a
//! snapshot precedes every update the peer applies.** The renderer refuses an
//! update that does not follow on, so a link that sent one after a reconnect or
//! a lag would leave the renderer stuck refusing everything until something else
//! happened to resynchronise it.
//!
//! The renderer being absent is not an error. The core runs, accepts edits and
//! serves clients whether or not anything is rendering; it retries quietly.

use std::io::{BufReader, BufWriter};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ovs_ipc::{
    read_frame, write_frame, ControlMessage, IpcError, MediaLocation, RendererMessage,
    IPC_PROTOCOL_VERSION,
};
use ovs_proto::realtime::{ServerMessage, StateChange};
use ovs_types::config::Show;
use ovs_types::telemetry::RendererStatus;
use tokio::sync::broadcast::error::RecvError;

use crate::server::AppState;

/// How long to wait before trying the renderer again.
///
/// Short enough that a renderer restart is invisible in practice, long enough
/// that a renderer which is not running does not fill the log.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// How long the link waits for an update before checking whether it should stop.
///
/// A plain blocking receive would only notice a shutdown after the next update
/// arrived, so a quiet show would hold the process open indefinitely. This is a
/// ceiling on how long that check is delayed, not a poll interval: an update
/// that arrives sooner is forwarded immediately.
const RECEIVE_TIMEOUT: Duration = Duration::from_millis(200);

/// Why one connection to the renderer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEnded {
    /// The renderer closed the channel or the connection broke.
    RendererWentAway,
    /// The core is shutting down.
    Stopping,
}

/// Connects to the renderer and keeps it in step, retrying forever.
///
/// Returns when `running` is cleared.
pub fn run(state: &AppState, address: &str, running: &AtomicBool) {
    let mut announced_failure = false;

    while running.load(Ordering::Relaxed) {
        match TcpStream::connect(address) {
            Ok(stream) => {
                announced_failure = false;
                tracing::info!(address, "connected to the renderer");

                let outcome = serve(state, stream, running);
                if matches!(outcome, Ok(LinkEnded::Stopping)) {
                    return;
                }

                // Contact is lost, so the core no longer knows what the renderer
                // is doing. Saying "stopped" here would be a guess, and usually
                // the wrong one: the renderer keeps rendering precisely so that
                // losing the core does not stop a show.
                mark_unreachable(state, "the control channel to the renderer dropped");

                match outcome {
                    Ok(LinkEnded::Stopping) => return,
                    Ok(LinkEnded::RendererWentAway) => {
                        tracing::warn!("the renderer went away; retrying");
                    }
                    Err(error) => tracing::warn!("renderer link failed: {error}; retrying"),
                }
            }
            Err(error) => {
                // Logged once per outage, not once per second. A renderer that
                // is not running is a normal state during setup.
                if !announced_failure {
                    tracing::warn!(address, "cannot reach the renderer: {error}; retrying");
                    announced_failure = true;
                }
                mark_unreachable(state, "the renderer is not answering");
            }
        }

        // Sleep in slices so a shutdown does not wait out the whole delay.
        let mut waited = Duration::ZERO;
        while waited < RETRY_DELAY && running.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            waited += Duration::from_millis(50);
        }
    }
}

/// Feeds one renderer connection until it ends.
fn serve(state: &AppState, stream: TcpStream, running: &AtomicBool) -> Result<LinkEnded, IpcError> {
    if let Err(error) = stream.set_nodelay(true) {
        tracing::warn!("could not disable Nagle on the renderer link: {error}");
    }

    let closer = stream.try_clone()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);

    // The renderer announces itself first. Reading it is what makes the version
    // check real rather than something the renderer merely declares.
    match read_frame::<RendererMessage>(&mut reader)? {
        RendererMessage::Hello {
            protocol_version,
            renderer_version,
        } => {
            if protocol_version != IPC_PROTOCOL_VERSION {
                // The two ship together but restart independently, so this means
                // a half-finished upgrade. Refusing is better than sending a
                // running renderer messages it may half-understand.
                tracing::error!(
                    renderer = %renderer_version,
                    renderer_protocol = protocol_version,
                    core_protocol = IPC_PROTOCOL_VERSION,
                    "the renderer speaks a different control protocol; not driving it"
                );
                let _ = closer.shutdown(Shutdown::Both);
                return Ok(LinkEnded::RendererWentAway);
            }
            tracing::info!(renderer = %renderer_version, "renderer identified itself");
        }
        other => {
            tracing::error!("the renderer sent {other:?} instead of a hello");
            let _ = closer.shutdown(Shutdown::Both);
            return Ok(LinkEnded::RendererWentAway);
        }
    }

    // A reader thread, whose real job is to notice the renderer going away.
    // Writing alone would not: the first write after a peer closes usually
    // succeeds, and on a quiet show there may be no second write for minutes.
    let peer_alive = Arc::new(AtomicBool::new(true));
    let reader_alive = Arc::clone(&peer_alive);
    let telemetry_state = state.clone();
    let reader_thread = std::thread::Builder::new()
        .name("ovs-renderer-link-rx".to_owned())
        .spawn(move || {
            loop {
                match read_frame::<RendererMessage>(&mut reader) {
                    Ok(RendererMessage::Telemetry {
                        outputs,
                        gpu,
                        playback,
                        playheads,
                        missing_outputs,
                        frame,
                    }) => {
                        telemetry_state.set_output_telemetry(outputs);
                        telemetry_state.set_gpu_telemetry(gpu);
                        telemetry_state.set_playback_telemetry(playback);
                        telemetry_state.set_playheads(playheads);
                        telemetry_state.set_missing_outputs(missing_outputs);
                        telemetry_state.set_frame_timings(frame);
                    }
                    Ok(RendererMessage::Status(status)) => {
                        telemetry_state.mutate_state(|current| {
                            let change = current.set_renderer(status);
                            ((), change)
                        });
                    }
                    Ok(RendererMessage::Connectors { connectors }) => {
                        telemetry_state.set_connectors(connectors);
                    }
                    Ok(RendererMessage::NdiSources { sources }) => {
                        telemetry_state.set_ndi_sources(sources);
                    }
                    Ok(RendererMessage::StreamChunk {
                        surface,
                        size,
                        keyframe,
                        timestamp_us,
                        data_base64,
                    }) => {
                        use base64::Engine as _;

                        match base64::engine::general_purpose::STANDARD.decode(&data_base64) {
                            Ok(data) => telemetry_state.publish_stream_chunk(
                                surface,
                                crate::server::StreamChunk {
                                    size,
                                    keyframe,
                                    timestamp_us,
                                    data,
                                },
                            ),
                            Err(error) => {
                                // One picture that did not survive the trip. The
                                // next one is a moment away, and a watcher whose
                                // stream stutters once has lost nothing.
                                tracing::debug!("a stream chunk did not decode: {error}");
                            }
                        }
                    }
                    Ok(RendererMessage::StreamFailed { surface, reason }) => {
                        telemetry_state.publish_stream_failure(surface, reason);
                    }
                    Ok(RendererMessage::StreamEncoding { surface, encoding }) => {
                        telemetry_state.publish_stream_encoding(surface, encoding);
                    }
                    Ok(RendererMessage::CompositionPreview { png_base64 }) => {
                        use base64::Engine as _;

                        match base64::engine::general_purpose::STANDARD.decode(&png_base64) {
                            Ok(png) => telemetry_state.set_composition_preview(Some(png)),
                            Err(error) => {
                                tracing::debug!("a composition preview did not decode: {error}");
                            }
                        }
                    }
                    Ok(RendererMessage::Preview {
                        output_id,
                        name,
                        png_base64,
                    }) => {
                        use base64::Engine as _;

                        match base64::engine::general_purpose::STANDARD.decode(&png_base64) {
                            Ok(png) => telemetry_state.set_preview(crate::server::Preview {
                                output_id,
                                name,
                                png,
                                received_at: ovs_types::time::Timestamp::now(),
                            }),
                            // A preview that cannot be decoded is one nobody
                            // sees. Nothing on stage depends on it.
                            Err(error) => {
                                tracing::debug!(%output_id, "unreadable preview: {error}");
                            }
                        }
                    }
                    Ok(RendererMessage::Hello { .. }) => {}
                    Err(_) => break,
                }
            }
            reader_alive.store(false, Ordering::Relaxed);
        })?;

    let result = pump(state, &mut writer, running, &peer_alive);

    // Unblock the reader thread so it does not outlive this connection.
    let _ = closer.shutdown(Shutdown::Both);
    let _ = reader_thread.join();

    result
}

/// Sends the snapshot and then every update, until something ends the link.
fn pump(
    state: &AppState,
    writer: &mut BufWriter<TcpStream>,
    running: &AtomicBool,
    peer_alive: &AtomicBool,
) -> Result<LinkEnded, IpcError> {
    // Subscribe before snapshotting. The other order has a window in which a
    // change lands after the snapshot is taken but before the subscription
    // exists; that update would be lost, and because the sequence would still
    // look contiguous the renderer would never know to ask for a new snapshot.
    let mut updates = state.subscribe();
    send_snapshot(state, writer)?;

    // Whether previews are wanted, straight after the snapshot: the first frame
    // a renderer receives is a snapshot and nothing may come before it, but a
    // renderer should not spend its first connection producing pictures nobody
    // asked for either.
    let mut previews_sent = state.previews_enabled();
    write_frame(
        writer,
        &ControlMessage::SetPreviews {
            enabled: previews_sent,
        },
    )?;

    // What each connector on this machine is for, before anything is drawn:
    // the renderer must not take the Service Display's socket even for the
    // first frame.
    send_connector_roles(state, writer)?;

    // And whether a wall is being lined up. A renderer that restarted mid-
    // commissioning would otherwise come back showing the show while the
    // engineer on the ladder is still looking for the grid.
    write_frame(
        writer,
        &ControlMessage::SetStreams {
            surfaces: state.watched_streams(),
        },
    )?;

    // What the renderer was last told each socket is for. A role is derived from
    // the show and from the machine's configuration file, so it changes without
    // anybody writing one, and the only reliable way to notice is to look.
    let mut roles_sent = state.connector_roles().unwrap_or_default();
    let mut console_mode_sent = state.console_mode();

    let mut test_pattern_sent = state.test_pattern();
    let mut highlighted_sent = state.highlighted_piece();
    write_frame(
        writer,
        &ControlMessage::SetTestPattern {
            mode: test_pattern_sent,
            highlighted: highlighted_sent,
        },
    )?;

    // A small runtime of its own, so the blocking receive can be given a
    // timeout. The core's main runtime is not borrowed here: this thread must
    // not be able to occupy one of its workers.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(IpcError::Io)?;

    // Set by any change to the show, and cleared by looking. Not a claim that the
    // roles differ — the comparison decides that — only that it is worth asking.
    let mut roles_may_have_changed = false;

    while running.load(Ordering::Relaxed) {
        if !peer_alive.load(Ordering::Relaxed) {
            return Ok(LinkEnded::RendererWentAway);
        }

        // An operator at the Service Console. The renderer is told to stop and
        // its supervisor starts it again; the core carries on serving clients
        // through the gap, which is the whole reason these are two processes.
        // An operator switching previews off, which is how the readback and
        // the encode are taken back on a machine that needs the headroom.
        let previews = state.previews_enabled();
        if previews != previews_sent {
            previews_sent = previews;
            write_frame(writer, &ControlMessage::SetPreviews { enabled: previews })?;
        }

        // Either half changing is a reason to send: moving the selection
        // while the pattern is up is the whole way somebody uses it, and a
        // message only sent when the *mode* changed would leave the wall
        // highlighting the piece they have just stopped looking at.
        let test_pattern = state.test_pattern();
        let highlighted = state.highlighted_piece();
        if test_pattern != test_pattern_sent || highlighted != highlighted_sent {
            test_pattern_sent = test_pattern;
            highlighted_sent = highlighted;
            write_frame(
                writer,
                &ControlMessage::SetTestPattern {
                    mode: test_pattern,
                    highlighted,
                },
            )?;
        }

        // Either an explicit write, or a show that has changed which sockets it
        // uses. The second is the ordinary case now — a role is derived — and it
        // arrives as an `Update` below, so the flag alone would never fire for it.
        if state.take_connector_roles_changed() || roles_may_have_changed {
            roles_may_have_changed = false;
            let roles = state.connector_roles().unwrap_or_else(|error| {
                tracing::warn!("could not read the connector roles: {error}");
                roles_sent.clone()
            });
            let console_mode = state.console_mode();
            // Either one moving is a reason to send. The mode used not to be
            // here, and comparing only the roles would have meant a mode change
            // on an unchanged connector never leaving this machine.
            if roles != roles_sent || console_mode != console_mode_sent {
                roles_sent.clone_from(&roles);
                console_mode_sent = console_mode;
                write_frame(
                    writer,
                    &ControlMessage::SetConnectorRoles {
                        roles,
                        console_mode,
                    },
                )?;
            }
        }

        if let Some(group) = state.take_counter_reset() {
            write_frame(writer, &ControlMessage::ResetCounters { group })?;
        }

        // Who is watching what. Sent as a whole list rather than as changes, so
        // a renderer that reconnected mid-show does not have to be caught up.
        if state.take_streams_changed() {
            write_frame(
                writer,
                &ControlMessage::SetStreams {
                    surfaces: state.watched_streams(),
                },
            )?;
        }

        if state.take_renderer_restart() {
            tracing::info!("restarting the renderer at an operator's request");
            write_frame(writer, &ControlMessage::Shutdown)?;
            return Ok(LinkEnded::RendererWentAway);
        }

        let received =
            runtime.block_on(async { tokio::time::timeout(RECEIVE_TIMEOUT, updates.recv()).await });

        match received {
            // Nothing happened in the show. Go round and re-check both flags.
            Err(_elapsed) => {}
            Ok(Ok(ServerMessage::Update { sequence, change })) => {
                // A different show means different media. Resolved from the
                // show in this very change rather than from the state as it
                // now stands, so the catalogue the renderer receives is the one
                // for the show it just applied.
                let media = match &change {
                    StateChange::ShowLoaded { show, .. } => {
                        Some(media_catalogue(state, show.as_deref()))
                    }
                    // A live value can name a picture the document does not,
                    // and the renderer cannot draw a file it was never told
                    // about. Resolved from the show as it stands, because this
                    // change did not carry one.
                    StateChange::EffectChanged { overrides, .. } if names_a_picture(overrides) => {
                        let show = state.with_state(|current| current.show().cloned());
                        Some(media_catalogue(state, show.as_ref()))
                    }
                    _ => None,
                };

                write_frame(writer, &ControlMessage::Update { sequence, change })?;

                // An output may have been put on a socket, or taken off one. Which
                // change it was is not worked out here: the roles are compared on
                // the next pass and sent only if they differ, so this is allowed to
                // be wrong in the cheap direction.
                roles_may_have_changed = true;

                if let Some(entries) = media {
                    write_frame(writer, &ControlMessage::Media { entries })?;
                }
            }
            // Telemetry, faults and progress are for clients. The renderer is
            // where telemetry comes from; sending it back would be a loop.
            Ok(Ok(_)) => {}
            Ok(Err(RecvError::Lagged(missed))) => {
                // The link fell behind. The updates it missed are gone, so the
                // renderer would refuse everything after the gap. Resynchronise
                // rather than sending an update it cannot use.
                tracing::warn!(missed, "renderer link fell behind; resending a snapshot");
                send_snapshot(state, writer)?;
            }
            Ok(Err(RecvError::Closed)) => return Ok(LinkEnded::Stopping),
        }
    }

    Ok(LinkEnded::Stopping)
}

/// Records that the core cannot say what the renderer is doing.
fn mark_unreachable(state: &AppState, reason: &str) {
    state.mutate_state(|current| {
        let change = current.set_renderer(RendererStatus::Unreachable {
            reason: reason.to_owned(),
        });
        ((), change)
    });
}

/// Sends the current authoritative state, and where its media lives.
///
/// The catalogue follows the snapshot rather than preceding it, because the
/// renderer needs the show before the media means anything, and a snapshot is
/// the one message that is always safe to apply.
/// Tells the renderer what each connector on this machine is for.
///
/// A connector nobody has decided about is simply absent, which the renderer
/// reads as "not a show output" — the safe direction: an unassigned socket stays
/// dark rather than having a show put on it.
fn send_connector_roles(
    state: &AppState,
    writer: &mut BufWriter<TcpStream>,
) -> Result<(), IpcError> {
    let roles = state.connector_roles().unwrap_or_else(|error| {
        tracing::warn!("could not read the connector roles: {error}");
        std::collections::BTreeMap::new()
    });
    write_frame(
        writer,
        &ControlMessage::SetConnectorRoles {
            roles,
            console_mode: state.console_mode(),
        },
    )
}

fn send_snapshot(state: &AppState, writer: &mut BufWriter<TcpStream>) -> Result<(), IpcError> {
    let snapshot = state.authoritative().snapshot();
    let entries = media_catalogue(state, snapshot.show.as_ref());

    write_frame(writer, &ControlMessage::Snapshot(Box::new(snapshot)))?;
    write_frame(writer, &ControlMessage::Media { entries })
}

/// Resolves every media item a show refers to into a file on this machine.
///
/// Everything that cannot be resolved is left out rather than reported: an item
/// still importing, one whose asset failed verification, one whose file has gone
/// missing and one under a server with no media store all mean the same thing to
/// the renderer, which is that there are no pixels for it yet. The layer draws
/// its placeholder, the show goes on, and the reason belongs in the operator's
/// media view rather than on the control channel.
fn media_catalogue(state: &AppState, show: Option<&Show>) -> Vec<MediaLocation> {
    let (Some(show), Some(store)) = (show, state.media_store()) else {
        return Vec::new();
    };

    let mut referenced = show.referenced_media();
    live_pictures(state, show, &mut referenced);
    state.with_database(|database| {
        referenced
            .iter()
            .filter_map(|&media_item_id| {
                let item = database.load_media_item(media_item_id).ok()?;
                let asset = database.playable_asset_for(media_item_id).ok().flatten()?;
                let path = store.existing_path_for(&asset.object_key)?;

                // A path that is not UTF-8 cannot be encoded as JSON, and a
                // failure to encode would take down the channel rather than the
                // one file. Leaving it out costs one layer its media; failing
                // here would cost the whole link.
                if path.to_str().is_none() {
                    tracing::warn!(
                        path = %path.display(),
                        "media path is not valid UTF-8; not offering it to the renderer"
                    );
                    return None;
                }

                Some(MediaLocation {
                    media_item_id,
                    kind: item.kind,
                    path,
                    dimensions: item.dimensions,
                    duration_ms: item.duration_ms,
                    frame_rate: item.frame_rate,
                })
            })
            .collect()
    })
}

/// Adds the pictures live values name to what the renderer will be told about.
///
/// **A live value is not in the document.** An operator who picks a mask in the
/// live panel has saved nothing, so `referenced_media` does not mention the
/// picture — and a renderer that was never told where the file is cannot draw
/// it, however correctly the value itself arrived. That is exactly what it did:
/// the effect ran against the white default and the layer came up unmasked
/// (found on the machine, 2026-10-13).
///
/// Which parameters name a picture is the effect's own declaration, so an
/// effect added later needs nothing here.
fn live_pictures(
    state: &AppState,
    show: &Show,
    into: &mut std::collections::BTreeSet<ovs_types::id::MediaItemId>,
) {
    let live = state.with_state(|current| current.snapshot().effects);
    for (effect_instance_id, values) in &live {
        let Some(instance) = crate::state::effect_instance(show, *effect_instance_id) else {
            continue;
        };
        let Some(manifest) = ovs_types::config::effects::manifest(&instance.type_id) else {
            continue;
        };
        for parameter in manifest.parameters {
            if !matches!(parameter.kind, ovs_types::config::ParameterKind::Image) {
                continue;
            }
            // The live value if there is one, and what the show configured
            // otherwise: this is the same order the renderer reads them in.
            if let Some(media_item_id) = manifest
                .live_over(&instance.parameters, Some(values))
                .media(parameter.key)
            {
                into.insert(media_item_id);
            }
        }
    }
}

/// Whether a live change names something that might be a picture.
///
/// Asked of every live effect change, and a console sends those forty-four
/// times a second, so it is answered from the values in the change itself and
/// never from the database. It over-answers on purpose: a value that merely
/// looks like an identifier costs one catalogue, and a value that is one must
/// never be missed.
fn names_a_picture(parameters: &ovs_types::config::ParameterMap) -> bool {
    parameters.values().any(|value| match value {
        ovs_types::config::slot::ParameterValue::Text(name) => {
            name.parse::<ovs_types::id::MediaItemId>().is_ok()
        }
        _ => false,
    })
}

/// Asks the renderer to stop, on a clean core shutdown.
///
/// Best effort. A renderer that cannot be reached is left running, which is the
/// safe direction: an orphaned renderer keeps showing the last frame, while a
/// stopped one shows nothing.
pub fn request_shutdown(address: &str) {
    let Ok(stream) = TcpStream::connect(address) else {
        tracing::warn!(address, "could not reach the renderer to stop it");
        return;
    };

    let Ok(read_half) = stream.try_clone() else {
        tracing::warn!("could not open the renderer link to stop it");
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = BufWriter::new(stream);

    // Read the renderer's hello before writing, the same as an ordinary
    // connection does. Writing immediately races the renderer's own first write:
    // it can fail there, abandon the connection, and never read the shutdown at
    // all, which would leave the renderer running after the core had gone.
    if let Err(error) = read_frame::<RendererMessage>(&mut reader) {
        tracing::warn!("the renderer did not answer while being asked to stop: {error}");
        return;
    }

    if let Err(error) = write_frame(&mut writer, &ControlMessage::Shutdown) {
        tracing::warn!("could not ask the renderer to stop: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Read};
    use std::net::TcpListener;
    use std::thread;

    use ovs_db::Database;
    use ovs_ipc::read_frame;
    use ovs_types::config::{Layer, Show, Slot};
    use ovs_types::id::MediaItemId;

    use super::*;

    fn show() -> Show {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        layer.slots.push(Slot::media(3, MediaItemId::new()));
        show.layers.push(layer);
        show
    }

    fn app_state() -> AppState {
        AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test")
    }

    /// Stands in for the renderer: listens, and lets a test read what arrives.
    struct FakeRenderer {
        address: String,
        listener: TcpListener,
    }

    impl FakeRenderer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
            let address = listener.local_addr().expect("has an address").to_string();
            Self { address, listener }
        }

        /// Accepts a connection and announces itself, as the real renderer does.
        fn accept(&self) -> BufReader<TcpStream> {
            let (stream, _) = self.listener.accept().expect("accepts");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("sets a timeout so a broken test fails instead of hanging");

            let mut writer = BufWriter::new(stream.try_clone().expect("clones"));
            write_frame(
                &mut writer,
                &RendererMessage::Hello {
                    protocol_version: IPC_PROTOCOL_VERSION,
                    renderer_version: "0.0.1-test".to_owned(),
                },
            )
            .expect("announces itself");

            BufReader::new(stream)
        }

        /// Accepts a connection and claims a protocol version the core cannot speak.
        fn accept_speaking(&self, protocol_version: u32) -> BufReader<TcpStream> {
            let (stream, _) = self.listener.accept().expect("accepts");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("sets a timeout");

            let mut writer = BufWriter::new(stream.try_clone().expect("clones"));
            write_frame(
                &mut writer,
                &RendererMessage::Hello {
                    protocol_version,
                    renderer_version: "0.0.1-ancient".to_owned(),
                },
            )
            .expect("announces itself");

            BufReader::new(stream)
        }
    }

    /// Reads the next control message, skipping media catalogues.
    ///
    /// Every snapshot is followed by one, and a test about sequencing is not a
    /// test about media. The tests that are about media use
    /// [`next_frame`] and read every frame in order.
    fn next(reader: &mut BufReader<TcpStream>) -> ControlMessage {
        loop {
            match next_frame(reader) {
                // None of these is part of the state stream these tests are
                // about: one is where files are, the rest are service settings —
                // whether anybody wants pictures, whether a wall is being lined
                // up, and what each socket on the machine is for.
                ControlMessage::Media { .. }
                | ControlMessage::SetPreviews { .. }
                | ControlMessage::SetTestPattern { .. }
                | ControlMessage::SetStreams { .. }
                | ControlMessage::SetConnectorRoles { .. } => {}
                other => return other,
            }
        }
    }

    /// Reads the next control message, whatever it is.
    fn next_frame(reader: &mut BufReader<TcpStream>) -> ControlMessage {
        read_frame(reader).expect("reads a control message")
    }

    /// Reads the next message that is about the show.
    ///
    /// Skips the service settings — previews, the test pattern and the
    /// connector roles — which are sent on connect and whenever an operator
    /// changes them, and are not part of the sequence these tests are checking.
    fn next_show_frame(reader: &mut BufReader<TcpStream>) -> ControlMessage {
        loop {
            match next_frame(reader) {
                ControlMessage::SetPreviews { .. }
                | ControlMessage::SetTestPattern { .. }
                | ControlMessage::SetStreams { .. }
                | ControlMessage::SetConnectorRoles { .. } => {}
                other => return other,
            }
        }
    }

    /// An app state with a media store, and a file actually sitting in it.
    ///
    /// Returns the item, the directory (which deletes itself when dropped) and
    /// the state. The file's content is irrelevant here: this is about whether
    /// the core offers the path at all.
    fn state_with_media(
        directory: &tempfile::TempDir,
    ) -> (
        AppState,
        ovs_types::media::MediaItem,
        ovs_types::media::MediaAsset,
    ) {
        use ovs_types::id::MediaAssetId;
        use ovs_types::media::{AssetValidation, MediaAsset, MediaItem, MediaState};
        use ovs_types::time::Timestamp;

        let root = directory.path().join("media");
        let state = AppState::with_media_root(
            Database::open_in_memory().expect("opens"),
            "0.0.1-test",
            &root,
        );

        let sha256 = "3".repeat(64);
        let item = MediaItem {
            kind: ovs_types::media::MediaKind::Image,
            state: MediaState::Ready,
            duration_ms: None,
            frame_rate: None,
            dimensions: Some(ovs_types::geometry::SizePx::new(1920, 1080)),
            ..MediaItem::importing("Backdrop", "backdrop.png")
        };
        let asset = MediaAsset {
            id: MediaAssetId::new(),
            media_item_id: item.id,
            object_key: MediaAsset::object_key_for(&sha256, "png").expect("a valid key"),
            sha256,
            byte_size: 6,
            container: "png".to_owned(),
            codec: "png".to_owned(),
            validation: AssetValidation::Verified {
                at: Timestamp::now(),
            },
            created_at: Timestamp::now(),
        };

        state.with_database(|database| {
            database.register_media(&item, &asset).expect("registers");
        });

        let store = crate::MediaStore::new(&root);
        let path = store.path_for(&asset.object_key).expect("resolves");
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("creates");
        std::fs::write(&path, b"pixels").expect("writes");

        (state, item, asset)
    }

    /// Runs the link on a thread and returns the handle plus its stop flag.
    fn spawn_link(state: &AppState, address: &str) -> (Arc<AtomicBool>, thread::JoinHandle<()>) {
        let running = Arc::new(AtomicBool::new(true));
        let thread_state = state.clone();
        let thread_address = address.to_owned();
        let thread_running = Arc::clone(&running);
        let handle = thread::spawn(move || {
            run(&thread_state, &thread_address, &thread_running);
        });
        (running, handle)
    }

    #[test]
    fn the_first_thing_a_renderer_receives_is_a_snapshot() {
        let renderer = FakeRenderer::start();
        let state = app_state();
        state.mutate_state(|current| {
            let change = current.load_show(show(), None);
            ((), Some(change))
        });

        let (running, handle) = spawn_link(&state, &renderer.address);
        let mut reader = renderer.accept();

        let ControlMessage::Snapshot(snapshot) = next(&mut reader) else {
            panic!("a renderer must receive a snapshot before anything else");
        };
        assert_eq!(
            snapshot.show.as_ref().map(|p| p.name.as_str()),
            Some("Show")
        );

        running.store(false, Ordering::Relaxed);
        drop(reader);
        let _ = handle.join();
    }

    #[test]
    fn a_live_effect_value_reaches_the_renderer_without_being_asked_for() {
        // The renderer cannot ask for anything. If the core does not send this,
        // an operator moves an effect control and the wall does not change.
        let renderer = FakeRenderer::start();
        let state = app_state();

        let mut show = show();
        let mut blur = ovs_types::config::EffectInstance::new("ovs.blur", 1);
        let instance = blur.id;
        blur.parameters.insert(
            "radius_px".to_owned(),
            ovs_types::config::ParameterValue::Float(8.0),
        );
        show.layers[0].effects.push(blur);
        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });

        let (running, handle) = spawn_link(&state, &renderer.address);
        let mut reader = renderer.accept();

        let ControlMessage::Snapshot(snapshot) = next(&mut reader) else {
            panic!("expected a snapshot");
        };
        assert!(
            snapshot.effects.is_empty(),
            "nothing has been moved yet, so the opening state says nothing"
        );

        state.mutate_state(|current| {
            let change = current
                .set_effect_parameters(
                    instance,
                    &ovs_types::config::ParameterMap::from([(
                        "radius_px".to_owned(),
                        ovs_types::config::ParameterValue::Float(40.0),
                    )]),
                    false,
                )
                .expect("the show has this effect");
            ((), change)
        });

        let ControlMessage::Update { change, .. } = next(&mut reader) else {
            panic!("expected an update");
        };
        let StateChange::EffectChanged {
            effect_instance_id,
            overrides,
        } = change
        else {
            panic!("expected an effect change, got {change:?}");
        };
        assert_eq!(effect_instance_id, instance);
        assert_eq!(
            overrides.get("radius_px"),
            Some(&ovs_types::config::ParameterValue::Float(40.0))
        );

        running.store(false, Ordering::Relaxed);
        drop(reader);
        let _ = handle.join();
    }

    #[test]
    fn updates_follow_the_snapshot_in_sequence() {
        let renderer = FakeRenderer::start();
        let state = app_state();
        let show = show();
        let layer_id = show.layers[0].id;
        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });

        let (running, handle) = spawn_link(&state, &renderer.address);
        let mut reader = renderer.accept();

        let ControlMessage::Snapshot(snapshot) = next(&mut reader) else {
            panic!("expected a snapshot");
        };
        let base = snapshot.sequence;

        state.mutate_state(|current| {
            let change = current.select_slot(layer_id, 3).expect("valid");
            ((), change)
        });

        let ControlMessage::Update { sequence, .. } = next(&mut reader) else {
            panic!("expected an update");
        };
        assert_eq!(
            sequence,
            base.next(),
            "the first update must follow the snapshot"
        );

        running.store(false, Ordering::Relaxed);
        drop(reader);
        let _ = handle.join();
    }

    #[test]
    fn the_catalogue_follows_the_snapshot_and_says_where_the_file_is() {
        // The renderer cannot ask the database anything. If the core does not
        // say where a file is, a layer with media on it draws a placeholder.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let (state, item, asset) = state_with_media(&directory);

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, item.id));
        show.layers.push(layer);
        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });

        let renderer = FakeRenderer::start();
        let (running, handle) = spawn_link(&state, &renderer.address);
        let mut reader = renderer.accept();

        assert!(matches!(
            next_frame(&mut reader),
            ControlMessage::Snapshot(_)
        ));
        let ControlMessage::Media { entries } = next_frame(&mut reader) else {
            panic!("a catalogue must follow the snapshot");
        };

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].media_item_id, item.id);
        assert_eq!(entries[0].kind, ovs_types::media::MediaKind::Image);
        assert_eq!(entries[0].dimensions, item.dimensions);
        assert!(
            entries[0].path.ends_with(format!("{}.png", asset.sha256)),
            "got {:?}",
            entries[0].path
        );
        assert!(entries[0].path.is_file(), "and the file is really there");

        running.store(false, Ordering::Relaxed);
        drop(reader);
        let _ = handle.join();
    }

    #[test]
    fn media_whose_file_is_missing_is_left_out_rather_than_offered() {
        // A half-restored backup is the ordinary way this happens. Offering a
        // path that is not there would move the failure into the renderer,
        // where it would surface as a decode error per frame.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let (state, item, asset) = state_with_media(&directory);
        let store = crate::MediaStore::new(directory.path().join("media"));
        std::fs::remove_file(store.path_for(&asset.object_key).expect("resolves"))
            .expect("removes");

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, item.id));
        show.layers.push(layer);
        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });

        let renderer = FakeRenderer::start();
        let (running, handle) = spawn_link(&state, &renderer.address);
        let mut reader = renderer.accept();

        assert!(matches!(
            next_frame(&mut reader),
            ControlMessage::Snapshot(_)
        ));
        let ControlMessage::Media { entries } = next_frame(&mut reader) else {
            panic!("a catalogue must follow the snapshot");
        };
        assert!(entries.is_empty(), "got {entries:?}");

        running.store(false, Ordering::Relaxed);
        drop(reader);
        let _ = handle.join();
    }

    #[test]
    fn loading_a_show_sends_the_catalogue_for_that_show() {
        // Media follows the show, so a show load is the one update that
        // has to carry one. Without this, a show loaded while the renderer is
        // already connected would play black.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let (state, item, _asset) = state_with_media(&directory);

        let renderer = FakeRenderer::start();
        let (running, handle) = spawn_link(&state, &renderer.address);
        let mut reader = renderer.accept();

        // The empty snapshot and its empty catalogue: no show yet.
        assert!(matches!(
            next_show_frame(&mut reader),
            ControlMessage::Snapshot(_)
        ));
        let ControlMessage::Media { entries } = next_show_frame(&mut reader) else {
            panic!("a catalogue must follow the snapshot");
        };
        assert!(entries.is_empty(), "nothing is loaded yet");

        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, item.id));
        show.layers.push(layer);
        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });

        assert!(matches!(
            next_show_frame(&mut reader),
            ControlMessage::Update { .. }
        ));
        let ControlMessage::Media { entries } = next_show_frame(&mut reader) else {
            panic!("a show load must be followed by its catalogue");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].media_item_id, item.id);

        running.store(false, Ordering::Relaxed);
        drop(reader);
        let _ = handle.join();
    }

    #[test]
    fn a_server_with_no_media_store_offers_nothing_rather_than_guessing() {
        let state = app_state();
        state.mutate_state(|current| {
            let change = current.load_show(show(), None);
            ((), Some(change))
        });

        let renderer = FakeRenderer::start();
        let (running, handle) = spawn_link(&state, &renderer.address);
        let mut reader = renderer.accept();

        assert!(matches!(
            next_frame(&mut reader),
            ControlMessage::Snapshot(_)
        ));
        let ControlMessage::Media { entries } = next_frame(&mut reader) else {
            panic!("a catalogue must follow the snapshot");
        };
        assert!(entries.is_empty());

        running.store(false, Ordering::Relaxed);
        drop(reader);
        let _ = handle.join();
    }

    #[test]
    fn a_reconnecting_renderer_receives_a_fresh_snapshot_not_a_gapped_update() {
        // The renderer refuses an update that does not follow on, so a link that
        // resumed mid-stream would leave it refusing everything.
        let renderer = FakeRenderer::start();
        let state = app_state();
        let show = show();
        let layer_id = show.layers[0].id;
        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });

        let (running, handle) = spawn_link(&state, &renderer.address);

        let mut reader = renderer.accept();
        assert!(matches!(next(&mut reader), ControlMessage::Snapshot(_)));

        // The renderer restarts, missing whatever happened in between.
        drop(reader);
        state.mutate_state(|current| {
            let change = current.select_slot(layer_id, 3).expect("valid");
            ((), change)
        });

        let mut reader = renderer.accept();
        let ControlMessage::Snapshot(snapshot) = next(&mut reader) else {
            panic!("a reconnected renderer must receive a snapshot, not an update");
        };
        assert_eq!(
            snapshot
                .layers
                .iter()
                .find(|live| live.layer_id == layer_id)
                .expect("present")
                .active_slot,
            3,
            "and it must carry what happened while it was away"
        );

        running.store(false, Ordering::Relaxed);
        drop(reader);
        let _ = handle.join();
    }

    #[test]
    fn the_core_runs_perfectly_well_with_no_renderer_at_all() {
        // Nothing here may depend on something rendering.
        let state = app_state();
        // Port 1 on loopback: nothing is listening, and nothing will be.
        let (running, handle) = spawn_link(&state, "127.0.0.1:1");

        let show = show();
        let layer_id = show.layers[0].id;
        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });
        let change = state.mutate_state(|current| {
            let change = current.select_slot(layer_id, 3).expect("valid");
            (change.clone(), change)
        });
        assert!(
            change.is_some(),
            "state must change whether or not anything renders"
        );
        assert_eq!(
            state.with_state(|current| current.layer(layer_id).map(|live| live.active_slot)),
            Some(3)
        );

        running.store(false, Ordering::Relaxed);
        handle.join().expect("the link thread stops promptly");
    }

    #[test]
    fn a_shutdown_request_reaches_a_listening_renderer() {
        let renderer = FakeRenderer::start();
        let address = renderer.address.clone();
        let accepted = thread::spawn(move || {
            let mut reader = renderer.accept();
            next(&mut reader)
        });

        request_shutdown(&address);
        assert_eq!(accepted.join().expect("accepted"), ControlMessage::Shutdown);
    }

    #[test]
    fn asking_an_absent_renderer_to_stop_is_not_a_failure() {
        // An orphaned renderer showing the last frame beats a core that refuses
        // to shut down.
        request_shutdown("127.0.0.1:1");
    }

    #[test]
    fn the_link_stops_promptly_when_the_core_shuts_down() {
        let renderer = FakeRenderer::start();
        let state = app_state();
        let (running, handle) = spawn_link(&state, &renderer.address);

        let mut reader = renderer.accept();
        let mut byte = [0_u8; 1];
        let _ = reader
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(100)));
        let _ = reader.get_mut().read(&mut byte);

        running.store(false, Ordering::Relaxed);
        drop(reader);
        handle.join().expect("the link thread stops");
    }

    #[test]
    fn a_renderer_speaking_another_protocol_is_not_driven() {
        // The two ship together but restart independently, so this means a
        // half-finished upgrade. Sending it messages it may half-understand
        // would be worse than leaving it showing its last frame.
        let renderer = FakeRenderer::start();
        let state = app_state();
        let (running, handle) = spawn_link(&state, &renderer.address);

        let mut reader = renderer.accept_speaking(IPC_PROTOCOL_VERSION + 1);
        let mut byte = [0_u8; 1];
        let _ = reader
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(500)));
        assert!(
            matches!(reader.get_mut().read(&mut byte), Ok(0) | Err(_)),
            "a mismatched renderer must receive no state"
        );

        running.store(false, Ordering::Relaxed);
        drop(reader);
        handle.join().expect("the link thread stops");
    }
}
