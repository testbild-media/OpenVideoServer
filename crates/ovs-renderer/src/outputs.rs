//! Which connector drives which configured output.
//!
//! **Outputs are forced, not discovered.** A show names a GPU and a connector
//! and the renderer drives that socket — whatever is plugged into it, and
//! whether or not the driver believes anything is. There is no hotplug: a wall
//! does not move during a show, and a processor that is powered on after the
//! server must find its content already there rather than have to be waited for.
//!
//! This is the rule, arrived at from the other
//! direction. The failure it forbids — a wall that is unplugged quietly handing
//! its content to the next wall along — cannot happen if nothing is ever
//! rematched. The failure forcing introduces instead is an operator who names
//! the wrong socket, which is visible the first time they look at the wall and
//! is theirs to fix.
//!
//! # Why EDID decides nothing
//!
//! An LED processor is not a monitor. Many report no EDID at all, or the same
//! serial on every unit in the rack, and many are powered from the
//! same distro as the wall — so they appear minutes after the server does.
//! Identity matching would take those walls dark for the night. What EDID is for
//! here is reading, not deciding: "DP-3 says it is a Novastar" belongs on the
//! patch page and in a support call.
//!
//! # Shows that name no connector
//!
//! Every test show, and every show authored before the renderer could see
//! hardware. Those are driven in order, as they always were, and the renderer
//! says so once. Refusing them would make this arrive as "the server stopped
//! working"; accepting them silently would make naming a connector pointless.

use ovs_types::config::{OutputConfig, OutputIdentity, Show};
use ovs_types::id::OutputId;

/// What one physical output was matched to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assignment {
    /// It drives this configured output.
    Drives {
        /// The configured output it drives.
        output_id: OutputId,
        /// How the two were matched.
        how: MatchedBy,
    },
    /// Nothing in the show asks for this output.
    ///
    /// Not a fault: a machine may have more connectors than a show uses, and a
    /// wall that is plugged in but not part of tonight's show is ordinary.
    Unused,
}

/// Why a physical output drives a configured one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedBy {
    /// The show names this GPU and this connector.
    Forced,
    /// Position, because the show names no connector at all.
    Order,
}

/// A configured output that is not being driven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    /// The configured output.
    pub output_id: OutputId,
    /// What the operator calls it.
    pub name: String,
    /// Why it is not being driven.
    pub reason: String,
}

/// The whole decision: what each physical output drives, and what is missing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// One entry per physical output, in the order they were given.
    pub assignments: Vec<Assignment>,
    /// Configured outputs with no hardware to drive them.
    pub missing: Vec<Missing>,
    /// Whether anything was driven by position rather than by being named.
    pub matched_by_order: bool,
}

