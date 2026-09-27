//! sACN receive: ANSI E1.31 data packets.
//!
//! A first-class implementation, not a compatibility shim. The
//! parts that make it first-class are the ones a shim leaves out: the source's
//! identity, its priority, and the difference between a source that stopped and
//! one that said it was stopping.
//!
//! # Three layers, each declaring its own length
//!
//! Root, framing and DMP, nested. Each carries a flags-and-length word whose low
//! twelve bits are the length of that layer including its own header. They are
//! checked rather than trusted: a packet whose inner layer claims to be longer
//! than the outer one is a packet that would have a receiver reading past what
//! arrived.
//!
//! # What is deliberately not here
//!
//! Synchronization packets and universe discovery. Both are optional, neither
//! changes what is on a wall, and a receiver that half-implemented
//! synchronization would hold frames waiting for a packet that is not coming.

use std::fmt;

/// The UDP port every E1.31 receiver listens on.
pub const SACN_PORT: u16 = 5568;

/// What every E1.17 packet carries after its preamble.
const ACN_IDENTIFIER: &[u8; 12] = b"ASC-E1.17\0\0\0";

/// Root layer vector for an E1.31 data packet.
const VECTOR_ROOT_DATA: u32 = 0x0000_0004;

/// Framing layer vector for a data packet.
const VECTOR_FRAMING_DATA: u32 = 0x0000_0002;

/// DMP layer vector for setting properties.
const VECTOR_DMP_SET_PROPERTY: u8 = 0x02;

/// The only address and data type an E1.31 data packet uses.
const DMP_ADDRESS_DATA_TYPE: u8 = 0xA1;

/// The priority a source gets when it does not say.
pub const DEFAULT_PRIORITY: u8 = 100;

/// The highest priority the standard allows.
pub const MAX_PRIORITY: u8 = 200;

/// How long a source may go quiet before it is considered gone.
///
/// E1.31's own figure. Shorter would drop a source that missed two frames on a
/// busy network; longer would hold a dead console's values on a wall.
pub const DATA_LOSS_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2_500);

/// A source's component identifier.
///
/// The thing that makes two consoles on one universe distinguishable. It is not
/// an address: a source that moves to another interface is the same source, and
/// two sources behind one address are still two.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cid(pub [u8; 16]);

impl fmt::Debug for Cid {
    /// Prints the RFC 4122 form, because that is how a console shows it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex: String = self.0.iter().map(|byte| format!("{byte:02x}")).collect();
        write!(
            formatter,
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
}

/// One E1.31 data packet, understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPacket<'a> {
    /// Which source sent it.
    pub cid: Cid,
    /// What that source calls itself, for an operator to recognise.
    pub source_name: String,
    /// The universe this is for.
    pub universe: u16,
    /// This source's priority for this universe, `0` to `200`.
    pub priority: u8,
    /// The sender's sequence number.
    pub sequence: u8,
    /// Whether the source is saying it has stopped.
    ///
    /// Different from going quiet: this is a source leaving deliberately, and a
    /// receiver can release the universe at once rather than holding it for the
    /// loss timeout.
    pub terminated: bool,
    /// Whether this is preview data rather than live output.
    ///
    /// A console previewing a cue sends this. Putting it on a wall would show an
    /// audience what the operator is about to do.
    pub preview: bool,
    /// The channel data, starting at channel 1.
    pub channels: &'a [u8],
}

/// Why a packet was not understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SacnError {
    /// Too short to hold the layers it needs.
    #[error("an E1.31 data packet is at least {MIN_PACKET_LEN} bytes and this is {length}")]
    TooShort {
        /// How long it was.
        length: usize,
    },

    /// Not an ACN packet.
    #[error("not an ACN packet")]
    NotAcn,

    /// ACN, but not an E1.31 data packet.
    ///
    /// Universe discovery and synchronization both arrive here, and neither is
    /// a fault.
    #[error("vector {vector:#010x} is not an E1.31 data packet")]
    NotData {
        /// The vector that arrived.
        vector: u32,
    },

    /// A layer that claims to be longer than what contains it.
    #[error("the {layer} layer claims {declared} bytes and {available} are there")]
    LayerOverruns {
        /// Which layer.
        layer: &'static str,
        /// What it claimed.
        declared: usize,
        /// What was there.
        available: usize,
    },

    /// A DMP layer that is not a property set, or not of the expected shape.
    #[error("the DMP layer is not one this receiver understands")]
    UnexpectedDmp,

    /// A start code other than zero.
    ///
    /// RDM and text packets share the wire and are not dimmer data.
    #[error("start code {start_code:#04x} is not DMX channel data")]
    NotChannelData {
        /// The start code that arrived.
        start_code: u8,
    },
}

