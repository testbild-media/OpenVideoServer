//! The realtime protocol.
//!
//! Two rules shape everything here:
//!
//! 1. **A client obtains an authoritative snapshot before processing incremental
//!    updates.** Not "should"; a client that applies a delta to state it guessed
//!    at is showing the operator something the server never said.
//! 2. **The protocol is versioned and an incompatible client is refused
//!    explicitly** rather than half-understood.
//!
//! Both are enforced by [`SequenceTracker`], which a client drives and which
//! refuses to report an update as applicable until a snapshot has arrived and
//! while any update is missing.

use ovs_types::config::{ParameterMap, Show};
use ovs_types::control::DeskControls;
use ovs_types::id::EffectInstanceId;
use ovs_types::live::{EffectOverrides, GeneratorOverrides, LayerLive, MasterLive};
use ovs_types::telemetry::{RendererStatus, ServerTelemetry};
use ovs_types::version::REALTIME_PROTOCOL_VERSION;
use serde::{Deserialize, Serialize};

/// Position of a message in the server's update stream.
///
/// Monotonic from the moment the server starts. A client that sees a gap knows
/// it has missed something and must ask for a fresh snapshot rather than
/// carrying on with state that is now a guess.
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
#[serde(transparent)]
pub struct Sequence(pub u64);

impl Sequence {
    /// The sequence a server starts at.
    pub const START: Self = Self(0);

    /// Returns the next sequence.
    ///
    /// Saturates rather than wrapping. At one update per millisecond, reaching
    /// the saturation point takes longer than the heat death of the venue, but
    /// wrapping would silently turn a gap into an apparent replay.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// The first message a client sends.
///
/// `Debug` is implemented by hand so a session token cannot reach a log through
/// a derived formatter.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClientHello {
    /// Realtime protocol version the client speaks.
    pub protocol_version: u32,
    /// Session token obtained from the HTTP API.
    ///
    /// Carried in the handshake rather than in a header or a query string. A
    /// browser cannot set headers on a WebSocket handshake, and a query string
    /// would write the token into every access log and proxy cache along the way.
    pub token: String,
    /// Human-readable client name, for diagnostics.
    pub client_name: String,
    /// Client build version, for diagnostics.
    pub client_version: String,
}

impl std::fmt::Debug for ClientHello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientHello")
            .field("protocol_version", &self.protocol_version)
            .field("token", &"<redacted>")
            .field("client_name", &self.client_name)
            .field("client_version", &self.client_version)
            .finish()
    }
}

impl ClientHello {
    /// Builds a hello for this build.
    #[must_use]
    pub fn current(
        token: impl Into<String>,
        client_name: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: REALTIME_PROTOCOL_VERSION,
            token: token.into(),
            client_name: client_name.into(),
            client_version: client_version.into(),
        }
    }
}

/// The server's answer to a hello.
///
/// One flat set of outcomes rather than "accepted or rejected, and separately
/// why". Nothing downstream acts on the grouping — a client has to do something
/// different for each specific outcome, update, sign in again or wait — and a
/// tag inside a tag is a shape a generated client cannot describe faithfully,
/// which is how a contract starts drifting from the code it was generated from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ServerHello {
    /// The connection is open. A snapshot follows.
    Accepted {
        /// Protocol version in use.
        protocol_version: u32,
        /// Server build version, for diagnostics.
        server_version: String,
    },
    /// The client speaks a protocol version this server cannot.
    IncompatibleProtocol {
        /// The version the client offered.
        client_version: u32,
        /// The version this server speaks.
        server_version: u32,
    },
    /// The session token was missing, expired or invalid.
    Unauthenticated,
    /// The server is starting and not yet able to serve state.
    NotReady,
}

impl ServerHello {
    /// Whether the connection was opened.
    #[must_use]
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }
}

