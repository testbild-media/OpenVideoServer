//! Who may change what: the lighting console, the client, an administrator.
//!
//! # The rule
//!
//! The owner's, 2026-10-19, and the whole of it:
//!
//! - **One switch hands a show between the desk and the client**: the DMX
//!   switch in Live ([`MasterLive::dmx_enabled`]). Art-Net and sACN only choose
//!   which protocol is read; both off is a way of locking the show, not a way
//!   of handing it over.
//! - **With the switch on, whatever a patched fixture has a channel for is the
//!   desk's alone.** The client shows it and cannot move it. That holds for the
//!   master, a layer, an effect and a generator alike. A parked fixture — one
//!   that is in the patch but not enabled — holds nothing.
//! - **A layer's slot set to [`SlotControl::Gui`] is the one exception.** Its
//!   slots are chosen in the client, and the desk's slot channel is ignored.
//! - **The tempo is the one thing both may do**: "Tap BPM ist das einzige was
//!   ich über beides machen würde". A desk with no cue for the tap still leaves
//!   an operator able to tap in the client.
//! - **With the switch off, everything is the client's**, including a slot set
//!   to DMX. The client starts from the values that are on stage.
//!
//! What has no channel stays the client's whether or not anything is patched:
//! a layer's bus, its blend mode and its transport, the beat reset, and every
//! parameter a plugin does not offer to a desk. Nobody could change those
//! otherwise.
//!
//! [`DeskControls::of`] reads the patch half of this from a show. The switch is
//! the caller's to apply, because it is live state and this is about a
//! document.
//!
//! # The lock
//!
//! An administrator can lock an effect or a generator. A locked one cannot be
//! removed, moved or put in another order **by anybody**, an administrator
//! included: unlocking is a deliberate second step, and only an administrator
//! may take it. What it is doing is not part of the lock — its values follow
//! the rule above, and switching an effect on or off stays open to everybody.
//!
//! [`check_edit`] is the one place a stored show is held to that, and
//! [`carry_locks`] is why no edit but the lock's own request can change one.
//!
//! [`MasterLive::dmx_enabled`]: crate::live::MasterLive::dmx_enabled

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::config::fixtures::definition;
use crate::config::layer::SlotControl;
use crate::config::slot::{ParameterMap, ParameterValue, SourceRef};
use crate::config::{effects, generators, EffectInstance, Layer, Parameter, Show};
use crate::id::{EffectInstanceId, GeneratorInstanceId, LayerId};

/// A master control a desk can hold.
///
/// The tempo is not here, and that is the owner's rule rather than a gap: a
/// tap counts from wherever it comes, the client's or the desk's.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MasterControl {
    /// The master dimmer.
    Dimmer,
    /// The A/B crossfader.
    Crossfader,
    /// The speed scale.
    SpeedScale,
    /// Freeze.
    Freeze,
}

/// A layer control a desk can hold.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LayerControl {
    /// The layer's live opacity.
    Opacity,
    /// Which slot the layer is showing.
    Slot,
    /// Where the layer's picture sits: position, scale and rotation.
    ///
    /// One control rather than five, because a client sends a transform whole
    /// and a desk that drives any part of it would have that part moved by a
    /// client sending the rest.
    Transform,
}

/// What the patch gives a lighting console, before the DMX switch is read.
///
/// Sent to every client beside the show, so that a client greys out exactly
/// what the server would refuse rather than working it out again from the
/// fixture personalities — a second copy of this reading would be the one that
/// went out of date the day a personality grew a channel.
///
/// Every entry is held **only while the DMX switch is on**. With it off the
/// client has the whole show, and nothing here is consulted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeskControls {
    /// The master controls a patched master fixture drives.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub master: BTreeSet<MasterControl>,
    /// The layer controls each patched layer fixture drives, by layer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layers: BTreeMap<LayerId, BTreeSet<LayerControl>>,
    /// The parameters a desk drives on each patched effect, by manifest key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effects: BTreeMap<EffectInstanceId, BTreeSet<String>>,
    /// The parameters a desk drives on each patched generator, by manifest key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub generators: BTreeMap<GeneratorInstanceId, BTreeSet<String>>,
}

