//! Universes, and who is allowed to drive them.
//!
//! Two protocols feed one model. What a universe holds is 512 channels; what
//! makes it interesting is deciding, packet by packet, whose channels they are.
//!
//! # Arbitration
//!
//! Higher priority wins. At equal priority the source already holding the
//! universe keeps it, which is not arbitrary: two consoles at 100 sending 44
//! frames a second each would otherwise swap the universe between them forty-four
//! times a second, and an operator would see a wall flickering between two looks
//! with nothing in either console to explain it. The conflict is reported
//! instead.
//!
//! # Loss is a hold, not a blackout
//!
//! A source that stops being heard from releases the universe after E1.31's
//! timeout, and the channels stay exactly as they were. A media server that
//! blacked out because a network switch hiccupped would be worse than one that
//! held the last look until somebody noticed (house rule §17).
//!
//! A source that says it is terminating releases at once. That is a console
//! being switched off deliberately, which is a different thing from one whose
//! network died, and the difference is worth honouring.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Instant;

use ovs_types::config::DmxProtocols;

use crate::sacn::{Cid, DATA_LOSS_TIMEOUT, DEFAULT_PRIORITY};

/// How many channels a universe has.
pub const UNIVERSE_SIZE: usize = 512;

/// Which source a packet came from.
///
/// An sACN source is its CID, because that is what identifies it across
/// interfaces and distinguishes two sources behind one address. Art-Net has no
/// such identifier, so an Art-Net source is the address it came from — the best
/// available answer, and one an operator can still act on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceKey {
    /// An Art-Net sender, identified by where it sent from.
    ArtNet(IpAddr),
    /// An sACN sender, identified by its component identifier.
    Sacn(Cid),
}

impl SourceKey {
    /// Whether this source reaches the show, given which protocols do.
    ///
    /// The switch is here rather than at the socket on purpose: the
    /// datagrams still arrive and the status still says what is on the wire,
    /// so an operator who has taken a protocol off the show can still tell
    /// that from a cable that has come out (`DmxProtocols`).
    #[must_use]
    pub const fn reaches(&self, protocols: DmxProtocols) -> bool {
        match self {
            Self::ArtNet(_) => protocols.artnet,
            Self::Sacn(_) => protocols.sacn,
        }
    }
}

/// What happened to a packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// The universe took it.
    Applied,
    /// The source released the universe.
    Released,
    /// Another source holds this universe at a higher priority.
    Outranked {
        /// The priority of the source that holds it.
        holder: u8,
    },
    /// Another source holds it at the same priority.
    ///
    /// Reported rather than resolved: two consoles patched to one universe is a
    /// mistake in the rig, and the server's job is to say so clearly while
    /// keeping the picture steady.
    Contended {
        /// Who is holding it.
        holder: SourceKey,
    },
    /// Older than what has already been applied.
    ///
    /// Packets overtake each other on a busy network. Applying an old one would
    /// move a fader backwards for a frame.
    OutOfOrder,
    /// Preview data, which is not output.
    Preview,
}

/// One universe, and whoever is driving it.
#[derive(Debug, Clone)]
pub struct Universe {
    channels: [u8; UNIVERSE_SIZE],
    /// How many channels the holding source actually sends.
    length: usize,
    holder: Option<Holder>,
}

/// The source currently driving a universe.
#[derive(Debug, Clone)]
struct Holder {
    key: SourceKey,
    name: String,
    priority: u8,
    last_seen: Instant,
    last_sequence: Option<u8>,
}

impl Default for Universe {
    fn default() -> Self {
        Self::new()
    }
}