/// Decides whether a client may connect.
///
/// The major version must match exactly. A client that speaks a different
/// version is told which version the server speaks, so the operator sees
/// "update the client" rather than an unexplained disconnect.
#[must_use]
pub fn negotiate(hello: &ClientHello) -> ServerHello {
    if hello.protocol_version == REALTIME_PROTOCOL_VERSION {
        ServerHello::Accepted {
            protocol_version: REALTIME_PROTOCOL_VERSION,
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    } else {
        ServerHello::IncompatibleProtocol {
            client_version: hello.protocol_version,
            server_version: REALTIME_PROTOCOL_VERSION,
        }
    }
}

/// The complete authoritative state, as of one sequence.
///
/// Sent on connect and whenever a client asks for a resynchronisation. Every
/// incremental update after it is relative to this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StateSnapshot {
    /// The sequence this snapshot is current as of.
    pub sequence: Sequence,
    /// The loaded show, or `None` when the server has none open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<Show>,
    /// Live master state.
    pub master: MasterLive,
    /// Live state of each layer in the loaded show.
    pub layers: Vec<LayerLive>,
    /// What a control source has done to the effects since the show opened.
    ///
    /// Here rather than only in the updates that follow, because a client that
    /// joins mid-show has to see the effects that are already moved. Without it
    /// a late client would draw every effect control at its configured value
    /// while the wall showed something else.
    ///
    /// Only the instances somebody has touched. An instance that is absent is
    /// one running at exactly what the show configured, which is where every
    /// effect starts.
    #[serde(default, skip_serializing_if = "EffectOverrides::is_empty")]
    pub effects: EffectOverrides,
    /// What a control source has done to the generators since the show
    /// opened.
    ///
    /// The same as [`effects`](Self::effects), for the other kind of module.
    #[serde(default, skip_serializing_if = "GeneratorOverrides::is_empty")]
    pub generators: GeneratorOverrides,
    /// What the open show's patch gives a lighting console.
    ///
    /// Held only while [`MasterLive::dmx_enabled`] is on; with it off the
    /// client has the whole show. A client reads the two together and greys
    /// out exactly what the server would refuse, rather than reading fixture
    /// personalities itself — a second reading of those would be the one that
    /// went out of date (owner's rule, 2026-10-19; see `ovs_types::control`).
    ///
    /// Changes only with the show, so it travels with the show: here and in
    /// [`StateChange::ShowLoaded`].
    #[serde(default, skip_serializing_if = "DeskControls::is_empty")]
    pub desk: DeskControls,
    /// What the renderer is doing.
    pub renderer: RendererStatus,
}