impl DeskControls {
    /// Reads what a show's patch gives a desk.
    ///
    /// A fixture is read the way the DMX receiver reads it: one it would refuse
    /// — a personality this build does not publish, a footprint that
    /// disagrees, a target the personality cannot drive — drives nothing, and so
    /// holds nothing. Locking a control nothing can move would leave it moved by
    /// nobody.
    #[must_use]
    pub fn of(show: &Show) -> Self {
        let mut controls = Self::default();

        for fixture in show.fixture_patch.iter().filter(|fixture| fixture.enabled) {
            let Some(personality) = definition(&fixture.type_id) else {
                continue;
            };
            if personality.footprint != fixture.footprint || !personality.drives(fixture.target) {
                continue;
            }

            for channel in personality.channels {
                let Some(parameter) = channel.parameter.bind(fixture.target) else {
                    continue;
                };
                controls.hold(show, parameter);
            }
        }

        controls
    }

    /// Records one parameter a channel drives.
    fn hold(&mut self, show: &Show, parameter: Parameter) {
        match parameter {
            Parameter::MasterDimmer => {
                self.master.insert(MasterControl::Dimmer);
            }
            Parameter::MasterCrossfader => {
                self.master.insert(MasterControl::Crossfader);
            }
            Parameter::MasterSpeedScale => {
                self.master.insert(MasterControl::SpeedScale);
            }
            Parameter::MasterFreeze => {
                self.master.insert(MasterControl::Freeze);
            }
            // Both may: see this module's documentation.
            Parameter::MasterBpm | Parameter::MasterTapTempo => {}

            Parameter::LayerOpacity { layer_id } => {
                self.hold_layer(layer_id, LayerControl::Opacity)
            }
            Parameter::LayerSlot { layer_id } => {
                // The exception. A layer whose slots are the client's leaves
                // the desk everything but the selection.
                let gui = show
                    .layer(layer_id)
                    .is_some_and(|layer| layer.slot_control == SlotControl::Gui);
                if !gui {
                    self.hold_layer(layer_id, LayerControl::Slot);
                }
            }
            // A retrigger chooses nothing: it starts again whatever the layer
            // is on, whoever put it there.
            Parameter::LayerTrigger { .. } => {}
            Parameter::LayerPositionX { layer_id }
            | Parameter::LayerPositionY { layer_id }
            | Parameter::LayerScaleX { layer_id }
            | Parameter::LayerScaleY { layer_id }
            | Parameter::LayerRotation { layer_id } => {
                self.hold_layer(layer_id, LayerControl::Transform);
            }

            Parameter::EffectParameter {
                effect_instance_id,
                key,
                ..
            } => {
                self.effects
                    .entry(effect_instance_id)
                    .or_default()
                    .insert(key.to_owned());
            }
            Parameter::GeneratorParameter {
                generator_instance_id,
                key,
                ..
            } => {
                self.generators
                    .entry(generator_instance_id)
                    .or_default()
                    .insert(key.to_owned());
            }
        }
    }

    fn hold_layer(&mut self, layer_id: LayerId, control: LayerControl) {
        self.layers.entry(layer_id).or_default().insert(control);
    }

    /// Whether the patch gives a desk nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.master.is_empty()
            && self.layers.is_empty()
            && self.effects.is_empty()
            && self.generators.is_empty()
    }

    /// Whether a desk holds one master control.
    #[must_use]
    pub fn holds_master(&self, control: MasterControl) -> bool {
        self.master.contains(&control)
    }

    /// Whether a desk holds one control of one layer.
    #[must_use]
    pub fn holds_layer(&self, layer_id: LayerId, control: LayerControl) -> bool {
        self.layers
            .get(&layer_id)
            .is_some_and(|controls| controls.contains(&control))
    }

    /// Whether a desk holds one parameter of one effect.
    #[must_use]
    pub fn holds_effect(&self, effect_instance_id: EffectInstanceId, key: &str) -> bool {
        self.effects
            .get(&effect_instance_id)
            .is_some_and(|keys| keys.contains(key))
    }

    /// Whether a desk holds one parameter of one generator.
    #[must_use]
    pub fn holds_generator(&self, generator_instance_id: GeneratorInstanceId, key: &str) -> bool {
        self.generators
            .get(&generator_instance_id)
            .is_some_and(|keys| keys.contains(key))
    }

    /// Every parameter a desk holds on one effect.
    pub fn effect_keys(&self, effect_instance_id: EffectInstanceId) -> impl Iterator<Item = &str> {
        self.effects
            .get(&effect_instance_id)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    /// Every parameter a desk holds on one generator.
    pub fn generator_keys(
        &self,
        generator_instance_id: GeneratorInstanceId,
    ) -> impl Iterator<Item = &str> {
        self.generators
            .get(&generator_instance_id)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }
}