/// Works out which physical output drives which configured one.
///
/// `discovered` is one identity per physical output, in the order the renderer
/// drives them. `None` means the backend cannot say what it is attached to,
/// which is every backend until the display path lands.
#[must_use]
pub fn plan(show: Option<&Show>, discovered: &[Option<OutputIdentity>]) -> Plan {
    let mut plan = Plan {
        assignments: vec![Assignment::Unused; discovered.len()],
        ..Plan::default()
    };

    let Some(show) = show else {
        return plan;
    };

    // Only the outputs that want a socket. A network output has no connector to
    // be assigned and none to be missing, and leaving it in would be worse than
    // untidy: it names no connector, so it would fall through to "matched in
    // order" and be handed the first spare one — a stream added to a show
    // quietly taking a wall, which is the silent remap §18 forbids arriving by
    // the back door.
    let wanted: Vec<&OutputConfig> = show
        .outputs
        .iter()
        .filter(|output| output.enabled && !output.is_on_the_network())
        .collect();
    let mut taken = vec![false; wanted.len()];

    // Named connectors first. Each configured output takes the socket it names
    // and no other, so this is a lookup rather than a search: two outputs cannot
    // compete for one connector, and one output cannot drift to another.
    for (index, identity) in discovered.iter().enumerate() {
        let Some(identity) = identity else {
            continue;
        };

        let found = wanted.iter().enumerate().position(|(slot, output)| {
            !taken[slot] && output.identity.match_against(identity).is_usable()
        });

        if let Some(slot) = found {
            taken[slot] = true;
            plan.assignments[index] = Assignment::Drives {
                output_id: wanted[slot].id,
                how: MatchedBy::Forced,
            };
        }
    }

    // What is left over. A show whose outputs name no connector is driven in
    // order; one that names a connector this machine is not driving is not,
    // because putting it somewhere else is exactly the silent remap §18
    // forbids.
    // Collected before anything is assigned, because assigning changes what is
    // spare and an iterator that noticed would be reading its own output.
    let mut spare: std::collections::VecDeque<usize> = plan
        .assignments
        .iter()
        .enumerate()
        .filter(|(_, assignment)| matches!(assignment, Assignment::Unused))
        .map(|(index, _)| index)
        .collect();

    for (slot, output) in wanted.iter().enumerate() {
        if taken[slot] {
            continue;
        }

        if output.identity != OutputIdentity::default() {
            plan.missing.push(Missing {
                output_id: output.id,
                name: output.name.clone(),
                reason: "this machine is not driving the connector it is forced onto".to_owned(),
            });
            continue;
        }

        match spare.pop_front() {
            Some(index) => {
                taken[slot] = true;
                plan.matched_by_order = true;
                plan.assignments[index] = Assignment::Drives {
                    output_id: output.id,
                    how: MatchedBy::Order,
                };
            }
            None => plan.missing.push(Missing {
                output_id: output.id,
                name: output.name.clone(),
                reason: "this machine is driving fewer outputs than the show asks for".to_owned(),
            }),
        }
    }

    plan
}

