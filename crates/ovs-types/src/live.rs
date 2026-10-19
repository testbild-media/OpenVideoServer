//! Live state.
//!
//! Everything here changes while a show runs and is driven by the operator, by
//! the master clock or by a lighting console over Art-Net or sACN. None of it is
//! show data.
//!
//! The separation is structural, not a convention: the persistence layer accepts
//! only [`crate::config`] types, so there is no code path that can write a
//! transient DMX value into a saved show.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::show::{
    CrossfaderCurve, MasterConfig, MAX_BPM, MAX_SPEED_SCALE, MIN_BPM, MIN_SPEED_SCALE,
};
use crate::config::{BlendMode, Layer, ParameterMap, Route};
use crate::geometry::Transform2D;
use crate::id::{EffectInstanceId, GeneratorInstanceId, LayerId};

/// Live effect parameter values, by effect instance.
///
/// A value here sits *on top of* what the show configured for that instance
/// and is never written back to it. [`EffectInstance::parameters`] is what the
/// effect opens with; this is what a control source has done to it since. That
/// is the same relationship [`LayerLive::opacity`] has with
/// [`Layer::default_opacity`], and it has the same consequence: closing and
/// reopening the show gives the configured value back, because the show
/// never heard about the live one.
///
/// An instance nobody has touched has no entry at all, and a parameter nobody
/// has touched is absent from its instance's map rather than present at its
/// configured value. Writing the configured value in here would be a second copy
/// of it, and the two would disagree the moment somebody edited the show.
///
/// Keyed by [`EffectInstanceId`] rather than by layer and chain position,
/// because a chain hangs off a slot as well as off a layer and the instance is
/// the only identity the two have in common. It is also the identity the
/// parameter registry names (`effect.<instance>.parameter.<name>`, TECHNICAL
/// §11).
///
/// [`EffectInstance::parameters`]: crate::config::EffectInstance::parameters
/// [`Layer::default_opacity`]: crate::config::Layer::default_opacity
pub type EffectOverrides = BTreeMap<EffectInstanceId, ParameterMap>;

/// Live generator parameter values, by generator instance.
///
/// Exactly what [`EffectOverrides`] is, for the other kind of module. An effect
/// and a generator are both plugins with declared parameters, and the owner's
/// instruction (2026-09-23) is that they use one model rather than two: a
/// client and a console move a generator's parameters through the same path
/// they move an effect's, and the renderer reads both the same way.
///
/// The same relationship to the show as well: a value here sits on top of
/// what the slot configured and is never written back, so closing and reopening
/// the show gives the configured value back.
pub type GeneratorOverrides = BTreeMap<GeneratorInstanceId, ParameterMap>;

/// The live state of the master bus and the global clock.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MasterLive {
    /// Master dimmer in `0.0..=1.0`, applied after the buses are combined.
    pub dimmer: f32,
    /// Crossfader position: `0.0` is fully A, `1.0` is fully B.
    pub crossfader: f32,
    /// Current master BPM. There is exactly one of these in the whole system.
    pub bpm: f64,
    /// Global speed scale, roughly `0.5` to `4.0`.
    pub speed_scale: f64,
    /// Whether the final output is frozen.
    ///
    /// Freezing holds the visible output but does not stop control or state
    /// processing, so the show resumes coherently.
    pub freeze: bool,
    /// Whether incoming DMX reaches this show at all.
    ///
    /// On by default, because a show is normally driven from the desk and a
    /// server that came up ignoring it would be a server that looks broken.
    ///
    /// Off is the operator taking the show by hand: Art-Net and sACN keep
    /// arriving, the status keeps saying what is on the wire, and **nothing
    /// they carry changes anything**. The show holds whatever it had, which
    /// is the same behaviour as a desk that has stopped sending — loss is a
    /// hold, not a blackout — and Live Mode drives it instead (owner,
    /// 2026-09-23).
    ///
    /// **The one switch between the desk and the client** (owner,
    /// 2026-10-19). On, whatever a patched fixture has a channel for is the
    /// desk's and the client cannot move it; off, the client has everything.
    /// See [`crate::control`].
    ///
    /// The server's rather than a show's: opening another show leaves it as it
    /// is, and a restart brings it back as it was.
    #[serde(default = "yes")]
    pub dmx_enabled: bool,
}

