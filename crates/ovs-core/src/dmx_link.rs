//! The core's end of DMX-over-IP.
//!
//! Sockets fill universes on their own threads; this thread decides what those
//! channels mean and is the only one that changes the show. Everything it
//! applies goes through [`crate::parameters::apply`], the same door the HTTP API
//! uses, so a console and a client can never disagree about what a value means.
//!
//! # Polling, not waking
//!
//! A console sends up to forty-four frames a second and re-sends every channel
//! in each one. Applying them as they land would put the show's write lock in
//! the path of a network interrupt and change the state thousands of times a
//! second for a handful of real movements. Instead the universes are read on a
//! fixed tick and only what changed comes out, which bounds the work per second
//! whatever the wire is doing and costs at most one tick of latency — a
//! hundredth of a second, against a fader an operator is moving by hand.
//!
//! # The operator's switch takes the whole show
//!
//! While the show's DMX switch is off, nothing is read into the show at all and
//! the mapper is emptied. Both halves matter. Dropping the updates alone would
//! still leave the mapper holding values it never applied, so switching the
//! console back on would change nothing until a desk moved a fader it was
//! already holding — and an operator who has just switched DMX back on is
//! asking for the console to take the show, now.
//!
//! The sockets stay open and the telemetry keeps counting throughout: "I
//! switched it off" has to stay distinguishable from "the cable is out".
//!
//! # Nothing here writes configuration
//!
//! A console driving a dimmer at 44 Hz must not slowly rewrite what tomorrow's
//! show opens with, so this reaches live state only. The patch it
//! reads is configuration, and it is read, never written.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ovs_dmx::mapping::{Mapper, PatchProblem};
use ovs_dmx::receiver::{Receiver, ReceiverConfig};
use ovs_types::config::{DmxProtocols, FixtureInstance};
use ovs_types::id::ShowId;

use crate::server::AppState;

/// How often the universes are read.
///
/// A hundred times a second, against a wire that carries forty-four: fast
/// enough that the tick is never what an operator is waiting for, slow enough
/// that an idle show is doing nothing measurable.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How often what is arriving is published for the Service Console.
///
/// The console asks once a second and works the packet rate out from the
/// difference, so anything faster than this is measured and thrown away.
const TELEMETRY_INTERVAL: Duration = Duration::from_millis(500);

/// How often the patch, and which protocols reach the show, are re-read.
///
/// Both are configuration: they change when somebody edits them, not while a
/// show runs. Re-reading the patch every tick would take the state lock a
/// hundred times a second and clone every fixture in the rig to find out
/// nothing had changed; re-reading the protocols every tick would put a SQLite
/// query and the database's own lock in a hundred-hertz loop, in front of every
/// request that needs them. Half a second late is invisible for an edit and
/// free for a show.
const PATCH_INTERVAL: Duration = Duration::from_millis(500);