/// Why an edit to a stored show was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditRefusal {
    /// The edit removes, moves or reorders something that is locked.
    #[error("{0}")]
    Locked(String),
    /// The edit changes a value the desk holds.
    #[error("{0}")]
    HeldByDesk(String),
}

/// Gives a document arriving whole the locks the stored show already has.
///
/// **A lock is never set or taken off by sending a document.** It has a
/// request of its own, which only an administrator may make, and every other
/// way into a show carries the locks across unchanged: an effect or a
/// generator `into` shares with `from` gets `from`'s lock, and one that is new
/// is unlocked.
///
/// Carried rather than refused, because the editor holds a copy of the show
/// that can be older than a lock. An operator saving a draft opened before an
/// administrator locked something has not asked to unlock it, and an
/// administrator saving one has not either; refusing the first would block a
/// save for a reason nobody can see, and obeying the second would undo a lock
/// without anybody touching it.
pub fn carry_locks(from: &Show, into: &mut Show) {
    let effects: BTreeMap<EffectInstanceId, bool> = effects_of(from)
        .into_iter()
        .map(|(id, (_, _, effect, _))| (id, effect.locked))
        .collect();
    let generators: BTreeMap<GeneratorInstanceId, bool> = generators_of(from)
        .into_iter()
        .map(|(id, generator)| (id, generator.locked))
        .collect();

    for layer in &mut into.layers {
        for effect in &mut layer.effects {
            effect.locked = effects.get(&effect.id).copied().unwrap_or(false);
        }
        for slot in &mut layer.slots {
            for effect in &mut slot.effects {
                effect.locked = effects.get(&effect.id).copied().unwrap_or(false);
            }
            if let SourceRef::Generator {
                instance_id,
                locked,
                ..
            } = &mut slot.source
            {
                *locked = generators.get(instance_id).copied().unwrap_or(false);
            }
        }
    }
}

/// Where an effect hangs: on a layer, or on one slot of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chain {
    Layer(LayerId),
    Slot(LayerId, u16),
}

/// Every effect in a show, with the chain it is in and its place there.
fn effects_of(show: &Show) -> BTreeMap<EffectInstanceId, (Chain, usize, &EffectInstance, &Layer)> {
    let mut found = BTreeMap::new();
    for layer in &show.layers {
        for (at, effect) in layer.effects.iter().enumerate() {
            found.insert(effect.id, (Chain::Layer(layer.id), at, effect, layer));
        }
        for slot in &layer.slots {
            for (at, effect) in slot.effects.iter().enumerate() {
                found.insert(
                    effect.id,
                    (Chain::Slot(layer.id, slot.index), at, effect, layer),
                );
            }
        }
    }
    found
}

/// One generator in a show: where it is, what it is, and whether it is locked.
struct GeneratorAt<'a> {
    layer: &'a Layer,
    slot_index: u16,
    type_id: &'a str,
    locked: bool,
    parameters: &'a ParameterMap,
}

/// Every generator in a show that has an identity, by that identity.
fn generators_of(show: &Show) -> BTreeMap<GeneratorInstanceId, GeneratorAt<'_>> {
    let mut found = BTreeMap::new();
    for layer in &show.layers {
        for slot in &layer.slots {
            if let SourceRef::Generator {
                instance_id,
                type_id,
                locked,
                parameters,
                ..
            } = &slot.source
            {
                if !instance_id.is_nil() {
                    found.insert(
                        *instance_id,
                        GeneratorAt {
                            layer,
                            slot_index: slot.index,
                            type_id,
                            locked: *locked,
                            parameters,
                        },
                    );
                }
            }
        }
    }
    found
}

/// What an operator calls an effect: its manifest's name, or its type.
fn effect_name(effect: &EffectInstance) -> &str {
    effects::manifest(&effect.type_id).map_or(effect.type_id.as_str(), |manifest| manifest.name)
}

