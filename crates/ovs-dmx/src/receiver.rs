//! The sockets, and the threads that read them.
//!
//! Two UDP sockets, one thread each, both feeding the same [`Universes`]. A
//! reader thread does the least it possibly can: parse, hand to the universe
//! model, go back to the socket. Everything else — deciding what the channels
//! mean, changing the show — happens on whoever calls [`Receiver::read`], which
//! keeps the show's own state on the show's own thread and keeps this lock
//! short enough that a burst of packets cannot make the server late.
//!
//! # Ports are shared
//!
//! Both sockets are opened with address reuse. A venue's machine frequently has
//! a visualiser, a monitoring tool or a second media server on it, and a
//! receiver that took sole ownership of port 5568 would stop all of them — or,
//! worse, fail to start because one of them was already running.
//!
//! # Nothing is sent
//!
//! No ArtPoll reply, no discovery, no sACN synchronisation, no output. These
//! sockets receive. Appearing in a lighting console's node list is a claim about
//! what this machine is, and it is not one to make without an operator asking
//! for it.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

use crate::artnet::{self, ART_NET_PORT};
use crate::sacn::{self, SACN_PORT};
use ovs_types::telemetry::{DmxProtocol, DmxTelemetry, UniverseTelemetry};

use crate::universe::{Accepted, SourceKey, Universes};

/// How long a reader waits on a socket before checking whether it should stop.
///
/// Short enough that shutting down is not something an operator waits for, long
/// enough that an idle receiver is not spinning.
const READ_TIMEOUT: Duration = Duration::from_millis(250);

/// The largest packet either protocol produces, with room to spare.
const RECEIVE_BUFFER: usize = 2048;

/// What to listen on.
#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    /// Whether to listen for Art-Net.
    pub artnet: bool,
    /// Whether to listen for sACN.
    pub sacn: bool,
    /// Which interface to bind and to join multicast groups on.
    ///
    /// Unspecified means every interface, which is what a single-homed machine
    /// wants. A machine with a separate lighting network wants to say so: an
    /// sACN multicast join goes out of one interface, and picking the wrong one
    /// is the usual reason a receiver sees nothing at all.
    pub interface: Ipv4Addr,
    /// The Art-Net port. Configurable so it can be tested; a rig never moves it.
    pub artnet_port: u16,
    /// The sACN port. As above.
    pub sacn_port: u16,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            artnet: true,
            sacn: true,
            interface: Ipv4Addr::UNSPECIFIED,
            artnet_port: ART_NET_PORT,
            sacn_port: SACN_PORT,
        }
    }
}

/// How much has arrived, for anybody watching the server.
///
/// Counted rather than logged. A receiver that logged every packet would write
/// a hundred and fifty thousand lines an hour for one universe, and an operator
/// asking "is the console reaching it?" wants a number that is going up.
#[derive(Debug, Default)]
pub struct Stats {
    artnet: AtomicU64,
    sacn: AtomicU64,
    unusable: AtomicU64,
    refused: AtomicU64,
}

impl Stats {
    /// Art-Net packets applied.
    #[must_use]
    pub fn artnet(&self) -> u64 {
        self.artnet.load(Ordering::Relaxed)
    }

    /// sACN packets applied.
    #[must_use]
    pub fn sacn(&self) -> u64 {
        self.sacn.load(Ordering::Relaxed)
    }

    /// Datagrams that were not a data packet of either protocol.
    ///
    /// Not a fault. A media server shares a wire with consoles polling for
    /// nodes, visualisers, and whatever else the venue plugged in.
    #[must_use]
    pub fn unusable(&self) -> u64 {
        self.unusable.load(Ordering::Relaxed)
    }

    /// Packets understood but not applied: outranked, contended, late or
    /// preview.
    #[must_use]
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }
}

/// Listening sockets and the universes they fill.
#[derive(Debug)]
pub struct Receiver {
    universes: Arc<Mutex<Universes>>,
    stats: Arc<Stats>,
    running: Arc<AtomicBool>,
    readers: Vec<JoinHandle<()>>,
    /// Kept so that multicast groups can be joined after the fact, as a patch
    /// changes which universes matter.
    sacn_socket: Option<Arc<UdpSocket>>,
    interface: Ipv4Addr,
    joined: Mutex<Vec<u16>>,
    artnet_port: Option<u16>,
    sacn_port: Option<u16>,
}

