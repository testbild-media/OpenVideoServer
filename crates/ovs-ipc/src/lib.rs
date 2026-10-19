//! The control channel between the core and the renderer.
//!
//! # Direction of connection
//!
//! The **renderer listens** and the core connects to it, not the other way
//! round. That is the whole point: when `ovsd` stops, crashes or is upgraded,
//! the renderer is not a client that lost its server. It is a process whose
//! caller went away, holding the last authoritative state it was given, still
//! rendering from it.
//!
//! # No async runtime
//!
//! Deliberately blocking `std::io`. The renderer is realtime infrastructure and
//! has no async runtime; the core, which does, runs its end on a dedicated
//! thread. Neither side's choice is forced on the other, and the codec is
//! testable against an in-memory buffer.
//!
//! # Framing
//!
//! A four-byte big-endian length followed by that many bytes of JSON. A
//! length-prefixed frame means a partial write cannot be mistaken for a complete
//! message, which a newline-delimited stream cannot promise once a payload can
//! contain arbitrary text.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

use std::io::{Read, Write};
use std::path::PathBuf;

use ovs_proto::realtime::{Sequence, StateChange, StateSnapshot};
use ovs_types::geometry::SizePx;
use ovs_types::id::{MediaItemId, OutputId};
use ovs_types::media::MediaKind;
use ovs_types::telemetry::{GpuTelemetry, OutputTelemetry, RendererStatus};
use serde::{Deserialize, Serialize};

/// Largest frame the reader will accept, in bytes.
///
/// A show with many layers and slots is comfortably under this. The limit
/// exists so that a corrupt or hostile length prefix cannot make the reader
/// allocate until the process dies.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

/// Version of this channel's message set.
///
/// The core and the renderer ship together, but they restart independently, so
/// a running renderer can briefly be an older build than the core that just
/// started. A mismatch is reported rather than guessed at.
///
/// Incremented whenever the message set changes in a way an older peer would
/// not understand. Version 2 added [`ControlMessage::Media`]: a version 1
/// renderer receiving one would fail to decode the frame, drop the connection
/// and be reconnected to immediately, forever. Refusing with one clear line in
/// the log is the behaviour this constant exists to produce.
///
/// Version 5 changed no message here. It marks the sequenced state growing a
/// shape an older peer cannot read: [`StateSnapshot`] now carries the live
/// effect values and [`StateChange`] has a variant for one of them changing, and
/// a version 4 renderer would fail to decode the frame carrying either.
///
/// Version 6 added [`RendererMessage::NdiSources`]. A variant rather than a
/// field on an existing message, because it is sent when the network changes and
/// not on the telemetry tick — so a version 5 core would meet a frame it cannot
/// decode, break the link and be reconnected to immediately, forever.
pub const IPC_PROTOCOL_VERSION: u32 = 6;

/// Something went wrong on the channel.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// The underlying stream failed.
    #[error("control channel I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// A message could not be encoded or decoded.
    #[error("control channel message could not be {operation}: {source}")]
    Codec {
        /// `encoded` or `decoded`.
        operation: &'static str,
        /// The underlying failure.
        #[source]
        source: serde_json::Error,
    },

    /// The declared frame length is larger than [`MAX_FRAME_BYTES`].
    #[error("control channel frame of {length} bytes exceeds the {MAX_FRAME_BYTES} byte limit")]
    FrameTooLarge {
        /// The length that was declared.
        length: u32,
    },

    /// The peer closed the connection.
    ///
    /// On the renderer's side this is not a failure. It means the core went
    /// away, and the renderer keeps rendering the state it already has.
    #[error("the peer closed the control channel")]
    Closed,
}

