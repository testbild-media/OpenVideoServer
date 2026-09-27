//! The patch, applied: universes in, parameters out.
//!
//! This is the whole of what a lighting console can do to a show. A patch entry
//! names a personality, a universe and a start address; the personality says
//! what each of its channels means; and everything a channel can mean is one
//! variant of [`Parameter`]. Nothing else in the server reads DMX, and nothing
//! reached from here writes configuration — a console driving a dimmer at 44 Hz
//! must never rewrite what tomorrow's show opens with.
//!
//! # Only what changed
//!
//! A console re-sends every channel forty-four times a second, and almost none
//! of them are different from the last time. Emitting all of them would put
//! thousands of no-op parameter changes a second through the state, each one
//! taking a lock and bumping a sequence number that clients would then have to
//! chew through. So a channel is emitted when its value changes, and otherwise
//! not mentioned.
//!
//! # Moments and values
//!
//! Three parameters are moments rather than values: a tempo tap, a phase reset
//! and a layer trigger. Those are emitted on the rising edge of their channel
//! only. A console holding a trigger channel high is a finger resting on a
//! button, not a request to restart the clip forty-four times a second — and the
//! difference is visible, because the second reading pins the layer to its first
//! frame for as long as the finger is down.
//!
//! A channel that is *already* high the first time a fixture is seen is adopted
//! without firing, for the same reason: a console left with a trigger button
//! down while this server boots should not start the show by retriggering every
//! layer it can reach.
//!
//! # A universe nobody is driving is not a control source
//!
//! Channels are held when a source stops being heard from, because a wall that
//! went dark for a network hiccup would be worse than one holding its last look
//! ([`crate::universe`]). What is held belongs to the fixtures that were already
//! reading it, though, and not to a fixture patched afterwards: the held
//! channels are usually zero, and zero is a layer at no opacity, on the black
//! slot, scaled to nothing. A wall that went black the moment somebody saved a
//! patch, with no console in the building, is what that would look like at four
//! in the afternoon.
//!
//! So a fixture read for the first time from a universe with no source takes
//! nothing from it, and remembers nothing either — the first source to come up
//! is then read in full, rather than against a frame nobody sent.

use std::collections::{BTreeMap, BTreeSet};

use ovs_types::config::{definition, ChannelValue, DmxProtocols, FixtureInstance, Parameter};
use ovs_types::id::FixtureInstanceId;

use crate::universe::{Universe, Universes};

/// One parameter change, ready for the parameter registry.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// What to change.
    pub parameter: Parameter,
    /// What to change it to. For a momentary parameter this is always
    /// `Flag(true)`, because the moment is the message.
    pub value: ChannelValue,
}

/// Something wrong with a patch entry.
///
/// Reported rather than guessed around. Every one of these means an operator has
/// a fixture in a console that this server cannot honour, and the only useful
/// thing to do is say which one and why — silently driving something near enough
/// would be worse than driving nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchProblem {
    /// The patch names a personality this build does not publish.
    ///
    /// Usually a version: definitions are immutable, so a console programmed
    /// against version 2 must not be fed into version 1's channel layout.
    UnknownDefinition {
        /// Which patch entry.
        fixture: FixtureInstanceId,
        /// What it asked for.
        type_id: String,
        /// Which version it asked for.
        version: u32,
    },
    /// The patch and the personality disagree about how many channels it takes.
    ///
    /// The console has the real answer physically programmed into its show file,
    /// so a disagreement means every fixture patched after this one is offset.
    FootprintDisagrees {
        /// Which patch entry.
        fixture: FixtureInstanceId,
        /// What the patch says.
        patched: u16,
        /// What the personality says.
        published: u16,
    },
    /// The personality does not fit what the patch entry points at.
    ///
    /// A layer personality aimed at the master would give one layer's opacity
    /// fader the master dimmer.
    WrongTarget {
        /// Which patch entry.
        fixture: FixtureInstanceId,
        /// The personality that does not fit.
        type_id: String,
    },
    /// The universe arrived shorter than the fixture needs.
    ///
    /// Legal in both protocols — a console sending twenty channels is sending
    /// twenty — and so not an error on the wire. It is still a fixture that
    /// cannot be read, and the missing channels are not zeroes.
    Truncated {
        /// Which patch entry.
        fixture: FixtureInstanceId,
        /// Which universe.
        universe: u16,
        /// The highest channel the fixture needs.
        needed: u32,
        /// How many the universe carries.
        received: usize,
    },
}

