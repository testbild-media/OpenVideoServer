//! The realtime socket, driven over a real TCP connection.
//!
//! The unit tests cover the pieces. This covers the thing that actually matters
//! and that only shows up end to end: that an unauthenticated client cannot
//! reach show state, and that a snapshot always precedes the first update.

use futures_util::{SinkExt, StreamExt};
use ovs_core::{router, AppState};
use ovs_db::{Account, Database};
use ovs_proto::realtime::{ClientHello, ClientMessage, ServerHello, ServerMessage};
use ovs_types::config::{Layer, Show, Slot};
use ovs_types::id::{AccountId, MediaItemId};
use ovs_types::time::Timestamp;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Starts the server on an ephemeral port and returns its address plus state.
async fn serve() -> (String, AppState) {
    let mut database = Database::open_in_memory().expect("opens");
    database
        .upsert_account(&Account {
            id: AccountId::new(),
            username: "operator".to_owned(),
            password_hash: "$argon2id$placeholder".to_owned(),
            role: ovs_types::account::Role::Admin,
            created_at: Timestamp::now(),
            last_login_at: None,
        })
        .expect("creates");

    let state = AppState::new(database, "0.0.1-test");
    let app = router(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let address = listener.local_addr().expect("has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (format!("ws://{address}/realtime"), state)
}

async fn connect(
    url: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("connects");
    socket
}

async fn next_message<S>(socket: &mut S) -> serde_json::Value
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let message = socket.next().await.expect("a message").expect("no error");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).expect("valid JSON");
        }
    }
}

/// The next message that is part of the sequenced stream.
///
/// Telemetry, the NDI source list and the connector list are sent outside it:
/// each one carries everything it has to say, so a client that misses one loses
/// nothing and needs no resynchronisation. A test asserting what follows a
/// snapshot means the next *sequenced* message, so it skips those — which is
/// exactly what the client does, and what stops a telemetry tick landing
/// mid-test from failing an assertion about ordering.
async fn next_sequenced<S>(socket: &mut S) -> ServerMessage
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let message: ServerMessage =
            serde_json::from_value(next_message(socket).await).expect("message");
        if !matches!(
            message,
            ServerMessage::Telemetry(_)
                | ServerMessage::NdiSources { .. }
                | ServerMessage::Connectors { .. }
        ) {
            return message;
        }
    }
}

fn show() -> Show {
    let mut show = Show::new("Show");
    let mut layer = Layer::new("Layer 1", 0);
    layer.slots.push(Slot::media(1, MediaItemId::new()));
    layer.slots.push(Slot::media(3, MediaItemId::new()));
    show.layers.push(layer);
    show
}

fn sign_in(state: &AppState) -> String {
    state
        .with_sessions(|sessions| sessions.sign_in(AccountId::new(), Timestamp::now()))
        .expect("issues")
        .token
}

#[tokio::test]
async fn an_unauthenticated_client_never_reaches_show_state() {
    let (url, state) = serve().await;
    state.mutate_state(|current| {
        let change = current.load_show(show(), None);
        ((), Some(change))
    });

    let mut socket = connect(&url).await;
    let hello = ClientHello::current("not a real token", "Intruder", "0.0.1");
    socket
        .send(Message::Text(
            serde_json::to_string(&hello).expect("serialises").into(),
        ))
        .await
        .expect("sends");

    let reply: ServerHello =
        serde_json::from_value(next_message(&mut socket).await).expect("hello");
    assert_eq!(reply, ServerHello::Unauthenticated);

    // Whatever happens next, no show state may follow. The connection ending
    // and the connection ending untidily are both fine; a snapshot is not.
    while let Some(Ok(message)) = socket.next().await {
        if let Message::Text(text) = message {
            panic!("a rejected client received data: {text}");
        }
    }
}

