//! Sends Art-Net or sACN at a server, so the receiving side can be proved.
//!
//! ```text
//! ovs-dmx-send --host 192.0.2.10 --protocol artnet --universe 0 --pattern ramp
//! ovs-dmx-send --host 192.0.2.10 --protocol sacn --universe 1 --priority 120 \
//!              --channels 1=255,2=128 --seconds 10
//! ```
//!
//! # Why this exists, and what it is not
//!
//! The server does not transmit. It receives, it does not answer ArtPoll and it
//! does not advertise itself, because appearing in a lighting network's
//! discovery is a claim about what this machine is and it is not one to make
//! without an operator asking (`ovs_dmx`'s own module documentation).
//!
//! This is a separate program an engineer runs. Nothing in `ovsd` or the
//! renderer links it, and it cannot be reached through the API. It exists
//! because everything in the DMX path had been proved against a socket on the
//! same machine, and a lighting network is not that: it has a switch, a subnet,
//! multicast groups that a NIC has to join, and a console that sends whether or
//! not anybody is listening.
//!
//! # What it can do
//!
//! Send one universe of channel data, at a rate, for a time, with a given
//! source name and priority, using either protocol. That is enough to see a
//! universe appear in the telemetry, to watch two sources contend for one
//! universe by priority, and to see a source time out when it stops.
//!
//! It deliberately does not implement discovery, synchronization or RDM. A tool
//! that half-implemented those would prove less than nothing.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ovs_dmx::artnet::ART_NET_PORT;
use ovs_dmx::sacn::{DEFAULT_PRIORITY, SACN_PORT};

/// How many channels a universe has.
const UNIVERSE_CHANNELS: usize = 512;

/// What the tool was asked to do.
struct Request {
    /// Where to send. An address rather than a name: a lighting network rarely
    /// has DNS, and a tool that resolved names would fail in a way that looked
    /// like the network being wrong.
    host: Ipv4Addr,
    /// Which protocol to speak.
    protocol: Protocol,
    /// Which universe. Art-Net calls this a port address.
    universe: u16,
    /// What sACN priority to claim. Ignored for Art-Net, which has none.
    priority: u8,
    /// What to call itself, so two of these can be told apart.
    source: String,
    /// What to put in the channels.
    pattern: Pattern,
    /// How many times a second to send.
    rate: f64,
    /// How long to go on for. Zero means until stopped.
    seconds: f64,
}

/// Which wire.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Protocol {
    /// Art-Net, unicast.
    ArtNet,
    /// sACN, to the universe's multicast group unless a host was given.
    Sacn,
}

/// What goes in the channels.
enum Pattern {
    /// Channel *n* holds *n* modulo 256. Every channel differs from its
    /// neighbours, so a patch that is off by one is visible at a glance.
    Ramp,
    /// One channel at full, moving along one channel per packet, everything
    /// else at zero. Shows which channel a fixture is actually reading.
    Chase,
    /// Exactly what was asked for, and zero everywhere else.
    Fixed(Vec<(usize, u8)>),
}