/// The shortest packet that can carry all three layers and one channel.
pub const MIN_PACKET_LEN: usize = 126;

/// Parses one E1.31 data packet.
///
/// # Errors
///
/// Returns [`SacnError`] describing why the bytes were not usable. As with
/// Art-Net, every case is ordinary on a live network.
pub fn parse(packet: &[u8]) -> Result<DataPacket<'_>, SacnError> {
    if packet.len() < MIN_PACKET_LEN {
        return Err(SacnError::TooShort {
            length: packet.len(),
        });
    }

    // --- root layer ------------------------------------------------------
    if &packet[4..16] != ACN_IDENTIFIER {
        return Err(SacnError::NotAcn);
    }
    let root_length = layer_length(&packet[16..18]);
    // The root layer's length counts from byte 16, so the packet must hold that
    // much beyond it.
    if root_length + 16 > packet.len() {
        return Err(SacnError::LayerOverruns {
            layer: "root",
            declared: root_length + 16,
            available: packet.len(),
        });
    }

    let root_vector = u32::from_be_bytes([packet[18], packet[19], packet[20], packet[21]]);
    if root_vector != VECTOR_ROOT_DATA {
        return Err(SacnError::NotData {
            vector: root_vector,
        });
    }

    let mut cid = [0_u8; 16];
    cid.copy_from_slice(&packet[22..38]);

    // --- framing layer ---------------------------------------------------
    let framing_length = layer_length(&packet[38..40]);
    if framing_length + 38 > packet.len() {
        return Err(SacnError::LayerOverruns {
            layer: "framing",
            declared: framing_length + 38,
            available: packet.len(),
        });
    }

    let framing_vector = u32::from_be_bytes([packet[40], packet[41], packet[42], packet[43]]);
    if framing_vector != VECTOR_FRAMING_DATA {
        return Err(SacnError::NotData {
            vector: framing_vector,
        });
    }

    let source_name = read_name(&packet[44..108]);
    let priority = packet[108];
    let sequence = packet[111];
    let options = packet[112];
    let universe = u16::from_be_bytes([packet[113], packet[114]]);

    // --- DMP layer -------------------------------------------------------
    let dmp_length = layer_length(&packet[115..117]);
    if dmp_length + 115 > packet.len() {
        return Err(SacnError::LayerOverruns {
            layer: "DMP",
            declared: dmp_length + 115,
            available: packet.len(),
        });
    }

    if packet[117] != VECTOR_DMP_SET_PROPERTY
        || packet[118] != DMP_ADDRESS_DATA_TYPE
        // First property address 0 and increment 1. Anything else is a packet
        // this receiver has no business guessing at.
        || u16::from_be_bytes([packet[119], packet[120]]) != 0
        || u16::from_be_bytes([packet[121], packet[122]]) != 1
    {
        return Err(SacnError::UnexpectedDmp);
    }

    let value_count = usize::from(u16::from_be_bytes([packet[123], packet[124]]));
    // The count includes the start code, so a packet with no channels is still
    // a count of one.
    if value_count == 0 || 125 + value_count > packet.len() {
        return Err(SacnError::LayerOverruns {
            layer: "DMP property",
            declared: 125 + value_count,
            available: packet.len(),
        });
    }

    let start_code = packet[125];
    if start_code != 0 {
        return Err(SacnError::NotChannelData { start_code });
    }

    Ok(DataPacket {
        cid: Cid(cid),
        source_name,
        universe,
        // A source that says nothing gets the standard's default rather than
        // zero, which would mean "never wins".
        priority: if priority == 0 {
            DEFAULT_PRIORITY
        } else {
            priority.min(MAX_PRIORITY)
        },
        sequence,
        terminated: options & 0b0100_0000 != 0,
        preview: options & 0b1000_0000 != 0,
        channels: &packet[126..125 + value_count],
    })
}