/// What one pass over the patch produced.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Collected {
    /// Parameter changes, in patch order.
    pub updates: Vec<Update>,
    /// Patch entries that could not be read, each reported once.
    pub problems: Vec<PatchProblem>,
}

impl Collected {
    /// Returns true when there is nothing to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.problems.is_empty()
    }
}

/// Turns universes into parameter changes, remembering what it already sent.
#[derive(Debug, Default)]
pub struct Mapper {
    /// The last channels read for each fixture, fixture-relative.
    seen: BTreeMap<FixtureInstanceId, Vec<u8>>,
    /// Patch entries already complained about.
    complained: BTreeSet<FixtureInstanceId>,
}

impl Mapper {
    /// Creates a mapper that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets every channel it has read.
    ///
    /// Called when the show underneath changes — a show opening, live state
    /// being restored — so that the next pass hands the console's current values
    /// back to a show that no longer has them. Without it the console would
    /// appear dead until an operator happened to move something, because nothing
    /// on the wire had changed.
    ///
    /// Triggers are not re-fired by this: a fixture being seen for the first
    /// time adopts its momentary channels rather than firing them.
    pub fn forget(&mut self) {
        self.seen.clear();
        self.complained.clear();
    }

    /// Reads the patch against what the universes currently hold.
    pub fn collect(
        &mut self,
        patch: &[FixtureInstance],
        universes: &Universes,
        protocols: DmxProtocols,
    ) -> Collected {
        let mut collected = Collected::default();

        for fixture in patch {
            if !fixture.enabled {
                // Parked. Forgotten too, so that re-enabling it takes the
                // console's values rather than comparing against whatever the
                // channels held whenever it was switched off.
                self.seen.remove(&fixture.id);
                self.complained.remove(&fixture.id);
                continue;
            }

            // A universe held by a protocol somebody has switched off is
            // read as silent. Not skipped for being absent — the packets
            // are still arriving and the status still says so — but not
            // allowed to move anything (owner, 2026-09-23).
            let held_by_something_off = universes
                .get(fixture.universe)
                .and_then(Universe::source)
                .is_some_and(|source| !source.reaches(protocols));
            if held_by_something_off {
                self.seen.remove(&fixture.id);
                continue;
            }

            match self.read(fixture, universes, &mut collected.updates) {
                Ok(()) => {
                    self.complained.remove(&fixture.id);
                }
                Err(Unreadable::Silent) => {}
                Err(Unreadable::Problem(problem)) => {
                    // Once. A broken patch entry is broken forty-four times a
                    // second, and a log that said so would bury everything else.
                    if self.complained.insert(fixture.id) {
                        collected.problems.push(problem);
                    }
                }
            }
        }

        collected
    }