/// What a serialised master with no `dmx_enabled` in it meant.
///
/// Every state written before the switch existed was one where the console
/// was driving, so reading one back as “off” would take a show off its desk
/// because it was saved by an older build.
const fn yes() -> bool {
    true
}

impl Default for MasterLive {
    fn default() -> Self {
        Self {
            dimmer: 1.0,
            crossfader: 0.0,
            bpm: 120.0,
            speed_scale: 1.0,
            freeze: false,
            dmx_enabled: true,
        }
    }
}

impl MasterLive {
    /// Returns the contribution of each crossfaded bus as `(a, b)`.
    ///
    /// The `M` bus bypasses the crossfader entirely and is not part of this
    /// calculation.
    #[must_use]
    pub fn bus_gains(&self, curve: CrossfaderCurve) -> (f32, f32) {
        let position = self.crossfader.clamp(0.0, 1.0);
        match curve {
            CrossfaderCurve::Linear => (1.0 - position, position),
            CrossfaderCurve::EqualPower => {
                let angle = position * std::f32::consts::FRAC_PI_2;
                (angle.cos(), angle.sin())
            }
        }
    }

    /// Builds the master state a server restart, or reopening a show, starts
    /// from.
    ///
    /// **Exactly what it was** (owner, 2026-10-19: "Nach neustart den
    /// kompletten status, so wie er war"). That reverses what this did until
    /// then, which was to put the show's own defaults back and keep only the
    /// tempo, on the argument that a dimmer hours old is not a safe thing to
    /// put on stage. The owner's answer is that it is the thing he left there,
    /// whether the desk put it there or the client did — and that a server
    /// which came back somewhere else is the one an operator cannot predict.
    /// The consequence is worth saying plainly: a show left with the master
    /// at zero comes back black.
    ///
    /// A snapshot written before the whole master was kept carries only the
    /// tempo, and gets what that build would have done with it. With no
    /// snapshot at all, the show's defaults.
    ///
    /// The clock's *phase* is not carried across: it starts a fresh epoch, so
    /// the first beat after a restart is a known point rather than an
    /// extrapolation from a clock that was not running.
    ///
    /// [`dmx_enabled`](Self::dmx_enabled) comes back on here, and is not this
    /// method's to decide: the switch belongs to the server rather than to a
    /// show, and whoever loads the show puts the server's back over it.
    #[must_use]
    pub fn on_restart(config: &MasterConfig, snapshot: Option<&LiveSnapshot>) -> Self {
        let mut master = match snapshot.and_then(|snapshot| snapshot.master) {
            Some(master) => Self {
                dmx_enabled: true,
                ..master
            },
            None => Self {
                dimmer: config.default_dimmer,
                crossfader: config.default_crossfader,
                bpm: snapshot
                    .and_then(|snapshot| snapshot.bpm)
                    .unwrap_or(config.default_bpm),
                speed_scale: config.default_speed_scale,
                freeze: false,
                dmx_enabled: true,
            },
        };
        master.clamp_in_place();
        master
    }

    /// Applies a dimmer update from a control source.
    ///
    /// A value that is not a number is discarded and the last good value holds.
    /// A corrupt DMX packet must not be able to change what is on stage, and
    /// holding the previous value is the defined loss behaviour (house rule §17).
    pub fn set_dimmer(&mut self, value: f32) {
        if let Some(value) = sanitize_unit(value) {
            self.dimmer = value;
        }
    }

    /// Applies a crossfader update from a control source, ignoring NaN.
    pub fn set_crossfader(&mut self, value: f32) {
        if let Some(value) = sanitize_unit(value) {
            self.crossfader = value;
        }
    }

    /// Applies a BPM update from a control source, ignoring NaN.
    pub fn set_bpm(&mut self, value: f64) {
        if let Some(value) = sanitize_range(value, MIN_BPM, MAX_BPM) {
            self.bpm = value;
        }
    }

    /// Applies a speed scale update from a control source, ignoring NaN.
    pub fn set_speed_scale(&mut self, value: f64) {
        if let Some(value) = sanitize_range(value, MIN_SPEED_SCALE, MAX_SPEED_SCALE) {
            self.speed_scale = value;
        }
    }