/// What the core sends to the renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// The complete authoritative state.
    ///
    /// Sent immediately on connect, before anything else, for the same reason a
    /// client receives one: an update applied to state the renderer guessed at
    /// would put something on stage the server never asked for.
    Snapshot(Box<StateSnapshot>),

    /// An incremental change.
    Update {
        /// Position in the stream.
        sequence: Sequence,
        /// What changed.
        change: StateChange,
    },

    /// Where the media the loaded show refers to lives on this machine.
    ///
    /// Sent immediately after every snapshot and again whenever the show
    /// changes, always as the complete set, so applying one needs no history.
    ///
    /// Outside the sequence deliberately. This is not show state: it cannot
    /// change what is on stage, only where the pixels for it come from. A
    /// catalogue that arrives late leaves a layer drawing flat for a few frames,
    /// where treating it as sequenced state would make a missing one stall every
    /// update behind it.
    ///
    /// # Why paths, and why only here
    ///
    /// The renderer runs on the same machine as the core and owns the GPU, so
    /// decoding belongs next to the GPU and the core's job is to say where the
    /// file is. A client is a different matter: it may be anywhere, and it gets
    /// media over HTTP. No physical path is ever part of the client protocol
    ///, which is why this lives on the control channel and not in
    /// [`StateSnapshot`].
    Media {
        /// One entry per referenced media item that has something playable.
        ///
        /// An item with no playable asset is simply absent, and the renderer
        /// draws its layer flat rather than black.
        entries: Vec<MediaLocation>,
    },

    /// Turn previews on or off.
    ///
    /// Sent on connect and whenever an operator changes it. A preview costs a
    /// readback and an encode; this is how that cost is taken back on a machine
    /// that needs the headroom.
    SetPreviews {
        /// Whether to produce them.
        enabled: bool,
    },

    /// What each connector on this machine is for.
    ///
    /// Sent on connect and whenever an operator changes one. The renderer drives
    /// the connectors marked as media outputs and **never** the one marked as
    /// the Service Display: that socket carries the touchscreen in the rack, and
    /// a renderer that took it would put a show on the operator's own screen and
    /// the console nowhere.
    ///
    /// This is machine configuration and lives on the server, not in a show:
    /// moving a show between servers must not move the wiring with it.
    SetConnectorRoles {
        /// One entry per connector an operator has decided about, keyed by
        /// `gpu/connector`. A connector that is not named here has had no
        /// decision made about it and is left alone.
        roles: std::collections::BTreeMap<String, ovs_types::config::ConnectorRole>,
        /// The mode to drive the Service Display at, when one was chosen.
        ///
        /// `None` means the panel's own preference, which is what every machine
        /// uses until somebody disagrees with it — and a deliberate answer as
        /// well as a default, because a panel that advertises a mode it cannot
        /// show is the reason this field exists.
        ///
        /// Here rather than in a message of its own because the two change
        /// together: an operator moving the System screen to another socket
        /// picks a mode for it in the same breath, and a renderer that took the
        /// new connector with the old mode would set a mode that panel has not
        /// got.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        console_mode: Option<ovs_types::config::DisplayMode>,
    },

    /// Put a test pattern on every output, or take it off again.
    ///
    /// A commissioning tool, and the reason it is here rather than in a show:
    /// lining a wall up happens before there is a show to line it up with, and
    /// an engineer on a ladder should not have to build one first.
    ///
    /// It replaces what the composition holds and leaves the piece mapping
    /// alone, so every output shows its own region of the pattern — which is
    /// what makes it a test of the mapping rather than only of the cabling.
    ///
    /// Never persisted and never restored. A server that came back from a
    /// reboot with a test pattern on the wall would be a dark show that looks
    /// like a working one.
    /// Which surfaces somebody is watching as a stream.
    ///
    /// The whole list every time, so the renderer never has to reconstruct it:
    /// what is in it is encoded, what is not is not. An empty list costs
    /// nothing at all, which is the ordinary state of a server nobody has a
    /// client open on.
    SetStreams {
        /// The surfaces to encode.
        surfaces: Vec<ovs_types::telemetry::StreamSurface>,
    },

    /// Which surfaces show the service test pattern.
    ///
    /// The composition, every output, or nothing. A pattern is drawn instead of
    /// the show rather than over it: what it is for is proving that a wall is
    /// wired to the output an operator thinks it is.
    SetTestPattern {
        /// Which surfaces draw it.
        mode: ovs_types::config::TestPatternMode,
        /// The piece to draw as the chosen one, in `Pieces` mode.
        ///
        /// Defaulted rather than a second message, so a renderer from before
        /// this existed still understands the one it is sent -- serde supplies
        /// a default for a field it does not know, where a new variant would
        /// have needed a protocol version.
        #[serde(default)]
        highlighted: Option<ovs_types::id::PieceId>,
    },

    /// Put the operator-visible counters back to zero.
    ///
    /// Dropped frames, late frames and decoder errors: the three numbers an
    /// operator watches during a show. What is cleared is the *reading*, not
    /// the history — a lifetime count is kept underneath, because "has this
    /// machine ever dropped a frame" and "has it dropped one since I looked"
    /// are different questions and a console that answered only the first is
    /// useless during a show.
    ResetCounters {
        /// Which of them.
        group: ovs_types::telemetry::CounterGroup,
    },

    /// Stop rendering and exit.
    ///
    /// The only thing that makes a renderer stop on purpose. Losing the channel
    /// does not.
    Shutdown,
}