/// What an operator calls a generator.
fn generator_name(type_id: &str) -> &str {
    generators::manifest(type_id).map_or(type_id, |manifest| manifest.name)
}

/// Holds an edit of a stored show to the lock and, where a desk is driving it,
/// to the desk.
///
/// `before` is the show as it is stored, `after` the show as the edit would
/// store it. `held` is what a desk holds right now — `None` when the show is
/// not the one on stage or the DMX switch is off, in which case no value is
/// the desk's.
///
/// # What the lock refuses
///
/// Whoever asks, an administrator included:
///
/// - A locked effect missing from `after`, in another chain, of another type,
///   or in another place among the effects of its chain that both documents
///   have. Adding an effect anywhere, or removing an unlocked one, leaves a
///   locked effect where it is and is not refused.
/// - A locked generator missing from its slot, or replaced by something else.
///   So is a layer removed with a locked effect or generator on it.
///
/// What it does not refuse is an effect being switched on or off, which is
/// everybody's, and its values, which follow `held`. Whether something is
/// locked at all is not this function's question: see [`carry_locks`].
///
/// # What a desk refuses
///
/// A configured value changed on a parameter the desk holds. In the editor
/// that value is still what a show puts on the wall when the desk has sent
/// nothing, so changing it would be the client moving something it may not.
///
/// # Errors
///
/// The first thing found wrong, said in a sentence an operator can act on.
pub fn check_edit(
    before: &Show,
    after: &Show,
    held: Option<&DeskControls>,
) -> Result<(), EditRefusal> {
    let effects_before = effects_of(before);
    let effects_after = effects_of(after);

    for (id, (chain, at, effect, layer)) in &effects_before {
        if !effect.locked {
            continue;
        }
        let name = effect_name(effect);
        let unlock_first = || {
            EditRefusal::Locked(format!(
                "{name} on layer {} is locked; an administrator has to unlock it first",
                layer.name
            ))
        };

        let Some((chain_after, at_after, effect_after, _)) = effects_after.get(id) else {
            return Err(unlock_first());
        };
        if chain_after != chain || effect_after.type_id != effect.type_id {
            return Err(unlock_first());
        }

        // Its place among the effects both documents have. Measured that way
        // so that adding an effect, or taking an unlocked one off, does not
        // count as moving the locked one.
        for (other, (other_chain, other_at, _, _)) in &effects_before {
            if other == id || other_chain != chain {
                continue;
            }
            let Some((other_chain_after, other_at_after, _, _)) = effects_after.get(other) else {
                continue;
            };
            if other_chain_after != chain {
                continue;
            }
            if (other_at < at) != (other_at_after < at_after) {
                return Err(unlock_first());
            }
        }
    }

    let generators_before = generators_of(before);
    let generators_after = generators_of(after);

    for (id, generator) in &generators_before {
        if !generator.locked {
            continue;
        }
        let still_there = generators_after.get(id).is_some_and(|after| {
            after.layer.id == generator.layer.id
                && after.slot_index == generator.slot_index
                && after.type_id == generator.type_id
        });
        if !still_there {
            return Err(EditRefusal::Locked(format!(
                "{} in slot {} of layer {} is locked; an administrator has to unlock it first",
                generator_name(generator.type_id),
                generator.slot_index,
                generator.layer.name
            )));
        }
    }

    let Some(held) = held else {
        return Ok(());
    };

    for (id, (_, _, effect, layer)) in &effects_after {
        let Some((_, _, effect_before, _)) = effects_before.get(id) else {
            continue;
        };
        let Some(manifest) = effects::manifest(&effect.type_id) else {
            continue;
        };
        for key in held.effect_keys(*id) {
            let was = manifest.value(&effect_before.parameters, key);
            let is = manifest.value(&effect.parameters, key);
            if was != is {
                return Err(held_by_desk(
                    key,
                    &format!("{} on layer {}", effect_name(effect), layer.name),
                ));
            }
        }
    }

    for (id, generator) in &generators_after {
        let Some(before) = generators_before.get(id) else {
            continue;
        };
        let Some(manifest) = generators::manifest(generator.type_id) else {
            continue;
        };
        for key in held.generator_keys(*id) {
            let was: Option<ParameterValue> = manifest.value(before.parameters, key);
            let is = manifest.value(generator.parameters, key);
            if was != is {
                return Err(held_by_desk(
                    key,
                    &format!(
                        "{} in slot {} of layer {}",
                        generator_name(generator.type_id),
                        generator.slot_index,
                        generator.layer.name
                    ),
                ));
            }
        }
    }

    Ok(())
}