    /// Forces every field into its valid range, replacing any NaN with a default.
    ///
    /// For values arriving live, prefer the `set_*` methods, which hold the last
    /// good value instead. This is for state restored from storage or received
    /// from another process, where there is no previous value to hold.
    ///
    /// A NaN dimmer becomes fully open rather than closed: corrupt data must not
    /// black out a running show.
    pub fn clamp_in_place(&mut self) {
        self.dimmer = sanitize_unit(self.dimmer).unwrap_or(1.0);
        self.crossfader = sanitize_unit(self.crossfader).unwrap_or(0.0);
        self.bpm = sanitize_range(self.bpm, MIN_BPM, MAX_BPM).unwrap_or(128.0);
        self.speed_scale =
            sanitize_range(self.speed_scale, MIN_SPEED_SCALE, MAX_SPEED_SCALE).unwrap_or(1.0);
    }
}

/// What a layer's playhead is doing.
///
/// Four things an operator can mean, and they must not be conflated
/// (owner decision Q16). Selecting **slot 0** puts the layer on Black, which is
/// a source; these are about the source it already has.
///
/// - [`Transport::Playing`] runs against the master clock, which is where every
///   layer starts.
/// - [`Transport::Paused`] freezes the playhead on the frame that is on screen.
///   Going back to `Playing` carries on from exactly there, which is the one
///   thing slot 0 cannot do: coming back from black restarts the clip.
/// - [`Transport::Stopped`] stops and rewinds — the active slot's playhead goes
///   back to its beginning and the first frame is what is shown.
///
/// A held frame is a picture, so a paused or stopped layer is still on stage
/// and still composited. Taking a layer off stage is slot 0, or
/// [`LayerLive::enabled`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// Running against the master clock.
    #[default]
    Playing,
    /// Frozen on the frame that was on screen.
    Paused,
    /// Stopped, with the playhead back at the start of the slot.
    Stopped,
}

impl Transport {
    /// Whether the playhead advances.
    #[must_use]
    pub const fn is_running(self) -> bool {
        matches!(self, Self::Playing)
    }

    /// Whether the playhead is held at the beginning rather than where it was.
    #[must_use]
    pub const fn is_rewound(self) -> bool {
        matches!(self, Self::Stopped)
    }
}

/// The live state of one layer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LayerLive {
    /// Which layer this state belongs to.
    pub layer_id: LayerId,
    /// The slot currently selected. `0` is the permanently black slot.
    pub active_slot: u16,
    /// How many times a slot has been selected on this layer.
    ///
    /// The number itself means nothing; what means something is that it
    /// changes. Re-selecting the slot that is already running is a *retrigger* —
    /// an operator hitting the same pad to restart the clip — and without this
    /// there is nothing in the state that says it happened, because the slot
    /// index did not move. The renderer restarts a source when this changes, so
    /// a retrigger reaches the stage rather than being lost between the console
    /// and the wall.
    pub trigger: u64,
    /// Live opacity in `0.0..=1.0`, usually driven by the Layer Fixture.
    pub opacity: f32,
    /// Live transform, usually driven by the Layer Fixture.
    pub transform: Transform2D,
    /// How this layer currently composites onto its bus.
    ///
    /// Live because it is something an operator changes while looking at the
    /// wall (owner, 2026-09-23), and configuration because a show has to
    /// open somewhere: [`Layer::blend_mode`] is where it starts, exactly as
    /// the grid and the opacity do.
    ///
    /// [`Layer::blend_mode`]: crate::config::Layer::blend_mode
    #[serde(default)]
    pub blend_mode: BlendMode,
    /// Which bus the layer currently feeds.
    ///
    /// A/B/M assignment is a live client-side function and is deliberately not
    /// part of the standard Layer Fixture.
    pub route: Route,
    /// What the playhead is doing.
    ///
    /// Live only. A show has no stopped layers in it — a show opens playing
    /// — so there is nothing here to restore and nothing to save.
    #[serde(default)]
    pub transport: Transport,
    /// Whether the layer is in the composition at all.
    ///
    /// Live, beside the configured [`Layer::enabled`] it starts from.
    /// A client or a console may switch a layer off during a show and
    /// that **never** writes back into the show: the layer keeps its slots,
    /// its transform and its chain, and comes back exactly as it was.
    ///
    /// Distinct from opacity at zero, which is a layer that is composited and
    /// contributes nothing: this one is not composited, so its decoder stops
    /// and its cost goes with it.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