impl Receiver {
    /// Opens the sockets and starts reading.
    ///
    /// # Errors
    ///
    /// Returns the first socket that could not be opened. A caller that wants
    /// the server to start anyway should say so and carry on: a media server
    /// with no DMX is still a media server, and one that refuses to boot cannot
    /// be fixed from the client.
    pub fn start(config: &ReceiverConfig) -> io::Result<Self> {
        let universes = Arc::new(Mutex::new(Universes::new()));
        let stats = Arc::new(Stats::default());
        let running = Arc::new(AtomicBool::new(true));
        let mut readers = Vec::new();
        let mut artnet_port = None;
        let mut sacn_port = None;
        let mut sacn_socket = None;

        if config.artnet {
            let socket = bind(config.artnet_port)?;
            artnet_port = Some(socket.local_addr()?.port());
            let socket = Arc::new(socket);
            readers.push(spawn_reader(
                "ovs-artnet",
                Arc::clone(&socket),
                Arc::clone(&universes),
                Arc::clone(&stats),
                Arc::clone(&running),
                read_artnet,
            )?);
        }

        if config.sacn {
            let socket = bind(config.sacn_port)?;
            sacn_port = Some(socket.local_addr()?.port());
            let socket = Arc::new(socket);
            readers.push(spawn_reader(
                "ovs-sacn",
                Arc::clone(&socket),
                Arc::clone(&universes),
                Arc::clone(&stats),
                Arc::clone(&running),
                read_sacn,
            )?);
            sacn_socket = Some(socket);
        }

        Ok(Self {
            universes,
            stats,
            running,
            readers,
            sacn_socket,
            interface: config.interface,
            joined: Mutex::new(Vec::new()),
            artnet_port,
            sacn_port,
        })
    }

    /// Joins the sACN multicast groups for these universes.
    ///
    /// Called with whatever the patch currently names. A console sending
    /// unicast reaches this receiver without any of it; multicast is the
    /// default in most consoles, and without the join the switch has no reason
    /// to deliver a single packet.
    ///
    /// Groups already joined are left alone, and leaving is deliberately not
    /// done: a universe that drops out of a patch for a moment while an
    /// operator edits it would otherwise take a network round trip to come
    /// back, in the middle of a show.
    pub fn listen_to(&self, universes: &[u16]) {
        let Some(socket) = &self.sacn_socket else {
            return;
        };
        let Ok(mut joined) = self.joined.lock() else {
            return;
        };

        for &universe in universes {
            if universe == 0 || joined.contains(&universe) {
                continue;
            }
            let group = multicast_group(universe);
            match socket.join_multicast_v4(&group, &self.interface) {
                Ok(()) => {
                    joined.push(universe);
                    tracing::info!(universe, %group, "joined sACN multicast group");
                }
                Err(error) => {
                    // Reported once per universe, because a join that fails
                    // fails every time and the reason is nearly always the
                    // interface.
                    joined.push(universe);
                    tracing::warn!(
                        universe,
                        %group,
                        interface = %self.interface,
                        "could not join the sACN multicast group: {error}"
                    );
                }
            }
        }
    }

    /// Reads the universes under the lock.
    ///
    /// The mapping pass runs here, on the caller's thread, so that changing the
    /// show never happens on a socket thread and a slow consumer can only ever
    /// make itself late.
    pub fn read<R>(&self, with: impl FnOnce(&mut Universes) -> R) -> R {
        match self.universes.lock() {
            Ok(mut universes) => with(&mut universes),
            // A reader thread panicked while holding the lock. The universes
            // are still whatever they were; a show should not stop for it.
            Err(poisoned) => with(&mut poisoned.into_inner()),
        }
    }

    /// Returns what this receiver is hearing, for the server's status.
    ///
    /// A universe with no source is still listed, with its channel count: loss
    /// holds the look rather than blacking it out, so "nothing is driving this"
    /// and "there is nothing here" are different answers and an operator in
    /// front of a frozen wall needs the first one.
    #[must_use]
    pub fn telemetry(&self) -> DmxTelemetry {
        let universes = self.read(|universes| {
            universes
                .iter()
                .map(|(number, universe)| UniverseTelemetry {
                    universe: number,
                    source: universe.source_name().map(str::to_owned),
                    protocol: universe.source().map(|key| match key {
                        SourceKey::ArtNet(_) => DmxProtocol::ArtNet,
                        SourceKey::Sacn(_) => DmxProtocol::Sacn,
                    }),
                    priority: universe.priority(),
                    channels: universe.channels().len() as u16,
                })
                .collect()
        });

        DmxTelemetry {
            artnet_packets: self.stats.artnet(),
            sacn_packets: self.stats.sacn(),
            unusable_packets: self.stats.unusable(),
            refused_packets: self.stats.refused(),
            universes,
        }
    }

