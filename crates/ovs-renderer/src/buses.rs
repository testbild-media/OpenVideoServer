//! The A/B/M buses and the global crossfader.
//!
//! Every layer routes to exactly one of three buses. `A` and `B` feed
//! the global crossfader; `M` bypasses it and goes straight to the master path.
//! That is what lets an operator build the next look on the idle side while a
//! persistent overlay stays up on `M`.
//!
//! ```text
//! layers routed A ─┐
//!                  ├─ crossfader ─┐
//! layers routed B ─┘              ├─ master dimmer ─ composition
//! layers routed M ────────────────┘
//! ```
//!
//! # Three pictures, not one
//!
//! Each bus is composited on its own — layers ordered among the layers they
//! share a bus with, never interleaved with another bus's — and what this module
//! computes is how the three finished pictures combine (owner decision Q8).
//!
//! That is why the numbers here are gains on a *bus* rather than on a layer. A
//! red layer on A under a blue one on B, crossfader at the middle, gives
//! (127, 0, 127): both at half, a true dissolve. Multiplying each layer's own
//! opacity instead would give (64, 0, 127) — the blue at half and the red at
//! half of what was left beneath it — which is what this renderer did until the
//! question was answered.
//!
//! The dimmer multiplies all three gains, which is the same picture as dimming
//! what they add up to, and saves a pass over the result.

use ovs_types::config::{CrossfaderCurve, Route};
use ovs_types::live::MasterLive;

/// The gain each bus contributes to the final image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BusGains {
    /// Crossfader side A.
    pub a: f32,
    /// Crossfader side B.
    pub b: f32,
    /// The direct master path. Never crossfaded.
    pub m: f32,
}

impl Default for BusGains {
    /// Everything at full, which is what a scene with no master state shows.
    ///
    /// Not "A only": a scene nobody has told about the crossfader is a scene
    /// being drawn for its own sake — a test, or a piece pass — and hiding two
    /// of its three buses would make that surprising.
    fn default() -> Self {
        Self {
            a: 1.0,
            b: 1.0,
            m: 1.0,
        }
    }
}

impl BusGains {
    /// Returns the gain for one route.
    #[must_use]
    pub const fn for_route(&self, route: Route) -> f32 {
        match route {
            Route::A => self.a,
            Route::B => self.b,
            Route::M => self.m,
        }
    }

    /// Returns the gains implied by the master state and the show's curve.
    ///
    /// The master dimmer applies after the buses are combined,
    /// so it multiplies all three. `M` gets the dimmer and nothing else: it
    /// bypasses the crossfader by definition, and a crossfade must never make a
    /// persistent overlay disappear.
    #[must_use]
    pub fn from_master(master: &MasterLive, curve: CrossfaderCurve) -> Self {
        let (a, b) = master.bus_gains(curve);
        let dimmer = master.dimmer.clamp(0.0, 1.0);

        Self {
            a: a * dimmer,
            b: b * dimmer,
            m: dimmer,
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn master(crossfader: f32, dimmer: f32) -> MasterLive {
        MasterLive {
            crossfader,
            dimmer,
            ..MasterLive::default()
        }
    }

    #[test]
    fn a_crossfader_at_one_end_shows_only_that_side() {
        let gains = BusGains::from_master(&master(0.0, 1.0), CrossfaderCurve::Linear);
        assert_eq!(gains.a, 1.0);
        assert_eq!(gains.b, 0.0);

        let gains = BusGains::from_master(&master(1.0, 1.0), CrossfaderCurve::Linear);
        assert_eq!(gains.a, 0.0);
        assert_eq!(gains.b, 1.0);
    }

    #[test]
    fn the_m_bus_is_never_crossfaded() {
        // The whole point of M: a persistent overlay must survive a crossfade,
        // whichever end it is at.
        for position in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let gains = BusGains::from_master(&master(position, 1.0), CrossfaderCurve::Linear);
            assert_eq!(
                gains.m, 1.0,
                "the crossfader at {position} must not touch M"
            );
        }
    }

    #[test]
    fn the_master_dimmer_applies_to_every_bus() {
        // It applies after the buses are combined, so it
        // multiplies all three including the one that bypasses the crossfader.
        let gains = BusGains::from_master(&master(0.5, 0.5), CrossfaderCurve::Linear);
        assert_eq!(gains.a, 0.25);
        assert_eq!(gains.b, 0.25);
        assert_eq!(gains.m, 0.5);
    }

    #[test]
    fn a_blackout_silences_everything_including_m() {
        let gains = BusGains::from_master(&master(0.5, 0.0), CrossfaderCurve::Linear);
        assert_eq!(gains.a, 0.0);
        assert_eq!(gains.b, 0.0);
        assert_eq!(gains.m, 0.0, "a blackout must black out the overlay too");
    }

    #[test]
    fn an_equal_power_crossfade_holds_perceived_level_through_the_middle() {
        // A linear crossfade dips in the middle, which is audible as a dip in
        // brightness on a wall. Equal power does not.
        let gains = BusGains::from_master(&master(0.5, 1.0), CrossfaderCurve::EqualPower);
        let power = gains.a * gains.a + gains.b * gains.b;
        assert!(
            (power - 1.0).abs() < 1e-5,
            "expected constant power, got {power}"
        );

        let linear = BusGains::from_master(&master(0.5, 1.0), CrossfaderCurve::Linear);
        let linear_power = linear.a * linear.a + linear.b * linear.b;
        assert!(
            linear_power < 0.75,
            "a linear crossfade does dip, by design"
        );
    }

    #[test]
    fn both_curves_reach_both_ends_exactly() {
        // An endpoint that is not quite an endpoint leaves a ghost of the
        // outgoing look on the wall.
        for curve in [CrossfaderCurve::Linear, CrossfaderCurve::EqualPower] {
            let start = BusGains::from_master(&master(0.0, 1.0), curve);
            assert!((start.a - 1.0).abs() < 1e-6, "{curve:?} at 0.0");
            assert!(start.b.abs() < 1e-6, "{curve:?} leaves a ghost of B at 0.0");

            let end = BusGains::from_master(&master(1.0, 1.0), curve);
            assert!(end.a.abs() < 1e-6, "{curve:?} leaves a ghost of A at 1.0");
            assert!((end.b - 1.0).abs() < 1e-6, "{curve:?} at 1.0");
        }
    }

    #[test]
    fn a_console_sending_nonsense_cannot_brighten_the_show() {
        // Live values are clamped on the way in, but the gain path must not be
        // able to exceed one even if something slipped through.
        let master = MasterLive {
            crossfader: 5.0,
            dimmer: 5.0,
            ..MasterLive::default()
        };
        let gains = BusGains::from_master(&master, CrossfaderCurve::Linear);
        for gain in [gains.a, gains.b, gains.m] {
            assert!((0.0..=1.0).contains(&gain), "gain {gain} is out of range");
        }
    }
}