/// What [`LayerLive::enabled`] is when a message does not carry it.
///
/// A layer somebody forgot to mention is on, because the alternative is a state
/// message from an older client silently emptying a wall.
const fn enabled_by_default() -> bool {
    true
}

impl LayerLive {
    /// Creates the live state a layer starts in: black slot, fully opaque, untransformed.
    #[must_use]
    pub fn black(layer_id: LayerId) -> Self {
        Self {
            layer_id,
            active_slot: 0,
            trigger: 0,
            blend_mode: BlendMode::default(),
            opacity: 1.0,
            transform: Transform2D::IDENTITY,
            route: Route::default(),
            transport: Transport::Playing,
            enabled: true,
        }
    }

    /// Returns true when this layer is showing the permanently black slot.
    #[must_use]
    pub const fn is_black(&self) -> bool {
        self.active_slot == 0
    }

    /// Builds the live state a layer starts from after a restart, or when its
    /// show is opened again.
    ///
    /// Exactly what it was, for the reasons in [`MasterLive::on_restart`]: the
    /// slot it was on, its opacity and transform, its bus and blend mode,
    /// whether it was on, and what its playhead was doing. A slot that has
    /// since been emptied comes back black rather than as a pad showing its
    /// placeholder.
    ///
    /// A snapshot written before the whole layer was kept carries only the
    /// slot, and everything else comes from the layer's configured defaults,
    /// which is what that build did.
    ///
    /// The timeline is not part of this type: playback restarts from the
    /// beginning of the clip.
    #[must_use]
    pub fn on_restart(layer: &Layer, snapshot: Option<&LiveSnapshot>) -> Self {
        let still_there =
            |index: &u16| *index == Layer::BLACK_SLOT_INDEX || layer.slot(*index).is_some();

        if let Some(kept) = snapshot.and_then(|snapshot| snapshot.layer(layer.id)) {
            let mut live = Self {
                layer_id: layer.id,
                active_slot: Some(kept.active_slot)
                    .filter(still_there)
                    .unwrap_or(Layer::BLACK_SLOT_INDEX),
                // A restored slot is not a trigger, and the counter means
                // nothing but that it moved.
                trigger: 0,
                ..*kept
            };
            live.clamp_in_place();
            return live;
        }

        let restored_slot = snapshot
            .and_then(|snapshot| snapshot.selected_slot(layer.id))
            .filter(still_there)
            .unwrap_or(Layer::BLACK_SLOT_INDEX);

        let mut live = Self {
            layer_id: layer.id,
            active_slot: restored_slot,
            // A restored slot is not a trigger. The show starts at the first
            // frame of whatever it comes back on, which is what a defined
            // opening state means.
            trigger: 0,
            blend_mode: layer.blend_mode,
            opacity: layer.default_opacity,
            transform: layer.default_transform,
            route: layer.route,
            // A show opens playing, whatever it was doing when it stopped
            //. A restart that came back paused would be a wall that
            // looks frozen and an operator looking for the fault.
            transport: Transport::Playing,
            // The show's own value, which is the initial live state rather
            // than a separate setting (owner decision Q16).
            enabled: layer.enabled,
        };
        live.clamp_in_place();
        live
    }

    /// Applies an opacity update from a control source.
    ///
    /// A value that is not a number is discarded and the last good value holds.
    pub fn set_opacity(&mut self, value: f32) {
        if let Some(value) = sanitize_unit(value) {
            self.opacity = value;
        }
    }

    /// Forces every field into its valid range, replacing any NaN with a default.
    ///
    /// A NaN opacity becomes fully opaque for the same reason a NaN dimmer opens
    /// fully: corrupt data must not remove a layer from a running show.
    pub fn clamp_in_place(&mut self) {
        self.opacity = sanitize_unit(self.opacity).unwrap_or(1.0);
        if !self.transform.x.is_finite() {
            self.transform.x = 0.0;
        }
        if !self.transform.y.is_finite() {
            self.transform.y = 0.0;
        }
        if !self.transform.scale_x.is_finite() {
            self.transform.scale_x = 1.0;
        }
        if !self.transform.scale_y.is_finite() {
            self.transform.scale_y = 1.0;
        }
        if !self.transform.rotation_deg.is_finite() {
            self.transform.rotation_deg = 0.0;
        }
    }
}