#[tokio::test]
async fn an_outdated_client_is_told_to_update_rather_than_to_sign_in() {
    let (url, state) = serve().await;
    let token = sign_in(&state);

    let mut socket = connect(&url).await;
    let hello = ClientHello {
        protocol_version: 999,
        ..ClientHello::current(token, "Old", "0.0.1")
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&hello).expect("serialises").into(),
        ))
        .await
        .expect("sends");

    let reply: ServerHello =
        serde_json::from_value(next_message(&mut socket).await).expect("hello");
    let ServerHello::IncompatibleProtocol { server_version, .. } = reply else {
        panic!("a valid token with a bad version must fail on the version, got {reply:?}");
    };
    assert_eq!(server_version, ovs_types::REALTIME_PROTOCOL_VERSION);
}

#[tokio::test]
async fn an_authenticated_client_receives_a_snapshot_before_any_update() {
    let (url, state) = serve().await;
    let token = sign_in(&state);
    let show = show();
    let layer_id = show.layers[0].id;
    state.mutate_state(|current| {
        let change = current.load_show(show, None);
        ((), Some(change))
    });

    let mut socket = connect(&url).await;
    socket
        .send(Message::Text(
            serde_json::to_string(&ClientHello::current(token, "OVS Desktop", "0.0.1"))
                .expect("serialises")
                .into(),
        ))
        .await
        .expect("sends");

    let hello: ServerHello =
        serde_json::from_value(next_message(&mut socket).await).expect("hello");
    assert!(matches!(hello, ServerHello::Accepted { .. }));

    // The very next message is the snapshot, before anything else.
    let first: ServerMessage =
        serde_json::from_value(next_message(&mut socket).await).expect("message");
    let ServerMessage::Snapshot(snapshot) = first else {
        panic!("the first message after the handshake must be a snapshot, got {first:?}");
    };
    assert_eq!(
        snapshot.show.as_ref().map(|show| show.name.as_str()),
        Some("Show")
    );

    // Now a change on the server reaches this client, in sequence.
    state.mutate_state(|current| {
        let change = current.select_slot(layer_id, 3).expect("valid");
        ((), change)
    });

    let next = next_sequenced(&mut socket).await;
    let ServerMessage::Update { sequence, .. } = next else {
        panic!("expected an update, got {next:?}");
    };
    assert_eq!(
        sequence,
        snapshot.sequence.next(),
        "the first update must follow directly on from the snapshot"
    );
}

#[tokio::test]
async fn a_client_joining_a_running_rig_is_told_what_is_on_the_network() {
    // The list is broadcast when it changes and not on a tick, so a client that
    // connects between two changes has to be told on arrival or it will show an
    // empty input browser for the rest of the evening. On a rig that was set up
    // before anybody opened a client — which is every rig — that is the only
    // time it would ever be told.
    let (url, state) = serve().await;
    let token = sign_in(&state);
    state.set_ndi_sources(vec![
        "JK-GAMING-02 (Test Pattern)".to_owned(),
        "STUDIO-1 (Program)".to_owned(),
    ]);

    let mut socket = connect(&url).await;
    let hello = ClientHello::current(token, "Console", "0.0.1");
    socket
        .send(Message::Text(
            serde_json::to_string(&hello).expect("serialises").into(),
        ))
        .await
        .expect("sends");
    let reply: ServerHello =
        serde_json::from_value(next_message(&mut socket).await).expect("hello");
    assert!(
        matches!(reply, ServerHello::Accepted { .. }),
        "got {reply:?}"
    );

    // After the snapshot, which is the protocol's promise and not negotiable.
    let first: ServerMessage =
        serde_json::from_value(next_message(&mut socket).await).expect("message");
    assert!(
        matches!(first, ServerMessage::Snapshot(_)),
        "the snapshot must come first, got {first:?}"
    );

    let second: ServerMessage =
        serde_json::from_value(next_message(&mut socket).await).expect("message");
    let ServerMessage::NdiSources { sources } = second else {
        panic!("expected the network sources, got {second:?}");
    };
    assert_eq!(
        sources,
        vec![
            "JK-GAMING-02 (Test Pattern)".to_owned(),
            "STUDIO-1 (Program)".to_owned()
        ]
    );
}