impl Assignment {
    /// Returns the configured output this drives, if any.
    #[must_use]
    pub const fn output_id(&self) -> Option<OutputId> {
        match self {
            Self::Drives { output_id, .. } => Some(*output_id),
            Self::Unused => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::{DisplayMode, OutputConfig};
    use ovs_types::geometry::{FrameRate, SizePx};

    use super::*;

    fn mode() -> DisplayMode {
        DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60)
    }

    fn identity(connector: &str, edid: Option<&str>) -> OutputIdentity {
        OutputIdentity {
            gpu_pci_address: Some("0000:01:00.0".to_owned()),
            connector: Some(connector.to_owned()),
            edid_hash: edid.map(str::to_owned),
            ..OutputIdentity::default()
        }
    }

    fn show_with(outputs: Vec<OutputConfig>) -> Show {
        let mut show = Show::new("Show");
        show.outputs = outputs;
        show
    }

    #[test]
    fn a_wall_is_found_by_what_it_is_not_by_where_it_was_enumerated() {
        // The whole point. The driver may hand the connectors over in any
        // order; the show must land on the wall it was built for.
        let left = OutputConfig {
            identity: identity("DP-1", Some("aaa")),
            ..OutputConfig::new("Left", mode())
        };
        let right = OutputConfig {
            identity: identity("DP-2", Some("bbb")),
            ..OutputConfig::new("Right", mode())
        };
        let (left_id, right_id) = (left.id, right.id);
        let show = show_with(vec![left, right]);

        // Discovered the other way round, which is what a reboot does.
        let discovered = vec![
            Some(identity("DP-2", Some("bbb"))),
            Some(identity("DP-1", Some("aaa"))),
        ];

        let plan = plan(Some(&show), &discovered);

        assert_eq!(plan.assignments[0].output_id(), Some(right_id));
        assert_eq!(plan.assignments[1].output_id(), Some(left_id));
        assert!(plan.missing.is_empty());
        assert!(!plan.matched_by_order);
    }

    #[test]
    fn an_unplugged_wall_is_a_fault_and_not_somebody_elses_content() {
        // §18's rule, and the failure it exists for: the second wall must not
        // start showing the first wall's content because the first came
        // unplugged.
        let left = OutputConfig {
            identity: identity("DP-1", Some("aaa")),
            ..OutputConfig::new("Left", mode())
        };
        let right = OutputConfig {
            identity: identity("DP-2", Some("bbb")),
            ..OutputConfig::new("Right", mode())
        };
        let right_id = right.id;
        let show = show_with(vec![left, right]);

        // Only the right-hand wall is connected.
        let discovered = vec![Some(identity("DP-2", Some("bbb")))];
        let plan = plan(Some(&show), &discovered);

        assert_eq!(plan.assignments[0].output_id(), Some(right_id));
        assert_eq!(plan.missing.len(), 1);
        assert_eq!(plan.missing[0].name, "Left");
    }

    #[test]
    fn an_led_processor_with_no_edid_drives_the_connector_it_is_named_on() {
        // The ordinary case for this product, not a degraded one: many
        // processors report no EDID at all.
        let wall = OutputConfig {
            identity: identity("DP-1", None),
            ..OutputConfig::new("Wall", mode())
        };
        let wall_id = wall.id;
        let show = show_with(vec![wall]);

        let plan = plan(Some(&show), &[Some(identity("DP-1", None))]);

        assert_eq!(
            plan.assignments[0],
            Assignment::Drives {
                output_id: wall_id,
                how: MatchedBy::Forced,
            }
        );
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn a_different_processor_on_the_named_connector_still_drives_that_wall() {
        // A spare swapped in between shows, or a unit that was powered off when
        // the server booted and now reports something else. The wall is the
        // socket, and refusing this would take it dark for the night.
        let wall = OutputConfig {
            identity: identity("DP-1", Some("the unit that was here in rehearsal")),
            ..OutputConfig::new("Wall", mode())
        };
        let wall_id = wall.id;
        let show = show_with(vec![wall]);

        let plan = plan(
            Some(&show),
            &[Some(identity(
                "DP-1",
                Some("the spare from the flight case"),
            ))],
        );

        assert_eq!(plan.assignments[0].output_id(), Some(wall_id));
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn each_output_takes_the_connector_it_names_whatever_order_they_come_in() {
        // Two walls, enumerated the other way round from how the show lists
        // them. Each takes its own socket; neither drifts.
        let first = OutputConfig {
            identity: identity("DP-1", None),
            ..OutputConfig::new("First", mode())
        };
        let second = OutputConfig {
            identity: identity("DP-2", Some("bbb")),
            ..OutputConfig::new("Second", mode())
        };
        let (first_id, second_id) = (first.id, second.id);
        let show = show_with(vec![first, second]);

        let discovered = vec![
            Some(identity("DP-2", Some("bbb"))),
            Some(identity("DP-1", None)),
        ];
        let plan = plan(Some(&show), &discovered);

        assert_eq!(plan.assignments[0].output_id(), Some(second_id));
        assert_eq!(plan.assignments[1].output_id(), Some(first_id));
    }

    #[test]
    fn a_wall_whose_connector_is_not_being_driven_is_a_fault_not_a_remap() {
        // The failure §18 exists to forbid, and the one forcing makes
        // impossible: the show for DP-3 does not appear on DP-1 because DP-1
        // happened to be free.
        let wall = OutputConfig {
            identity: identity("DP-3", None),
            ..OutputConfig::new("Wall Left", mode())
        };
        let wall_id = wall.id;
        let show = show_with(vec![wall]);

        let plan = plan(Some(&show), &[Some(identity("DP-1", None))]);

        assert_eq!(plan.assignments[0], Assignment::Unused);
        assert_eq!(plan.missing.len(), 1);
        assert_eq!(plan.missing[0].output_id, wall_id);
        assert_eq!(plan.missing[0].name, "Wall Left");
        assert!(
            plan.missing[0].reason.contains("forced"),
            "got {:?}",
            plan.missing[0].reason
        );
    }

    #[test]
    fn a_show_with_no_identities_is_still_matched_in_order() {
        // Every show authored before the renderer could discover hardware,
        // and every test show. Refusing these would make the strict rule
        // arrive as "the server stopped working".
        let show = show_with(vec![
            OutputConfig::new("First", mode()),
            OutputConfig::new("Second", mode()),
        ]);
        let first_id = show.outputs[0].id;
        let second_id = show.outputs[1].id;

        let plan = plan(Some(&show), &[None, None]);

        assert_eq!(plan.assignments[0].output_id(), Some(first_id));
        assert_eq!(plan.assignments[1].output_id(), Some(second_id));
        assert!(plan.matched_by_order, "and it says so");
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn a_spare_connector_is_not_a_fault() {
        // A machine may have more outputs than tonight's show uses.
        let show = show_with(vec![OutputConfig::new("Only", mode())]);
        let plan = plan(Some(&show), &[None, None]);

        assert!(plan.assignments[0].output_id().is_some());
        assert_eq!(plan.assignments[1], Assignment::Unused);
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn a_show_asking_for_more_walls_than_exist_says_which_are_missing() {
        let show = show_with(vec![
            OutputConfig::new("First", mode()),
            OutputConfig::new("Second", mode()),
        ]);
        let second_id = show.outputs[1].id;

        let plan = plan(Some(&show), &[None]);

        assert_eq!(plan.missing.len(), 1);
        assert_eq!(plan.missing[0].output_id, second_id);
    }

    #[test]
    fn a_disabled_output_asks_for_nothing() {
        // Turning an output off in a show is an operator saying "not
        // tonight", which is not a missing wall.
        let show = show_with(vec![
            OutputConfig::new("First", mode()),
            OutputConfig {
                enabled: false,
                ..OutputConfig::new("Second", mode())
            },
        ]);

        let plan = plan(Some(&show), &[None]);
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn no_show_drives_nothing_and_faults_nothing() {
        let plan = plan(None, &[None, None]);
        assert_eq!(
            plan.assignments,
            vec![Assignment::Unused, Assignment::Unused]
        );
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn a_network_output_never_takes_a_socket() {
        // The fault this guards against: a stream names no connector, so before
        // it was filtered out it matched "in order" and was handed the first
        // spare one. Adding a network feed to a show would have taken a wall.
        let mut show = Show::new("Show");
        show.outputs
            .push(OutputConfig::published("Programme feed", mode()));
        let wall = OutputConfig::new("LED Wall", mode());
        let wall_id = wall.id;
        show.outputs.push(wall);

        let plan = plan(Some(&show), &[None]);

        assert_eq!(
            plan.assignments[0].output_id(),
            Some(wall_id),
            "the one connector must go to the wall, not to the stream"
        );
        assert!(
            plan.missing.is_empty(),
            "a stream is not an output this machine is failing to drive, got {:?}",
            plan.missing
        );
    }

    #[test]
    fn a_network_output_does_not_make_a_wall_look_missing() {
        // Two streams and one wall on a machine with one connector. Before the
        // filter the streams would have eaten the spare socket and the wall
        // would have been reported missing — the right complaint about the
        // wrong output.
        let mut show = Show::new("Show");
        show.outputs.push(OutputConfig::published("Feed A", mode()));
        show.outputs.push(OutputConfig::published("Feed B", mode()));
        let wall = OutputConfig::new("LED Wall", mode());
        let wall_id = wall.id;
        show.outputs.push(wall);

        let plan = plan(Some(&show), &[None]);

        assert_eq!(plan.assignments[0].output_id(), Some(wall_id));
        assert!(plan.missing.is_empty(), "got {:?}", plan.missing);
    }
}