impl Universe {
    /// Creates an empty universe with no source.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            channels: [0; UNIVERSE_SIZE],
            length: 0,
            holder: None,
        }
    }

    /// Returns the channels a source has actually sent.
    ///
    /// Shorter than 512 when the source sends fewer, which is ordinary: a
    /// console patched to twenty channels sends twenty, and the rest are not
    /// zero — they are nothing.
    #[must_use]
    pub fn channels(&self) -> &[u8] {
        &self.channels[..self.length]
    }

    /// Returns who is driving this universe, if anybody.
    #[must_use]
    pub fn source(&self) -> Option<&SourceKey> {
        self.holder.as_ref().map(|holder| &holder.key)
    }

    /// Returns what the driving source calls itself.
    #[must_use]
    pub fn source_name(&self) -> Option<&str> {
        self.holder.as_ref().map(|holder| holder.name.as_str())
    }

    /// Returns the priority the driving source claims.
    #[must_use]
    pub fn priority(&self) -> Option<u8> {
        self.holder.as_ref().map(|holder| holder.priority)
    }

    /// Offers a packet to this universe.
    #[allow(
        clippy::too_many_arguments,
        reason = "one argument per protocol field, and combining them would hide what is being decided"
    )]
    fn offer(
        &mut self,
        key: SourceKey,
        name: &str,
        priority: u8,
        sequence: Option<u8>,
        terminated: bool,
        channels: &[u8],
        now: Instant,
    ) -> Accepted {
        // A holder nobody has heard from is not a holder any more. Checked here
        // rather than only on a timer, so a new source takes over the instant it
        // is entitled to rather than at the next sweep.
        if let Some(holder) = &self.holder {
            if now.saturating_duration_since(holder.last_seen) >= DATA_LOSS_TIMEOUT {
                self.holder = None;
            }
        }

        match &self.holder {
            Some(holder) if holder.key != key => {
                if priority > holder.priority {
                    // Take it. A higher-priority source is exactly what priority
                    // is for: a backup console taking over from a failed one.
                } else if priority < holder.priority {
                    return Accepted::Outranked {
                        holder: holder.priority,
                    };
                } else {
                    return Accepted::Contended {
                        holder: holder.key.clone(),
                    };
                }
            }
            Some(holder) => {
                // The same source. Out-of-order packets are dropped rather than
                // applied backwards.
                if let (Some(previous), Some(current)) = (holder.last_sequence, sequence) {
                    if is_old(previous, current) {
                        return Accepted::OutOfOrder;
                    }
                }
            }
            None => {}
        }

        if terminated {
            self.holder = None;
            // The channels stay. A console leaving does not mean the wall goes
            // dark; it means nothing is driving it any more.
            return Accepted::Released;
        }

        let length = channels.len().min(UNIVERSE_SIZE);
        self.channels[..length].copy_from_slice(&channels[..length]);
        self.length = length;
        self.holder = Some(Holder {
            key,
            name: name.to_owned(),
            priority,
            last_seen: now,
            last_sequence: sequence,
        });

        Accepted::Applied
    }

    /// Releases the universe if its source has gone quiet.
    ///
    /// Returns true when it did. The channels are left exactly as they were.
    fn expire(&mut self, now: Instant) -> bool {
        let gone = self.holder.as_ref().is_some_and(|holder| {
            now.saturating_duration_since(holder.last_seen) >= DATA_LOSS_TIMEOUT
        });
        if gone {
            self.holder = None;
        }
        gone
    }
}

/// Returns true when `current` is not newer than `previous`.
///
/// E1.31's rule, and the right one for Art-Net too: a difference in `-20..=0`
/// means the packet is a duplicate or arrived late. Anything further back is
/// treated as a source that restarted its numbering rather than a very old
/// packet, because a console rebooting must not be locked out for 236 frames.
fn is_old(previous: u8, current: u8) -> bool {
    let difference = i16::from(current) - i16::from(previous);
    let wrapped = if difference > 127 {
        difference - 256
    } else if difference < -128 {
        difference + 256
    } else {
        difference
    };
    (-20..=0).contains(&wrapped)
}

/// Every universe this receiver has heard about.
#[derive(Debug, Default)]
pub struct Universes {
    universes: BTreeMap<u16, Universe>,
}