    /// Reads one fixture.
    fn read(
        &mut self,
        fixture: &FixtureInstance,
        universes: &Universes,
        updates: &mut Vec<Update>,
    ) -> Result<(), Unreadable> {
        let Some(published) = definition(&fixture.type_id, fixture.version) else {
            return Err(Unreadable::Problem(PatchProblem::UnknownDefinition {
                fixture: fixture.id,
                type_id: fixture.type_id.clone(),
                version: fixture.version,
            }));
        };

        if published.footprint != fixture.footprint {
            return Err(Unreadable::Problem(PatchProblem::FootprintDisagrees {
                fixture: fixture.id,
                patched: fixture.footprint,
                published: published.footprint,
            }));
        }

        if !published.drives(fixture.target) {
            return Err(Unreadable::Problem(PatchProblem::WrongTarget {
                fixture: fixture.id,
                type_id: fixture.type_id.clone(),
            }));
        }

        // Nothing has arrived for this universe. Not a problem: a patch may name
        // universes a console has not been switched on for yet, and saying so
        // every frame would make the real problems unfindable.
        let Some(universe) = universes.get(fixture.universe) else {
            return Err(Unreadable::Silent);
        };
        let channels = universe.channels();

        let first = u32::from(fixture.start_address);
        let needed = first + u32::from(fixture.footprint) - 1;
        if first == 0 || needed > channels.len() as u32 {
            return Err(Unreadable::Problem(PatchProblem::Truncated {
                fixture: fixture.id,
                universe: fixture.universe,
                needed,
                received: channels.len(),
            }));
        }
        let slice = &channels[(first - 1) as usize..needed as usize];

        // Nothing is driving this universe, and this fixture has never been read
        // from it. What the channels hold is a look a source left behind, and a
        // fixture patched afterwards has no claim on it — see this module's
        // documentation. Nothing is remembered either, so that the first source
        // to come up is read in full rather than against a frame nobody sent.
        if universe.source().is_none() && !self.seen.contains_key(&fixture.id) {
            return Err(Unreadable::Silent);
        }

        let previous = self.seen.get(&fixture.id);
        for channel in published.channels {
            let Some(value) = channel.encoding.decode(slice, channel.offset) else {
                continue;
            };
            let Some(parameter) = channel.parameter.bind(fixture.target) else {
                continue;
            };
            let before = previous.and_then(|bytes| channel.encoding.decode(bytes, channel.offset));

            if parameter.is_momentary() {
                // The rising edge, and only when there was something to rise
                // from: a fixture seen for the first time adopts its momentary
                // channels rather than firing them.
                if value == ChannelValue::Flag(true) && before == Some(ChannelValue::Flag(false)) {
                    updates.push(Update {
                        parameter,
                        value: ChannelValue::Flag(true),
                    });
                }
            } else if before != Some(value) {
                updates.push(Update { parameter, value });
            }
        }

        self.seen.insert(fixture.id, slice.to_vec());
        Ok(())
    }
}

/// Why a fixture produced nothing.
enum Unreadable {
    /// Ordinary, and not worth saying.
    ///
    /// Either nothing has ever arrived for the universe, or nothing is driving
    /// it and this fixture has not read it before. Both are a patch naming a
    /// universe no console is on, which is what an afternoon in a venue looks
    /// like.
    Silent,
    /// Worth saying once.
    Problem(PatchProblem),
}

#[cfg(test)]
mod tests {
    use ovs_types::config::DmxProtocols;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::{Duration, Instant};

    use ovs_types::config::patch::FixtureTarget;
    use ovs_types::config::{LAYER_V1, MASTER_V1};
    use ovs_types::id::LayerId;

    use super::*;
    use crate::artnet::ArtDmx;
    use crate::sacn::DATA_LOSS_TIMEOUT;

    fn universes_holding(channels: &[u8]) -> Universes {
        let mut universes = Universes::new();
        universes.accept_artnet(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &ArtDmx {
                port_address: 1,
                sequence: None,
                channels,
            },
            Instant::now(),
        );
        universes
    }

    fn master_fixture(start_address: u16) -> FixtureInstance {
        FixtureInstance {
            id: FixtureInstanceId::new(),
            type_id: MASTER_V1.type_id.to_owned(),
            version: MASTER_V1.version,
            target: FixtureTarget::Master,
            universe: 1,
            start_address,
            footprint: MASTER_V1.footprint,
            enabled: true,
        }
    }

    fn layer_fixture(start_address: u16, layer_id: LayerId) -> FixtureInstance {
        FixtureInstance {
            id: FixtureInstanceId::new(),
            type_id: LAYER_V1.type_id.to_owned(),
            version: LAYER_V1.version,
            target: FixtureTarget::Layer { layer_id },
            universe: 1,
            start_address,
            footprint: LAYER_V1.footprint,
            enabled: true,
        }
    }

    /// A master fixture's six channels, starting at channel 1.
    ///
    /// The owner's layout of 2026-09-22: dimmer, freeze, tap, sync, scale,
    /// crossfade, every one of them eight bits.
    fn master_channels(dimmer: u8, tap: u8, freeze: u8) -> Vec<u8> {
        let mut channels = vec![0_u8; MASTER_V1.footprint as usize];
        channels[0] = dimmer;
        channels[1] = freeze;
        channels[2] = tap;
        channels
    }