    /// Returns what has arrived so far.
    #[must_use]
    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    /// The port Art-Net is being read on, if it is.
    #[must_use]
    pub fn artnet_port(&self) -> Option<u16> {
        self.artnet_port
    }

    /// The port sACN is being read on, if it is.
    #[must_use]
    pub fn sacn_port(&self) -> Option<u16> {
        self.sacn_port
    }

    /// Stops the reader threads and waits for them.
    pub fn stop(mut self) {
        self.shut_down();
    }

    fn shut_down(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// Returns the multicast group an sACN universe is sent to.
///
/// `239.255.<high>.<low>`, from E1.31 Appendix A.
#[must_use]
pub fn multicast_group(universe: u16) -> Ipv4Addr {
    Ipv4Addr::new(239, 255, (universe >> 8) as u8, (universe & 0xFF) as u8)
}

/// Opens one receiving socket.
fn bind(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

    // Both protocols expect several programs on one machine to hear the same
    // traffic. Without this, a visualiser already running takes the port and
    // this server never starts.
    socket.set_reuse_address(true)?;

    // Bound to every interface even when one was named. A multicast group is
    // joined on a chosen interface, but binding to that interface's address
    // would also throw away the broadcast traffic Art-Net sends, which arrives
    // addressed to the subnet rather than to this machine.
    socket.bind(&SocketAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).into())?;
    socket.set_read_timeout(Some(READ_TIMEOUT))?;

    Ok(socket.into())
}

/// Starts one reader thread.
fn spawn_reader(
    name: &str,
    socket: Arc<UdpSocket>,
    universes: Arc<Mutex<Universes>>,
    stats: Arc<Stats>,
    running: Arc<AtomicBool>,
    read: fn(&[u8], IpAddr, &mut Universes, &Stats),
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; RECEIVE_BUFFER];
            while running.load(Ordering::Relaxed) {
                let (length, from) = match socket.recv_from(&mut buffer) {
                    Ok(received) => received,
                    Err(error) => {
                        match error.kind() {
                            // The read timeout, which is how this thread
                            // notices it should stop.
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {}
                            // Windows reports an ICMP unreachable from an
                            // earlier datagram as a receive error on an
                            // unconnected socket. Nothing here sent anything,
                            // and the next packet is unaffected.
                            io::ErrorKind::ConnectionReset => {}
                            _ => {
                                tracing::debug!("dmx receive failed: {error}");
                            }
                        }
                        continue;
                    }
                };

                let Ok(mut held) = universes.lock() else {
                    continue;
                };
                read(&buffer[..length], from.ip(), &mut held, &stats);
            }
        })
}