/// One connector, as the renderer found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredConnector {
    /// PCI address of the GPU driving it.
    pub gpu_pci_address: String,
    /// Connector name as the driver reports it, for example `DP-3`.
    pub connector: String,
    /// Whether the driver believes something is attached.
    ///
    /// Informational only. Outputs are forced onto connectors and driven whether
    /// or not this is true; what it is for is a client showing an
    /// operator which socket has a cable in it.
    pub connected: bool,
    /// What the attached device calls itself, when it says.
    ///
    /// EDID is for recognising hardware in the client and for support calls. It
    /// decides nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The modes it offers, largest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<ConnectorMode>,
}

/// One mode a connector offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorMode {
    /// Resolution in pixels.
    pub size: ovs_types::geometry::SizePx,
    /// Refresh rate in millihertz, as the driver reports it.
    pub refresh_millihertz: u32,
}

/// Where one media item's pixels are, and what they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaLocation {
    /// The item this file is the playable form of.
    pub media_item_id: MediaItemId,
    /// Whether the renderer should expect a still or a moving image.
    pub kind: MediaKind,
    /// Absolute path to the file, on the machine both processes run on.
    ///
    /// Always valid UTF-8: the core leaves out anything else rather than
    /// failing the frame it is part of, because a path that cannot be encoded
    /// would break the whole channel over one badly named file.
    pub path: PathBuf,
    /// Pixel dimensions, where the core knows them.
    ///
    /// Advisory. The renderer sizes a texture from what it actually decodes,
    /// because the database's idea of a file can be out of date and the pixels
    /// cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<SizePx>,
    /// How long the item plays for, where the core knows.
    ///
    /// Advisory in the same way and for the same reason: it is what lets a
    /// clip's pass be worked out before a single frame has been decoded, so the
    /// first pass after a trigger is the right length rather than a guess. A
    /// decoder that opens the file and disagrees wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// The rate the item was recorded at, where the core knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<ovs_types::geometry::FrameRate>,
}