/// An incremental change to the authoritative state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum StateChange {
    /// A different show was loaded, or the loaded one was replaced.
    ///
    /// Carries the whole show: a show is edited and saved whole, so a
    /// field-level diff would be more machinery than the case deserves.
    ///
    /// It carries the live state as well, and that is the part worth explaining.
    /// Loading a show replaces the live state too — what the restart policy
    /// says each layer and the master start at — so a peer given only
    /// the show would have to derive the rest. That means a second
    /// implementation of the policy, in every peer, which has to agree with the
    /// server's forever. Sending it is one field; deriving it is a class of bug
    /// where a client shows a dimmer the server does not have.
    ShowLoaded {
        /// The show now in effect, or `None` when it was closed.
        show: Option<Box<Show>>,
        /// Live master state as it now stands.
        master: MasterLive,
        /// Live state of every layer in the show now loaded.
        ///
        /// Empty when the show was closed.
        layers: Vec<LayerLive>,
        /// What a control source has done to the effects of the show now
        /// loaded.
        ///
        /// What they were when the show was last open, since 2026-10-19: a
        /// show comes back the way it was left, effects included. It is sent
        /// rather than worked out for the same reason `master` and `layers`
        /// are: the restart policy lives in the core, and a peer that derived
        /// its own copy would be a second implementation of it that has to
        /// agree with the server's forever.
        #[serde(default, skip_serializing_if = "EffectOverrides::is_empty")]
        effects: EffectOverrides,
        /// What a control source has done to the generators of the show now
        /// open.
        generators: GeneratorOverrides,
        /// What the patch of the show now in effect gives a lighting console.
        ///
        /// See [`StateSnapshot::desk`].
        #[serde(default, skip_serializing_if = "DeskControls::is_empty")]
        desk: DeskControls,
    },
    /// Master live state changed.
    MasterChanged {
        /// The new master state.
        master: MasterLive,
    },
    /// One layer's live state changed.
    LayerChanged {
        /// The new layer state.
        layer: LayerLive,
    },
    /// One effect instance's live values changed.
    ///
    /// A key and a value rather than one struct, because that is what an entry
    /// of [`StateSnapshot::effects`] is: the collection is a map, and a type
    /// invented to hold the pair would have exactly one user.
    EffectChanged {
        /// Which instance moved.
        effect_instance_id: EffectInstanceId,
        /// Every live value now on it, whole rather than as a difference.
        ///
        /// Empty when the last live value was cleared, which is how "this
        /// effect is back at what the show configured" is said. A peer
        /// replaces what it holds for this instance with this, so applying one
        /// needs no history and a client cannot end up holding a value nobody
        /// sent.
        #[serde(default, skip_serializing_if = "ParameterMap::is_empty")]
        overrides: ParameterMap,
    },
    /// One generator instance's live values changed.
    ///
    /// Everything [`EffectChanged`](Self::EffectChanged) is, for the other kind
    /// of module: the whole map rather than a difference, so applying one needs
    /// no history, and empty when the last live value was cleared.
    GeneratorChanged {
        /// Which instance moved.
        generator_instance_id: ovs_types::id::GeneratorInstanceId,
        /// Every live value now on it, whole rather than as a difference.
        #[serde(default, skip_serializing_if = "ParameterMap::is_empty")]
        overrides: ParameterMap,
    },
    /// The renderer's status changed.
    RendererChanged {
        /// The new status.
        renderer: RendererStatus,
    },
}

/// A message from the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The authoritative state. Always precedes any update a client applies.
    Snapshot(Box<StateSnapshot>),
    /// An incremental change.
    Update {
        /// Position of this update in the stream.
        sequence: Sequence,
        /// What changed.
        change: StateChange,
    },
    /// Measured server and output state.
    ///
    /// Outside the sequenced stream: telemetry is a periodic sample, and a
    /// dropped sample needs no resynchronisation.
    Telemetry(Box<ServerTelemetry>),
    /// What this machine is wired to, whenever it changes.
    ///
    /// A monitor swapped, a processor powered up, a cable moved: each of them
    /// changes what an operator may assign an output to, and a chooser filled
    /// in when they signed in would go on describing what was there before.
    ///
    /// Outside the sequenced stream for the same reason as the rest: the whole
    /// list every time, so a dropped one costs nothing.
    Connectors {
        /// One entry per socket, with what an operator has decided it is for.
        connectors: Vec<crate::http::ConnectorInfo>,
    },
    /// Every NDI source the renderer can see on the network.
    ///
    /// Outside the sequenced stream, for the reason telemetry is: each message
    /// carries the whole list, so a dropped one costs nothing and needs no
    /// resynchronisation. Sent when the set changes rather than periodically,
    /// because that is when an operator's list of what they can put on a layer
    /// becomes wrong.
    ///
    /// An empty list is an answer: the runtime is there and nothing is sending.
    /// Absent NDI looks the same from here, which is why the renderer says
    /// which it is in its own log rather than leaving a client to guess.
    NdiSources {
        /// Full source names, sorted, shaped `MACHINE (Source Name)`.
        sources: Vec<String>,
    },
    /// Progress of a long-running media operation.
    MediaProgress {
        /// The item being worked on.
        media_item_id: ovs_types::id::MediaItemId,
        /// Completion between 0.0 and 1.0.
        progress: f32,
        /// What is happening, for the operator.
        stage: String,
    },
    /// Something failed in a way the operator should see.
    Fault {
        /// Short machine-readable code.
        code: String,
        /// Human-readable description.
        message: String,
    },
    /// Answer to a ping.
    Pong {
        /// The nonce from the ping.
        nonce: u64,
    },
}

