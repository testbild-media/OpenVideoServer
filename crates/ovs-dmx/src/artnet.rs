//! Art-Net receive: the ArtDMX packet.
//!
//! One opcode of one protocol. A media server is a *receiver*, so ArtPoll,
//! ArtPollReply, ArtAddress and the rest of the protocol are not implemented and
//! not answered: replying would put this machine into a lighting network's
//! discovery, which is a claim about what it is that a media server should not
//! be making without an operator asking for it.
//!
//! # Port-Address
//!
//! Art-Net addresses a universe with fifteen bits in three pieces: Net, SubNet
//! and Universe. A console shows them as `Net:SubNet:Universe` or as a single
//! number, and both mean the same fifteen bits, so the patch stores the number
//! and this assembles it.
//!
//! # Sequence
//!
//! Every packet carries a sequence number, and zero means the sender does not
//! use them. When it does, a packet older than the last one is a packet that
//! overtook another on the network, and applying it would move a fader
//! backwards for one frame.

/// The UDP port every Art-Net node listens on.
pub const ART_NET_PORT: u16 = 6454;

/// What every Art-Net packet starts with.
const ART_NET_ID: &[u8; 8] = b"Art-Net\0";

/// The opcode for a packet carrying DMX data.
const OP_DMX: u16 = 0x5000;

/// The lowest protocol version this accepts.
///
/// Fourteen is what Art-Net II settled on and every release since has used.
const MIN_PROTOCOL_VERSION: u16 = 14;

/// The largest universe a fifteen-bit port address can name.
pub const MAX_PORT_ADDRESS: u16 = 0x7FFF;

/// One ArtDMX packet, understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtDmx<'a> {
    /// The fifteen-bit port address this is for.
    pub port_address: u16,
    /// The sender's sequence number, or `None` when it does not use them.
    pub sequence: Option<u8>,
    /// The channel data, starting at channel 1.
    pub channels: &'a [u8],
}

/// Why a packet was not understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArtNetError {
    /// Too short to be an Art-Net packet at all.
    #[error("an Art-Net packet is at least {MIN_PACKET_LEN} bytes and this is {length}")]
    TooShort {
        /// How long it was.
        length: usize,
    },

    /// Not Art-Net.
    ///
    /// Ordinary on a shared network. Something else is using the port, or a
    /// stray broadcast arrived.
    #[error("not an Art-Net packet")]
    NotArtNet,

    /// Art-Net, but not DMX data.
    ///
    /// Also ordinary: a console polling for nodes sends ArtPoll to the same
    /// port, and this server does not answer it.
    #[error("Art-Net opcode {opcode:#06x} is not ArtDmx")]
    NotDmx {
        /// The opcode that arrived.
        opcode: u16,
    },

    /// A protocol version older than anything still in use.
    #[error("Art-Net protocol version {version} is older than {MIN_PROTOCOL_VERSION}")]
    OldProtocol {
        /// The version that arrived.
        version: u16,
    },

    /// The declared data length does not fit the packet.
    #[error("the packet declares {declared} bytes of channel data and carries {available}")]
    LengthMismatch {
        /// What the header said.
        declared: usize,
        /// What was there.
        available: usize,
    },

    /// A data length the protocol does not allow.
    #[error("{length} is not a legal ArtDMX data length")]
    IllegalLength {
        /// The declared length.
        length: usize,
    },
}

/// The shortest packet that can carry a header.
pub const MIN_PACKET_LEN: usize = 18;