/// Reads one datagram as Art-Net.
fn read_artnet(datagram: &[u8], from: IpAddr, universes: &mut Universes, stats: &Stats) {
    let Ok(packet) = artnet::parse(datagram) else {
        stats.unusable.fetch_add(1, Ordering::Relaxed);
        return;
    };

    match universes.accept_artnet(from, &packet, Instant::now()) {
        Accepted::Applied => {
            stats.artnet.fetch_add(1, Ordering::Relaxed);
        }
        _ => {
            stats.refused.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Reads one datagram as sACN.
fn read_sacn(datagram: &[u8], _from: IpAddr, universes: &mut Universes, stats: &Stats) {
    let Ok(packet) = sacn::parse(datagram) else {
        stats.unusable.fetch_add(1, Ordering::Relaxed);
        return;
    };

    match universes.accept_sacn(&packet, Instant::now()) {
        Accepted::Applied => {
            stats.sacn.fetch_add(1, Ordering::Relaxed);
        }
        _ => {
            stats.refused.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A receiver on ports the operating system picked, so a test never fights
    /// whatever else on this machine is listening on 6454.
    fn ephemeral() -> Receiver {
        Receiver::start(&ReceiverConfig {
            artnet_port: 0,
            sacn_port: 0,
            ..ReceiverConfig::default()
        })
        .expect("sockets")
    }

    /// Waits for a universe to carry what was sent, or gives up.
    fn wait_for(receiver: &Receiver, universe: u16, expected: &[u8]) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let found = receiver.read(|universes| {
                universes
                    .get(universe)
                    .is_some_and(|held| held.channels() == expected)
            });
            if found {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_packet_on_the_wire_reaches_the_universes() {
        // The whole path: a real socket, a real datagram, a real reader thread.
        let receiver = ephemeral();
        let port = receiver.artnet_port().expect("art-net listening");

        let console = UdpSocket::bind("127.0.0.1:0").expect("a console");
        console
            .send_to(
                &artnet_packet(1, 1, &[11, 22, 33, 44]),
                SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            )
            .expect("sent");

        assert!(wait_for(&receiver, 1, &[11, 22, 33, 44]));
        assert_eq!(receiver.stats().artnet(), 1);
    }

    #[test]
    fn something_else_on_the_port_is_counted_and_forgotten() {
        // A media server shares a wire with consoles polling for nodes and
        // whatever else the venue plugged in. None of it may stop the reader.
        let receiver = ephemeral();
        let port = receiver.artnet_port().expect("art-net listening");
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

        let console = UdpSocket::bind("127.0.0.1:0").expect("a console");
        console.send_to(b"hello?", target).expect("sent");
        console
            .send_to(&artnet_packet(2, 1, &[7, 7]), target)
            .expect("sent");

        assert!(
            wait_for(&receiver, 2, &[7, 7]),
            "the good packet still arrived"
        );
        assert!(receiver.stats().unusable() >= 1);
    }

    #[test]
    fn both_protocols_are_read_at_once() {
        let receiver = ephemeral();
        assert!(receiver.artnet_port().is_some());
        assert!(receiver.sacn_port().is_some());
        assert_ne!(receiver.artnet_port(), receiver.sacn_port());
    }

    #[test]
    fn a_receiver_can_be_asked_for_one_protocol_only() {
        let receiver = Receiver::start(&ReceiverConfig {
            sacn: false,
            artnet_port: 0,
            sacn_port: 0,
            ..ReceiverConfig::default()
        })
        .expect("sockets");

        assert!(receiver.artnet_port().is_some());
        assert_eq!(receiver.sacn_port(), None);
    }

    #[test]
    fn two_receivers_can_share_a_port() {
        // A venue's machine often has a visualiser or a second server on it. A
        // receiver that took sole ownership of the port would stop all of them,
        // or fail to start because one of them was already running.
        let first = Receiver::start(&ReceiverConfig {
            artnet_port: 0,
            sacn: false,
            ..ReceiverConfig::default()
        })
        .expect("first");
        let port = first.artnet_port().expect("listening");

        let second = Receiver::start(&ReceiverConfig {
            artnet_port: port,
            sacn: false,
            ..ReceiverConfig::default()
        });
        assert!(second.is_ok(), "the port is shared, not owned");
    }

    #[test]
    fn a_universe_is_sent_to_the_group_the_standard_says() {
        // Getting this wrong means joining a group nothing is sent to, and a
        // receiver that sits there hearing nothing with no error to show for it.
        assert_eq!(multicast_group(1), Ipv4Addr::new(239, 255, 0, 1));
        assert_eq!(multicast_group(256), Ipv4Addr::new(239, 255, 1, 0));
        assert_eq!(multicast_group(63_999), Ipv4Addr::new(239, 255, 249, 255));
    }

    #[test]
    fn telemetry_tells_a_held_universe_from_an_empty_one() {
        // An operator in front of a frozen wall is asking which of the two this
        // is, and a status that could not say would send them to the wrong end
        // of the building.
        let receiver = ephemeral();
        let port = receiver.artnet_port().expect("art-net listening");

        assert!(receiver.telemetry().universes.is_empty(), "nothing yet");

        let console = UdpSocket::bind("127.0.0.1:0").expect("a console");
        console
            .send_to(
                &artnet_packet(3, 1, &[1, 2, 3, 4]),
                SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            )
            .expect("sent");
        assert!(wait_for(&receiver, 3, &[1, 2, 3, 4]));

        let telemetry = receiver.telemetry();
        assert_eq!(telemetry.artnet_packets, 1);
        let universe = &telemetry.universes[0];
        assert_eq!(universe.universe, 3);
        assert_eq!(universe.channels, 4);
        assert_eq!(universe.protocol, Some(DmxProtocol::ArtNet));
        assert_eq!(universe.priority, Some(100));
        assert!(universe.source.is_some());
    }

    #[test]
    fn stopping_ends_the_reader_threads() {
        let receiver = ephemeral();
        receiver.stop();
        // Returning from stop() at all is the assertion: it joins the threads,
        // so a reader that ignored the flag would hang here rather than pass.
    }
}