/// A message from a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Ask for a fresh snapshot.
    ///
    /// Sent after a detected gap, and after a reconnect.
    Resynchronise,
    /// Measure round-trip time.
    Ping {
        /// Echoed back in the pong.
        nonce: u64,
    },
}

/// Why a client cannot apply an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// No snapshot has arrived yet.
    #[error("received an update before any snapshot; a client must resynchronise first")]
    NoSnapshotYet,
    /// One or more updates were missed.
    #[error("missed updates between sequence {expected} and {received}; resynchronise")]
    Gap {
        /// The sequence that was expected next.
        expected: u64,
        /// The sequence that actually arrived.
        received: u64,
    },
}

/// Client-side tracking of the update stream.
///
/// Exists so that "obtain a snapshot before processing incremental updates" is
/// something a client cannot forget to do: an update offered before a snapshot,
/// or after a gap, is refused rather than applied.
#[derive(Debug, Clone, Default)]
pub struct SequenceTracker {
    last: Option<Sequence>,
}

impl SequenceTracker {
    /// Creates a tracker that has not yet seen a snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a snapshot, making subsequent updates applicable.
    pub fn accept_snapshot(&mut self, snapshot: &StateSnapshot) {
        self.last = Some(snapshot.sequence);
    }

    /// Decides whether an update may be applied.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::NoSnapshotYet`] before any snapshot has arrived,
    /// and [`StreamError::Gap`] when updates were missed. Both mean the client
    /// must send [`ClientMessage::Resynchronise`].
    pub fn accept_update(&mut self, sequence: Sequence) -> Result<(), StreamError> {
        let Some(last) = self.last else {
            return Err(StreamError::NoSnapshotYet);
        };

        let expected = last.next();
        if sequence == expected {
            self.last = Some(sequence);
            return Ok(());
        }

        if sequence <= last {
            // A replay of something already applied. Harmless to ignore, and not
            // worth forcing a resynchronisation over.
            return Ok(());
        }

        Err(StreamError::Gap {
            expected: expected.0,
            received: sequence.0,
        })
    }