/// The refusal for a value a desk holds, in the words every route uses.
fn held_by_desk(key: &str, what: &str) -> EditRefusal {
    EditRefusal::HeldByDesk(format!(
        "{key} of {what} is driven by the lighting console while DMX is on; \
         switch DMX off in Live to change it here"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::patch::FixtureTarget;
    use crate::config::{FixtureInstance, Slot, LAYER_V1, MASTER_V1};
    use crate::id::{FixtureInstanceId, MediaItemId};

    fn fixture(type_id: &str, footprint: u16, target: FixtureTarget) -> FixtureInstance {
        FixtureInstance {
            id: FixtureInstanceId::new(),
            type_id: type_id.to_owned(),
            version: 1,
            target,
            universe: 1,
            start_address: 1,
            footprint,
            enabled: true,
        }
    }

    /// A show with one layer and a blur on it, and their identities.
    fn a_show() -> (Show, LayerId, EffectInstanceId) {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("FX", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        let blur = EffectInstance::new("ovs.blur", 1);
        let blur_id = blur.id;
        layer.effects.push(blur);
        let layer_id = layer.id;
        show.layers.push(layer);
        (show, layer_id, blur_id)
    }

    #[test]
    fn nothing_patched_holds_nothing() {
        let (show, _, _) = a_show();
        assert_eq!(DeskControls::of(&show), DeskControls::default());
    }

    #[test]
    fn a_patched_master_holds_everything_but_the_tempo() {
        // The owner, 2026-10-19: tap is the one thing both may do.
        let (mut show, _, _) = a_show();
        show.fixture_patch.push(fixture(
            MASTER_V1.type_id,
            MASTER_V1.footprint,
            FixtureTarget::Master,
        ));

        let held = DeskControls::of(&show);
        for control in [
            MasterControl::Dimmer,
            MasterControl::Crossfader,
            MasterControl::SpeedScale,
            MasterControl::Freeze,
        ] {
            assert!(held.holds_master(control), "{control:?}");
        }
        assert_eq!(held.master.len(), 4, "and nothing else: {:?}", held.master);
    }

    #[test]
    fn a_patched_layer_holds_its_dimmer_slot_and_transform() {
        let (mut show, layer_id, _) = a_show();
        show.fixture_patch.push(fixture(
            LAYER_V1.type_id,
            LAYER_V1.footprint,
            FixtureTarget::Layer { layer_id },
        ));

        let held = DeskControls::of(&show);
        assert!(held.holds_layer(layer_id, LayerControl::Opacity));
        assert!(held.holds_layer(layer_id, LayerControl::Slot));
        assert!(held.holds_layer(layer_id, LayerControl::Transform));
    }

    #[test]
    fn a_layer_whose_slots_are_the_guis_leaves_the_desk_everything_else() {
        let (mut show, layer_id, _) = a_show();
        show.layers[0].slot_control = SlotControl::Gui;
        show.fixture_patch.push(fixture(
            LAYER_V1.type_id,
            LAYER_V1.footprint,
            FixtureTarget::Layer { layer_id },
        ));

        let held = DeskControls::of(&show);
        assert!(!held.holds_layer(layer_id, LayerControl::Slot));
        assert!(held.holds_layer(layer_id, LayerControl::Opacity));
        assert!(held.holds_layer(layer_id, LayerControl::Transform));
    }

    #[test]
    fn a_parked_fixture_holds_nothing() {
        let (mut show, layer_id, _) = a_show();
        let mut parked = fixture(
            LAYER_V1.type_id,
            LAYER_V1.footprint,
            FixtureTarget::Layer { layer_id },
        );
        parked.enabled = false;
        show.fixture_patch.push(parked);

        assert_eq!(DeskControls::of(&show), DeskControls::default());
    }

    #[test]
    fn a_fixture_the_receiver_would_refuse_holds_nothing() {
        // A footprint that disagrees is a fixture the receiver does not read,
        // so nothing it names is moved by anybody if it is held.
        let (mut show, layer_id, _) = a_show();
        show.fixture_patch.push(fixture(
            LAYER_V1.type_id,
            LAYER_V1.footprint + 1,
            FixtureTarget::Layer { layer_id },
        ));

        assert_eq!(DeskControls::of(&show), DeskControls::default());
    }

    #[test]
    fn a_patched_effect_holds_the_parameters_its_plugin_offers() {
        let (mut show, _, blur_id) = a_show();
        let personality = definition("ovs.blur").expect("blur offers itself to a desk");
        show.fixture_patch.push(fixture(
            personality.type_id,
            personality.footprint,
            FixtureTarget::Effect {
                effect_instance_id: blur_id,
            },
        ));

        let held = DeskControls::of(&show);
        let keys: Vec<&str> = held.effect_keys(blur_id).collect();
        assert!(!keys.is_empty(), "the blur's channels are held");
        for channel in personality.channels {
            assert!(held.holds_effect(blur_id, channel.parameter.name()));
        }
    }

    fn locked(mut show: Show) -> Show {
        show.layers[0].effects[0].locked = true;
        show
    }

    #[test]
    fn nobody_removes_a_locked_effect() {
        // Not an administrator either: there is no administrator's version of
        // this check, which is what makes unlocking a step of its own.
        let (show, _, _) = a_show();
        let before = locked(show);
        let mut after = before.clone();
        after.layers[0].effects.clear();

        assert!(matches!(
            check_edit(&before, &after, None),
            Err(EditRefusal::Locked(_))
        ));
    }

    #[test]
    fn a_layer_with_a_locked_effect_on_it_cannot_be_removed() {
        let (show, _, _) = a_show();
        let before = locked(show);
        let mut after = before.clone();
        after.layers.clear();

        assert!(matches!(
            check_edit(&before, &after, None),
            Err(EditRefusal::Locked(_))
        ));
    }

    #[test]
    fn a_locked_effect_cannot_be_moved_past_another() {
        let (show, _, _) = a_show();
        let mut before = locked(show);
        before.layers[0]
            .effects
            .push(EffectInstance::new("ovs.invert", 1));
        let mut after = before.clone();
        after.layers[0].effects.reverse();

        assert!(matches!(
            check_edit(&before, &after, None),
            Err(EditRefusal::Locked(_))
        ));
    }

    #[test]
    fn a_locked_effect_cannot_be_moved_to_a_slot() {
        let (show, _, _) = a_show();
        let before = locked(show);
        let mut after = before.clone();
        let effect = after.layers[0].effects.remove(0);
        after.layers[0].slots[0].effects.push(effect);

        assert!(matches!(
            check_edit(&before, &after, None),
            Err(EditRefusal::Locked(_))
        ));
    }

    #[test]
    fn adding_and_removing_around_a_locked_effect_leaves_it_where_it_is() {
        let (show, _, _) = a_show();
        let mut before = locked(show);
        before.layers[0]
            .effects
            .insert(0, EffectInstance::new("ovs.invert", 1));
        let mut after = before.clone();
        // The unlocked one before it goes, and two arrive either side.
        after.layers[0].effects.remove(0);
        after.layers[0]
            .effects
            .insert(0, EffectInstance::new("ovs.invert", 1));
        after.layers[0]
            .effects
            .push(EffectInstance::new("ovs.invert", 1));

        assert_eq!(check_edit(&before, &after, None), Ok(()));
    }

    #[test]
    fn switching_a_locked_effect_off_is_everybodys() {
        let (show, _, _) = a_show();
        let before = locked(show);
        let mut after = before.clone();
        after.layers[0].effects[0].enabled = false;

        assert_eq!(check_edit(&before, &after, None), Ok(()));
    }

    #[test]
    fn a_document_sent_whole_carries_the_locks_the_show_already_has() {
        // An editor's draft opened before an administrator locked the blur
        // says it is unlocked. Saving it has not asked for that.
        let (unlocked, _, _) = a_show();
        let stored = locked(unlocked.clone());

        let mut stale_draft = unlocked.clone();
        carry_locks(&stored, &mut stale_draft);
        assert!(stale_draft.layers[0].effects[0].locked, "the lock survives");

        // And one that says it is locked, when it is not, locks nothing.
        let mut claiming = stored.clone();
        carry_locks(&unlocked, &mut claiming);
        assert!(!claiming.layers[0].effects[0].locked);

        // A new effect in the document arrives unlocked, whatever it says.
        let mut adding = unlocked.clone();
        let mut sneaked = EffectInstance::new("ovs.invert", 1);
        sneaked.locked = true;
        adding.layers[0].effects.push(sneaked);
        carry_locks(&unlocked, &mut adding);
        assert!(!adding.layers[0].effects[1].locked);
    }

    #[test]
    fn a_generator_keeps_its_lock_through_a_document_sent_whole() {
        let (stored, _) = with_a_generator(true);
        let mut draft = stored.clone();
        if let SourceRef::Generator { locked, .. } = &mut draft.layers[0].slots[0].source {
            *locked = false;
        }
        carry_locks(&stored, &mut draft);
        assert!(matches!(
            draft.layers[0].slots[0].source,
            SourceRef::Generator { locked: true, .. }
        ));
    }

    fn with_a_generator(locked: bool) -> (Show, GeneratorInstanceId) {
        let (mut show, _, _) = a_show();
        let instance_id = GeneratorInstanceId::new();
        show.layers[0].slots[0].source = SourceRef::Generator {
            instance_id,
            type_id: "ovs.solid_colour".to_owned(),
            version: 1,
            locked,
            parameters: ParameterMap::new(),
        };
        (show, instance_id)
    }

    #[test]
    fn a_locked_generator_stays_in_its_slot() {
        let (before, _) = with_a_generator(true);

        let mut emptied = before.clone();
        emptied.layers[0].slots.clear();
        let mut replaced = before.clone();
        replaced.layers[0].slots[0].source = SourceRef::Media {
            media_item_id: MediaItemId::new(),
        };
        let mut moved = before.clone();
        moved.layers[0].slots[0].index = 2;

        for after in [emptied, replaced, moved] {
            assert!(matches!(
                check_edit(&before, &after, None),
                Err(EditRefusal::Locked(_))
            ));
        }
    }

    #[test]
    fn an_unlocked_generator_can_go() {
        let (before, _) = with_a_generator(false);
        let mut after = before.clone();
        after.layers[0].slots.clear();

        assert_eq!(check_edit(&before, &after, None), Ok(()));
    }

    #[test]
    fn a_value_the_desk_holds_cannot_be_changed_in_the_editor() {
        let (before, _, blur_id) = a_show();
        let personality = definition("ovs.blur").expect("blur offers itself to a desk");
        let key = personality.channels[0].parameter.name();
        let mut held = DeskControls::default();
        held.effects
            .entry(blur_id)
            .or_default()
            .insert(key.to_owned());

        let manifest = effects::manifest("ovs.blur").expect("published");
        let definition = manifest.parameter(key).expect("declared");
        let mut after = before.clone();
        after.layers[0].effects[0].parameters.insert(
            key.to_owned(),
            match definition.kind {
                crate::config::ParameterKind::Number { max, .. } => ParameterValue::Float(max),
                _ => panic!("the blur's first channel is a number"),
            },
        );

        assert!(matches!(
            check_edit(&before, &after, Some(&held)),
            Err(EditRefusal::HeldByDesk(_))
        ));
        // With the switch off, or on a show that is not on stage, it is the
        // editor's.
        assert_eq!(check_edit(&before, &after, None), Ok(()));
    }

    #[test]
    fn writing_down_the_value_a_desk_holds_is_not_changing_it() {
        // A client that writes the default it was showing has changed nothing.
        let (before, _, blur_id) = a_show();
        let personality = definition("ovs.blur").expect("blur offers itself to a desk");
        let key = personality.channels[0].parameter.name();
        let mut held = DeskControls::default();
        held.effects
            .entry(blur_id)
            .or_default()
            .insert(key.to_owned());

        let manifest = effects::manifest("ovs.blur").expect("published");
        let mut after = before.clone();
        after.layers[0].effects[0].parameters.insert(
            key.to_owned(),
            manifest
                .value(&ParameterMap::new(), key)
                .expect("a default"),
        );

        assert_eq!(check_edit(&before, &after, Some(&held)), Ok(()));
    }
}
