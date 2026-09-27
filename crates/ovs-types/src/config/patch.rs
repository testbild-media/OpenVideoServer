//! DMX-over-IP fixture patch.
//!
//! The fixture system itself is V0.2 work, but the patch lives in the
//! show schema, so the types exist from the first commit. Adding them later
//! would mean a schema migration for every existing show on the day the
//! feature lands, which is exactly what versioned contracts are meant to avoid.
//!
//! Fixture semantics never depend on whether a value arrived over Art-Net or
//! sACN: the transport is abstracted below this layer.

use serde::{Deserialize, Serialize};

use crate::error::{ValidationError, ValidationErrors};
use crate::id::{EffectInstanceId, FixtureInstanceId, GeneratorInstanceId, LayerId};

/// The highest valid DMX start address, given a one-channel footprint.
pub const MAX_DMX_ADDRESS: u16 = 512;

/// What a patched fixture controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FixtureTarget {
    /// The single show-wide master fixture.
    ///
    /// Carries global controls only: master dimmer, A/B crossfader, BPM, BPM tap,
    /// BPM reset, master speed scale and freeze.
    Master,
    /// One layer.
    ///
    /// Explicitly does not carry BPM tap, BPM reset, an effect master or a layer
    /// black control.
    Layer {
        /// The layer this fixture drives.
        layer_id: LayerId,
    },
    /// One effect instance that opts into DMX control.
    Effect {
        /// The instance this fixture drives.
        effect_instance_id: EffectInstanceId,
    },
    /// One generator instance that opts into DMX control.
    ///
    /// The same thing as [`Effect`](Self::Effect) for the other kind of module.
    /// Both are plugins, both declare their own channels, and both are driven
    /// through one parameter model (owner, 2026-09-23) — they are two variants
    /// rather than one only because the identity differs.
    Generator {
        /// The instance this fixture drives.
        generator_instance_id: GeneratorInstanceId,
    },
}

/// A fixture definition bound to a target, universe and start address.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FixtureInstance {
    /// Stable identifier of this patch entry.
    pub id: FixtureInstanceId,
    /// Stable fixture type identifier, for example `ovs.master`.
    pub type_id: String,
    /// Definition version this patch was created against.
    ///
    /// Published fixture definitions are immutable, because a console has the
    /// channel layout physically programmed into its show file.
    pub version: u32,
    /// What this fixture controls.
    pub target: FixtureTarget,
    /// Universe number.
    pub universe: u16,
    /// One-based DMX start address within the universe.
    pub start_address: u16,
    /// Number of channels this fixture occupies.
    pub footprint: u16,
    /// Whether this patch entry is active.
    pub enabled: bool,
}

impl FixtureInstance {
    /// Returns the inclusive one-based channel range this fixture occupies.
    ///
    /// Returns `None` when the fixture does not fit inside its universe.
    #[must_use]
    pub const fn channel_range(&self) -> Option<(u16, u16)> {
        if self.start_address == 0 || self.footprint == 0 {
            return None;
        }
        let last = self.start_address as u32 + self.footprint as u32 - 1;
        if last > MAX_DMX_ADDRESS as u32 {
            return None;
        }
        Some((self.start_address, last as u16))
    }

    /// Returns true when this fixture overlaps another in the same universe.
    ///
    /// Disabled entries never collide, so an operator can park a spare patch
    /// without having to delete it.
    #[must_use]
    pub fn collides_with(&self, other: &Self) -> bool {
        if !self.enabled || !other.enabled || self.universe != other.universe {
            return false;
        }
        match (self.channel_range(), other.channel_range()) {
            (Some((a_start, a_end)), Some((b_start, b_end))) => {
                a_start <= b_end && b_start <= a_end
            }
            _ => false,
        }
    }

    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        if self.type_id.trim().is_empty() {
            out.push(ValidationError::new(
                format!("{path}.type_id"),
                "must not be empty",
            ));
        }
        if self.version == 0 {
            out.push(ValidationError::new(
                format!("{path}.version"),
                "fixture definition versions start at 1",
            ));
        }
        if self.footprint == 0 {
            out.push(ValidationError::new(
                format!("{path}.footprint"),
                "must occupy at least one channel",
            ));
        }
        if self.channel_range().is_none() {
            out.push(ValidationError::new(
                format!("{path}.start_address"),
                format!(
                    "address {} with a footprint of {} does not fit in a {MAX_DMX_ADDRESS}-channel universe",
                    self.start_address, self.footprint
                ),
            ));
        }
    }
}