    /// Forgets the stream position, as after a disconnect.
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// Returns the last applied sequence, if any.
    #[must_use]
    pub fn position(&self) -> Option<Sequence> {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(at: u64) -> StateSnapshot {
        StateSnapshot {
            sequence: Sequence(at),
            show: None,
            master: MasterLive::default(),
            layers: Vec::new(),
            effects: EffectOverrides::new(),
            generators: GeneratorOverrides::new(),
            desk: Default::default(),
            renderer: RendererStatus::Running,
        }
    }

    #[test]
    fn a_matching_client_is_accepted() {
        let hello = ClientHello::current("t0ken", "OVS Desktop", "0.0.1");
        assert!(matches!(negotiate(&hello), ServerHello::Accepted { .. }));
    }

    #[test]
    fn an_incompatible_client_is_told_which_version_to_speak() {
        // An unexplained disconnect sends an operator hunting through logs.
        let hello = ClientHello {
            protocol_version: 99,
            ..ClientHello::current("t0ken", "Old", "0.0.1")
        };
        let ServerHello::IncompatibleProtocol {
            client_version,
            server_version,
        } = negotiate(&hello)
        else {
            panic!("an incompatible client must be refused");
        };
        assert_eq!(client_version, 99);
        assert_eq!(server_version, REALTIME_PROTOCOL_VERSION);
    }

    #[test]
    fn an_update_before_a_snapshot_is_refused() {
        let mut tracker = SequenceTracker::new();
        assert_eq!(
            tracker.accept_update(Sequence(1)),
            Err(StreamError::NoSnapshotYet)
        );
        assert_eq!(tracker.position(), None);
    }

    #[test]
    fn updates_in_order_are_applied() {
        let mut tracker = SequenceTracker::new();
        tracker.accept_snapshot(&snapshot(10));
        for sequence in 11..=20 {
            assert_eq!(tracker.accept_update(Sequence(sequence)), Ok(()));
        }
        assert_eq!(tracker.position(), Some(Sequence(20)));
    }

    #[test]
    fn a_gap_is_detected_and_named() {
        let mut tracker = SequenceTracker::new();
        tracker.accept_snapshot(&snapshot(10));
        assert_eq!(tracker.accept_update(Sequence(11)), Ok(()));

        let error = tracker
            .accept_update(Sequence(14))
            .expect_err("missed 12 and 13");
        assert_eq!(
            error,
            StreamError::Gap {
                expected: 12,
                received: 14
            }
        );
        assert!(error.to_string().contains("resynchronise"));

        // The tracker does not advance past a gap: everything after it is
        // equally unapplicable until a snapshot arrives.
        assert_eq!(tracker.position(), Some(Sequence(11)));
    }

    #[test]
    fn a_snapshot_after_a_gap_restores_the_stream() {
        let mut tracker = SequenceTracker::new();
        tracker.accept_snapshot(&snapshot(10));
        assert!(tracker.accept_update(Sequence(20)).is_err());

        tracker.accept_snapshot(&snapshot(25));
        assert_eq!(tracker.accept_update(Sequence(26)), Ok(()));
    }

    #[test]
    fn a_replayed_update_is_ignored_rather_than_treated_as_a_gap() {
        let mut tracker = SequenceTracker::new();
        tracker.accept_snapshot(&snapshot(10));
        assert_eq!(tracker.accept_update(Sequence(11)), Ok(()));
        assert_eq!(
            tracker.accept_update(Sequence(11)),
            Ok(()),
            "a replay is harmless"
        );
        assert_eq!(tracker.accept_update(Sequence(5)), Ok(()));
        assert_eq!(tracker.position(), Some(Sequence(11)));
    }

    #[test]
    fn a_reconnect_requires_a_fresh_snapshot() {
        let mut tracker = SequenceTracker::new();
        tracker.accept_snapshot(&snapshot(10));
        tracker.reset();
        assert_eq!(
            tracker.accept_update(Sequence(11)),
            Err(StreamError::NoSnapshotYet)
        );
    }

    #[test]
    fn sequences_saturate_rather_than_wrapping() {
        // Wrapping would turn a gap into an apparent replay, which is the one
        // failure mode this whole mechanism exists to catch.
        assert_eq!(Sequence(u64::MAX).next(), Sequence(u64::MAX));
        assert_eq!(Sequence::START.next(), Sequence(1));
    }

    #[test]
    fn messages_round_trip_through_json() {
        let messages = vec![
            ServerMessage::Snapshot(Box::new(snapshot(1))),
            ServerMessage::Update {
                sequence: Sequence(2),
                change: StateChange::MasterChanged {
                    master: MasterLive::default(),
                },
            },
            ServerMessage::Fault {
                code: "output_missing".to_owned(),
                message: "DP-1 did not come back".to_owned(),
            },
            ServerMessage::Pong { nonce: 7 },
        ];
        for message in messages {
            let json = serde_json::to_string(&message).expect("serialises");
            let back: ServerMessage = serde_json::from_str(&json).expect("deserialises");
            assert_eq!(back, message);
        }
    }

    #[test]
    fn client_messages_round_trip_through_json() {
        let messages = vec![
            ClientMessage::Resynchronise,
            ClientMessage::Ping { nonce: 42 },
        ];
        for message in messages {
            let json = serde_json::to_string(&message).expect("serialises");
            let back: ClientMessage = serde_json::from_str(&json).expect("deserialises");
            assert_eq!(back, message);
        }
    }

    #[test]
    fn the_socket_carries_no_commands() {
        // The socket carries state outwards and nothing inwards but a request
        // to resend it. A command here would be one without a bearer token, an
        // answer or a status code, and a client author who found one in this
        // enum would reasonably expect it to work.
        for message in [
            ClientMessage::Resynchronise,
            ClientMessage::Ping { nonce: 1 },
        ] {
            let json = serde_json::to_string(&message).expect("serialises");
            for command in ["select", "slot", "load", "show", "master", "crossfader"] {
                assert!(
                    !json.contains(command),
                    "{json} looks like a command; commands go over HTTP"
                );
            }
        }
    }
}