/// Parses one ArtDMX packet.
///
/// # Errors
///
/// Returns [`ArtNetError`] describing why the bytes were not usable. Every case
/// is ordinary on a live network and none of them is a reason to stop
/// listening: a media server shares a wire with consoles, nodes and whatever
/// else the venue plugged in.
pub fn parse(packet: &[u8]) -> Result<ArtDmx<'_>, ArtNetError> {
    if packet.len() < MIN_PACKET_LEN {
        return Err(ArtNetError::TooShort {
            length: packet.len(),
        });
    }
    if &packet[..8] != ART_NET_ID {
        return Err(ArtNetError::NotArtNet);
    }

    // Little-endian, unlike everything else in the header. That is what the
    // specification says, and it is the single most common way to misread it.
    let opcode = u16::from_le_bytes([packet[8], packet[9]]);
    if opcode != OP_DMX {
        return Err(ArtNetError::NotDmx { opcode });
    }

    // And this one is big-endian. Also what the specification says.
    let version = u16::from_be_bytes([packet[10], packet[11]]);
    if version < MIN_PROTOCOL_VERSION {
        return Err(ArtNetError::OldProtocol { version });
    }

    let sequence = packet[12];
    let sub_universe = packet[14];
    let net = packet[15];
    let declared = usize::from(u16::from_be_bytes([packet[16], packet[17]]));

    // Two to 512, and even. An odd length is a sender that has miscounted, and
    // a receiver that accepted it would be reading one byte of something else.
    if !(2..=512).contains(&declared) || declared % 2 != 0 {
        return Err(ArtNetError::IllegalLength { length: declared });
    }

    let channels = packet
        .get(MIN_PACKET_LEN..MIN_PACKET_LEN + declared)
        .ok_or(ArtNetError::LengthMismatch {
            declared,
            available: packet.len() - MIN_PACKET_LEN,
        })?;

    Ok(ArtDmx {
        port_address: (u16::from(net) << 8) | u16::from(sub_universe),
        // Zero means the sender does not use sequence numbers, which is
        // different from "this is packet zero".
        sequence: (sequence != 0).then_some(sequence),
        channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an ArtDMX packet the way a console would.
    fn packet(net: u8, sub_universe: u8, sequence: u8, channels: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(ART_NET_ID);
        bytes.extend_from_slice(&OP_DMX.to_le_bytes());
        bytes.extend_from_slice(&MIN_PROTOCOL_VERSION.to_be_bytes());
        bytes.push(sequence);
        bytes.push(0); // physical
        bytes.push(sub_universe);
        bytes.push(net);
        bytes.extend_from_slice(&(channels.len() as u16).to_be_bytes());
        bytes.extend_from_slice(channels);
        bytes
    }

    #[test]
    fn a_console_packet_decodes_to_its_universe_and_its_channels() {
        let data = [10_u8, 20, 30, 40];
        let bytes = packet(0, 3, 7, &data);
        let parsed = parse(&bytes).expect("a valid packet");

        assert_eq!(parsed.port_address, 3);
        assert_eq!(parsed.sequence, Some(7));
        assert_eq!(parsed.channels, &data);
    }

    #[test]
    fn the_port_address_is_net_and_sub_universe_together() {
        // Fifteen bits in three pieces. Reading only the low byte is the usual
        // mistake, and it puts every universe above 255 on top of a low one.
        let bytes = packet(2, 0x35, 1, &[0, 0]);
        let parsed = parse(&bytes).expect("valid");
        assert_eq!(parsed.port_address, 0x0235);

        let bytes = packet(0x7F, 0xFF, 1, &[0, 0]);
        let highest = parse(&bytes).expect("valid");
        assert_eq!(highest.port_address, MAX_PORT_ADDRESS);
    }

    #[test]
    fn sequence_zero_means_the_sender_does_not_use_them() {
        // Different from "packet zero". A receiver that treated it as a number
        // would drop every packet from a sender that does not sequence.
        let bytes = packet(0, 0, 0, &[1, 2]);
        let parsed = parse(&bytes).expect("valid");
        assert_eq!(parsed.sequence, None);
    }

    #[test]
    fn the_opcode_is_little_endian() {
        // The single most common way to misread this header. Read big-endian,
        // every ArtDMX packet would look like something else and the server
        // would sit there receiving nothing it recognised.
        let good = packet(0, 0, 1, &[1, 2]);
        assert!(parse(&good).is_ok());

        let mut swapped = good;
        swapped.swap(8, 9);
        assert!(matches!(parse(&swapped), Err(ArtNetError::NotDmx { .. })));
    }

    #[test]
    fn a_protocol_older_than_anything_still_in_use_is_refused() {
        // The version field is big-endian, unlike the opcode above it. A
        // receiver that read it the other way round would see 0x0E00 where 14
        // was sent and accept everything, including packets laid out
        // differently.
        let mut ancient = packet(0, 0, 1, &[1, 2]);
        ancient[10] = 0;
        ancient[11] = 12;
        assert!(matches!(
            parse(&ancient),
            Err(ArtNetError::OldProtocol { version: 12 })
        ));

        let good = packet(0, 0, 1, &[1, 2]);
        assert_eq!(good[10], 0, "the version's high byte comes first");
        assert_eq!(good[11], 14);
        assert!(parse(&good).is_ok());
    }

    #[test]
    fn something_else_on_the_wire_is_not_an_error_worth_stopping_for() {
        // A media server shares a network with consoles, nodes and whatever the
        // venue plugged in. Every one of these is ordinary.
        assert!(matches!(
            parse(b"not art-net at all............"),
            Err(ArtNetError::NotArtNet)
        ));
        assert!(matches!(parse(b"short"), Err(ArtNetError::TooShort { .. })));

        // ArtPoll, which a console sends to the same port and this server does
        // not answer.
        let mut poll = packet(0, 0, 1, &[0, 0]);
        poll[8] = 0x00;
        poll[9] = 0x20;
        assert!(matches!(
            parse(&poll),
            Err(ArtNetError::NotDmx { opcode: 0x2000 })
        ));
    }

    #[test]
    fn a_length_the_protocol_does_not_allow_is_refused() {
        // Odd lengths and oversized ones both mean a sender that has miscounted.
        // Accepting either would read a byte of something else as a channel.
        let mut odd = packet(0, 0, 1, &[1, 2, 3]);
        odd[16] = 0;
        odd[17] = 3;
        assert!(matches!(
            parse(&odd),
            Err(ArtNetError::IllegalLength { length: 3 })
        ));

        let mut huge = packet(0, 0, 1, &[1, 2]);
        huge[16] = 0x02;
        huge[17] = 0x02;
        assert!(matches!(
            parse(&huge),
            Err(ArtNetError::IllegalLength { .. })
        ));
    }

    #[test]
    fn a_packet_that_promises_more_than_it_carries_is_refused() {
        // Truncated on the wire, or crafted. Either way the bytes that are not
        // there are not zeroes.
        let mut truncated = packet(0, 0, 1, &[1, 2, 3, 4]);
        truncated.truncate(MIN_PACKET_LEN + 2);
        assert!(matches!(
            parse(&truncated),
            Err(ArtNetError::LengthMismatch { declared: 4, .. })
        ));
    }

    #[test]
    fn a_short_universe_is_accepted_as_what_it_says() {
        // A console sending 24 channels is not saying the other 488 are zero,
        // and a receiver that padded them would black out everything patched
        // above it.
        let bytes = packet(0, 0, 1, &[255; 24]);
        let parsed = parse(&bytes).expect("valid");
        assert_eq!(parsed.channels.len(), 24);
    }

    #[test]
    fn a_full_universe_is_accepted() {
        let bytes = packet(0, 0, 1, &[7; 512]);
        let parsed = parse(&bytes).expect("valid");
        assert_eq!(parsed.channels.len(), 512);
        assert_eq!(parsed.channels[511], 7);
    }
}