    fn number(value: ChannelValue) -> f64 {
        match value {
            ChannelValue::Number(number) => number,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    fn find(collected: &Collected, parameter: Parameter) -> Option<ChannelValue> {
        collected
            .updates
            .iter()
            .find(|update| update.parameter == parameter)
            .map(|update| update.value)
    }

    #[test]
    fn a_master_fixture_turns_its_channels_into_master_parameters() {
        let patch = vec![master_fixture(1)];
        let universes = universes_holding(&master_channels(255, 0, 255));
        let collected = Mapper::new().collect(&patch, &universes, DmxProtocols::default());

        assert_eq!(
            number(find(&collected, Parameter::MasterDimmer).expect("dimmer")),
            1.0
        );
        assert_eq!(
            find(&collected, Parameter::MasterFreeze),
            Some(ChannelValue::Flag(true))
        );
        assert!(collected.problems.is_empty());
    }

    #[test]
    fn a_fixture_reads_from_its_own_start_address() {
        // The whole point of a start address. Reading from channel 1 regardless
        // would work perfectly on every rig with one fixture in it.
        let mut channels = vec![0_u8; 100];
        channels[19] = 0xFF;
        channels[20] = 0xFF;

        let patch = vec![master_fixture(20)];
        let collected = Mapper::new().collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );

        assert_eq!(
            number(find(&collected, Parameter::MasterDimmer).expect("dimmer")),
            1.0
        );
    }

    #[test]
    fn only_what_changed_is_emitted() {
        // A console re-sends every channel forty-four times a second. Passing
        // all of them on would put thousands of no-op changes a second through
        // the state, each one bumping a sequence number that every client then
        // has to chew through.
        let patch = vec![master_fixture(1)];
        let universes = universes_holding(&master_channels(128, 0, 0));
        let mut mapper = Mapper::new();

        assert!(!mapper
            .collect(&patch, &universes, DmxProtocols::default())
            .updates
            .is_empty());
        assert!(
            mapper
                .collect(&patch, &universes, DmxProtocols::default())
                .is_empty(),
            "the same packet again says nothing new"
        );

        let moved = universes_holding(&master_channels(129, 0, 0));
        let collected = mapper.collect(&patch, &moved, DmxProtocols::default());
        assert_eq!(collected.updates.len(), 1, "one fader moved, one change");
        assert_eq!(collected.updates[0].parameter, Parameter::MasterDimmer);
    }

    #[test]
    fn a_trigger_fires_on_the_edge_and_not_while_it_is_held() {
        // A finger resting on a button is not a request forty-four times a
        // second. Tested on the master's tap, because the owner's layer layout
        // has no trigger channel at all (2026-09-22): selecting a slot is what
        // plays it, and a *re*trigger therefore cannot be sent from a desk —
        // re-selecting the slot that is running does not move the channel.
        let patch = vec![master_fixture(1)];
        let mut mapper = Mapper::new();

        let mut channels = master_channels(255, 0, 0);
        mapper.collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );

        channels[2] = 255;
        let pressed = mapper.collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );
        assert_eq!(
            find(&pressed, Parameter::MasterTapTempo),
            Some(ChannelValue::Flag(true))
        );