/// Receives DMX and applies it to the show until `running` is cleared.
///
/// Returns immediately, having logged, if the sockets cannot be opened. A media
/// server with no DMX is still a media server; one that refused to start could
/// not be fixed from the client.
pub fn run(state: &AppState, config: &ReceiverConfig, running: &AtomicBool) {
    let receiver = match Receiver::start(config) {
        Ok(receiver) => receiver,
        Err(error) => {
            tracing::error!("DMX receive is not available: {error}");
            return;
        }
    };
    tracing::info!(
        artnet = ?receiver.artnet_port(),
        sacn = ?receiver.sacn_port(),
        "DMX receive listening"
    );

    let mut mapper = Mapper::new();
    let mut patch: Vec<FixtureInstance> = Vec::new();
    let mut show: Option<ShowId> = None;
    let mut patch_read_at = Instant::now() - PATCH_INTERVAL;
    let mut published_at = Instant::now() - TELEMETRY_INTERVAL;
    let mut protocols = DmxProtocols::default();

    while running.load(Ordering::Relaxed) {
        if patch_read_at.elapsed() >= PATCH_INTERVAL {
            patch_read_at = Instant::now();
            refresh_patch(state, &receiver, &mut mapper, &mut patch, &mut show);
            protocols = state
                .with_database(|database| database.dmx_protocols())
                .unwrap_or_default();
        }

        if published_at.elapsed() >= TELEMETRY_INTERVAL {
            published_at = Instant::now();
            state.set_dmx_telemetry(Some(receiver.telemetry()));
        }

        // The operator's own switch (owner, 2026-09-23), read before anything
        // is collected rather than as each update is applied. Off means the
        // show is theirs: nothing on any wire reaches it, and the mapper is
        // emptied so that switching the console back on gives it the show at
        // once instead of waiting for a desk to move a fader it is already
        // holding.
        let driving = state.with_state(|current| current.master().dmx_enabled);
        if !driving {
            mapper.forget();
        }

        let collected = receiver.read(|universes| {
            for universe in universes.expire(Instant::now()) {
                // Worth one line: a console that stopped sending is either a
                // cable somebody pulled or a desk somebody switched off, and
                // the show is now holding its last look either way.
                tracing::info!(universe, "no source is sending this universe any more");
            }
            // Nothing patched, or nothing allowed through, still means the
            // universes are read: the status has to tell the truth about what
            // is on the wire before anybody has patched it and after somebody
            // has switched it off.
            (driving && !patch.is_empty()).then(|| mapper.collect(&patch, universes, protocols))
        });

        if let Some(collected) = collected {
            for problem in &collected.problems {
                report(problem);
            }
            for update in collected.updates {
                state.mutate_state(|current| {
                    let change = crate::parameters::apply(current, update.parameter, update.value);
                    ((), change)
                });
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    receiver.stop();
    // Absent again, because absent means "not listening" and this no longer is.
    state.set_dmx_telemetry(None);
    tracing::info!("DMX receive stopped");
}

/// Re-reads the patch, and tells the receiver which universes now matter.
fn refresh_patch(
    state: &AppState,
    receiver: &Receiver,
    mapper: &mut Mapper,
    patch: &mut Vec<FixtureInstance>,
    show: &mut Option<ShowId>,
) {
    let (current_show, current_patch) = state.with_state(|current| {
        (
            current.show_id(),
            current
                .show()
                .map(|show| show.fixture_patch.clone())
                .unwrap_or_default(),
        )
    });

    if *show != current_show {
        // A different show. What the console is holding has to be applied to it
        // from scratch, because none of it has been applied to *this* show and
        // nothing on the wire is going to change to say so.
        *show = current_show;
        mapper.forget();
    }

    if *patch == current_patch {
        return;
    }

    *patch = current_patch;

    // A patch edit can move a fixture's address, so what was read for it last
    // time is not about the same channels any more.
    mapper.forget();

    let mut universes: Vec<u16> = patch
        .iter()
        .filter(|fixture| fixture.enabled)
        .map(|fixture| fixture.universe)
        .collect();
    universes.sort_unstable();
    universes.dedup();
    receiver.listen_to(&universes);
}

/// Says once what is wrong with a patch entry.
fn report(problem: &PatchProblem) {
    match problem {
        PatchProblem::UnknownDefinition { fixture, type_id } => tracing::warn!(
            %fixture,
            type_id,
            "this build does not publish that fixture personality, so the fixture is not being read"
        ),
        PatchProblem::FootprintDisagrees {
            fixture,
            patched,
            published,
        } => tracing::warn!(
            %fixture,
            patched,
            published,
            "the patch and the personality disagree about the channel count, so every fixture patched after it is offset"
        ),
        PatchProblem::WrongTarget { fixture, type_id } => tracing::warn!(
            %fixture,
            type_id,
            "that personality cannot drive what this patch entry points at"
        ),
        PatchProblem::Truncated {
            fixture,
            universe,
            needed,
            received,
        } => tracing::warn!(
            %fixture,
            universe,
            needed,
            received,
            "the universe is shorter than the fixture needs, and the missing channels are not zeroes"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use ovs_db::Database;
    use ovs_dmx::sacn::{Cid, DataPacket, DATA_LOSS_TIMEOUT};
    use ovs_dmx::universe::Universes;
    use ovs_types::config::patch::FixtureTarget;
    use ovs_types::config::{FixtureInstance, Layer, Show, Slot, LAYER_V1, MASTER_V1};
    use ovs_types::id::{FixtureInstanceId, LayerId, MediaItemId};

    use super::*;
    use crate::state::AuthoritativeState;

    /// A show as it was measured: one layer showing slot 1, opaque, untouched.
    fn a_show_showing_slot_one() -> (AuthoritativeState, LayerId) {
        let mut state = AuthoritativeState::new();
        let mut show = Show::new("Wall");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        // A second slot, unselected: a desk moving the layer to a slot it is
        // already on proves nothing about whether the channel arrived.
        layer.slots.push(Slot::media(2, MediaItemId::new()));
        let layer_id = layer.id;
        show.layers.push(layer);
        state.load_show(show, None);
        state
            .select_slot(layer_id, 1)
            .expect("the layer has slot 1");
        (state, layer_id)
    }

    /// The layer fixture an operator would patch: universe 1, address 1.
    fn a_layer_fixture(layer_id: LayerId) -> FixtureInstance {
        FixtureInstance {
            id: FixtureInstanceId::new(),
            type_id: LAYER_V1.type_id.to_owned(),
            version: LAYER_V1.version,
            target: FixtureTarget::Layer { layer_id },
            universe: 1,
            start_address: 1,
            footprint: LAYER_V1.footprint,
            enabled: true,
        }
    }

    /// One full universe as a desk sends it: 512 channels, most of them zero.
    fn a_full_universe(set: &[(usize, u8)]) -> Vec<u8> {
        let mut channels = vec![0_u8; 512];
        for (channel, value) in set {
            channels[channel - 1] = *value;
        }
        channels
    }

    /// Offers one sACN frame to the universes, the way a reader thread does.
    fn desk_sends(universes: &mut Universes, channels: &[u8], sequence: u8, now: Instant) {
        universes.accept_sacn(
            &DataPacket {
                cid: Cid([7; 16]),
                source_name: "a desk".to_owned(),
                universe: 1,
                priority: 100,
                sequence,
                terminated: false,
                preview: false,
                channels,
            },
            now,
        );
    }

    /// One turn of the link's own loop, without the sockets it normally has.
    fn one_pass(
        mapper: &mut Mapper,
        patch: &[FixtureInstance],
        universes: &Universes,
        state: &mut AuthoritativeState,
    ) {
        for update in mapper
            .collect(patch, universes, ovs_types::config::DmxProtocols::default())
            .updates
        {
            crate::parameters::apply(state, update.parameter, update.value);
        }
    }

    /// A universe that a desk was on earlier in the day and has since left.
    ///
    /// The channels are still there — loss is a hold, not a blackout — and
    /// nothing is driving them.
    fn a_universe_a_desk_has_left() -> Universes {
        let mut universes = Universes::new();
        let start = Instant::now();
        desk_sends(&mut universes, &a_full_universe(&[]), 1, start);
        universes.expire(start + DATA_LOSS_TIMEOUT + Duration::from_millis(1));
        universes
    }

    #[test]
    fn patching_a_layer_fixture_with_no_console_on_the_universe_leaves_the_show_alone() {
        // Four in the afternoon, nothing in the building sending anything, and
        // somebody saves a patch. The universe is still holding a frame from a
        // desk that has gone, and that frame is zeroes: no opacity, the black
        // slot, no scale. Taking it would put the wall out.
        let (mut state, layer_id) = a_show_showing_slot_one();
        let patch = vec![a_layer_fixture(layer_id)];
        let before = *state.layer(layer_id).expect("live");

        one_pass(
            &mut Mapper::new(),
            &patch,
            &a_universe_a_desk_has_left(),
            &mut state,
        );

        assert_eq!(
            state.layer(layer_id).expect("live"),
            &before,
            "a patch is not a control source"
        );
    }

    #[test]
    fn a_master_fixture_patched_with_no_console_on_the_universe_does_not_black_the_show_out() {
        // The same rule, and the loudest case of it: the held frame's dimmer is
        // zero, and adopting it would take every output down at once.
        let (mut state, _) = a_show_showing_slot_one();
        let patch = vec![FixtureInstance {
            id: FixtureInstanceId::new(),
            type_id: MASTER_V1.type_id.to_owned(),
            version: MASTER_V1.version,
            target: FixtureTarget::Master,
            universe: 1,
            start_address: 1,
            footprint: MASTER_V1.footprint,
            enabled: true,
        }];

        one_pass(
            &mut Mapper::new(),
            &patch,
            &a_universe_a_desk_has_left(),
            &mut state,
        );

        assert!(state.master().dimmer >= 0.999, "the show is still up");
    }

    /// A fixture's home values, as the bytes a console would send.
    ///
    /// What a desk's home or default preset contains. Built from the published
    /// definition rather than typed out, because a channel added to a
    /// personality that this forgot would be sent as zero — which is exactly
    /// the failure both tests below are about.
    fn homed(definition: &ovs_types::config::fixtures::FixtureDefinition) -> Vec<(usize, u8)> {
        let mut sent = Vec::new();
        for channel in definition.channels {
            let first = channel.offset as usize;
            if channel.encoding.width() == 2 {
                sent.push((first, (channel.home >> 8) as u8));
                sent.push((first + 1, (channel.home & 0xFF) as u8));
            } else {
                sent.push((first, channel.home as u8));
            }
        }
        sent
    }

    #[test]
    fn a_console_driving_only_the_dimmer_and_the_slot_leaves_the_layer_scaled_to_nothing() {
        // Measured on the real server, and then decided rather than fixed. A
        // desk sends a *whole* universe, so the channels it has not programmed
        // arrive as zeroes, and on this personality a scale of zero is a scale
        // of zero. The owner refused making zero mean neutral
        // (owner decision Q15): a patched fixture belongs to the console and
        // this server does not guess whether a zero was meant.
        //
        // So this is not a defect, it is the contract, and it is worth a test
        // of its own because the obliging fix is the one somebody will reach
        // for next time it is reported.
        let (mut state, layer_id) = a_show_showing_slot_one();
        let patch = vec![a_layer_fixture(layer_id)];
        let mut mapper = Mapper::new();
        let mut universes = a_universe_a_desk_has_left();

        one_pass(&mut mapper, &patch, &universes, &mut state);

        // Channel 1 is the layer dimmer, channel 2 the slot. Nothing else.
        desk_sends(
            &mut universes,
            &a_full_universe(&[(1, 255), (2, 8)]),
            2,
            Instant::now(),
        );
        one_pass(&mut mapper, &patch, &universes, &mut state);

        let live = state.layer(layer_id).expect("live");
        assert_eq!(live.active_slot, 2, "the slot the desk asked for");
        assert!(live.opacity >= 0.999, "at the opacity it asked for");
        assert!(
            live.transform.scale_x == 0.0 && live.transform.scale_y == 0.0,
            "and scaled to nothing, because that is what was sent: {:?}",
            live.transform
        );
    }

    #[test]
    fn a_console_sending_the_published_homes_puts_the_layer_back_on_the_wall() {
        // The other half of Q15's answer, and the half that makes the first one
        // workable: every channel carries a home, published in the definition,
        // so a desk can put a freshly patched fixture somewhere sensible
        // without anybody redefining what zero means.
        let (mut state, layer_id) = a_show_showing_slot_one();
        let patch = vec![a_layer_fixture(layer_id)];
        let mut mapper = Mapper::new();
        let mut universes = a_universe_a_desk_has_left();

        one_pass(&mut mapper, &patch, &universes, &mut state);

        // The homes, and then the slot on top of them: homing leaves a layer on
        // Black, which is the point of homing.
        let mut sending = homed(&LAYER_V1);
        sending.push((2, 8));
        desk_sends(
            &mut universes,
            &a_full_universe(&sending),
            2,
            Instant::now(),
        );
        one_pass(&mut mapper, &patch, &universes, &mut state);

        let live = state.layer(layer_id).expect("live");
        assert_eq!(live.active_slot, 2);
        assert!(live.opacity >= 0.999, "the dimmer homes to full");
        assert!(
            (live.transform.scale_x - 1.0).abs() < 0.001
                && (live.transform.scale_y - 1.0).abs() < 0.001,
            "at its normal size: {:?}",
            live.transform
        );
        // In pixels, because that is what the transform holds: a sixteen-bit
        // channel cannot land exactly on the centre of a 1920-wide canvas, and
        // a thirtieth of a pixel is the whole of the error.
        assert!(
            live.transform.x.abs() < 1.0 && live.transform.y.abs() < 1.0,
            "in the middle: {:?}",
            live.transform
        );
        assert!(
            live.transform.rotation_deg.abs() < 0.01,
            "and upright: {:?}",
            live.transform
        );
    }

    /// Builds an ArtDMX packet the way a console would.
    fn artnet_packet(port_address: u16, sequence: u8, channels: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"Art-Net\0");
        bytes.extend_from_slice(&0x5000_u16.to_le_bytes());
        bytes.extend_from_slice(&14_u16.to_be_bytes());
        bytes.push(sequence);
        bytes.push(0);
        bytes.push((port_address & 0xFF) as u8);
        bytes.push((port_address >> 8) as u8);
        bytes.extend_from_slice(&(channels.len() as u16).to_be_bytes());
        bytes.extend_from_slice(channels);
        bytes
    }

    fn state_with_a_master_fixture() -> AppState {
        let database = Database::open_in_memory().expect("database");
        let state = AppState::new(database, "test");

        let mut show = Show::new("Patched");
        show.fixture_patch.push(FixtureInstance {
            id: FixtureInstanceId::new(),
            type_id: MASTER_V1.type_id.to_owned(),
            version: MASTER_V1.version,
            target: FixtureTarget::Master,
            universe: 1,
            start_address: 1,
            footprint: MASTER_V1.footprint,
            enabled: true,
        });

        state.mutate_state(|current| {
            let change = current.load_show(show, None);
            ((), Some(change))
        });
        state
    }

    #[test]
    fn a_console_on_the_wire_moves_the_master_dimmer() {
        // The whole path in one test: a datagram on a real socket, through the
        // universes, through the patch, into the show's live state.
        let state = state_with_a_master_fixture();
        let running = Arc::new(AtomicBool::new(true));

        // Ports the operating system picked, so the test never fights whatever
        // else on this machine is listening on 6454.
        let config = ReceiverConfig {
            sacn: false,
            artnet_port: 0,
            sacn_port: 0,
            ..ReceiverConfig::default()
        };

        // The port has to be known before the link owns the receiver, so the
        // link is given a receiver that has already bound.
        let probe = Receiver::start(&config).expect("sockets");
        let port = probe.artnet_port().expect("listening");
        drop(probe);

        let link_state = state.clone();
        let link_running = Arc::clone(&running);
        let link = std::thread::spawn(move || {
            run(
                &link_state,
                &ReceiverConfig {
                    artnet_port: port,
                    ..config
                },
                &link_running,
            );
        });

        let console = UdpSocket::bind("127.0.0.1:0").expect("a console");
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        // Rounded up to an even count: Art-Net carries no odd length, and the
        // master has been five channels since sync left it.
        let mut dimmer = vec![0_u8; usize::from(MASTER_V1.footprint).next_multiple_of(2)];
        dimmer[0] = 0xFF;
        dimmer[1] = 0xFF;

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut applied = false;
        let mut sequence = 1_u8;
        while Instant::now() < deadline && !applied {
            // Re-sent, because the link may not have opened its socket yet and
            // a console re-sends anyway.
            let _ = console.send_to(&artnet_packet(1, sequence, &dimmer), target);
            sequence = sequence.wrapping_add(1).max(1);
            std::thread::sleep(Duration::from_millis(20));
            applied = state.with_state(|current| current.master().dimmer >= 0.999);
        }

        // What the Service Console shows: the universe, who is driving it and
        // how much has arrived. Published on its own half-second cadence, so
        // this waits for it rather than assuming the two ticks lined up.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut telemetry = state.dmx_telemetry().unwrap_or_default();
        while Instant::now() < deadline && telemetry.artnet_packets == 0 {
            let _ = console.send_to(&artnet_packet(1, sequence, &dimmer), target);
            sequence = sequence.wrapping_add(1).max(1);
            std::thread::sleep(Duration::from_millis(50));
            telemetry = state.dmx_telemetry().unwrap_or_default();
        }

        running.store(false, Ordering::Relaxed);
        let _ = link.join();

        assert!(applied, "the console's dimmer reached the show");
        assert!(telemetry.artnet_packets > 0, "packets were counted");
        let universe = telemetry
            .universes
            .first()
            .expect("the universe the console is sending");
        assert_eq!(universe.universe, 1);
        assert!(universe.source.is_some(), "somebody is driving it");

        assert_eq!(
            state.dmx_telemetry(),
            None,
            "a stopped link is not a link hearing nothing"
        );
    }

    /// Starts the link on a port of its own, and returns the port it is on.
    ///
    /// The port has to be known before the link owns the receiver, so a probe
    /// binds first and is dropped. Ports the operating system picked, so a test
    /// never fights whatever else on this machine is listening on 6454.
    fn link_on_its_own_port(
        state: &AppState,
        running: &Arc<AtomicBool>,
    ) -> (u16, std::thread::JoinHandle<()>) {
        let config = ReceiverConfig {
            sacn: false,
            artnet_port: 0,
            sacn_port: 0,
            ..ReceiverConfig::default()
        };
        let probe = Receiver::start(&config).expect("sockets");
        let port = probe.artnet_port().expect("listening");
        drop(probe);

        let link_state = state.clone();
        let link_running = Arc::clone(running);
        let link = std::thread::spawn(move || {
            run(
                &link_state,
                &ReceiverConfig {
                    artnet_port: port,
                    ..config
                },
                &link_running,
            );
        });
        (port, link)
    }

    /// Waits for `settled` to hold, and says whether it ever did.
    ///
    /// The console re-sends throughout, because a desk re-sends and because the
    /// link may not have bound its socket yet.
    fn while_the_console_sends(
        console: &UdpSocket,
        target: SocketAddr,
        frame: &[u8],
        sequence: &mut u8,
        patience: Duration,
        settled: impl Fn() -> bool,
    ) -> bool {
        let deadline = Instant::now() + patience;
        while Instant::now() < deadline {
            let _ = console.send_to(&artnet_packet(1, *sequence, frame), target);
            *sequence = sequence.wrapping_add(1).max(1);
            std::thread::sleep(Duration::from_millis(20));
            if settled() {
                return true;
            }
        }
        false
    }

    #[test]
    fn with_dmx_switched_off_the_client_owns_the_show_and_switching_it_on_hands_it_back() {
        // The owner's report, 2026-09-23: "wenn DMX ausgeschaltet ist, sollte
        // man die komplette kontrolle wieder im Client/in der GUI haben."
        //
        // A console is on the wire holding the master dimmer at nothing, which
        // is what a desk that has not been programmed for this show sends. The
        // operator switches DMX off and brings the dimmer up by hand.
        let state = state_with_a_master_fixture();
        state.mutate_state(|current| {
            let change = current.update_master(|master| master.dmx_enabled = false);
            ((), change)
        });

        let running = Arc::new(AtomicBool::new(true));
        let (port, link) = link_on_its_own_port(&state, &running);

        let console = UdpSocket::bind("127.0.0.1:0").expect("a console");
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        // Every channel at zero: on this personality that is a blackout, and it
        // is exactly what the show must not take while the switch is off.
        // Even, as Art-Net requires.
        let blackout = vec![0_u8; usize::from(MASTER_V1.footprint).next_multiple_of(2)];
        let mut sequence = 1_u8;

        // Long enough for many passes of a ten-millisecond loop, and for the
        // half-second on which the patch is read.
        let taken = while_the_console_sends(
            &console,
            target,
            &blackout,
            &mut sequence,
            Duration::from_secs(2),
            || state.with_state(|current| current.master().dimmer < 0.999),
        );
        assert!(!taken, "a switched-off console must not reach the show");

        // By hand, the way the client does it, while the console is still
        // sending. This is the value that has to survive.
        state.mutate_state(|current| {
            let change = current.update_master(|master| master.set_dimmer(0.5));
            ((), change)
        });

        let moved = while_the_console_sends(
            &console,
            target,
            &blackout,
            &mut sequence,
            Duration::from_millis(600),
            || state.with_state(|current| (current.master().dimmer - 0.5).abs() > 0.001),
        );
        assert!(!moved, "the operator's own value stays where they put it");

        // And back. The console has not changed anything it is sending — it is
        // holding the same frame it has held all along — so a link that only
        // dropped the updates would leave the show at 0.5 until somebody
        // touched a fader out there. Switching DMX on means the console has
        // the show.
        state.mutate_state(|current| {
            let change = current.update_master(|master| master.dmx_enabled = true);
            ((), change)
        });

        let handed_over = while_the_console_sends(
            &console,
            target,
            &blackout,
            &mut sequence,
            Duration::from_secs(5),
            || state.with_state(|current| current.master().dimmer < 0.001),
        );

        running.store(false, Ordering::Relaxed);
        let _ = link.join();

        assert!(
            handed_over,
            "switching DMX on gives the console the show at once, without it moving"
        );
    }

    #[test]
    fn a_link_with_nothing_patched_still_starts_and_stops() {
        // A server with no DMX in its show still listens, because a patch can
        // be added while it runs. What it must not do is spin, take the state
        // lock, or hold the process open when it is asked to stop.
        let database = Database::open_in_memory().expect("database");
        let state = AppState::new(database, "test");
        let running = Arc::new(AtomicBool::new(true));

        let link_state = state.clone();
        let link_running = Arc::clone(&running);
        let link = std::thread::spawn(move || {
            run(
                &link_state,
                &ReceiverConfig {
                    artnet_port: 0,
                    sacn_port: 0,
                    ..ReceiverConfig::default()
                },
                &link_running,
            );
        });

        std::thread::sleep(Duration::from_millis(50));
        running.store(false, Ordering::Relaxed);
        link.join().expect("the link stopped when it was asked to");
    }
}