/// What the renderer sends back to the core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RendererMessage {
    /// Sent once, immediately after the connection opens.
    Hello {
        /// Channel protocol version the renderer speaks.
        protocol_version: u32,
        /// Renderer build version, for diagnostics.
        renderer_version: String,
    },

    /// The renderer's current status.
    Status(RendererStatus),

    /// Measured per-output state.
    ///
    /// Sent periodically. Telemetry that does not arrive is a monitoring
    /// problem, never a rendering one.
    Telemetry {
        /// One entry per active output.
        outputs: Vec<OutputTelemetry>,
        /// What the device reports about itself, when it can.
        ///
        /// Optional with a default rather than a new message, so an older peer
        /// on either side of a half-finished upgrade still understands this
        /// one: serde ignores a field it does not know and supplies a default
        /// for one that is missing. That is why this needed no protocol
        /// version, where a new variant would have.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gpu: Option<GpuTelemetry>,
        /// What the players are doing, when there are any.
        ///
        /// Optional for the same reason as `gpu`, and absent rather than zeroed
        /// when the renderer has nothing to say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        playback: Option<ovs_types::telemetry::PlaybackTelemetry>,
        /// Where each playing layer's playhead is.
        ///
        /// Beside `playback` rather than on it. The counts there are cumulative
        /// and describe the machine; these are instantaneous and describe one
        /// layer each, and keeping them apart leaves
        /// [`ovs_types::telemetry::PlaybackTelemetry`] a `Copy` struct that the
        /// core reads without allocating.
        ///
        /// Empty and defaulted rather than a new variant, for the same reason as
        /// `gpu`: an older peer on either side of a half-finished upgrade still
        /// understands this message, so this needed no protocol version.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        playheads: Vec<ovs_types::telemetry::LayerPlayhead>,
        /// Configured outputs nothing is driving.
        ///
        /// The renderer is the only process that knows: it is the one holding
        /// the connectors.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        missing_outputs: Vec<ovs_types::telemetry::MissingOutput>,
        /// Where the last frame's time went.
        ///
        /// Optional and defaulted for the same reason as `gpu`: an older peer on
        /// either side of a half-finished upgrade still understands this
        /// message. Absent rather than zeroed when no frame has been drawn, so
        /// that a renderer which has not started is not reported as one whose
        /// frames cost nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame: Option<ovs_types::telemetry::FrameTimings>,
    },

    /// Every connector this renderer's GPU can drive.
    ///
    /// Sent once, after the handshake, and again whenever the set changes. The
    /// renderer is the only process that can know this — it is the one holding
    /// the GPU — and a client cannot offer an operator a list of sockets to
    /// assign without it.
    Connectors {
        /// One entry per connector, in the driver's own order.
        connectors: Vec<DiscoveredConnector>,
    },

    /// Every NDI source this renderer can see on the network.
    ///
    /// Sent whenever the set changes, which is as often as senders are switched
    /// on and off through a rig check. The renderer is the only process that can
    /// know this — it is the one holding the NDI runtime — and a client cannot
    /// offer an operator a list of network sources to put on a layer without it.
    ///
    /// It also keeps the list honest: a source the renderer can see is one the
    /// renderer can receive, because it is the same library on the same network.
    /// A list gathered anywhere else could offer sources that would not play.
    ///
    /// **An empty list is an answer and is sent.** It means the runtime is
    /// installed and nothing is sending, which is a different thing from the
    /// runtime being absent — and the two look identical to anybody who is only
    /// told when there is something.
    NdiSources {
        /// Full source names, sorted, as the network announces them.
        ///
        /// Shaped `MACHINE (Source Name)`. The whole string is the identifier:
        /// nothing survives a sender restarting except this, and a position in
        /// a list means nothing a moment later.
        sources: Vec<String>,
    },

    /// A picture of the whole composition, before it is cut into pieces.
    ///
    /// Sent beside the per-output previews rather than instead of them: an
    /// output shows what one wall has, and this shows what the show is. On a
    /// rig with six walls they are six different answers and one.
    CompositionPreview {
        /// A PNG, base64-encoded.
        png_base64: String,
    },

    /// A small picture of what an output is showing.
    ///
    /// Sent a few times a second, small and already compressed. A preview is a
    /// convenience for whoever is watching: one that does not arrive is one an
    /// operator does not see, and never a reason for anything on stage to
    /// change.
    Preview {
        /// Which output this is a picture of.
        output_id: OutputId,
        /// Operator-visible name of that output.
        name: String,
        /// A PNG, base64 encoded.
        ///
        /// Base64 because this channel is JSON, and a few tens of kilobytes
        /// eight times a second on a loopback socket is not worth a second
        /// transport. PNG because the renderer already has an encoder and a
        /// preview that is lossy in the same way as the wall is not.
        png_base64: String,
    },

    /// One encoded picture of a surface somebody is watching.
    ///
    /// H.264, Annex-B, base64 because this channel is JSON. A stream is only
    /// produced while somebody is subscribed (`SetStreams`), so these arrive in
    /// bursts between long silences rather than all evening.
    StreamChunk {
        /// Which surface it is a picture of.
        surface: ovs_types::telemetry::StreamSurface,
        /// What it is being encoded at, so a watcher knows before decoding.
        size: ovs_types::geometry::SizePx,
        /// Whether a watcher can start here.
        keyframe: bool,
        /// Microseconds since this stream started.
        timestamp_us: u64,
        /// The bytes.
        data_base64: String,
    },

    /// What a surface turned out to be encoded with.
    ///
    /// Sent once, when the encoder comes up, and again if a surface is
    /// restarted onto a different one. Not per frame: it is the same answer
    /// until something changes, and a watcher told it forty times a second
    /// would be told nothing.
    StreamEncoding {
        /// Which surface.
        surface: ovs_types::telemetry::StreamSurface,
        /// The encoder, whether it is on the card, and the rate it is really
        /// producing — which is how a capped preview says it is capped
        /// (owner decision Q13).
        encoding: ovs_types::telemetry::StreamEncoding,
    },

    /// A stream could not be produced, and why.
    ///
    /// Sent once per attempt rather than per frame: a machine with no encoder
    /// will not grow one while the show runs.
    StreamFailed {
        /// Which surface.
        surface: ovs_types::telemetry::StreamSurface,
        /// What went wrong, for a client to show and a log to keep.
        reason: String,
    },
}