/// The live state of a show, kept so that a restart brings it back.
///
/// Stored separately from the show, never inside it. Restoring it therefore
/// does not write live state into show configuration, and the separation is
/// preserved: nothing here is a [`crate::config`] type and nothing here reaches
/// the show writer.
///
/// **All of it** (owner, 2026-10-19: "Nach neustart den kompletten status, so
/// wie er war"): the master, every layer, and every value a control source has
/// put on an effect or a generator, whether a desk sent it or the client did.
/// Until then this held only the tempo and the selected slots, on the
/// argument that anything a console drives is unsafe to put back hours later;
/// [`MasterLive::on_restart`] says why that was reversed.
///
/// `bpm` and `selected_slots` are still written, and still read from a
/// snapshot that has nothing else, so that the first restart after an update
/// comes back where the previous build would have.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LiveSnapshot {
    /// The master BPM in effect when the snapshot was taken.
    ///
    /// `None` when no tempo had been set, in which case the show default is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    /// The slot each layer was showing, keyed by layer.
    ///
    /// A layer absent from this map comes back black, which is the safe default
    /// for a layer nobody had selected anything on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub selected_slots: BTreeMap<LayerId, u16>,
    /// The whole master as it was.
    ///
    /// Its DMX switch is written down and not read back from here: the switch
    /// is the server's, not the show's, and is kept on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<MasterLive>,
    /// Every layer as it was, keyed by layer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layers: BTreeMap<LayerId, LayerLive>,
    /// What a control source had done to the effects.
    #[serde(default, skip_serializing_if = "EffectOverrides::is_empty")]
    pub effects: EffectOverrides,
    /// What a control source had done to the generators.
    #[serde(default, skip_serializing_if = "GeneratorOverrides::is_empty")]
    pub generators: GeneratorOverrides,
}

impl LiveSnapshot {
    /// Records what a layer is currently showing.
    pub fn record_selected_slot(&mut self, layer_id: LayerId, slot_index: u16) {
        self.selected_slots.insert(layer_id, slot_index);
    }

    /// Returns the slot a layer was showing, if the snapshot has one for it.
    #[must_use]
    pub fn selected_slot(&self, layer_id: LayerId) -> Option<u16> {
        self.layers
            .get(&layer_id)
            .map(|layer| layer.active_slot)
            .or_else(|| self.selected_slots.get(&layer_id).copied())
    }

    /// Returns the whole of a layer as it was, if the snapshot kept it.
    #[must_use]
    pub fn layer(&self, layer_id: LayerId) -> Option<&LayerLive> {
        self.layers.get(&layer_id)
    }

    /// Drops entries for layers that no longer exist in the show.
    ///
    /// A snapshot outlives the show edits made while the server was down, so
    /// it can name layers that have since been deleted.
    pub fn retain_known_layers(&mut self, layers: &[Layer]) {
        let known = |layer_id: &LayerId| layers.iter().any(|layer| layer.id == *layer_id);
        self.selected_slots.retain(|layer_id, _| known(layer_id));
        self.layers.retain(|layer_id, _| known(layer_id));
    }
}

/// Clamps a value into `0.0..=1.0`, returning `None` only for NaN.
///
/// Infinities are legitimate out-of-range values and clamp to the endpoints,
/// exactly as an over-range control value should. NaN carries no position at
/// all, so it is the only input the caller has to decide about.
fn sanitize_unit(value: f32) -> Option<f32> {
    (!value.is_nan()).then(|| value.clamp(0.0, 1.0))
}