#[tokio::test]
async fn a_sender_switched_on_reaches_a_client_already_connected() {
    let (url, state) = serve().await;
    let token = sign_in(&state);

    let mut socket = connect(&url).await;
    let hello = ClientHello::current(token, "Console", "0.0.1");
    socket
        .send(Message::Text(
            serde_json::to_string(&hello).expect("serialises").into(),
        ))
        .await
        .expect("sends");
    let _hello: ServerHello =
        serde_json::from_value(next_message(&mut socket).await).expect("hello");

    // The snapshot, then the empty list this rig starts with. Empty is an
    // answer and is sent: a client told only about non-empty lists cannot tell
    // "nothing is sending" from "nobody has looked".
    let snapshot: ServerMessage =
        serde_json::from_value(next_message(&mut socket).await).expect("message");
    assert!(matches!(snapshot, ServerMessage::Snapshot(_)));
    // Looked for rather than counted: several lists are sent after a snapshot,
    // and a test that counts them breaks whenever another is added — which is
    // exactly what happened to this one.
    let mut empty = false;
    for _ in 0..4 {
        let message: ServerMessage =
            serde_json::from_value(next_message(&mut socket).await).expect("message");
        if message
            == (ServerMessage::NdiSources {
                sources: Vec::new(),
            })
        {
            empty = true;
            break;
        }
    }
    assert!(empty, "an empty list is an answer and is sent");

    state.set_ndi_sources(vec!["STUDIO-1 (Camera 2)".to_owned()]);

    // Looked for, for the same reason as above: what follows the empty list is
    // the connector list, and a read that took the next message would get that.
    let mut told = false;
    for _ in 0..4 {
        let message: ServerMessage =
            serde_json::from_value(next_message(&mut socket).await).expect("message");
        if message
            == (ServerMessage::NdiSources {
                sources: vec!["STUDIO-1 (Camera 2)".to_owned()],
            })
        {
            told = true;
            break;
        }
    }
    assert!(
        told,
        "a sender switched on must reach a client already connected"
    );
}

#[tokio::test]
async fn a_client_joining_is_told_what_the_machine_is_wired_to() {
    // The list is sent when it moves, so a client that connects between two
    // changes has to be told on arrival — otherwise its chooser describes
    // whatever was plugged in when the renderer started, which is the fault this
    // whole path exists to stop (owner, 2026-09-25: "es gibt keinen refresh").
    let (url, state) = serve().await;
    let token = sign_in(&state);
    state.set_connectors(vec![ovs_ipc::DiscoveredConnector {
        gpu_pci_address: "0000:01:00.0".to_owned(),
        connector: "HDMI-0".to_owned(),
        connected: true,
        model: Some("Sony LMD-2110W".to_owned()),
        modes: vec![ovs_ipc::ConnectorMode {
            size: ovs_types::geometry::SizePx::new(1920, 1080),
            refresh_millihertz: 59940,
        }],
    }]);

    let mut socket = connect(&url).await;
    let hello = ClientHello::current(token, "Console", "0.0.1");
    socket
        .send(Message::Text(
            serde_json::to_string(&hello).expect("serialises").into(),
        ))
        .await
        .expect("sends");
    let _hello: ServerHello =
        serde_json::from_value(next_message(&mut socket).await).expect("hello");

    // Snapshot, then the two lists that are sent when they move.
    let mut connectors = None;
    for _ in 0..4 {
        let message: ServerMessage =
            serde_json::from_value(next_message(&mut socket).await).expect("message");
        if let ServerMessage::Connectors { connectors: found } = message {
            connectors = Some(found);
            break;
        }
    }
    let connectors = connectors.expect("a joining client is told the connectors");
    assert_eq!(connectors.len(), 1);
    assert_eq!(connectors[0].connector, "HDMI-0");
    assert_eq!(
        connectors[0].modes[0].refresh_millihertz, 59940,
        "the modes come with it, which is the point"
    );
}