        let held = mapper.collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );
        assert_eq!(
            find(&held, Parameter::MasterTapTempo),
            None,
            "a held button fires once"
        );

        channels[2] = 0;
        mapper.collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );
        channels[2] = 255;
        let again = mapper.collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );
        assert_eq!(
            find(&again, Parameter::MasterTapTempo),
            Some(ChannelValue::Flag(true)),
            "and again after it is released"
        );
    }

    #[test]
    fn a_trigger_already_high_when_the_server_starts_does_not_fire() {
        // A console left with a button down while this machine boots should not
        // start the show by retriggering every layer it can reach.
        let layer = LayerId::new();
        let patch = vec![layer_fixture(1, layer)];

        let mut channels = vec![0_u8; LAYER_V1.footprint as usize];
        channels[3] = 255;

        let collected = Mapper::new().collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );
        assert_eq!(
            find(&collected, Parameter::LayerTrigger { layer_id: layer }),
            None
        );
    }

    #[test]
    fn each_layer_fixture_drives_its_own_layer() {
        let first = LayerId::new();
        let second = LayerId::new();
        let patch = vec![layer_fixture(1, first), layer_fixture(20, second)];

        let mut channels = vec![0_u8; 40];
        channels[0] = 0xFF; // the first layer's opacity, coarse
        channels[19] = 0x80; // the second layer's opacity, coarse

        let collected = Mapper::new().collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );

        let one =
            number(find(&collected, Parameter::LayerOpacity { layer_id: first }).expect("first"));
        let two =
            number(find(&collected, Parameter::LayerOpacity { layer_id: second }).expect("second"));
        assert!(one > 0.99, "{one}");
        assert!((0.4..0.6).contains(&two), "{two}");
    }

    #[test]
    fn the_stepped_channels_decode_to_steps_and_grids() {
        let layer = LayerId::new();
        let patch = vec![layer_fixture(1, layer)];

        let mut channels = vec![0_u8; LAYER_V1.footprint as usize];
        // Slot Playback. Three values a slot, Black at the bottom, so slot 2 is
        // 7 to 9 and a desk one value out either way still lands on it.
        channels[1] = 8;

        let collected = Mapper::new().collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );

        assert_eq!(
            find(&collected, Parameter::LayerSlot { layer_id: layer }),
            Some(ChannelValue::Index(2)),
            "the middle of slot 2's band"
        );

        // And the bottom of the channel is Black rather than slot 1.
        channels[1] = 3;
        let collected = Mapper::new().collect(
            &patch,
            &universes_holding(&channels),
            DmxProtocols::default(),
        );
        assert_eq!(
            find(&collected, Parameter::LayerSlot { layer_id: layer }),
            Some(ChannelValue::Index(0))
        );
    }

    #[test]
    fn the_masters_scale_ladder_puts_unity_in_the_middle_of_the_channel() {
        // Six bands over 256 values, 42 or 43 apiece, with `1x` fourth — so
        // half travel is the show's own tempo and 128 is the one value on this
        // channel an operator can find without counting (owner, 2026-09-23).
        let patch = vec![master_fixture(1)];
        let mut channels = master_channels(255, 0, 0);

        for (value, wanted) in [(0, 0.125), (128, 1.0), (170, 1.0), (255, 4.0)] {
            channels[4] = value;
            let collected = Mapper::new().collect(
                &patch,
                &universes_holding(&channels),
                DmxProtocols::default(),
            );
            let read =
                number(find(&collected, Parameter::MasterSpeedScale).expect("the scale channel"));
            assert!(
                (read - wanted).abs() < f64::EPSILON,
                "DMX {value} read as {read}, wanted {wanted}"
            );
        }
    }

    #[test]
    fn a_parked_fixture_is_not_mapped_and_takes_the_console_back_when_it_returns() {
        let mut patch = vec![master_fixture(1)];
        let universes = universes_holding(&master_channels(64, 0, 0));
        let mut mapper = Mapper::new();

        patch[0].enabled = false;
        assert!(mapper
            .collect(&patch, &universes, DmxProtocols::default())
            .is_empty());

        patch[0].enabled = true;
        assert!(
            !mapper
                .collect(&patch, &universes, DmxProtocols::default())
                .updates
                .is_empty(),
            "un-parking adopts what the console is holding"
        );
    }

    #[test]
    fn a_personality_this_build_does_not_publish_is_reported_once() {
        // Definitions are immutable, so a console programmed against version 2
        // must not be poured into version 1's channel layout: it would move the
        // wrong things, slowly enough that nobody would connect the two.
        let mut patch = vec![master_fixture(1)];
        patch[0].version = 2;
        let universes = universes_holding(&master_channels(255, 0, 0));
        let mut mapper = Mapper::new();

        let first = mapper.collect(&patch, &universes, DmxProtocols::default());
        assert!(first.updates.is_empty());
        assert!(matches!(
            first.problems.as_slice(),
            [PatchProblem::UnknownDefinition { version: 2, .. }]
        ));

        assert!(
            mapper
                .collect(&patch, &universes, DmxProtocols::default())
                .problems
                .is_empty(),
            "a broken patch entry is broken forty-four times a second"
        );
    }

    #[test]
    fn a_footprint_that_disagrees_with_the_personality_is_refused() {
        // The console has the real answer physically programmed into its show
        // file. A disagreement means every fixture patched after this one is
        // offset, so reading this one anyway would be the smallest part of the
        // problem.
        let mut patch = vec![master_fixture(1)];
        patch[0].footprint = MASTER_V1.footprint + 2;
        let universes = universes_holding(&master_channels(255, 0, 0));

        let collected = Mapper::new().collect(&patch, &universes, DmxProtocols::default());
        assert!(collected.updates.is_empty());
        assert!(matches!(
            collected.problems.as_slice(),
            [PatchProblem::FootprintDisagrees { published, .. }]
                if *published == MASTER_V1.footprint
        ));
    }

    #[test]
    fn a_layer_personality_aimed_at_the_master_is_refused() {
        // It would give one layer's opacity fader the master dimmer.
        let mut patch = vec![layer_fixture(1, LayerId::new())];
        patch[0].target = FixtureTarget::Master;
        let universes = universes_holding(&[255_u8; 40]);

        let collected = Mapper::new().collect(&patch, &universes, DmxProtocols::default());
        assert!(collected.updates.is_empty());
        assert!(matches!(
            collected.problems.as_slice(),
            [PatchProblem::WrongTarget { .. }]
        ));
    }

    #[test]
    fn a_universe_shorter_than_the_fixture_is_reported_rather_than_guessed() {
        // Legal on the wire — a console sending twenty channels is sending
        // twenty — and still a fixture that cannot be read. The channels that
        // are not there are not zeroes.
        let patch = vec![master_fixture(200)];
        let universes = universes_holding(&[1, 2, 3, 4]);

        let collected = Mapper::new().collect(&patch, &universes, DmxProtocols::default());
        assert!(collected.updates.is_empty());
        assert!(matches!(
            collected.problems.as_slice(),
            [PatchProblem::Truncated {
                universe: 1,
                needed: 205,
                received: 4,
                ..
            }]
        ));
    }

    #[test]
    fn a_fixture_patched_to_a_universe_nobody_is_driving_takes_nothing_from_it() {
        // The four-o'clock case. A desk was on this universe earlier and has
        // gone; the channels are held, and they are zeroes. A layer that
        // adopted them would go to no opacity, the black slot and no scale at
        // the moment an operator saved a patch, with nothing in the building
        // sending anything.
        let patch = vec![master_fixture(1)];
        let mut universes = universes_holding(&master_channels(0, 0, 0));
        universes.expire(Instant::now() + DATA_LOSS_TIMEOUT + Duration::from_millis(1));
        assert!(
            universes.get(1).expect("still there").source().is_none(),
            "the look is held, but nothing is driving it"
        );

        assert!(Mapper::new()
            .collect(&patch, &universes, DmxProtocols::default())
            .is_empty());
    }

    #[test]
    fn a_source_coming_up_on_a_held_universe_hands_the_whole_fixture_over() {
        // And the values it sends are the show's values, even when they are
        // byte for byte what the universe was already holding. Comparing
        // against the held frame would leave the desk at one look and the wall
        // at another, with nothing in the desk to explain it.
        let patch = vec![master_fixture(1)];
        let channels = master_channels(64, 0, 0);
        let start = Instant::now();

        let mut universes = universes_holding(&channels);
        universes.expire(start + DATA_LOSS_TIMEOUT + Duration::from_millis(1));

        let mut mapper = Mapper::new();
        assert!(mapper
            .collect(&patch, &universes, DmxProtocols::default())
            .is_empty());

        // The same frame, from a source that is now on the wire.
        let awake = universes_holding(&channels);
        let collected = mapper.collect(&patch, &awake, DmxProtocols::default());
        let dimmer = number(find(&collected, Parameter::MasterDimmer).expect("dimmer"));
        assert!(
            (dimmer - 0.25).abs() < 0.001,
            "the desk's own value, not a delta from a frame nobody sent: {dimmer}"
        );
    }

    #[test]
    fn a_fixture_already_reading_a_universe_holds_its_look_when_the_source_goes() {
        // The other half of the same rule. Loss is a hold, not a blackout: a
        // fixture that was being driven keeps what it was driven to, and the
        // silence says nothing new.
        let patch = vec![master_fixture(1)];
        let live = universes_holding(&master_channels(128, 0, 0));
        let mut mapper = Mapper::new();
        assert!(!mapper
            .collect(&patch, &live, DmxProtocols::default())
            .updates
            .is_empty());

        let mut quiet = universes_holding(&master_channels(128, 0, 0));
        quiet.expire(Instant::now() + DATA_LOSS_TIMEOUT + Duration::from_millis(1));
        assert!(
            mapper
                .collect(&patch, &quiet, DmxProtocols::default())
                .is_empty(),
            "nothing changed, and nothing was taken away"
        );
    }

    #[test]
    fn a_universe_nobody_is_sending_yet_is_not_a_problem() {
        // A patch may name universes a console has not been switched on for,
        // and saying so every frame would bury the problems that matter.
        let patch = vec![master_fixture(1)];
        let collected = Mapper::new().collect(&patch, &Universes::new(), DmxProtocols::default());
        assert!(collected.is_empty());
    }

    #[test]
    fn forgetting_hands_the_show_back_to_the_console_without_firing_anything() {
        // What a show opening does. Nothing on the wire changed, so without
        // this the console would appear dead until an operator moved something.
        let layer = LayerId::new();
        let patch = vec![layer_fixture(1, layer)];
        let mut channels = vec![0_u8; LAYER_V1.footprint as usize];
        channels[0] = 0xFF;
        channels[3] = 255; // a trigger button held down throughout
        let universes = universes_holding(&channels);

        let mut mapper = Mapper::new();
        mapper.collect(&patch, &universes, DmxProtocols::default());
        assert!(mapper
            .collect(&patch, &universes, DmxProtocols::default())
            .is_empty());

        mapper.forget();
        let collected = mapper.collect(&patch, &universes, DmxProtocols::default());
        assert!(
            find(&collected, Parameter::LayerOpacity { layer_id: layer }).is_some(),
            "the console's opacity is adopted again"
        );
        assert_eq!(
            find(&collected, Parameter::LayerTrigger { layer_id: layer }),
            None,
            "and the held button is not a new press"
        );
    }

    #[test]
    fn a_protocol_that_is_switched_off_moves_nothing() {
        // The owner's two switches (2026-09-23). A venue's house desk may speak
        // sACN while a visiting console brings Art-Net, and switching one off
        // has to leave the other driving the show.
        let patch = vec![master_fixture(1)];
        let channels = master_channels(200, 0, 0);
        let universes = universes_holding(&channels);

        let on = Mapper::new().collect(&patch, &universes, DmxProtocols::default());
        assert!(!on.updates.is_empty(), "nothing arrived with both on");

        let off = Mapper::new().collect(
            &patch,
            &universes,
            DmxProtocols {
                artnet: false,
                sacn: true,
            },
        );
        assert!(
            off.updates.is_empty(),
            "Art-Net was switched off and still moved the show"
        );
    }

    #[test]
    fn switching_the_other_protocol_off_changes_nothing_for_this_one() {
        // The whole point of there being two switches rather than one.
        let patch = vec![master_fixture(1)];
        let channels = master_channels(200, 0, 0);
        let universes = universes_holding(&channels);

        let collected = Mapper::new().collect(
            &patch,
            &universes,
            DmxProtocols {
                artnet: true,
                sacn: false,
            },
        );
        assert!(
            !collected.updates.is_empty(),
            "switching sACN off stopped Art-Net as well"
        );
    }

    #[test]
    fn a_universe_nobody_is_driving_is_unaffected_by_the_switches() {
        // Nothing holds it, so there is no protocol to be off. The fixture is
        // silent either way, and silent is not an error.
        let patch = vec![master_fixture(1)];
        let universes = Universes::new();

        assert!(Mapper::new()
            .collect(&patch, &universes, DmxProtocols::NONE)
            .updates
            .is_empty());
    }
}