fn main() -> ExitCode {
    let request = match parse_arguments() {
        Ok(request) => request,
        Err(reason) => {
            eprintln!("{reason}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match send(&request) {
        Ok(packets) => {
            println!("sent {packets} packets");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("could not send: {error}");
            ExitCode::FAILURE
        }
    }
}

/// How to call it.
const USAGE: &str = "\
usage: ovs-dmx-send --host <ipv4> [options]

  --host <ipv4>          where to send (required)
  --protocol artnet|sacn  default artnet
  --universe <n>         default 0
  --priority <1-200>     sACN only, default 100
  --source <name>        what to call itself, default \"ovs-dmx-send\"
  --pattern ramp|chase   default ramp
  --channels 1=255,...   exact values instead of a pattern
  --rate <hz>            default 30
  --seconds <n>          default 5, 0 to go on until stopped
";

/// Reads the arguments, or says what was wrong with them.
fn parse_arguments() -> Result<Request, String> {
    let mut host: Option<Ipv4Addr> = None;
    let mut protocol = Protocol::ArtNet;
    let mut universe: u16 = 0;
    let mut priority = DEFAULT_PRIORITY;
    let mut source = "ovs-dmx-send".to_owned();
    let mut pattern = Pattern::Ramp;
    let mut rate = 30.0_f64;
    let mut seconds = 5.0_f64;

    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .ok_or_else(|| format!("{flag} needs a value"))
        };

        match flag.as_str() {
            "--host" => {
                host = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--host must be an IPv4 address".to_owned())?,
                );
            }
            "--protocol" => {
                protocol = match value()?.as_str() {
                    "artnet" => Protocol::ArtNet,
                    "sacn" => Protocol::Sacn,
                    other => return Err(format!("{other:?} is not a protocol this speaks")),
                };
            }
            "--universe" => {
                universe = value()?
                    .parse()
                    .map_err(|_| "--universe must be a number".to_owned())?;
            }
            "--priority" => {
                priority = value()?
                    .parse()
                    .map_err(|_| "--priority must be a number".to_owned())?;
            }
            "--source" => source = value()?,
            "--pattern" => {
                pattern = match value()?.as_str() {
                    "ramp" => Pattern::Ramp,
                    "chase" => Pattern::Chase,
                    other => return Err(format!("{other:?} is not a pattern this draws")),
                };
            }
            "--channels" => pattern = Pattern::Fixed(parse_channels(&value()?)?),
            "--rate" => {
                rate = value()?
                    .parse()
                    .map_err(|_| "--rate must be a number".to_owned())?;
            }
            "--seconds" => {
                seconds = value()?
                    .parse()
                    .map_err(|_| "--seconds must be a number".to_owned())?;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("{other:?} is not an option this takes")),
        }
    }

    Ok(Request {
        host: host.ok_or_else(|| "--host is required".to_owned())?,
        protocol,
        universe,
        priority,
        source,
        pattern,
        rate: rate.clamp(0.1, 200.0),
        seconds: seconds.max(0.0),
    })
}

/// Reads `1=255,2=128` into channel numbers and values.
fn parse_channels(spec: &str) -> Result<Vec<(usize, u8)>, String> {
    let mut channels = Vec::new();
    for pair in spec.split(',').filter(|pair| !pair.is_empty()) {
        let (channel, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("{pair:?} is not channel=value"))?;
        let channel: usize = channel
            .trim()
            .parse()
            .map_err(|_| format!("{channel:?} is not a channel number"))?;
        let value: u8 = value
            .trim()
            .parse()
            .map_err(|_| format!("{value:?} is not a value between 0 and 255"))?;
        if !(1..=UNIVERSE_CHANNELS).contains(&channel) {
            return Err(format!("channel {channel} is not in a universe"));
        }
        channels.push((channel, value));
    }
    Ok(channels)
}

/// Sends until the time is up, and says how many packets went.
fn send(request: &Request) -> std::io::Result<u64> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;

    let port = match request.protocol {
        Protocol::ArtNet => ART_NET_PORT,
        Protocol::Sacn => SACN_PORT,
    };
    let target = SocketAddrV4::new(request.host, port);

    let cid = component_identifier(&request.source);
    let interval = Duration::from_secs_f64(1.0 / request.rate);
    let deadline =
        (request.seconds > 0.0).then(|| Instant::now() + Duration::from_secs_f64(request.seconds));

    let mut sequence: u8 = 0;
    let mut sent: u64 = 0;
    let mut channels = vec![0_u8; UNIVERSE_CHANNELS];

    println!(
        "sending {} to {target} on universe {} as {:?}",
        match request.protocol {
            Protocol::ArtNet => "Art-Net",
            Protocol::Sacn => "sACN",
        },
        request.universe,
        request.source
    );

    loop {
        fill(&mut channels, &request.pattern, sent);

        let packet = match request.protocol {
            Protocol::ArtNet => artnet_packet(request.universe, sequence, &channels),
            Protocol::Sacn => sacn_packet(request, &cid, sequence, &channels),
        };
        socket.send_to(&packet, target)?;

        sequence = sequence.wrapping_add(1);
        sent += 1;

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(sent);
        }
        std::thread::sleep(interval);
    }
}