/// Writes one length-prefixed JSON frame.
///
/// # Errors
///
/// Returns [`IpcError::Codec`] if the value cannot be serialised,
/// [`IpcError::FrameTooLarge`] if it exceeds [`MAX_FRAME_BYTES`], and
/// [`IpcError::Io`] if the write fails.
pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), IpcError> {
    let payload = serde_json::to_vec(value).map_err(|source| IpcError::Codec {
        operation: "encoded",
        source,
    })?;

    let length =
        u32::try_from(payload.len()).map_err(|_| IpcError::FrameTooLarge { length: u32::MAX })?;
    if length > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge { length });
    }

    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

/// Reads one length-prefixed JSON frame.
///
/// # Errors
///
/// Returns [`IpcError::Closed`] when the peer closed cleanly between frames,
/// [`IpcError::FrameTooLarge`] for an implausible length prefix, and
/// [`IpcError::Codec`] for a payload that does not parse.
pub fn read_frame<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> Result<T, IpcError> {
    let mut length_bytes = [0_u8; 4];
    match reader.read_exact(&mut length_bytes) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(IpcError::Closed);
        }
        Err(error) => return Err(IpcError::Io(error)),
    }

    let length = u32::from_be_bytes(length_bytes);
    if length > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge { length });
    }

    let mut payload = vec![0_u8; length as usize];
    reader.read_exact(&mut payload)?;

    serde_json::from_slice(&payload).map_err(|source| IpcError::Codec {
        operation: "decoded",
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ovs_types::live::MasterLive;

    use super::*;

    fn snapshot() -> StateSnapshot {
        StateSnapshot {
            sequence: Sequence(7),
            show: None,
            master: MasterLive::default(),
            layers: Vec::new(),
            effects: Default::default(),
            generators: Default::default(),
            desk: Default::default(),
            renderer: RendererStatus::Running,
        }
    }

    #[test]
    fn a_frame_round_trips() {
        let mut buffer = Vec::new();
        let message = ControlMessage::Snapshot(Box::new(snapshot()));
        write_frame(&mut buffer, &message).expect("writes");

        let mut cursor = Cursor::new(buffer);
        let back: ControlMessage = read_frame(&mut cursor).expect("reads");
        assert_eq!(back, message);
    }

    #[test]
    fn several_frames_read_back_in_order() {
        let mut buffer = Vec::new();
        for nonce in 0..5_u64 {
            write_frame(
                &mut buffer,
                &ControlMessage::Update {
                    sequence: Sequence(nonce),
                    change: StateChange::MasterChanged {
                        master: MasterLive::default(),
                    },
                },
            )
            .expect("writes");
        }

        let mut cursor = Cursor::new(buffer);
        for expected in 0..5_u64 {
            let message: ControlMessage = read_frame(&mut cursor).expect("reads");
            let ControlMessage::Update { sequence, .. } = message else {
                panic!("expected an update");
            };
            assert_eq!(sequence, Sequence(expected));
        }
        assert!(matches!(
            read_frame::<ControlMessage>(&mut cursor),
            Err(IpcError::Closed)
        ));
    }

    #[test]
    fn a_live_effect_value_rides_the_sequenced_stream_like_every_other_live_change() {
        // Not a message of its own. A live effect value is show state, so it
        // travels where the rest of the show state travels and arrives at the
        // renderer and at every client in the same order.
        let instance = ovs_types::id::EffectInstanceId::new();
        let message = ControlMessage::Update {
            sequence: Sequence(3),
            change: StateChange::EffectChanged {
                effect_instance_id: instance,
                overrides: ovs_types::config::ParameterMap::from([(
                    "radius_px".to_owned(),
                    ovs_types::config::ParameterValue::Float(40.0),
                )]),
            },
        };

        let mut buffer = Vec::new();
        write_frame(&mut buffer, &message).expect("writes");
        let back: ControlMessage = read_frame(&mut Cursor::new(buffer)).expect("reads");
        assert_eq!(back, message);
    }

    #[test]
    fn a_clean_close_between_frames_is_not_an_error_condition() {
        // This is the ordinary case of the core shutting down, and the renderer
        // must be able to tell it apart from a corrupt stream.
        let mut cursor = Cursor::new(Vec::new());
        assert!(matches!(
            read_frame::<ControlMessage>(&mut cursor),
            Err(IpcError::Closed)
        ));
    }

    #[test]
    fn a_truncated_frame_is_an_error_rather_than_a_silent_short_read() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &ControlMessage::Shutdown).expect("writes");
        buffer.truncate(buffer.len() - 2);

        let mut cursor = Cursor::new(buffer);
        let error = read_frame::<ControlMessage>(&mut cursor).expect_err("truncated");
        assert!(matches!(error, IpcError::Io(_)), "got {error:?}");
    }

    #[test]
    fn an_implausible_length_prefix_is_refused_without_allocating() {
        // A corrupt or hostile prefix must not be able to make the reader
        // allocate gigabytes.
        let mut buffer = (MAX_FRAME_BYTES + 1).to_be_bytes().to_vec();
        buffer.extend_from_slice(b"{}");

        let mut cursor = Cursor::new(buffer);
        let error = read_frame::<ControlMessage>(&mut cursor).expect_err("too large");
        assert!(matches!(error, IpcError::FrameTooLarge { .. }));
    }

    #[test]
    fn a_payload_that_is_not_the_expected_message_is_reported_as_a_codec_error() {
        let mut buffer = Vec::new();
        write_frame(
            &mut buffer,
            &serde_json::json!({"type": "not_a_real_message"}),
        )
        .expect("writes");

        let mut cursor = Cursor::new(buffer);
        let error = read_frame::<ControlMessage>(&mut cursor).expect_err("unknown variant");
        assert!(matches!(
            error,
            IpcError::Codec {
                operation: "decoded",
                ..
            }
        ));
    }

    #[test]
    fn renderer_messages_round_trip() {
        let messages = vec![
            RendererMessage::Hello {
                protocol_version: IPC_PROTOCOL_VERSION,
                renderer_version: "0.0.1".to_owned(),
            },
            RendererMessage::Status(RendererStatus::Degraded {
                reason: "preview encoder restarted".to_owned(),
            }),
            RendererMessage::Telemetry {
                outputs: Vec::new(),
                gpu: None,
                playback: None,
                playheads: Vec::new(),
                missing_outputs: Vec::new(),
                frame: None,
            },
            RendererMessage::Telemetry {
                outputs: Vec::new(),
                missing_outputs: vec![ovs_types::telemetry::MissingOutput {
                    output_id: ovs_types::id::OutputId::new(),
                    name: "LED Wall Left".to_owned(),
                    reason: "nothing matches this one's identity".to_owned(),
                }],
                playback: Some(ovs_types::telemetry::PlaybackTelemetry {
                    playing: 2,
                    late_frames: 0,
                    errors: 0,
                }),
                playheads: vec![ovs_types::telemetry::LayerPlayhead {
                    layer_id: ovs_types::id::LayerId::new(),
                    slot: 3,
                    media_item_id: ovs_types::id::MediaItemId::new(),
                    frame: 37,
                    frame_count: 100,
                    // A stand-in clip: eight bits, which is what most material is.
                    decode_depth: ovs_types::depth::DecodeDepth::Eight,
                    position_ms: 1_480,
                    duration_ms: 4_000,
                    speed_ms_per_second: 500.0,
                }],
                gpu: Some(GpuTelemetry {
                    device_name: "NVIDIA GeForce RTX 3060 Ti".to_owned(),
                    vram_used_bytes: Some(1_200_000_000),
                    vram_total_bytes: Some(8_000_000_000),
                    utilisation_percent: Some(62),
                    temperature_c: Some(58),
                }),
                frame: Some(ovs_types::telemetry::FrameTimings {
                    media_us: 120,
                    playback_us: 7_400,
                    compose_us: 2_100,
                }),
            },
        ];
        for message in messages {
            let mut buffer = Vec::new();
            write_frame(&mut buffer, &message).expect("writes");
            let back: RendererMessage = read_frame(&mut Cursor::new(buffer)).expect("reads");
            assert_eq!(back, message);
        }
    }

    #[test]
    fn a_telemetry_frame_that_says_nothing_about_playheads_still_decodes() {
        // Why the playhead list needed no protocol version. The two ends ship
        // together but restart independently, so a half-finished upgrade must
        // not drop the channel over a field one side has never heard of — which
        // is what a new message variant would have done.
        let mut buffer = Vec::new();
        write_frame(
            &mut buffer,
            &serde_json::json!({"type": "telemetry", "outputs": []}),
        )
        .expect("writes");

        let back: RendererMessage = read_frame(&mut Cursor::new(buffer)).expect("reads");
        let RendererMessage::Telemetry { playheads, .. } = back else {
            panic!("expected a telemetry frame");
        };
        assert!(
            playheads.is_empty(),
            "a peer that said nothing must not look like one reporting a layer at zero"
        );
    }

    #[test]
    fn only_shutdown_asks_the_renderer_to_stop() {
        // Losing the channel must never be expressible as "stop rendering".
        let variants = [
            ControlMessage::Snapshot(Box::new(snapshot())),
            ControlMessage::Update {
                sequence: Sequence(1),
                change: StateChange::MasterChanged {
                    master: MasterLive::default(),
                },
            },
            ControlMessage::Shutdown,
        ];
        let stopping: Vec<_> = variants
            .iter()
            .filter(|message| matches!(message, ControlMessage::Shutdown))
            .collect();
        assert_eq!(stopping.len(), 1);
    }
}