/// Returns the lowest address a fixture of `footprint` fits at in `universe`.
///
/// What a patch editor offers when an operator adds a fixture. Lowest rather
/// than next-after-the-last, because a rig that has had fixtures removed from
/// it has gaps, and an operator patching by hand fills them — an editor that
/// always appended would leave a universe looking full at address 300 with
/// nothing between 1 and 40.
///
/// `moving` is the entry being re-addressed, if any: a fixture does not collide
/// with where it already is.
///
/// Returns `None` when there is no room, which is a real answer. A universe has
/// 512 channels and a show can want more than that.
#[must_use]
pub fn suggest_address(
    patch: &[FixtureInstance],
    universe: u16,
    footprint: u16,
    moving: Option<FixtureInstanceId>,
) -> Option<u16> {
    if footprint == 0 || footprint > MAX_DMX_ADDRESS {
        return None;
    }

    // Disabled entries are parked, and a parked fixture holds no channels:
    // that is what parking is for (see `collides_with`).
    let mut taken: Vec<(u16, u16)> = patch
        .iter()
        .filter(|fixture| {
            fixture.enabled && fixture.universe == universe && Some(fixture.id) != moving
        })
        .filter_map(FixtureInstance::channel_range)
        .collect();
    taken.sort_unstable();

    let mut address = 1_u16;
    for (start, end) in taken {
        if address + footprint - 1 < start {
            return Some(address);
        }
        address = address.max(end.saturating_add(1));
        if address > MAX_DMX_ADDRESS {
            return None;
        }
    }

    (address as u32 + footprint as u32 - 1 <= MAX_DMX_ADDRESS as u32).then_some(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lowest address a fixture of `footprint` fits at in universe 1.
    fn suggestion(patch: &[FixtureInstance], footprint: u16) -> Option<u16> {
        suggest_address(patch, 1, footprint, None)
    }

    fn fixture(universe: u16, start: u16, footprint: u16) -> FixtureInstance {
        FixtureInstance {
            id: FixtureInstanceId::new(),
            type_id: "ovs.layer".to_owned(),
            version: 1,
            target: FixtureTarget::Layer {
                layer_id: LayerId::new(),
            },
            universe,
            start_address: start,
            footprint,
            enabled: true,
        }
    }

    #[test]
    fn channel_ranges_are_inclusive_and_one_based() {
        assert_eq!(fixture(0, 1, 1).channel_range(), Some((1, 1)));
        assert_eq!(fixture(0, 1, 10).channel_range(), Some((1, 10)));
        assert_eq!(fixture(0, 503, 10).channel_range(), Some((503, 512)));
    }

    #[test]
    fn a_fixture_may_not_run_past_the_end_of_a_universe() {
        assert_eq!(fixture(0, 504, 10).channel_range(), None);
        assert_eq!(fixture(0, 512, 2).channel_range(), None);
        assert_eq!(fixture(0, 512, 1).channel_range(), Some((512, 512)));
    }

    #[test]
    fn address_zero_is_invalid_because_dmx_addressing_is_one_based() {
        assert_eq!(fixture(0, 0, 10).channel_range(), None);
        let mut errors = ValidationErrors::new();
        fixture(0, 0, 10).validate("patch[0]", &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn overlapping_fixtures_collide() {
        let a = fixture(1, 1, 10);
        let b = fixture(1, 10, 10);
        assert!(a.collides_with(&b));
        assert!(b.collides_with(&a));
    }

    #[test]
    fn adjacent_fixtures_do_not_collide() {
        let a = fixture(1, 1, 10);
        let b = fixture(1, 11, 10);
        assert!(!a.collides_with(&b));
        assert!(!b.collides_with(&a));
    }

    #[test]
    fn fixtures_in_different_universes_never_collide() {
        assert!(!fixture(1, 1, 10).collides_with(&fixture(2, 1, 10)));
    }

    #[test]
    fn a_disabled_fixture_is_parked_rather_than_colliding() {
        let a = fixture(1, 1, 10);
        let parked = FixtureInstance {
            enabled: false,
            ..fixture(1, 1, 10)
        };
        assert!(!a.collides_with(&parked));
        assert!(!parked.collides_with(&a));
    }

    #[test]
    fn a_fixture_collides_with_an_identical_copy_of_itself() {
        let a = fixture(1, 100, 24);
        assert!(a.collides_with(&fixture(1, 100, 24)));
    }

    #[test]
    fn an_empty_universe_starts_at_one() {
        assert_eq!(suggestion(&[], 14), Some(1));
    }

    #[test]
    fn a_fixture_goes_after_the_ones_already_there() {
        let patch = vec![fixture(1, 1, 10)];
        assert_eq!(suggestion(&patch, 14), Some(11));
    }

    #[test]
    fn a_gap_is_filled_rather_than_stepped_over() {
        // A rig that has had fixtures removed has gaps, and an operator
        // patching by hand fills them. An editor that always appended would
        // leave a universe looking full at 300 with nothing between 1 and 40.
        let patch = vec![fixture(1, 1, 10), fixture(1, 40, 10)];
        assert_eq!(suggestion(&patch, 20), Some(11));
    }

    #[test]
    fn a_gap_too_small_is_not_offered() {
        let patch = vec![fixture(1, 1, 10), fixture(1, 20, 10)];
        assert_eq!(
            suggestion(&patch, 20),
            Some(30),
            "past both, not into the gap"
        );
    }

    #[test]
    fn a_parked_fixture_holds_no_channels() {
        // Which is what parking is for: an operator keeps a spare patch entry
        // without it taking up the addresses.
        let parked = FixtureInstance {
            enabled: false,
            ..fixture(1, 1, 100)
        };
        assert_eq!(suggestion(&[parked], 14), Some(1));
    }

    #[test]
    fn other_universes_are_not_in_the_way() {
        let patch = vec![fixture(2, 1, 400)];
        assert_eq!(suggestion(&patch, 14), Some(1));
    }

    #[test]
    fn a_universe_with_no_room_says_so() {
        // Five hundred and twelve channels is what there is. A show that wants
        // more needs another universe, and an editor that offered an address
        // anyway would be patching over something.
        let patch = vec![fixture(1, 1, 500)];
        assert_eq!(suggestion(&patch, 14), None);
        assert_eq!(suggestion(&patch, 12), Some(501));
    }

    #[test]
    fn a_fixture_being_moved_does_not_block_itself() {
        // Re-addressing an existing fixture: it has to be allowed to stay where
        // it is, or an editor would refuse to suggest the address it already
        // has.
        let existing = fixture(1, 1, 10);
        let id = existing.id;
        let patch = vec![existing];

        assert_eq!(suggest_address(&patch, 1, 10, Some(id)), Some(1));
        assert_eq!(suggest_address(&patch, 1, 10, None), Some(11));
    }

    #[test]
    fn a_footprint_that_could_never_fit_is_refused() {
        assert_eq!(suggestion(&[], 0), None);
        assert_eq!(suggestion(&[], 513), None);
        assert_eq!(suggestion(&[], 512), Some(1), "exactly a universe fits");
    }
}