#[tokio::test]
async fn a_monitor_changed_reaches_a_client_already_connected() {
    let (url, state) = serve().await;
    let token = sign_in(&state);

    let mut socket = connect(&url).await;
    let hello = ClientHello::current(token, "Console", "0.0.1");
    socket
        .send(Message::Text(
            serde_json::to_string(&hello).expect("serialises").into(),
        ))
        .await
        .expect("sends");
    let _hello: ServerHello =
        serde_json::from_value(next_message(&mut socket).await).expect("hello");
    // The snapshot and the two empty lists this rig starts with.
    for _ in 0..3 {
        let _each: ServerMessage =
            serde_json::from_value(next_message(&mut socket).await).expect("message");
    }

    state.set_connectors(vec![ovs_ipc::DiscoveredConnector {
        gpu_pci_address: "0000:01:00.0".to_owned(),
        connector: "HDMI-0".to_owned(),
        connected: true,
        model: Some("Sony LMD-2110W".to_owned()),
        modes: vec![ovs_ipc::ConnectorMode {
            size: ovs_types::geometry::SizePx::new(1920, 1080),
            refresh_millihertz: 59940,
        }],
    }]);

    let told: ServerMessage =
        serde_json::from_value(next_message(&mut socket).await).expect("message");
    let ServerMessage::Connectors { connectors } = told else {
        panic!("expected the connectors, got {told:?}");
    };
    assert_eq!(connectors[0].model.as_deref(), Some("Sony LMD-2110W"));
}

#[tokio::test]
async fn a_client_can_ask_for_a_fresh_snapshot() {
    let (url, state) = serve().await;
    let token = sign_in(&state);

    let mut socket = connect(&url).await;
    socket
        .send(Message::Text(
            serde_json::to_string(&ClientHello::current(token, "OVS Desktop", "0.0.1"))
                .expect("serialises")
                .into(),
        ))
        .await
        .expect("sends");
    let _hello = next_message(&mut socket).await;
    let _snapshot = next_message(&mut socket).await;

    socket
        .send(Message::Text(
            serde_json::to_string(&ClientMessage::Resynchronise)
                .expect("serialises")
                .into(),
        ))
        .await
        .expect("sends");

    let message = next_sequenced(&mut socket).await;
    assert!(
        matches!(message, ServerMessage::Snapshot(_)),
        "resynchronising must produce a snapshot, got {message:?}"
    );
}

#[tokio::test]
async fn the_socket_is_not_a_second_command_channel() {
    // Commands carry a bearer token and produce a status code, so they go over
    // HTTP. Accepting them here would be a second, weaker authorisation path.
    let (url, state) = serve().await;
    let token = sign_in(&state);
    let show = show();
    let layer_id = show.layers[0].id;
    state.mutate_state(|current| {
        let change = current.load_show(show, None);
        ((), Some(change))
    });

    let mut socket = connect(&url).await;
    socket
        .send(Message::Text(
            serde_json::to_string(&ClientHello::current(token, "OVS Desktop", "0.0.1"))
                .expect("serialises")
                .into(),
        ))
        .await
        .expect("sends");
    let _hello = next_message(&mut socket).await;
    let _snapshot = next_message(&mut socket).await;

    // What a client would send if it believed the socket took commands.
    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "select_slot",
                "layer_id": layer_id.to_string(),
                "slot_index": 3,
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("sends");

    let message = next_sequenced(&mut socket).await;
    let ServerMessage::Fault { code, .. } = message else {
        panic!("expected a fault, got {message:?}");
    };
    assert_eq!(code, "wrong_channel");
    assert_eq!(
        state.with_state(|current| current.layer(layer_id).map(|live| live.active_slot)),
        Some(0),
        "the layer must not have changed"
    );
}