/// Puts the pattern into the channels.
fn fill(channels: &mut [u8], pattern: &Pattern, packet: u64) {
    channels.fill(0);
    match pattern {
        Pattern::Ramp => {
            for (index, channel) in channels.iter_mut().enumerate() {
                *channel = ((index + 1) % 256) as u8;
            }
        }
        Pattern::Chase => {
            let lit = (packet as usize) % channels.len();
            channels[lit] = 255;
        }
        Pattern::Fixed(values) => {
            for &(channel, value) in values {
                channels[channel - 1] = value;
            }
        }
    }
}

/// An ArtDMX packet.
///
/// The layout is Art-Net 4's, and the only part worth a comment is the length:
/// it is big-endian while the opcode is little-endian, which is in the
/// specification and catches everybody once.
fn artnet_packet(port_address: u16, sequence: u8, channels: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(18 + channels.len());
    packet.extend_from_slice(b"Art-Net\0");
    packet.extend_from_slice(&0x5000_u16.to_le_bytes());
    packet.extend_from_slice(&[0, 14]);
    // A sequence of zero means "this source does not use sequence numbers", so
    // it is skipped: a tool that claimed not to use them could not be used to
    // test that they are honoured.
    packet.push(if sequence == 0 { 1 } else { sequence });
    packet.push(0);
    packet.push((port_address & 0xff) as u8);
    packet.push((port_address >> 8) as u8);
    packet.extend_from_slice(&(channels.len() as u16).to_be_bytes());
    packet.extend_from_slice(channels);
    packet
}

/// An E1.31 data packet: root, framing and DMP layers, nested.
fn sacn_packet(request: &Request, cid: &[u8; 16], sequence: u8, channels: &[u8]) -> Vec<u8> {
    let property_count = channels.len() + 1; // the start code counts as one
    let dmp_length = 10 + property_count;
    let framing_length = 77 + dmp_length;
    let root_length = 22 + framing_length;

    let mut packet = Vec::with_capacity(38 + framing_length);

    // Root layer.
    packet.extend_from_slice(&0x0010_u16.to_be_bytes());
    packet.extend_from_slice(&0x0000_u16.to_be_bytes());
    packet.extend_from_slice(b"ASC-E1.17\0\0\0");
    packet.extend_from_slice(&flags_and_length(root_length));
    packet.extend_from_slice(&0x0000_0004_u32.to_be_bytes());
    packet.extend_from_slice(cid);

    // Framing layer.
    packet.extend_from_slice(&flags_and_length(framing_length));
    packet.extend_from_slice(&0x0000_0002_u32.to_be_bytes());
    let mut name = [0_u8; 64];
    let bytes = request.source.as_bytes();
    let take = bytes.len().min(63);
    name[..take].copy_from_slice(&bytes[..take]);
    packet.extend_from_slice(&name);
    packet.push(request.priority);
    packet.extend_from_slice(&0_u16.to_be_bytes()); // no synchronization universe
    packet.push(sequence);
    packet.push(0); // no options: not preview, not terminated
    packet.extend_from_slice(&request.universe.to_be_bytes());

    // DMP layer.
    packet.extend_from_slice(&flags_and_length(dmp_length));
    packet.push(0x02);
    packet.push(0xA1);
    packet.extend_from_slice(&0_u16.to_be_bytes()); // first property address
    packet.extend_from_slice(&1_u16.to_be_bytes()); // address increment
    packet.extend_from_slice(&(property_count as u16).to_be_bytes());
    packet.push(0); // start code: channel data
    packet.extend_from_slice(channels);

    packet
}

/// The flags-and-length word every E1.31 layer starts with.
fn flags_and_length(length: usize) -> [u8; 2] {
    (0x7000_u16 | (length as u16 & 0x0fff)).to_be_bytes()
}