/// Clamps a value into `min..=max`, returning `None` only for NaN.
fn sanitize_range(value: f64, min: f64, max: f64) -> Option<f64> {
    (!value.is_nan()).then(|| value.clamp(min, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linear_crossfader_hits_both_endpoints_exactly() {
        let mut master = MasterLive {
            crossfader: 0.0,
            ..MasterLive::default()
        };
        assert_eq!(master.bus_gains(CrossfaderCurve::Linear), (1.0, 0.0));
        master.crossfader = 1.0;
        assert_eq!(master.bus_gains(CrossfaderCurve::Linear), (0.0, 1.0));
        master.crossfader = 0.5;
        assert_eq!(master.bus_gains(CrossfaderCurve::Linear), (0.5, 0.5));
    }

    #[test]
    fn an_equal_power_crossfader_hits_both_endpoints_exactly() {
        let mut master = MasterLive {
            crossfader: 0.0,
            ..MasterLive::default()
        };
        let (a, b) = master.bus_gains(CrossfaderCurve::EqualPower);
        assert!(
            (a - 1.0).abs() < 1e-6,
            "A must be fully open at 0.0, got {a}"
        );
        assert!(b.abs() < 1e-6, "B must be fully closed at 0.0, got {b}");

        master.crossfader = 1.0;
        let (a, b) = master.bus_gains(CrossfaderCurve::EqualPower);
        assert!(a.abs() < 1e-6, "A must be fully closed at 1.0, got {a}");
        assert!(
            (b - 1.0).abs() < 1e-6,
            "B must be fully open at 1.0, got {b}"
        );
    }

    #[test]
    fn an_equal_power_crossfader_holds_power_through_the_middle() {
        let master = MasterLive {
            crossfader: 0.5,
            ..MasterLive::default()
        };
        let (a, b) = master.bus_gains(CrossfaderCurve::EqualPower);
        assert!(
            (a * a + b * b - 1.0).abs() < 1e-6,
            "expected constant power, got {a} and {b}"
        );
    }

    #[test]
    fn crossfader_positions_outside_the_range_are_clamped_not_extrapolated() {
        let past_end = MasterLive {
            crossfader: 2.5,
            ..MasterLive::default()
        };
        assert_eq!(past_end.bus_gains(CrossfaderCurve::Linear), (0.0, 1.0));
        let before_start = MasterLive {
            crossfader: -1.0,
            ..MasterLive::default()
        };
        assert_eq!(before_start.bus_gains(CrossfaderCurve::Linear), (1.0, 0.0));
    }

    #[test]
    fn out_of_range_control_values_clamp_to_the_endpoints() {
        let mut master = MasterLive::default();
        master.set_crossfader(f32::INFINITY);
        assert_eq!(master.crossfader, 1.0);
        master.set_crossfader(f32::NEG_INFINITY);
        assert_eq!(master.crossfader, 0.0);
        master.set_speed_scale(1e9);
        assert_eq!(master.speed_scale, MAX_SPEED_SCALE);
        master.set_bpm(-5.0);
        assert_eq!(master.bpm, MIN_BPM);
    }

    #[test]
    fn a_corrupt_control_value_leaves_the_show_exactly_as_it_was() {
        let mut master = MasterLive {
            dimmer: 0.75,
            crossfader: 0.25,
            bpm: 174.0,
            ..MasterLive::default()
        };
        master.set_dimmer(f32::NAN);
        master.set_crossfader(f32::NAN);
        master.set_bpm(f64::NAN);
        master.set_speed_scale(f64::NAN);

        assert_eq!(
            master.dimmer, 0.75,
            "a corrupt packet must not change the dimmer"
        );
        assert_eq!(master.crossfader, 0.25);
        assert_eq!(master.bpm, 174.0);
        assert_eq!(master.speed_scale, 1.0);
    }

    #[test]
    fn restored_state_with_no_previous_value_never_blacks_out() {
        // clamp_in_place has no last-good value to fall back on, so its NaN
        // defaults are chosen to keep the show visible.
        let mut master = MasterLive {
            dimmer: f32::NAN,
            crossfader: f32::INFINITY,
            bpm: f64::NAN,
            speed_scale: 1e9,
            freeze: false,
            dmx_enabled: true,
        };
        master.clamp_in_place();
        assert_eq!(
            master.dimmer, 1.0,
            "corrupt state must not black out the stage"
        );
        assert_eq!(master.crossfader, 1.0);
        assert_eq!(master.bpm, 128.0);
        assert_eq!(master.speed_scale, MAX_SPEED_SCALE);
    }

    fn show_master() -> MasterConfig {
        MasterConfig {
            default_bpm: 120.0,
            beats_per_bar: 4,
            default_speed_scale: 1.0,
            default_dimmer: 0.9,
            default_crossfader: 0.0,
            crossfader_curve: CrossfaderCurve::Linear,
        }
    }

    #[test]
    fn a_restart_returns_the_master_to_show_defaults() {
        let config = show_master();
        let restarted = MasterLive::on_restart(&config, None);

        assert_eq!(
            restarted.dimmer, 0.9,
            "dimmer comes from the show, not from live state"
        );
        assert_eq!(restarted.crossfader, 0.0);
        assert_eq!(restarted.speed_scale, 1.0);
        assert!(
            !restarted.freeze,
            "freeze is never carried across a restart"
        );
        assert_eq!(restarted.bpm, 120.0, "no snapshot means the show default");
    }

    #[test]
    fn a_snapshot_from_before_the_whole_master_was_kept_carries_the_tempo() {
        // What the build before 2026-10-19 wrote. The first restart after an
        // update comes back where that build would have.
        let config = show_master();
        let snapshot = LiveSnapshot {
            bpm: Some(174.0),
            ..LiveSnapshot::default()
        };
        let restarted = MasterLive::on_restart(&config, Some(&snapshot));

        assert_eq!(restarted.bpm, 174.0, "a tapped tempo survives a restart");
        assert_eq!(restarted.dimmer, config.default_dimmer);
        assert_eq!(restarted.crossfader, config.default_crossfader);
        assert!(!restarted.freeze);
    }

    #[test]
    fn a_show_left_mid_crossfade_comes_back_mid_crossfade() {
        // The owner, 2026-10-19: "Nach neustart den kompletten status, so wie
        // er war" — this test used to say the opposite.
        let left = MasterLive {
            dimmer: 0.0,
            crossfader: 0.5,
            bpm: 174.0,
            speed_scale: 2.0,
            freeze: true,
            dmx_enabled: false,
        };
        let snapshot = LiveSnapshot {
            master: Some(left),
            ..LiveSnapshot::default()
        };
        let restarted = MasterLive::on_restart(&show_master(), Some(&snapshot));

        assert_eq!(restarted.crossfader, 0.5);
        assert_eq!(restarted.dimmer, 0.0, "left black, comes back black");
        assert_eq!(restarted.bpm, 174.0);
        assert_eq!(restarted.speed_scale, 2.0);
        assert!(restarted.freeze);
        assert!(
            restarted.dmx_enabled,
            "the switch is the server's, and whoever loads the show puts it back"
        );
    }

    #[test]
    fn a_corrupt_snapshot_cannot_produce_an_invalid_master() {
        let snapshot = LiveSnapshot {
            bpm: Some(f64::NAN),
            ..LiveSnapshot::default()
        };
        let restarted = MasterLive::on_restart(&show_master(), Some(&snapshot));
        assert_eq!(restarted.bpm, 128.0);

        let too_fast = LiveSnapshot {
            bpm: Some(100_000.0),
            ..LiveSnapshot::default()
        };
        assert_eq!(
            MasterLive::on_restart(&show_master(), Some(&too_fast)).bpm,
            MAX_BPM
        );

        let corrupt = LiveSnapshot {
            master: Some(MasterLive {
                dimmer: f32::NAN,
                bpm: 100_000.0,
                ..MasterLive::default()
            }),
            ..LiveSnapshot::default()
        };
        let restarted = MasterLive::on_restart(&show_master(), Some(&corrupt));
        assert_eq!(restarted.dimmer, 1.0, "corrupt state never blacks out");
        assert_eq!(restarted.bpm, MAX_BPM);
    }

    #[test]
    fn a_restart_brings_the_whole_layer_back() {
        let mut layer = Layer::new("Layer 1", 0);
        layer
            .slots
            .push(crate::config::Slot::media(3, crate::id::MediaItemId::new()));

        let left = LayerLive {
            layer_id: layer.id,
            active_slot: 3,
            trigger: 41,
            opacity: 0.25,
            transform: Transform2D {
                x: 120.0,
                ..Transform2D::IDENTITY
            },
            blend_mode: BlendMode::Add,
            route: Route::B,
            transport: Transport::Paused,
            enabled: false,
        };
        let mut snapshot = LiveSnapshot::default();
        snapshot.layers.insert(layer.id, left);

        let restarted = LayerLive::on_restart(&layer, Some(&snapshot));
        assert_eq!(
            restarted,
            LayerLive { trigger: 0, ..left },
            "everything but the trigger count, which means nothing but that it moved"
        );

        // And a slot that has been emptied since comes back black.
        layer.slots.clear();
        assert!(LayerLive::on_restart(&layer, Some(&snapshot)).is_black());
    }

    #[test]
    fn a_snapshot_from_before_the_whole_layer_was_kept_restores_the_slot() {
        let mut layer = Layer::new("Layer 1", 0);
        layer.default_opacity = 0.8;
        layer.route = Route::B;
        layer
            .slots
            .push(crate::config::Slot::media(3, crate::id::MediaItemId::new()));

        let mut snapshot = LiveSnapshot::default();
        snapshot.record_selected_slot(layer.id, 3);

        let restarted = LayerLive::on_restart(&layer, Some(&snapshot));
        assert_eq!(
            restarted.active_slot, 3,
            "the show comes back showing its clips"
        );
        assert_eq!(
            restarted.opacity, 0.8,
            "opacity comes from the layer, not from live state"
        );
        assert_eq!(restarted.route, Route::B);
        assert!(restarted.transform.is_identity());
    }

    #[test]
    fn a_layer_with_no_snapshot_entry_comes_back_black() {
        let layer = Layer::new("Layer 1", 0);
        let restarted = LayerLive::on_restart(&layer, None);
        assert!(restarted.is_black());
        assert_eq!(
            LayerLive::on_restart(&layer, Some(&LiveSnapshot::default())).active_slot,
            0
        );
    }

    #[test]
    fn a_snapshot_naming_a_deleted_slot_comes_back_black() {
        // The show can be edited while the server is down.
        let layer = Layer::new("Layer 1", 0);
        let mut snapshot = LiveSnapshot::default();
        snapshot.record_selected_slot(layer.id, 7);

        let restarted = LayerLive::on_restart(&layer, Some(&snapshot));
        assert!(
            restarted.is_black(),
            "a slot that no longer exists must not be selected"
        );
    }

    #[test]
    fn a_snapshot_forgets_layers_the_show_no_longer_has() {
        let kept = Layer::new("Kept", 0);
        let removed = Layer::new("Removed", 1);
        let mut snapshot = LiveSnapshot::default();
        snapshot.record_selected_slot(kept.id, 1);
        snapshot.record_selected_slot(removed.id, 2);

        snapshot.retain_known_layers(std::slice::from_ref(&kept));
        assert_eq!(snapshot.selected_slot(kept.id), Some(1));
        assert_eq!(snapshot.selected_slot(removed.id), None);
    }

    #[test]
    fn a_layer_starts_black() {
        let live = LayerLive::black(LayerId::new());
        assert!(live.is_black());
        assert_eq!(live.active_slot, 0);
    }

    #[test]
    fn a_corrupt_opacity_leaves_the_layer_where_it_was() {
        let mut live = LayerLive {
            opacity: 0.4,
            ..LayerLive::black(LayerId::new())
        };
        live.set_opacity(f32::NAN);
        assert_eq!(
            live.opacity, 0.4,
            "a corrupt packet must not fade a layer out"
        );
        live.set_opacity(5.0);
        assert_eq!(live.opacity, 1.0, "an over-range value still clamps");
    }

    #[test]
    fn restored_layer_state_is_forced_into_range() {
        let mut live = LayerLive {
            opacity: f32::NAN,
            transform: Transform2D {
                x: f32::NAN,
                scale_x: f32::INFINITY,
                ..Transform2D::IDENTITY
            },
            ..LayerLive::black(LayerId::new())
        };
        live.clamp_in_place();
        assert_eq!(live.opacity, 1.0);
        // A transform has no meaningful endpoint to clamp an infinity to, so an
        // unusable value returns to the neutral transform.
        assert_eq!(live.transform.x, 0.0);
        assert_eq!(live.transform.scale_x, 1.0);
    }
}