impl Universes {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns one universe, if anything has ever arrived for it.
    #[must_use]
    pub fn get(&self, universe: u16) -> Option<&Universe> {
        self.universes.get(&universe)
    }

    /// Returns every universe with data, lowest first.
    pub fn iter(&self) -> impl Iterator<Item = (u16, &Universe)> {
        self.universes
            .iter()
            .map(|(number, universe)| (*number, universe))
    }

    /// Returns how many universes are being received.
    #[must_use]
    pub fn len(&self) -> usize {
        self.universes.len()
    }

    /// Returns true when nothing has arrived at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.universes.is_empty()
    }

    /// Offers an Art-Net packet.
    ///
    /// Art-Net carries no priority, so it is given E1.31's default. That is what
    /// lets the two protocols share a rig sensibly: an sACN source that wants to
    /// override an Art-Net one says so by sending above 100, which is exactly
    /// what it would do against another sACN source.
    pub fn accept_artnet(
        &mut self,
        from: IpAddr,
        packet: &crate::artnet::ArtDmx<'_>,
        now: Instant,
    ) -> Accepted {
        self.universes
            .entry(packet.port_address)
            .or_default()
            .offer(
                SourceKey::ArtNet(from),
                &from.to_string(),
                DEFAULT_PRIORITY,
                packet.sequence,
                false,
                packet.channels,
                now,
            )
    }

    /// Offers an sACN packet.
    pub fn accept_sacn(&mut self, packet: &crate::sacn::DataPacket<'_>, now: Instant) -> Accepted {
        // Turned away before the universe exists at all. A console previewing a
        // cue must not make a universe appear in what this server reports itself
        // as receiving, any more than it may reach a wall.
        if packet.preview {
            return Accepted::Preview;
        }

        self.universes.entry(packet.universe).or_default().offer(
            SourceKey::Sacn(packet.cid),
            &packet.source_name,
            packet.priority,
            Some(packet.sequence),
            packet.terminated,
            packet.channels,
            now,
        )
    }

    /// Releases every universe whose source has gone quiet.
    ///
    /// Returns the universes that were released, so a caller can say so once
    /// rather than noticing it silently.
    pub fn expire(&mut self, now: Instant) -> Vec<u16> {
        self.universes
            .iter_mut()
            .filter_map(|(number, universe)| universe.expire(now).then_some(*number))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    use super::*;
    use crate::artnet::ArtDmx;
    use crate::sacn::DataPacket;

    fn artnet(channels: &[u8], sequence: u8) -> ArtDmx<'_> {
        ArtDmx {
            port_address: 1,
            sequence: (sequence != 0).then_some(sequence),
            channels,
        }
    }

    fn sacn<'a>(cid: u8, priority: u8, sequence: u8, channels: &'a [u8]) -> DataPacket<'a> {
        DataPacket {
            cid: Cid([cid; 16]),
            source_name: format!("Console {cid}"),
            universe: 1,
            priority,
            sequence,
            terminated: false,
            preview: false,
            channels,
        }
    }

    fn address(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    #[test]
    fn a_packet_becomes_the_universes_channels() {
        let mut universes = Universes::new();
        let now = Instant::now();

        assert_eq!(
            universes.accept_artnet(address(1), &artnet(&[1, 2, 3, 4], 1), now),
            Accepted::Applied
        );

        let universe = universes.get(1).expect("received");
        assert_eq!(universe.channels(), &[1, 2, 3, 4]);
        assert_eq!(universe.source(), Some(&SourceKey::ArtNet(address(1))));
    }

    #[test]
    fn a_short_universe_stays_short() {
        // A console patched to twenty channels sends twenty. The rest are not
        // zero, they are nothing, and padding them would black out everything
        // patched above.
        let mut universes = Universes::new();
        universes.accept_artnet(address(1), &artnet(&[7; 20], 1), Instant::now());

        assert_eq!(universes.get(1).expect("received").channels().len(), 20);
    }

    #[test]
    fn a_higher_priority_source_takes_over() {
        // What priority is for: a backup console taking over from a failed one.
        let mut universes = Universes::new();
        let now = Instant::now();

        universes.accept_sacn(&sacn(1, 100, 1, &[10, 10]), now);
        assert_eq!(
            universes.accept_sacn(&sacn(2, 150, 1, &[20, 20]), now),
            Accepted::Applied
        );

        assert_eq!(universes.get(1).expect("received").channels(), &[20, 20]);
        assert_eq!(universes.get(1).expect("received").priority(), Some(150));
    }

    #[test]
    fn a_lower_priority_source_is_ignored_and_told_why() {
        let mut universes = Universes::new();
        let now = Instant::now();

        universes.accept_sacn(&sacn(1, 150, 1, &[10, 10]), now);

        assert_eq!(
            universes.accept_sacn(&sacn(2, 100, 1, &[20, 20]), now),
            Accepted::Outranked { holder: 150 }
        );
        assert_eq!(universes.get(1).expect("received").channels(), &[10, 10]);
    }

    #[test]
    fn two_sources_at_the_same_priority_do_not_take_turns() {
        // Two consoles at 100 sending 44 frames a second each would swap the
        // universe forty-four times a second, and an operator would see a wall
        // flickering between two looks with nothing in either console to
        // explain it. The first one keeps it and the conflict is reported.
        let mut universes = Universes::new();
        let now = Instant::now();

        universes.accept_sacn(&sacn(1, 100, 1, &[10, 10]), now);
        let outcome = universes.accept_sacn(&sacn(2, 100, 1, &[20, 20]), now);

        assert_eq!(
            outcome,
            Accepted::Contended {
                holder: SourceKey::Sacn(Cid([1; 16]))
            }
        );
        assert_eq!(universes.get(1).expect("received").channels(), &[10, 10]);
    }

    #[test]
    fn a_source_that_goes_quiet_releases_the_universe_and_the_channels_stay() {
        // A media server that blacked out because a switch hiccupped would be
        // worse than one that held the last look until somebody noticed.
        let mut universes = Universes::new();
        let start = Instant::now();
        universes.accept_sacn(&sacn(1, 100, 1, &[42, 42]), start);

        let later = start + DATA_LOSS_TIMEOUT + Duration::from_millis(1);
        assert_eq!(universes.expire(later), vec![1]);

        let universe = universes.get(1).expect("still there");
        assert_eq!(universe.source(), None, "nothing is driving it");
        assert_eq!(universe.channels(), &[42, 42], "and the look is held");
    }

    #[test]
    fn a_universe_nobody_holds_is_taken_by_whoever_sends_next() {
        // Even at a lower priority. The high-priority console is gone.
        let mut universes = Universes::new();
        let start = Instant::now();
        universes.accept_sacn(&sacn(1, 200, 1, &[10, 10]), start);

        let later = start + DATA_LOSS_TIMEOUT + Duration::from_millis(1);
        assert_eq!(
            universes.accept_sacn(&sacn(2, 50, 1, &[20, 20]), later),
            Accepted::Applied
        );
        assert_eq!(universes.get(1).expect("received").channels(), &[20, 20]);
    }

    #[test]
    fn a_source_that_says_it_is_leaving_releases_at_once() {
        // Different from going quiet. A console switched off deliberately
        // should not hold a universe for another two and a half seconds.
        let mut universes = Universes::new();
        let now = Instant::now();
        universes.accept_sacn(&sacn(1, 100, 1, &[10, 10]), now);

        let goodbye = DataPacket {
            terminated: true,
            ..sacn(1, 100, 2, &[10, 10])
        };
        assert_eq!(universes.accept_sacn(&goodbye, now), Accepted::Released);

        let universe = universes.get(1).expect("still there");
        assert_eq!(universe.source(), None);
        assert_eq!(universe.channels(), &[10, 10], "the look is held");
    }

    #[test]
    fn preview_data_never_reaches_a_universe() {
        // A console previewing a cue. Putting it on a wall would show an
        // audience what the operator is about to do.
        let mut universes = Universes::new();
        let preview = DataPacket {
            preview: true,
            ..sacn(1, 100, 1, &[99, 99])
        };

        assert_eq!(
            universes.accept_sacn(&preview, Instant::now()),
            Accepted::Preview
        );
        assert!(universes.get(1).is_none(), "nothing arrived");
    }

    #[test]
    fn a_packet_that_overtook_another_is_dropped() {
        // Packets overtake each other on a busy network. Applying an old one
        // would move a fader backwards for a frame, which on a dimmer is a
        // visible flicker.
        let mut universes = Universes::new();
        let now = Instant::now();
        universes.accept_sacn(&sacn(1, 100, 10, &[10, 10]), now);

        assert_eq!(
            universes.accept_sacn(&sacn(1, 100, 9, &[20, 20]), now),
            Accepted::OutOfOrder
        );
        assert_eq!(universes.get(1).expect("received").channels(), &[10, 10]);

        // And the next one in order is taken.
        assert_eq!(
            universes.accept_sacn(&sacn(1, 100, 11, &[30, 30]), now),
            Accepted::Applied
        );
    }

    #[test]
    fn a_sequence_that_wraps_is_not_mistaken_for_going_backwards() {
        let mut universes = Universes::new();
        let now = Instant::now();
        universes.accept_sacn(&sacn(1, 100, 254, &[10, 10]), now);

        for sequence in [255, 0, 1] {
            assert_eq!(
                universes.accept_sacn(&sacn(1, 100, sequence, &[sequence, sequence]), now),
                Accepted::Applied,
                "sequence {sequence}"
            );
        }
    }

    #[test]
    fn a_source_that_restarted_its_numbering_is_not_locked_out() {
        // A console rebooting starts again at 1. Treating that as 253 packets
        // of history would lock it out for nearly six seconds.
        let mut universes = Universes::new();
        let now = Instant::now();
        universes.accept_sacn(&sacn(1, 100, 200, &[10, 10]), now);

        assert_eq!(
            universes.accept_sacn(&sacn(1, 100, 1, &[20, 20]), now),
            Accepted::Applied
        );
    }

    #[test]
    fn a_sender_that_does_not_sequence_is_never_dropped_for_it() {
        // Art-Net's zero means "no sequence numbers", and a receiver that read
        // it as a number would drop every packet such a sender sent.
        let mut universes = Universes::new();
        let now = Instant::now();

        for value in [1_u8, 2, 3] {
            assert_eq!(
                universes.accept_artnet(address(1), &artnet(&[value, value], 0), now),
                Accepted::Applied
            );
        }
        assert_eq!(universes.get(1).expect("received").channels(), &[3, 3]);
    }

    #[test]
    fn art_net_and_sacn_share_a_rig_through_priority() {
        // Art-Net carries no priority, so it gets the standard's default. An
        // sACN source overrides it by sending above 100, which is exactly what
        // it would do against another sACN source.
        let mut universes = Universes::new();
        let now = Instant::now();

        universes.accept_artnet(address(1), &artnet(&[10, 10], 1), now);
        assert_eq!(universes.get(1).expect("received").priority(), Some(100));

        assert_eq!(
            universes.accept_sacn(&sacn(2, 150, 1, &[20, 20]), now),
            Accepted::Applied
        );
        assert_eq!(universes.get(1).expect("received").channels(), &[20, 20]);
    }

    #[test]
    fn universes_are_kept_apart() {
        let mut universes = Universes::new();
        let now = Instant::now();

        universes.accept_sacn(&sacn(1, 100, 1, &[1, 1]), now);
        universes.accept_sacn(
            &DataPacket {
                universe: 7,
                ..sacn(1, 100, 1, &[7, 7])
            },
            now,
        );

        assert_eq!(universes.len(), 2);
        assert_eq!(universes.get(1).expect("received").channels(), &[1, 1]);
        assert_eq!(universes.get(7).expect("received").channels(), &[7, 7]);
    }
}