/// A component identifier derived from the source name.
///
/// Stable, so two runs of this tool with the same name are the same source and
/// a run with a different name is a different one — which is what makes it
/// possible to test two sources contending for one universe. Not a real UUID,
/// and it does not pretend to be: a console's CID is its own and this is a
/// test tool's.
fn component_identifier(source: &str) -> [u8; 16] {
    let mut cid = [0_u8; 16];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    cid[..8].copy_from_slice(&hash.to_be_bytes());
    cid[8..].copy_from_slice(&hash.rotate_left(17).to_be_bytes());
    cid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_this_sends_is_what_the_receiver_reads() {
        // The whole point of the tool: if these two disagree it proves nothing.
        let channels: Vec<u8> = (0..512).map(|index| (index % 256) as u8).collect();
        let packet = artnet_packet(3, 7, &channels);

        let read = ovs_dmx::artnet::parse(&packet).expect("the receiver understands it");
        assert_eq!(read.port_address, 3);
        assert_eq!(read.sequence, Some(7));
        assert_eq!(read.channels, &channels[..]);
    }

    #[test]
    fn an_sacn_packet_carries_its_source_its_priority_and_its_universe() {
        let request = Request {
            host: Ipv4Addr::LOCALHOST,
            protocol: Protocol::Sacn,
            universe: 42,
            priority: 120,
            source: "a desk".to_owned(),
            pattern: Pattern::Ramp,
            rate: 30.0,
            seconds: 1.0,
        };
        let channels = vec![9_u8; 512];
        let packet = sacn_packet(&request, &component_identifier("a desk"), 5, &channels);

        let read = ovs_dmx::sacn::parse(&packet).expect("the receiver understands it");
        assert_eq!(read.universe, 42);
        assert_eq!(read.priority, 120);
        assert_eq!(read.source_name, "a desk");
        assert_eq!(read.sequence, 5);
        assert_eq!(read.channels, &channels[..]);
    }

    #[test]
    fn two_names_are_two_sources_and_one_name_is_one() {
        // What makes it possible to test two consoles contending for a
        // universe, and to test that one of them coming back is the same
        // source rather than a new one.
        assert_eq!(
            component_identifier("desk A"),
            component_identifier("desk A")
        );
        assert_ne!(
            component_identifier("desk A"),
            component_identifier("desk B")
        );
    }

    #[test]
    fn a_ramp_makes_every_channel_differ_from_its_neighbour() {
        // So that a patch that is off by one is visible rather than deduced.
        let mut channels = vec![0_u8; 512];
        fill(&mut channels, &Pattern::Ramp, 0);

        assert_eq!(channels[0], 1, "channel 1 holds 1");
        assert_eq!(channels[254], 255);
        assert_eq!(channels[255], 0, "and it wraps");
    }

    #[test]
    fn a_chase_lights_one_channel_and_moves_along() {
        let mut channels = vec![0_u8; 512];

        fill(&mut channels, &Pattern::Chase, 0);
        assert_eq!(channels[0], 255);
        assert_eq!(channels.iter().filter(|value| **value > 0).count(), 1);

        fill(&mut channels, &Pattern::Chase, 1);
        assert_eq!(channels[1], 255);
        assert_eq!(channels[0], 0);
    }

    #[test]
    fn exact_values_are_sent_exactly_and_nothing_else_is() {
        let mut channels = vec![7_u8; 512];
        fill(
            &mut channels,
            &Pattern::Fixed(vec![(1, 255), (512, 128)]),
            0,
        );

        assert_eq!(channels[0], 255);
        assert_eq!(channels[511], 128);
        assert_eq!(channels[1], 0, "and whatever was there is gone");
    }

    #[test]
    fn a_channel_outside_a_universe_is_refused_rather_than_wrapped() {
        assert!(parse_channels("1=255,2=128").is_ok());
        assert!(parse_channels("0=255").is_err(), "channels start at one");
        assert!(parse_channels("513=255").is_err(), "and end at 512");
        assert!(parse_channels("1=300").is_err(), "values are bytes");
        assert!(parse_channels("one=255").is_err());
    }
}