/// Returns a layer's declared length.
///
/// The top four bits are flags and are not part of it.
fn layer_length(bytes: &[u8]) -> usize {
    usize::from(u16::from_be_bytes([bytes[0], bytes[1]]) & 0x0FFF)
}

/// Reads a source name from its fixed-width field.
///
/// Null-terminated inside sixty-four bytes, and not necessarily valid UTF-8: a
/// console's name field is whatever somebody typed into it. Invalid sequences
/// are replaced rather than refused, because a name is for an operator to read
/// and never for anything to decide on.
fn read_name(field: &[u8]) -> String {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a data packet the way a console would.
    struct Builder {
        cid: [u8; 16],
        name: &'static str,
        universe: u16,
        priority: u8,
        sequence: u8,
        options: u8,
        start_code: u8,
        channels: Vec<u8>,
    }

    impl Default for Builder {
        fn default() -> Self {
            Self {
                cid: [0xAB; 16],
                name: "Console 1",
                universe: 1,
                priority: DEFAULT_PRIORITY,
                sequence: 1,
                options: 0,
                start_code: 0,
                channels: vec![1, 2, 3, 4],
            }
        }
    }

    impl Builder {
        fn build(&self) -> Vec<u8> {
            let value_count = self.channels.len() + 1;
            let dmp_length = 10 + value_count;
            let framing_length = 77 + dmp_length;
            let root_length = 22 + framing_length;

            let mut bytes = Vec::new();
            bytes.extend_from_slice(&0x0010_u16.to_be_bytes()); // preamble
            bytes.extend_from_slice(&0x0000_u16.to_be_bytes()); // postamble
            bytes.extend_from_slice(ACN_IDENTIFIER);
            bytes.extend_from_slice(&(0x7000 | root_length as u16).to_be_bytes());
            bytes.extend_from_slice(&VECTOR_ROOT_DATA.to_be_bytes());
            bytes.extend_from_slice(&self.cid);

            bytes.extend_from_slice(&(0x7000 | framing_length as u16).to_be_bytes());
            bytes.extend_from_slice(&VECTOR_FRAMING_DATA.to_be_bytes());
            let mut name = [0_u8; 64];
            name[..self.name.len()].copy_from_slice(self.name.as_bytes());
            bytes.extend_from_slice(&name);
            bytes.push(self.priority);
            bytes.extend_from_slice(&0_u16.to_be_bytes()); // synchronization address
            bytes.push(self.sequence);
            bytes.push(self.options);
            bytes.extend_from_slice(&self.universe.to_be_bytes());

            bytes.extend_from_slice(&(0x7000 | dmp_length as u16).to_be_bytes());
            bytes.push(VECTOR_DMP_SET_PROPERTY);
            bytes.push(DMP_ADDRESS_DATA_TYPE);
            bytes.extend_from_slice(&0_u16.to_be_bytes()); // first address
            bytes.extend_from_slice(&1_u16.to_be_bytes()); // increment
            bytes.extend_from_slice(&(value_count as u16).to_be_bytes());
            bytes.push(self.start_code);
            bytes.extend_from_slice(&self.channels);

            bytes
        }
    }

    #[test]
    fn a_console_packet_decodes_to_its_universe_source_and_channels() {
        let packet = Builder::default().build();
        let parsed = parse(&packet).expect("a valid packet");

        assert_eq!(parsed.universe, 1);
        assert_eq!(parsed.source_name, "Console 1");
        assert_eq!(parsed.cid, Cid([0xAB; 16]));
        assert_eq!(parsed.priority, DEFAULT_PRIORITY);
        assert_eq!(parsed.channels, &[1, 2, 3, 4]);
        assert!(!parsed.terminated);
        assert!(!parsed.preview);
    }

    #[test]
    fn a_source_is_its_identifier_and_not_its_address() {
        // Two consoles behind one address are two sources, and a source that
        // moves interface is still one. Only the CID says which.
        let first = Builder {
            cid: [1; 16],
            ..Builder::default()
        }
        .build();
        let second = Builder {
            cid: [2; 16],
            ..Builder::default()
        }
        .build();

        assert_ne!(
            parse(&first).expect("valid").cid,
            parse(&second).expect("valid").cid
        );
    }

    #[test]
    fn a_priority_of_zero_becomes_the_standard_default() {
        // A source that says nothing means "normal", not "never wins".
        let packet = Builder {
            priority: 0,
            ..Builder::default()
        }
        .build();
        assert_eq!(parse(&packet).expect("valid").priority, DEFAULT_PRIORITY);
    }

    #[test]
    fn a_priority_above_the_standards_maximum_is_held_at_it() {
        let packet = Builder {
            priority: 255,
            ..Builder::default()
        }
        .build();
        assert_eq!(parse(&packet).expect("valid").priority, MAX_PRIORITY);
    }

    #[test]
    fn a_terminating_source_says_so_rather_than_going_quiet() {
        // The difference between a console being switched off deliberately and
        // one whose network died. A receiver can release the universe at once
        // for the first and has to wait out the timeout for the second.
        let packet = Builder {
            options: 0b0100_0000,
            ..Builder::default()
        }
        .build();
        let parsed = parse(&packet).expect("valid");

        assert!(parsed.terminated);
        assert!(!parsed.preview);
    }

    #[test]
    fn preview_data_is_marked_rather_than_mistaken_for_output() {
        // A console previewing a cue sends this. Putting it on a wall would
        // show an audience what the operator is about to do.
        let packet = Builder {
            options: 0b1000_0000,
            ..Builder::default()
        }
        .build();
        let parsed = parse(&packet).expect("valid");

        assert!(parsed.preview);
        assert!(!parsed.terminated);
    }

    #[test]
    fn a_start_code_other_than_zero_is_not_channel_data() {
        // RDM and text packets share the wire with dimmer data.
        let packet = Builder {
            start_code: 0xCC,
            ..Builder::default()
        }
        .build();
        assert!(matches!(
            parse(&packet),
            Err(SacnError::NotChannelData { start_code: 0xCC })
        ));
    }

    #[test]
    fn something_else_on_the_wire_is_not_a_fault() {
        assert!(matches!(parse(b"short"), Err(SacnError::TooShort { .. })));

        let mut not_acn = Builder::default().build();
        not_acn[4] = b'X';
        assert!(matches!(parse(&not_acn), Err(SacnError::NotAcn)));

        // Universe discovery, which shares the port and is not a data packet.
        let mut discovery = Builder::default().build();
        discovery[40..44].copy_from_slice(&0x0000_0008_u32.to_be_bytes());
        assert!(matches!(parse(&discovery), Err(SacnError::NotData { .. })));
    }

    #[test]
    fn a_layer_that_claims_more_than_arrived_is_refused() {
        // Truncated on the wire, or crafted. A receiver that trusted the
        // declared length would read past what it was given.
        let mut packet = Builder::default().build();
        packet[123] = 0xFF;
        packet[124] = 0xFF;
        assert!(matches!(
            parse(&packet),
            Err(SacnError::LayerOverruns { .. })
        ));
    }

    #[test]
    fn a_dmp_layer_of_another_shape_is_refused_rather_than_guessed_at() {
        let mut packet = Builder::default().build();
        packet[119] = 0;
        packet[120] = 5; // a first property address that is not zero
        assert!(matches!(parse(&packet), Err(SacnError::UnexpectedDmp)));
    }

    #[test]
    fn a_name_that_is_not_valid_text_is_still_a_name() {
        // A console's name field is whatever somebody typed into it, and a name
        // is for an operator to read rather than for anything to decide on.
        let mut packet = Builder::default().build();
        packet[44] = 0xFF;
        let parsed = parse(&packet).expect("valid");
        assert!(!parsed.source_name.is_empty());
    }

    #[test]
    fn a_full_universe_arrives_whole() {
        let packet = Builder {
            channels: vec![9; 512],
            ..Builder::default()
        }
        .build();
        let parsed = parse(&packet).expect("valid");

        assert_eq!(parsed.channels.len(), 512);
        assert_eq!(parsed.channels[511], 9);
    }

    #[test]
    fn an_identifier_prints_the_way_a_console_shows_it() {
        let cid = Cid([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ]);
        assert_eq!(format!("{cid:?}"), "01234567-89ab-cdef-0123-456789abcdef");
    }
}
