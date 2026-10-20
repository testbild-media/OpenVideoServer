//! The authoritative show and live state.
//!
//! This is the thing the specification means by "the server is authoritative"
//!. The client does not hold state and reconcile it; it
//! subscribes to this and displays what it says.
//!
//! Two properties are structural rather than conventional:
//!
//! - **A change cannot be emitted without advancing the sequence.** Every
//!   mutation returns a [`SequencedChange`] or nothing at all, and the sequence
//!   is advanced inside the same call that produces it. There is no way to
//!   broadcast a change the stream does not account for, which is what makes the
//!   client-side gap detection in `ovs_proto::SequenceTracker` meaningful.
//! - **Live state never reaches the show.** The show is held as a
//!   [`Show`] and is only ever replaced wholesale by a load. No method on this
//!   type writes a live value into it.

use std::collections::BTreeMap;
use std::time::Instant;

use ovs_clock::{MasterClock, TapTempo};
use ovs_proto::realtime::{Sequence, StateChange, StateSnapshot};
use ovs_types::config::slot::ParameterValue;
use ovs_types::config::{
    effects, generators, Choice, EffectInstance, Layer, ParameterMap, Show, SourceRef,
};
use ovs_types::control::DeskControls;
use ovs_types::id::{EffectInstanceId, GeneratorInstanceId, LayerId, ShowId};
use ovs_types::live::{EffectOverrides, GeneratorOverrides, LayerLive, LiveSnapshot, MasterLive};
use ovs_types::telemetry::RendererStatus;

/// Why a state change was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    /// No show is open, so there is nothing to change.
    #[error("no show is loaded")]
    NoShow,

    /// The layer is not part of the loaded show.
    #[error("layer {layer_id} is not in the loaded show")]
    UnknownLayer {
        /// The layer that was asked for.
        layer_id: LayerId,
    },

    /// The slot is not configured on that layer.
    #[error("layer {layer_id} has no slot {slot_index}")]
    UnknownSlot {
        /// The layer that was asked about.
        layer_id: LayerId,
        /// The slot that does not exist.
        slot_index: u16,
    },

    /// No effect instance in the loaded show has that identifier.
    #[error("effect instance {effect_instance_id} is not in the loaded show")]
    UnknownEffect {
        /// The instance that was asked for.
        effect_instance_id: EffectInstanceId,
    },

    /// The show names an effect this build does not publish.
    ///
    /// Not the caller's fault and not a value problem: the show arrived from
    /// a build that has an effect this one does not. Nothing can be
    /// validated against a manifest that is not here, so the value is refused
    /// rather than applied unchecked.
    #[error("{type_id} is not published by this build")]
    UnpublishedEffect {
        /// The effect type the instance names.
        type_id: String,
    },

    /// The show has no generator instance with that identity.
    #[error("this show has no generator instance {generator_instance_id}")]
    UnknownGenerator {
        /// The instance that was asked for.
        generator_instance_id: GeneratorInstanceId,
    },

    /// The show names a generator this build does not publish.
    ///
    /// The same as [`UnpublishedEffect`](Self::UnpublishedEffect) for the other
    /// kind of module: a show from a build with a plugin this one lacks.
    #[error("this build does not publish generator {type_id}")]
    UnpublishedGenerator {
        /// The generator type the slot names.
        type_id: String,
    },

    /// A live value does not match what the effect declares for that parameter.
    #[error("{key} {reason}")]
    InvalidEffectParameter {
        /// The parameter key that was sent.
        key: String,
        /// Why it cannot be used, as the manifest puts it.
        reason: String,
    },
}

/// A change together with the sequence it happened at.
///
/// Produced only by the mutating methods of [`AuthoritativeState`], so a change
/// and its position in the stream cannot come apart.
#[derive(Debug, Clone, PartialEq)]
pub struct SequencedChange {
    /// Position in the update stream.
    pub sequence: Sequence,
    /// What changed.
    pub change: StateChange,
}

/// The server's authoritative state.
#[derive(Debug, Clone)]
pub struct AuthoritativeState {
    sequence: Sequence,
    show: Option<Show>,
    master: MasterLive,
    layers: BTreeMap<LayerId, LayerLive>,
    renderer: RendererStatus,
    /// The one global clock.
    ///
    /// Held here because there is exactly one of it and this is where the one
    /// authoritative state lives. A layer has a ratio to it, never its own.
    clock: MasterClock,
    /// Taps waiting to become a tempo.
    tap: TapTempo,
    /// What a control source has done to the effects since the show opened.
    ///
    /// Live state, held beside the show and never inside it: the show
    /// keeps what each effect was configured with, and reopening it gives that
    /// back.
    effects: EffectOverrides,
    /// What a control source has done to the generators, by instance.
    generators: GeneratorOverrides,
    /// What the open show's patch gives a lighting console.
    ///
    /// Read once per document rather than per request: it changes when the
    /// show does, and every live request from a client asks it.
    desk: DeskControls,
}

impl Default for AuthoritativeState {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthoritativeState {
    /// Creates empty state: no show, renderer stopped.
    #[must_use]
    pub fn new() -> Self {
        let master = MasterLive::default();
        Self {
            sequence: Sequence::START,
            show: None,
            master,
            layers: BTreeMap::new(),
            generators: GeneratorOverrides::new(),
            renderer: RendererStatus::Stopped,
            clock: MasterClock::new(master.bpm, 4, Instant::now()),
            tap: TapTempo::new(),
            effects: EffectOverrides::new(),
            desk: DeskControls::default(),
        }
    }

    /// Returns the current position in the update stream.
    #[must_use]
    pub const fn sequence(&self) -> Sequence {
        self.sequence
    }

    /// Returns the loaded show, if there is one.
    #[must_use]
    pub const fn show(&self) -> Option<&Show> {
        self.show.as_ref()
    }

    /// Returns the identifier of the loaded show, if there is one.
    #[must_use]
    pub fn show_id(&self) -> Option<ShowId> {
        self.show.as_ref().map(|show| show.id)
    }

    /// Returns the live master state.
    #[must_use]
    pub const fn master(&self) -> &MasterLive {
        &self.master
    }

    /// What the open show's patch gives a lighting console, switch or no switch.
    #[must_use]
    pub const fn desk(&self) -> &DeskControls {
        &self.desk
    }

    /// What a lighting console holds right now, so that a client may not move it.
    ///
    /// `None` while the DMX switch is off, which is the client holding the
    /// whole show (owner, 2026-10-19; see [`ovs_types::control`]).
    #[must_use]
    pub const fn desk_holds(&self) -> Option<&DeskControls> {
        if self.master.dmx_enabled {
            Some(&self.desk)
        } else {
            None
        }
    }

    /// Returns the global clock.
    #[must_use]
    pub const fn clock(&self) -> &MasterClock {
        &self.clock
    }

    /// Records a tap and, once there is a tempo, applies it.
    ///
    /// This is the Master Fixture's BPM Tap. It reaches the same clock whether
    /// it came from a button in the client or from a DMX channel crossing a
    /// threshold.
    pub fn tap_tempo(&mut self, now: Instant) -> Option<SequencedChange> {
        let tempo = self.tap.tap(now)?;
        self.update_master_at(now, |master| master.set_bpm(tempo))
    }

    /// Resets the clock's phase without changing the tempo.
    ///
    /// The Master Fixture's BPM Reset. It changes no live value, so it emits no
    /// update: the phase is derived from the clock, not carried in state.
    pub fn reset_phase(&mut self, now: Instant) {
        self.clock.reset(now);
    }

    /// Returns the size of the loaded show's composition.
    ///
    /// `None` with nothing open. A control that is a fraction of the canvas —
    /// a layer's position — needs this, and a fixture cannot carry it: a
    /// personality is patched once and the canvas changes with the show.
    #[must_use]
    pub fn composition_size(&self) -> Option<ovs_types::geometry::SizePx> {
        self.show.as_ref().map(|show| show.composition.size)
    }

    /// Returns the live state of one layer.
    #[must_use]
    pub fn layer(&self, layer_id: LayerId) -> Option<&LayerLive> {
        self.layers.get(&layer_id)
    }

    /// Returns what the renderer is doing.
    #[must_use]
    pub const fn renderer(&self) -> &RendererStatus {
        &self.renderer
    }

    /// Builds the authoritative snapshot a connecting client receives.
    #[must_use]
    pub fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            sequence: self.sequence,
            show: self.show.clone(),
            master: self.master,
            layers: self.layers.values().copied().collect(),
            effects: self.effects.clone(),
            generators: self.generators.clone(),
            desk: self.desk.clone(),
            renderer: self.renderer.clone(),
        }
    }

    /// Opens a show, applying the restart policy.
    ///
    /// Live state comes from `snapshot` — the show as it was when it was last
    /// open, all of it (owner, 2026-10-19) — and from the show's defaults where
    /// there is none. Never from whatever the previous show was doing, with one
    /// exception: the DMX switch, which is the server's and stays as it is.
    pub fn load_show(
        &mut self,
        mut show: Show,
        snapshot: Option<&LiveSnapshot>,
    ) -> SequencedChange {
        // A generator written before generators could be patched has no
        // identity of its own. It gets one here rather than at save time, so
        // that everything downstream — a patch, a live value, the renderer —
        // sees one identity for the whole time the show is open.
        show.assign_missing_ids();
        // And one of each: a show saved while a module still had a second
        // version opens numbered as the module now is.
        show.adopt_one_of_each();
        let dmx_enabled = self.master.dmx_enabled;
        self.master = MasterLive::on_restart(&show.master, snapshot);
        self.master.dmx_enabled = dmx_enabled;
        self.layers = show
            .layers
            .iter()
            .map(|layer| (layer.id, LayerLive::on_restart(layer, snapshot)))
            .collect();

        // What a control source had put on the effects and generators comes
        // back too, as far as this show and this build can still take it: an
        // instance the show no longer has, or a value its manifest now
        // refuses, is dropped rather than restored as something nothing can
        // draw.
        self.effects = snapshot
            .map(|snapshot| restorable_effects(&show, &snapshot.effects))
            .unwrap_or_default();
        self.generators = snapshot
            .map(|snapshot| restorable_generators(&show, &snapshot.generators))
            .unwrap_or_default();
        self.desk = DeskControls::of(&show);

        // A fresh clock at the show's tempo. The phase deliberately starts at
        // zero: it is not restored across a load, so the first beat of a show is
        // a known point rather than an extrapolation from a clock that was not
        // running. The tap run is dropped for the same reason — half
        // a run from the last show must not combine with the first tap of this one.
        let now = Instant::now();
        self.clock = MasterClock::new(self.master.bpm, show.master.beats_per_bar, now);
        self.tap = TapTempo::new();

        self.show = Some(show);

        self.emit(StateChange::ShowLoaded {
            show: self.show.clone().map(Box::new),
            master: self.master,
            layers: self.layers.values().copied().collect(),
            effects: self.effects.clone(),
            generators: self.generators.clone(),
            desk: self.desk.clone(),
        })
    }

    /// Writes what is on stage right now into a document.
    ///
    /// **What a save is**, since 2026-09-23. The owner's instruction: "Save
    /// stores the complete current live state into the show." An operator
    /// who has spent an evening finding a look presses Save and the document
    /// holds that look; reopening the show gives it back.
    ///
    /// This does not contradict the rule that a value arriving over DMX must
    /// never reach a show file. That rule is about a *silent* write-back — live state leaking into the
    /// document because some code path forgot which it was holding. This is an
    /// operator pressing a button labelled Save, and what it captures is what
    /// they can see. The types stay separate: the live values are read out of
    /// `ovs_types::live` and written into the matching `config` fields by hand,
    /// here, where it can be read.
    ///
    /// What is captured is what an operator would expect to get back: each
    /// layer's bus, opacity, blend mode, trigger grid and whether it is on, and
    /// the master's tempo, dimmer, crossfader and speed. What is **not** is the
    /// slot each layer is playing and the transport it is in — those are the
    /// restart snapshot's, which already survives a
    /// restart and is deliberately not part of the document.
    ///
    /// Effect and generator parameters a control source has moved are captured
    /// too, because they are part of the look and the operator moved them.
    pub fn capture_live(&self, show: &mut Show) {
        show.master.default_bpm = self.master.bpm;
        show.master.default_dimmer = self.master.dimmer;
        show.master.default_crossfader = self.master.crossfader;
        show.master.default_speed_scale = self.master.speed_scale;

        for layer in &mut show.layers {
            let Some(live) = self.layers.get(&layer.id) else {
                continue;
            };
            layer.enabled = live.enabled;
            layer.route = live.route;
            layer.default_opacity = live.opacity;
            layer.blend_mode = live.blend_mode;

            for effect in &mut layer.effects {
                Self::capture_parameters(&self.effects, effect);
            }
            for slot in &mut layer.slots {
                for effect in &mut slot.effects {
                    Self::capture_parameters(&self.effects, effect);
                }
                if let SourceRef::Generator {
                    instance_id,
                    parameters,
                    ..
                } = &mut slot.source
                {
                    if let Some(live) = self.generators.get(instance_id) {
                        for (key, value) in live.iter() {
                            parameters.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
        }
    }

    /// Folds one instance's live values into what the document configures.
    ///
    /// Merged rather than replaced: the overrides hold only what has been
    /// moved, and replacing would clear every parameter nobody touched back to
    /// the manifest's default.
    fn capture_parameters(overrides: &EffectOverrides, effect: &mut EffectInstance) {
        let Some(live) = overrides.get(&effect.id) else {
            return;
        };
        for (key, value) in live.iter() {
            effect.parameters.insert(key.clone(), value.clone());
        }
    }

    /// Replaces the open show's document without restarting the show.
    ///
    /// An edit made while a show is running — a clip dragged onto an empty
    /// pad — changes the document, and nothing else.
    /// [`load_show`](Self::load_show) is the obvious way to install it
    /// and is the wrong one: it applies the restart policy, so every layer
    /// would jump to its opening slot and the master to its opening dimmer,
    /// mid-show, because somebody filled a pad they were not using.
    ///
    /// So the live state is carried across untouched, and so is the clock: the
    /// beat does not restart either. A layer the new document does not have
    /// loses its live state; one it has gained starts at what the restart
    /// policy says, which is the only answer there is for a layer that has
    /// never been live.
    ///
    /// The change published is [`StateChange::ShowLoaded`] — "the loaded
    /// one was replaced", which is what its own documentation says it covers.
    /// A peer applies the document and the live state that travels with it,
    /// and because that live state is the one already running, nothing moves.
    /// It carries the whole document rather than the one slot that changed,
    /// for the same reason a save does: a show is edited whole, and a
    /// field-level diff is more machinery than this deserves.
    pub fn replace_show(&mut self, mut show: Show) -> SequencedChange {
        show.assign_missing_ids();
        show.adopt_one_of_each();

        let mut layers = BTreeMap::new();
        for layer in &show.layers {
            let live = self
                .layers
                .get(&layer.id)
                .copied()
                .unwrap_or_else(|| LayerLive::on_restart(layer, None));
            layers.insert(layer.id, live);
        }
        self.layers = layers;

        // A patch edit is a document edit, so this is where a fixture parked or
        // patched starts or stops holding what it drives.
        self.desk = DeskControls::of(&show);
        self.show = Some(show);

        self.emit(StateChange::ShowLoaded {
            show: self.show.clone().map(Box::new),
            master: self.master,
            layers: self.layers.values().copied().collect(),
            effects: self.effects.clone(),
            generators: self.generators.clone(),
            desk: self.desk.clone(),
        })
    }

    /// Takes a layer off stage, if it is showing the slot named.
    ///
    /// For a slot that is being emptied. A layer left pointing at a slot that
    /// no longer exists shows its placeholder, which on the night reads as a
    /// fault rather than as the empty pad it is.
    ///
    /// Publishes nothing on its own: it is called immediately before
    /// [`replace_show`](Self::replace_show), whose change carries the
    /// live state out with the document.
    ///
    /// Black rather than the next slot along, because black is the one thing
    /// an operator can always ask for and the one thing that is never wrong.
    pub fn black_out_slot(&mut self, layer_id: LayerId, slot_index: u16) {
        if let Some(live) = self.layers.get_mut(&layer_id) {
            if live.active_slot == slot_index {
                live.active_slot = Layer::BLACK_SLOT_INDEX;
            }
        }
    }

    /// Keeps a layer on what it is showing when the slot under it moves.
    ///
    /// For a pad dragged to another pad. A layer showing slot 1 when slot 1 is
    /// moved to 10 goes to 10 with it, and one showing 10 when 1 and 10 swap
    /// goes to 1: **an edit does not change what is on stage**, which is the
    /// rule a save already keeps. Left alone, the layer would show whatever
    /// had arrived at its old number — nothing, or the other clip.
    ///
    /// The clip starts its pass again, because a new slot number is a new pass
    /// to the renderer, as it is when a pad is pressed.
    ///
    /// Publishes nothing on its own, like [`black_out_slot`](Self::black_out_slot):
    /// [`replace_show`](Self::replace_show) carries it out with the document.
    pub fn follow_moved_slot(&mut self, layer_id: LayerId, from: u16, to: u16) {
        if let Some(live) = self.layers.get_mut(&layer_id) {
            if live.active_slot == from {
                live.active_slot = to;
            } else if live.active_slot == to {
                live.active_slot = from;
            }
        }
    }

    /// Closes the loaded show, leaving the outputs black.
    ///
    /// Returns `None` when no show was open, so closing twice does not
    /// produce a change the stream has to carry.
    pub fn close_show(&mut self) -> Option<SequencedChange> {
        self.show.take()?;
        self.layers.clear();
        self.effects.clear();
        self.generators.clear();
        self.desk = DeskControls::default();
        Some(self.emit(StateChange::ShowLoaded {
            show: None,
            master: self.master,
            layers: Vec::new(),
            effects: EffectOverrides::new(),
            generators: GeneratorOverrides::new(),
            desk: DeskControls::default(),
        }))
    }

    /// Selects a slot on a layer.
    ///
    /// Slot 0 is the permanently black slot and is always selectable, on any
    /// layer, without being configured anywhere.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::UnknownLayer`] or [`StateError::UnknownSlot`]. A
    /// refused change does not advance the sequence.
    pub fn select_slot(
        &mut self,
        layer_id: LayerId,
        slot_index: u16,
    ) -> Result<Option<SequencedChange>, StateError> {
        self.select_slot_at(layer_id, slot_index, Instant::now())
    }

    /// Selects a slot as of a given moment.
    ///
    /// The moment no longer decides anything — a pad takes effect when it is
    /// pressed — and it is kept as an argument because the tests that hold this
    /// behaviour down press at a stated instant, which is how they say "not on
    /// a boundary" without a clock of their own.
    ///
    /// # Errors
    ///
    /// As [`select_slot`](Self::select_slot).
    pub fn select_slot_at(
        &mut self,
        layer_id: LayerId,
        slot_index: u16,
        now: Instant,
    ) -> Result<Option<SequencedChange>, StateError> {
        let show = self.show.as_ref().ok_or(StateError::NoShow)?;
        let layer = show
            .layer(layer_id)
            .ok_or(StateError::UnknownLayer { layer_id })?;

        // **An empty pad is black** (owner, 2026-09-23): "jeder nicht belegte
        // Slot ist ein Black/Full Tranzparent für den Layer."
        //
        // It used to be refused, which made an operator reach for slot 0 to
        // take a layer off — a thirteenth pad that existed for that alone. Now
        // any pad the layer has banks for is somewhere to go, and one with
        // nothing in it shows nothing, which is what black is. Slot 0 stays in
        // the model and on the DMX desk, where it is a number a console sends.
        let highest = layer.bank_count() * Layer::SLOTS_PER_BANK;
        if slot_index != Layer::BLACK_SLOT_INDEX && slot_index > highest {
            return Err(StateError::UnknownSlot {
                layer_id,
                slot_index,
            });
        }

        // **A pad takes effect when it is pressed** (owner, 2026-09-23):
        // "Trigger Grid weg. Beats ist das was man braucht."
        //
        // There used to be a grid here, and a whole machinery under it: a
        // planner that worked out the next boundary, a map of switches waiting
        // for one, a timer that fired them, a `pending_slot` on the live state
        // and a decoder opened early so the beat did not start with a seek.
        // None of it has anything to answer any more. What a slot says about
        // time is how long its pass is and how fast to run through it — the
        // beats, not when the beat starts.
        let _ = now;
        Ok(Some(self.switch_now(layer_id, slot_index)?))
    }

    /// Switches a layer's slot now, and says so.
    fn switch_now(
        &mut self,
        layer_id: LayerId,
        slot_index: u16,
    ) -> Result<SequencedChange, StateError> {
        let live = self
            .layers
            .get_mut(&layer_id)
            .ok_or(StateError::UnknownLayer { layer_id })?;

        // Always a change, even when the slot is the one already running.
        // Re-selecting it is a retrigger — an operator hitting the same pad to
        // restart the clip — and the slot index alone cannot say that happened.
        // The counter can, and the renderer restarts the source when it moves.
        live.active_slot = slot_index;
        live.trigger = live.trigger.saturating_add(1);
        let layer = *live;
        Ok(self.emit(StateChange::LayerChanged { layer }))
    }

    /// Applies a master update from a control source.
    ///
    /// Returns `None` when the update left the state unchanged, which is the
    /// common case for a console re-sending the same DMX frame at 44 Hz.
    pub fn update_master(
        &mut self,
        apply: impl FnOnce(&mut MasterLive),
    ) -> Option<SequencedChange> {
        self.update_master_at(Instant::now(), apply)
    }

    /// Applies a master update as of a given moment.
    ///
    /// The clock follows tempo and speed changes here rather than at each call
    /// site, so there is no path that changes the BPM an operator sees without
    /// changing the clock the show actually runs on.
    pub fn update_master_at(
        &mut self,
        now: Instant,
        apply: impl FnOnce(&mut MasterLive),
    ) -> Option<SequencedChange> {
        let before = self.master;
        apply(&mut self.master);
        self.master.clamp_in_place();

        if self.master == before {
            return None;
        }

        // Re-anchoring rather than restarting: the beat position must not jump
        // when a tempo changes mid-show.
        if self.master.bpm != before.bpm {
            self.clock.set_bpm(self.master.bpm, now);
        }
        if self.master.speed_scale != before.speed_scale {
            self.clock.set_speed_scale(self.master.speed_scale, now);
        }

        let master = self.master;
        Some(self.emit(StateChange::MasterChanged { master }))
    }

    /// Applies a live update to one layer.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::UnknownLayer`] when the layer is not loaded.
    pub fn update_layer(
        &mut self,
        layer_id: LayerId,
        apply: impl FnOnce(&mut LayerLive),
    ) -> Result<Option<SequencedChange>, StateError> {
        let live = self
            .layers
            .get_mut(&layer_id)
            .ok_or(StateError::UnknownLayer { layer_id })?;

        let before = *live;
        apply(live);
        live.clamp_in_place();

        if *live == before {
            return Ok(None);
        }
        let layer = *live;
        Ok(Some(self.emit(StateChange::LayerChanged { layer })))
    }

    /// Returns everything a control source has done to the effects.
    ///
    /// Only the values that were moved. An instance that is absent is one
    /// running at exactly what the show configured.
    #[must_use]
    pub const fn effect_overrides(&self) -> &EffectOverrides {
        &self.effects
    }

    /// Returns everything a control source has done to the generators.
    #[must_use]
    pub const fn generator_overrides(&self) -> &GeneratorOverrides {
        &self.generators
    }

    /// Returns what one parameter of one generator instance is worth now.
    ///
    /// The generator twin of [`effect_value`](Self::effect_value), and the same
    /// order: the live value, the slot's, then the manifest's default.
    #[must_use]
    pub fn generator_value(
        &self,
        generator_instance_id: GeneratorInstanceId,
        key: &str,
    ) -> Option<ParameterValue> {
        if let Some(value) = self
            .generators
            .get(&generator_instance_id)
            .and_then(|live| live.get(key))
        {
            return Some(value.clone());
        }

        let show = self.show.as_ref()?;
        let (type_id, parameters) = generator_instance(show, generator_instance_id)?;
        let manifest = generators::manifest(type_id)?;
        manifest.value(parameters, key)
    }

    /// The choices one parameter of one effect instance offers, in band order.
    ///
    /// `None` unless that parameter is a choice list. The DMX side needs it
    /// because a choice channel answers with a band number and the vocabulary
    /// lives in the manifest.
    #[must_use]
    pub fn effect_choices(
        &self,
        effect_instance_id: EffectInstanceId,
        key: &str,
    ) -> Option<&'static [Choice]> {
        let show = self.show.as_ref()?;
        let instance = effect_instance(show, effect_instance_id)?;
        let manifest = effects::manifest(&instance.type_id)?;
        choices_of(manifest.parameter(key)?)
    }

    /// The same, for a generator instance.
    #[must_use]
    pub fn generator_choices(
        &self,
        generator_instance_id: GeneratorInstanceId,
        key: &str,
    ) -> Option<&'static [Choice]> {
        let show = self.show.as_ref()?;
        let (type_id, _) = generator_instance(show, generator_instance_id)?;
        let manifest = generators::manifest(type_id)?;
        choices_of(manifest.parameter(key)?)
    }

    /// Puts live values on one generator instance's parameters.
    ///
    /// Everything [`set_effect_parameters`](Self::set_effect_parameters) is,
    /// for the other kind of module: checked whole before anything is applied,
    /// absent rather than empty when nothing is overridden, and silent when the
    /// request changed nothing.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::NoShow`] with nothing open,
    /// [`StateError::UnknownGenerator`] for an instance this show does not
    /// have, [`StateError::UnpublishedGenerator`] for one this build cannot
    /// describe, and [`StateError::InvalidEffectParameter`] for a value the
    /// manifest refuses.
    pub fn set_generator_parameters(
        &mut self,
        generator_instance_id: GeneratorInstanceId,
        overrides: &ParameterMap,
        reset: bool,
    ) -> Result<Option<SequencedChange>, StateError> {
        let show = self.show.as_ref().ok_or(StateError::NoShow)?;
        let (type_id, _) = generator_instance(show, generator_instance_id).ok_or(
            StateError::UnknownGenerator {
                generator_instance_id,
            },
        )?;
        let manifest =
            generators::manifest(type_id).ok_or_else(|| StateError::UnpublishedGenerator {
                type_id: type_id.to_owned(),
            })?;

        for (key, value) in overrides {
            if let Some(reason) = manifest.refuse(key, value) {
                return Err(StateError::InvalidEffectParameter {
                    key: key.clone(),
                    reason,
                });
            }
        }

        let mut next = if reset {
            ParameterMap::new()
        } else {
            self.generators
                .get(&generator_instance_id)
                .cloned()
                .unwrap_or_default()
        };
        for (key, value) in overrides {
            next.insert(key.clone(), value.clone());
        }

        let unchanged = self
            .generators
            .get(&generator_instance_id)
            .map_or(next.is_empty(), |current| *current == next);
        if unchanged {
            return Ok(None);
        }

        if next.is_empty() {
            self.generators.remove(&generator_instance_id);
        } else {
            self.generators.insert(generator_instance_id, next.clone());
        }

        Ok(Some(self.emit(StateChange::GeneratorChanged {
            generator_instance_id,
            overrides: next,
        })))
    }

    /// Returns the live values on one effect instance, if it has any.
    #[must_use]
    pub fn effect(&self, effect_instance_id: EffectInstanceId) -> Option<&ParameterMap> {
        self.effects.get(&effect_instance_id)
    }

    /// Returns what one parameter of one effect instance is worth now.
    ///
    /// The live value where a control source has moved it, the show's where
    /// it has not, and the manifest's default where the show did not set it
    /// either — which is the same order every other reader of an effect's
    /// values uses. `None` for an instance this show does not have, for an
    /// effect this build cannot describe, or for a key its manifest does not
    /// declare.
    ///
    /// It exists for the DMX path: a colour arrives as four separate channels,
    /// and each one has to read the whole colour before it can replace its own
    /// part of it.
    #[must_use]
    pub fn effect_value(
        &self,
        effect_instance_id: EffectInstanceId,
        key: &str,
    ) -> Option<ParameterValue> {
        if let Some(value) = self
            .effects
            .get(&effect_instance_id)
            .and_then(|live| live.get(key))
        {
            return Some(value.clone());
        }

        let show = self.show.as_ref()?;
        let instance = effect_instance(show, effect_instance_id)?;
        let manifest = effects::manifest(&instance.type_id)?;
        manifest.value(&instance.parameters, key)
    }

    /// Puts live values on one effect instance's parameters.
    ///
    /// `overrides` names only the parameters to move; the rest keep whatever
    /// they had, which is either an earlier live value or the show's.
    /// `reset` drops every live value on the instance first, which is how an
    /// operator puts an effect back the way the show was built — something no
    /// set of values can express, because "no override" is an absence.
    ///
    /// Returns `None` when the request left the state exactly as it was, which
    /// is what a client holding a control still produces. Nothing is published
    /// in that case, for the same reason a console re-sending a DMX frame at
    /// 44 Hz publishes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::NoShow`] with nothing open,
    /// [`StateError::UnknownEffect`] for an instance this show does not have,
    /// [`StateError::UnpublishedEffect`] for one this build cannot describe, and
    /// [`StateError::InvalidEffectParameter`] for a value the manifest refuses.
    /// A refused request changes nothing at all, and publishes nothing, so a
    /// client can tell a refusal from a change by the answer it gets and from
    /// silence by the update that does or does not follow.
    pub fn set_effect_parameters(
        &mut self,
        effect_instance_id: EffectInstanceId,
        overrides: &ParameterMap,
        reset: bool,
    ) -> Result<Option<SequencedChange>, StateError> {
        let show = self.show.as_ref().ok_or(StateError::NoShow)?;
        let instance = effect_instance(show, effect_instance_id)
            .ok_or(StateError::UnknownEffect { effect_instance_id })?;
        let manifest =
            effects::manifest(&instance.type_id).ok_or_else(|| StateError::UnpublishedEffect {
                type_id: instance.type_id.clone(),
            })?;

        // Everything is checked before anything is applied. A request naming
        // three parameters, one of them wrong, must not leave an effect halfway
        // between two looks with no way to say which half arrived.
        for (key, value) in overrides {
            if let Some(reason) = manifest.refuse(key, value) {
                return Err(StateError::InvalidEffectParameter {
                    key: key.clone(),
                    reason,
                });
            }
        }

        let mut next = if reset {
            ParameterMap::new()
        } else {
            self.effects
                .get(&effect_instance_id)
                .cloned()
                .unwrap_or_default()
        };
        for (key, value) in overrides {
            next.insert(key.clone(), value.clone());
        }

        // An instance with nothing on it is held as an absence rather than as an
        // empty map, so there is one representation of "running at what the
        // show configured" instead of two.
        let unchanged = self
            .effects
            .get(&effect_instance_id)
            .map_or(next.is_empty(), |current| *current == next);
        if unchanged {
            return Ok(None);
        }

        if next.is_empty() {
            self.effects.remove(&effect_instance_id);
        } else {
            self.effects.insert(effect_instance_id, next.clone());
        }

        Ok(Some(self.emit(StateChange::EffectChanged {
            effect_instance_id,
            overrides: next,
        })))
    }

    /// Records what the renderer is doing.
    pub fn set_renderer(&mut self, renderer: RendererStatus) -> Option<SequencedChange> {
        if self.renderer == renderer {
            return None;
        }
        self.renderer = renderer.clone();
        Some(self.emit(StateChange::RendererChanged { renderer }))
    }

    /// Builds the snapshot to persist so a restart can restore what it should.
    ///
    /// All of it (owner, 2026-10-19): the master, every layer, every live
    /// value on an effect or a generator. The tempo and the selected slots are
    /// written the old way as well, which costs a few bytes and lets a build
    /// from before this read what it left.
    #[must_use]
    pub fn live_snapshot(&self) -> LiveSnapshot {
        LiveSnapshot {
            bpm: Some(self.master.bpm),
            selected_slots: self
                .layers
                .iter()
                .map(|(layer_id, live)| (*layer_id, live.active_slot))
                .collect(),
            master: Some(self.master),
            layers: self.layers.clone(),
            effects: self.effects.clone(),
            generators: self.generators.clone(),
        }
    }

    /// Advances the sequence and pairs it with the change.
    ///
    /// The only way a change gets a sequence, which is what keeps the two from
    /// drifting apart.
    fn emit(&mut self, change: StateChange) -> SequencedChange {
        self.sequence = self.sequence.next();
        SequencedChange {
            sequence: self.sequence,
            change,
        }
    }
}

/// Finds one effect instance, wherever in the show it hangs.
///
/// A chain hangs off a layer and off each of its slots, and an
/// instance identifier is unique across both, so a live value names an instance
/// and never a position. The search is linear over a handful of effects on a
/// handful of layers, and it happens when an operator moves a control rather
/// than per frame.
/// The choice list a parameter offers, when it offers one.
fn choices_of(
    definition: &'static ovs_types::config::ParameterDefinition,
) -> Option<&'static [Choice]> {
    match definition.kind {
        ovs_types::config::ParameterKind::Choice { choices, .. } => Some(choices),
        _ => None,
    }
}

/// Finds one generator instance in a show, wherever its slot is.
///
/// Returns what a caller needs to check a value against: the type and what the
/// slot configured. A generator is a source rather than a chain
/// entry, so there is one per slot and the identity is the slot's own.
fn generator_instance(
    show: &Show,
    generator_instance_id: GeneratorInstanceId,
) -> Option<(&str, &ParameterMap)> {
    show.layers
        .iter()
        .flat_map(|layer| layer.slots.iter())
        .find_map(|slot| match &slot.source {
            ovs_types::config::SourceRef::Generator {
                instance_id,
                type_id,
                parameters,
                ..
            } if *instance_id == generator_instance_id => Some((type_id.as_str(), parameters)),
            _ => None,
        })
}

/// The live effect values a snapshot kept that this show can still take.
///
/// An instance the show no longer has is dropped, and so is a value its
/// manifest refuses — a build that has changed what an effect accepts must not
/// be handed a value it would refuse from a client.
fn restorable_effects(show: &Show, kept: &EffectOverrides) -> EffectOverrides {
    let mut restored = EffectOverrides::new();
    for (id, values) in kept {
        let Some(manifest) =
            effect_instance(show, *id).and_then(|instance| effects::manifest(&instance.type_id))
        else {
            continue;
        };
        let accepted: ParameterMap = values
            .iter()
            .filter(|(key, value)| manifest.refuse(key, value).is_none())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if !accepted.is_empty() {
            restored.insert(*id, accepted);
        }
    }
    restored
}

/// The same, for the generators.
fn restorable_generators(show: &Show, kept: &GeneratorOverrides) -> GeneratorOverrides {
    let mut restored = GeneratorOverrides::new();
    for (id, values) in kept {
        let Some(manifest) =
            generator_instance(show, *id).and_then(|(type_id, _)| generators::manifest(type_id))
        else {
            continue;
        };
        let accepted: ParameterMap = values
            .iter()
            .filter(|(key, value)| manifest.refuse(key, value).is_none())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if !accepted.is_empty() {
            restored.insert(*id, accepted);
        }
    }
    restored
}

pub(crate) fn effect_instance(
    show: &Show,
    effect_instance_id: EffectInstanceId,
) -> Option<&EffectInstance> {
    show.layers
        .iter()
        .flat_map(|layer| {
            layer
                .effects
                .iter()
                .chain(layer.slots.iter().flat_map(|slot| slot.effects.iter()))
        })
        .find(|instance| instance.id == effect_instance_id)
}

#[cfg(test)]
mod tests {
    use ovs_types::config::{MasterConfig, Slot, SourceRef};
    use ovs_types::id::MediaItemId;

    use super::*;

    fn show() -> Show {
        let mut show = Show::new("Show");
        show.master = MasterConfig {
            default_bpm: 120.0,
            default_dimmer: 0.9,
            default_crossfader: 0.0,
            ..MasterConfig::default()
        };
        let mut first = Layer::new("Layer 1", 0);
        first.slots.push(Slot::media(1, MediaItemId::new()));
        first.slots.push(Slot::media(3, MediaItemId::new()));
        let mut second = Layer::new("Layer 2", 1);
        second.slots.push(Slot::media(1, MediaItemId::new()));
        show.layers.push(first);
        show.layers.push(second);
        show
    }

    fn at(start: Instant, seconds: f64) -> Instant {
        start + std::time::Duration::from_secs_f64(seconds)
    }

    #[test]
    fn replacing_the_document_does_not_restart_the_show() {
        // The one thing this method exists to avoid. `load_show` would put
        // every layer back on its opening slot and the master back to its opening
        // dimmer, which is right when a show is opened and catastrophic when a
        // pad is filled between two tracks.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show.clone(), None);

        state
            .select_slot(layer_id, 3)
            .expect("slot 3 is configured");
        state
            .update_layer(layer_id, |live| live.set_opacity(0.25))
            .expect("the layer is there");
        state.update_master(|master| master.set_dimmer(0.5));
        let before = *state.layer(layer_id).expect("live state");
        let master_before = *state.master();

        let mut edited = show;
        edited.layers[0]
            .assign_slot(
                9,
                SourceRef::Media {
                    media_item_id: MediaItemId::new(),
                },
            )
            .expect("a slot a layer can hold");
        state.replace_show(edited);

        assert_eq!(*state.layer(layer_id).expect("live state"), before);
        assert_eq!(*state.master(), master_before);
        assert!(
            state
                .show()
                .and_then(|show| show.layer(layer_id))
                .and_then(|layer| layer.slot(9))
                .is_some(),
            "the edit is not in the running document"
        );
    }

    #[test]
    fn what_a_replacement_publishes_is_the_live_state_that_is_running() {
        // A peer applies the document and the live state that travels with it. If
        // that live state were the restart policy's, every client and the renderer
        // would jump even though the server had not.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show.clone(), None);
        state
            .select_slot(layer_id, 3)
            .expect("slot 3 is configured");

        let published = state.replace_show(show);
        let StateChange::ShowLoaded { layers, .. } = published.change else {
            panic!("a document change is published as a document change");
        };
        assert_eq!(
            layers
                .iter()
                .find(|live| live.layer_id == layer_id)
                .map(|live| live.active_slot),
            Some(3)
        );
    }

    #[test]
    fn a_layer_the_new_document_does_not_have_loses_its_live_state() {
        let mut state = AuthoritativeState::new();
        let show = show();
        let gone = show.layers[1].id;
        state.load_show(show.clone(), None);
        assert!(state.layer(gone).is_some());

        let mut edited = show;
        edited.layers.remove(1);
        state.replace_show(edited);

        assert!(state.layer(gone).is_none(), "live state outlived its layer");
    }

    #[test]
    fn emptying_the_slot_a_layer_is_showing_takes_it_to_black() {
        // Not to the next slot along. A layer pointing at a slot that no longer
        // exists draws its placeholder, which on the night reads as a fault.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show.clone(), None);
        state
            .select_slot(layer_id, 3)
            .expect("slot 3 is configured");

        let mut edited = show;
        assert!(edited.layers[0].clear_slot(3).is_some());
        state.black_out_slot(layer_id, 3);
        state.replace_show(edited);

        assert_eq!(
            state.layer(layer_id).expect("live state").active_slot,
            Layer::BLACK_SLOT_INDEX
        );
    }

    #[test]
    fn the_clock_runs_at_the_tempo_the_master_reports() {
        // One BPM, reported in one place and running in another, would be two
        // BPMs. This is the test that they are the same one.
        let mut state = AuthoritativeState::new();
        let start = Instant::now();
        state.update_master_at(start, |master| master.set_bpm(120.0));

        assert_eq!(state.master().bpm, 120.0);

        // The rate, not the absolute position. The clock was created a moment
        // before `start`, so its beat count at `start` is already a little past
        // zero by however long `AuthoritativeState::new` took — which is not
        // what this test is about, and is small enough to be noise one day and
        // a failure the next.
        let beats = state.clock().beats_at(at(start, 1.0)) - state.clock().beats_at(start);
        assert!((beats - 2.0).abs() < 1e-6, "got {beats} beats in a second");
    }

    #[test]
    fn changing_the_tempo_mid_show_does_not_jump_the_beat() {
        // A jump is audible as a stumble. The clock re-anchors; this checks that
        // the state layer did not undo that by rebuilding it.
        let mut state = AuthoritativeState::new();
        let start = Instant::now();
        state.update_master_at(start, |master| master.set_bpm(120.0));

        let change_at = at(start, 3.0);
        let before = state.clock().beats_at(change_at);
        state.update_master_at(change_at, |master| master.set_bpm(174.0));
        let after = state.clock().beats_at(change_at);

        assert!(
            (after - before).abs() < 1e-6,
            "the beat position jumped from {before} to {after}"
        );
        assert_eq!(state.master().bpm, 174.0);
    }

    #[test]
    fn the_speed_scale_reaches_the_clock_too() {
        let mut state = AuthoritativeState::new();
        let start = Instant::now();
        state.update_master_at(start, |master| master.set_bpm(120.0));

        let change_at = at(start, 1.0);
        state.update_master_at(change_at, |master| master.set_speed_scale(2.0));

        // Double rate from here: one more second is four more beats.
        let before = state.clock().beats_at(change_at);
        let after = state.clock().beats_at(at(start, 2.0));
        assert!(
            (after - before - 4.0).abs() < 1e-6,
            "got {} beats",
            after - before
        );
    }

    #[test]
    fn tapping_a_tempo_sets_it() {
        // The Master Fixture's BPM Tap, reaching the clock the show runs on.
        let mut state = AuthoritativeState::new();
        let start = Instant::now();

        // Six taps at 174 BPM.
        let interval = 60.0 / 174.0;
        let mut change = None;
        for index in 0..6 {
            change = state.tap_tempo(at(start, interval * f64::from(index)));
        }

        assert!(change.is_some(), "a run of taps must produce a tempo");
        assert!(
            (state.master().bpm - 174.0).abs() < 0.5,
            "tapped 174, got {}",
            state.master().bpm
        );
        assert!((state.clock().bpm() - state.master().bpm).abs() < 1e-9);
    }

    #[test]
    fn a_single_tap_changes_nothing() {
        // One tap is a moment, not a tempo, and must not emit an update either.
        let mut state = AuthoritativeState::new();
        let before = state.sequence();

        assert!(state.tap_tempo(Instant::now()).is_none());
        assert_eq!(state.sequence(), before);
        assert_eq!(
            state.master().bpm,
            120.0,
            "still the tempo a show starts at"
        );
    }

    #[test]
    fn resetting_the_phase_leaves_the_tempo_alone_and_emits_nothing() {
        // The phase is derived from the clock rather than carried in live state,
        // so a reset changes no value a client holds and needs no update.
        let mut state = AuthoritativeState::new();
        let start = Instant::now();
        state.update_master_at(start, |master| master.set_bpm(174.0));

        let reset_at = at(start, 5.0);
        assert!(state.clock().beats_at(reset_at) > 0.0);
        let before = state.sequence();

        state.reset_phase(reset_at);

        assert_eq!(state.clock().beats_at(reset_at), 0.0);
        assert_eq!(state.master().bpm, 174.0, "a reset is not a tempo change");
        assert_eq!(state.sequence(), before, "and it is not a state change");
    }

    #[test]
    fn loading_a_show_starts_its_tempo_from_zero() {
        // The BPM value is restored, the phase is not. The first beat
        // of a show is a known point rather than an extrapolation.
        let mut state = AuthoritativeState::new();
        let mut show = show();
        show.master.default_bpm = 90.0;
        show.master.beats_per_bar = 3;

        state.load_show(show, None);

        assert!((state.clock().bpm() - 90.0).abs() < 1e-9);
        assert_eq!(state.clock().beats_per_bar(), 3);
        assert!(
            state.clock().beats_at(Instant::now()) < 0.1,
            "the phase starts fresh"
        );
    }

    #[test]
    fn a_tapped_run_does_not_survive_loading_a_different_show() {
        // Half a run of taps from the last show must not combine with the first
        // tap of the next one.
        let mut state = AuthoritativeState::new();
        let start = Instant::now();
        state.tap_tempo(start);
        state.tap_tempo(at(start, 0.5));

        state.load_show(show(), None);

        assert!(
            state.tap_tempo(at(start, 1.0)).is_none(),
            "the first tap after a load starts a new run"
        );
    }

    #[test]
    fn empty_state_has_no_show_and_a_stopped_renderer() {
        let state = AuthoritativeState::new();
        assert!(state.show().is_none());
        assert_eq!(state.sequence(), Sequence::START);
        assert!(!state.renderer().is_rendering());
        assert!(state.snapshot().layers.is_empty());
    }

    #[test]
    fn loading_a_show_builds_live_state_from_its_defaults() {
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;

        state.load_show(show, None);

        assert_eq!(state.master().bpm, 120.0);
        assert_eq!(state.master().dimmer, 0.9);
        assert!(!state.master().freeze);
        assert!(state.layer(layer_id).expect("loaded").is_black());
    }

    #[test]
    fn loading_a_show_restores_the_selected_slot_from_a_snapshot() {
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;

        let mut snapshot = LiveSnapshot {
            bpm: Some(174.0),
            ..LiveSnapshot::default()
        };
        snapshot.record_selected_slot(layer_id, 3);

        state.load_show(show, Some(&snapshot));

        assert_eq!(state.master().bpm, 174.0, "a tapped tempo survives");
        assert_eq!(state.layer(layer_id).expect("loaded").active_slot, 3);
        assert_eq!(
            state.master().dimmer,
            0.9,
            "the dimmer still comes from the show"
        );
    }

    #[test]
    fn every_change_advances_the_sequence_by_exactly_one() {
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;

        let mut expected = 0_u64;
        let check = |change: &SequencedChange, expected: &mut u64| {
            *expected += 1;
            assert_eq!(change.sequence, Sequence(*expected));
        };

        check(&state.load_show(show, None), &mut expected);
        check(
            &state
                .select_slot(layer_id, 1)
                .expect("valid")
                .expect("changed"),
            &mut expected,
        );
        check(
            &state
                .update_master(|master| master.set_bpm(140.0))
                .expect("changed"),
            &mut expected,
        );
        check(
            &state
                .set_renderer(RendererStatus::Running)
                .expect("changed"),
            &mut expected,
        );
        assert_eq!(state.sequence(), Sequence(expected));
    }

    #[test]
    fn a_refused_change_does_not_advance_the_sequence() {
        // Otherwise every client would see a gap and resynchronise over an
        // error that never produced a change.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);
        let before = state.sequence();

        assert!(state.select_slot(layer_id, 99).is_err());
        assert!(state.select_slot(LayerId::new(), 1).is_err());
        assert_eq!(state.sequence(), before);
    }

    #[test]
    fn a_change_that_changes_nothing_produces_no_update() {
        // A console re-sending the same DMX frame at 44 Hz must not flood the
        // stream with updates that say nothing.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);

        state
            .select_slot(layer_id, 1)
            .expect("valid")
            .expect("changed");
        let before = state.sequence();

        assert_eq!(
            state.update_master(|master| master.set_bpm(state_bpm())),
            None
        );
        assert_eq!(state.set_renderer(RendererStatus::Stopped), None);
        assert_eq!(state.sequence(), before);
    }

    #[test]
    fn re_selecting_the_running_slot_is_a_retrigger_and_says_so() {
        // The slot index does not move, so nothing but the counter can carry
        // it. Without that, an operator hitting the same pad to restart a clip
        // would be sending an instruction that dies in the server.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);

        let first = state
            .select_slot(layer_id, 1)
            .expect("valid")
            .expect("changed");
        let StateChange::LayerChanged { layer: first } = first.change else {
            panic!("expected a layer change");
        };

        let again = state
            .select_slot(layer_id, 1)
            .expect("valid")
            .expect("a retrigger is a change");
        let StateChange::LayerChanged { layer: again } = again.change else {
            panic!("expected a layer change");
        };

        assert_eq!(again.active_slot, first.active_slot);
        assert_eq!(again.trigger, first.trigger + 1, "the counter moved");
    }

    #[test]
    fn a_pad_takes_effect_when_it_is_pressed() {
        // The owner, 2026-09-23: "Trigger Grid weg. Beats ist das was man
        // braucht."
        //
        // Eleven tests used to live here, and every one of them was about a
        // press that had not happened yet: a boundary being waited for, a press
        // a fraction early being forgiven, a mind changed before the beat, a
        // pad hit twice while it waited. None of that exists. A pad is a pad.
        //
        // Pressed a sixteenth of a beat into the bar, which is exactly where
        // the old code would have made it wait.
        let mut show = show();
        let layer_id = show.layers[0].id;
        let mut state = AuthoritativeState::new();
        state.load_show(show.clone(), None);
        let start = Instant::now();
        state.update_master_at(start, |master| master.set_bpm(120.0));

        let change = state
            .select_slot_at(layer_id, 3, at(start, 0.0625))
            .expect("valid")
            .expect("something changed");
        let StateChange::LayerChanged { layer } = change.change else {
            panic!("a layer changed");
        };
        assert_eq!(layer.active_slot, 3, "the pad is on, now");
        assert_eq!(
            state.layer(layer_id).expect("the layer").active_slot,
            3,
            "and the state agrees without anything having to fire later"
        );

        // Black too, which was the one thing that never waited even before.
        state
            .select_slot_at(layer_id, Layer::BLACK_SLOT_INDEX, at(start, 0.07))
            .expect("valid")
            .expect("something changed");
        assert_eq!(state.layer(layer_id).expect("the layer").active_slot, 0);

        // Nothing about a slot makes a trigger wait any more, whatever is on
        // it. The one thing a slot still says about time is how long its pass
        // is, and that is read when it plays rather than when it starts.
        show.layers[0].slots[0].sync = ovs_types::config::SlotSync::BeatSync { beats: 7 };
        show.layers[0].slots[0].speed = 0.5;
        let mut state = AuthoritativeState::new();
        state.load_show(show, None);
        let index = state.show().expect("loaded").layers[0].slots[0].index;
        state
            .select_slot_at(layer_id, index, at(start, 0.0625))
            .expect("valid")
            .expect("something changed");
        assert_eq!(
            state.layer(layer_id).expect("the layer").active_slot,
            index,
            "a beat-locked slot still starts the moment it is asked for"
        );
    }

    fn state_bpm() -> f64 {
        // The value load_show already set, so the update is a no-op.
        120.0
    }

    #[test]
    fn the_black_slot_is_always_selectable_without_being_configured() {
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);

        state
            .select_slot(layer_id, 1)
            .expect("valid")
            .expect("changed");
        let change = state
            .select_slot(layer_id, 0)
            .expect("slot 0 is always valid");
        assert!(change.is_some());
        assert!(state.layer(layer_id).expect("loaded").is_black());
    }

    #[test]
    fn selecting_an_empty_pad_shows_nothing_rather_than_being_refused() {
        // The owner, 2026-09-23: "jeder nicht belegte Slot ist ein Black/Full
        // Tranzparent für den Layer."
        //
        // Slot 7 of this layer holds nothing. Pressing it is how a layer is
        // taken off stage now that the Black pad has gone, so it is a place to
        // go rather than a mistake — and what it shows is nothing, because
        // there is nothing in it.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);

        state
            .select_slot(layer_id, 7)
            .expect("an empty pad is a place to go");
        assert_eq!(state.layer(layer_id).expect("loaded").active_slot, 7);
    }

    #[test]
    fn selecting_a_pad_the_layer_does_not_have_is_refused_by_name() {
        // Past the banks the layer shows is not an empty pad: it is a pad that
        // is not there, and a console patched against another show is where
        // that comes from.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);

        let error = state.select_slot(layer_id, 40).expect_err("no slot 40");
        assert_eq!(
            error,
            StateError::UnknownSlot {
                layer_id,
                slot_index: 40
            }
        );
        assert!(error.to_string().contains("has no slot 40"));
    }

    #[test]
    fn nothing_can_be_selected_without_a_show() {
        let mut state = AuthoritativeState::new();
        assert_eq!(
            state.select_slot(LayerId::new(), 1),
            Err(StateError::NoShow)
        );
    }

    #[test]
    fn closing_a_show_clears_the_layers_once() {
        let mut state = AuthoritativeState::new();
        state.load_show(show(), None);

        assert!(state.close_show().is_some());
        assert!(state.show().is_none());
        assert!(state.snapshot().layers.is_empty());

        let before = state.sequence();
        assert_eq!(state.close_show(), None, "closing twice changes nothing");
        assert_eq!(state.sequence(), before);
    }

    #[test]
    fn a_snapshot_reflects_the_current_sequence_and_state() {
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);
        state.select_slot(layer_id, 3).expect("valid");

        let snapshot = state.snapshot();
        assert_eq!(snapshot.sequence, state.sequence());
        assert_eq!(snapshot.layers.len(), 2);
        assert!(snapshot.show.is_some());
        assert_eq!(
            snapshot
                .layers
                .iter()
                .find(|live| live.layer_id == layer_id)
                .expect("present")
                .active_slot,
            3
        );
    }

    #[test]
    fn the_persisted_snapshot_carries_the_whole_live_state() {
        // The owner, 2026-10-19: "Nach neustart den kompletten status, so wie
        // er war." This test used to say the opposite.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);
        state.select_slot(layer_id, 3).expect("valid");
        state.update_master(|master| {
            master.set_bpm(174.0);
            master.set_dimmer(0.25);
            master.set_crossfader(1.0);
        });
        state
            .update_layer(layer_id, |live| live.set_opacity(0.5))
            .expect("loaded");

        let persisted = state.live_snapshot();
        assert_eq!(persisted.bpm, Some(174.0), "still written the old way too");
        assert_eq!(persisted.selected_slot(layer_id), Some(3));
        let master = persisted.master.expect("the master");
        assert_eq!(master.dimmer, 0.25);
        assert_eq!(master.crossfader, 1.0);
        assert_eq!(persisted.layer(layer_id).expect("the layer").opacity, 0.5);
    }

    #[test]
    fn a_full_restart_cycle_brings_everything_back_but_the_switch() {
        // Run a show, persist, restart, and check what comes back.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        let layer_id = show.layers[0].id;
        state.load_show(show.clone(), None);

        state.update_master(|master| {
            master.set_bpm(174.0);
            master.set_dimmer(0.2);
            master.set_crossfader(1.0);
            master.freeze = true;
            master.dmx_enabled = false;
        });
        state
            .update_layer(layer_id, |live| {
                live.set_opacity(0.4);
                live.transform.x = 64.0;
                live.route = ovs_types::config::Route::B;
                live.transport = ovs_types::live::Transport::Paused;
            })
            .expect("loaded");
        state
            .set_effect_parameters(blur, &radius(12.0), false)
            .expect("a value the blur takes");
        let persisted = state.live_snapshot();

        // A server that has just started, with its DMX switch on.
        let mut restarted = AuthoritativeState::new();
        restarted.load_show(show, Some(&persisted));

        let master = restarted.master();
        assert_eq!(master.bpm, 174.0);
        assert_eq!(master.dimmer, 0.2, "left down, comes back down");
        assert_eq!(master.crossfader, 1.0, "mid-crossfade, as it was left");
        assert!(master.freeze);
        assert!(
            master.dmx_enabled,
            "the switch is the server's, and a show does not carry it back"
        );

        let layer = restarted.layer(layer_id).expect("loaded");
        assert_eq!(layer.opacity, 0.4);
        assert_eq!(layer.transform.x, 64.0);
        assert_eq!(layer.route, ovs_types::config::Route::B);
        assert_eq!(layer.transport, ovs_types::live::Transport::Paused);

        assert_eq!(
            restarted.effect_value(blur, "radius_px"),
            Some(ParameterValue::Float(12.0)),
            "the blur somebody pushed is still pushed"
        );
    }

    #[test]
    fn opening_another_show_leaves_the_dmx_switch_where_it_is() {
        let mut state = AuthoritativeState::new();
        state.load_show(show(), None);
        state.update_master(|master| master.dmx_enabled = false);

        state.load_show(show(), None);
        assert!(
            !state.master().dmx_enabled,
            "switched off by hand, it stays off when the next show opens"
        );
    }

    #[test]
    fn a_restored_value_the_show_can_no_longer_take_is_dropped() {
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        let mut snapshot = LiveSnapshot::default();
        snapshot.effects.insert(blur, radius(100_000.0));
        snapshot
            .effects
            .insert(EffectInstanceId::new(), radius(4.0));

        state.load_show(show, Some(&snapshot));
        assert!(
            state.effect_overrides().is_empty(),
            "neither a refused value nor an instance the show lacks comes back: {:?}",
            state.effect_overrides()
        );
    }

    #[test]
    fn a_layer_update_from_a_console_is_clamped_not_rejected() {
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);

        // A layer starts at its configured opacity of 1.0, so fade it down first
        // for the clamp to be observable as a change at all.
        state
            .update_layer(layer_id, |live| live.set_opacity(0.5))
            .expect("known layer")
            .expect("changed");
        assert_eq!(state.layer(layer_id).expect("loaded").opacity, 0.5);

        state
            .update_layer(layer_id, |live| live.set_opacity(5.0))
            .expect("known layer")
            .expect("changed");
        assert_eq!(
            state.layer(layer_id).expect("loaded").opacity,
            1.0,
            "an over-range value clamps rather than being refused"
        );
    }

    #[test]
    fn an_over_range_value_that_clamps_to_the_current_one_is_not_a_change() {
        // A console sitting at full sending 255 repeatedly must not produce an
        // update per frame.
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);
        let before = state.sequence();

        assert_eq!(
            state
                .update_layer(layer_id, |live| live.set_opacity(5.0))
                .expect("known layer"),
            None
        );
        assert_eq!(state.sequence(), before);
    }

    #[test]
    fn a_corrupt_console_value_leaves_the_layer_alone() {
        let mut state = AuthoritativeState::new();
        let show = show();
        let layer_id = show.layers[0].id;
        state.load_show(show, None);
        state
            .update_layer(layer_id, |live| live.set_opacity(0.4))
            .expect("known layer")
            .expect("changed");
        let before = state.sequence();

        let change = state
            .update_layer(layer_id, |live| live.set_opacity(f32::NAN))
            .expect("known layer");
        assert_eq!(change, None, "a corrupt value is not a change");
        assert_eq!(state.layer(layer_id).expect("loaded").opacity, 0.4);
        assert_eq!(state.sequence(), before);
    }

    /// A show whose first layer has a blur configured at a radius of eight.
    ///
    /// Eight rather than the published default of zero, so that "the live value
    /// won" and "the configured value came back" are different answers from
    /// "nobody set anything".
    fn show_with_a_blur() -> (Show, EffectInstanceId) {
        let mut show = show();
        let mut blur = ovs_types::config::EffectInstance::new("ovs.blur", 1);
        blur.parameters.insert(
            "radius_px".to_owned(),
            ovs_types::config::ParameterValue::Float(8.0),
        );
        let effect_instance_id = blur.id;
        show.layers[0].effects.push(blur);
        (show, effect_instance_id)
    }

    fn radius(value: f64) -> ParameterMap {
        ParameterMap::from([(
            "radius_px".to_owned(),
            ovs_types::config::ParameterValue::Float(value),
        )])
    }

    #[test]
    fn a_live_effect_value_moves_the_effect_without_touching_the_show() {
        // The leak that matters, in the one place the specification has
        // always said a live value belongs.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show, None);

        state
            .set_effect_parameters(blur, &radius(40.0), false)
            .expect("the show has this effect")
            .expect("something moved");

        assert_eq!(
            state.effect(blur).and_then(|live| live.get("radius_px")),
            Some(&ovs_types::config::ParameterValue::Float(40.0))
        );
        assert_eq!(
            state.show().expect("open").layers[0].effects[0]
                .parameters
                .get("radius_px"),
            Some(&ovs_types::config::ParameterValue::Float(8.0)),
            "the show still says what the show was built with"
        );
    }

    #[test]
    fn reopening_the_show_brings_its_live_values_back_and_leaves_the_document_alone() {
        // Two promises, and since 2026-10-19 they are different ones. A show
        // comes back the way it was left ("so wie er war"), so the pushed
        // value is there again; and the value was never written into the
        // show, so opening it with nothing kept gives what it was built with.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show.clone(), None);
        state
            .set_effect_parameters(blur, &radius(40.0), false)
            .expect("applies");

        state.load_show(show.clone(), Some(&state.live_snapshot()));
        assert_eq!(
            state.effect(blur).and_then(|live| live.get("radius_px")),
            Some(&ovs_types::config::ParameterValue::Float(40.0)),
            "what was left on the blur comes back with the show"
        );
        assert_eq!(
            state.show().expect("open").layers[0].effects[0]
                .parameters
                .get("radius_px"),
            Some(&ovs_types::config::ParameterValue::Float(8.0)),
            "and the document never heard about it"
        );

        state.load_show(show, None);
        assert!(state.effect(blur).is_none());
    }

    #[test]
    fn a_live_value_outside_the_manifests_range_is_refused() {
        // A live control that could put a blur radius at four thousand would be
        // a way to stop a show from a client.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show, None);

        let error = state
            .set_effect_parameters(blur, &radius(4_000.0), false)
            .expect_err("out of range");
        assert!(
            matches!(error, StateError::InvalidEffectParameter { ref key, .. } if key == "radius_px"),
            "{error:?}"
        );
        assert!(error.to_string().contains("between 0 and 64"), "{error}");
        assert!(
            state.effect(blur).is_none(),
            "a refused request changes nothing"
        );
    }

    #[test]
    fn a_live_value_for_a_parameter_the_effect_never_declared_is_refused() {
        // Nearly always a typo.
        // Accepting it silently means an operator moving a control and seeing
        // nothing happen.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show, None);

        let values = ParameterMap::from([(
            "brightness".to_owned(),
            ovs_types::config::ParameterValue::Float(1.0),
        )]);
        let error = state
            .set_effect_parameters(blur, &values, false)
            .expect_err("a blur has no brightness");
        assert!(
            error.to_string().contains("ovs.blur has no such parameter"),
            "{error}"
        );
    }

    #[test]
    fn nothing_in_a_refused_request_is_applied() {
        // Three parameters, one of them wrong. An effect left halfway between
        // two looks, with no way to say which half arrived, is worse than one
        // that did not move.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show, None);

        let values = ParameterMap::from([
            (
                "radius_px".to_owned(),
                ovs_types::config::ParameterValue::Float(12.0),
            ),
            (
                "amount".to_owned(),
                ovs_types::config::ParameterValue::Float(9.0),
            ),
        ]);
        assert!(state.set_effect_parameters(blur, &values, false).is_err());
        assert!(state.effect(blur).is_none());
    }

    #[test]
    fn a_live_value_for_an_effect_the_show_does_not_have_is_refused() {
        let mut state = AuthoritativeState::new();
        let (show, _) = show_with_a_blur();
        state.load_show(show, None);

        let stranger = EffectInstanceId::new();
        let error = state
            .set_effect_parameters(stranger, &radius(4.0), false)
            .expect_err("not in this show");
        assert_eq!(
            error,
            StateError::UnknownEffect {
                effect_instance_id: stranger
            }
        );

        let mut empty = AuthoritativeState::new();
        assert_eq!(
            empty.set_effect_parameters(stranger, &radius(4.0), false),
            Err(StateError::NoShow)
        );
    }

    #[test]
    fn a_live_value_reaches_an_effect_on_a_slot_as_well_as_on_a_layer() {
        // A chain hangs off both, and a live value names an
        // instance rather than a position, so neither needs its own endpoint.
        let mut state = AuthoritativeState::new();
        let mut show = show();
        let mut tint = ovs_types::config::EffectInstance::new("ovs.tint", 1);
        let instance = tint.id;
        tint.parameters.insert(
            "amount".to_owned(),
            ovs_types::config::ParameterValue::Float(0.25),
        );
        show.layers[0].slots[0].effects.push(tint);
        state.load_show(show, None);

        let values = ParameterMap::from([(
            "amount".to_owned(),
            ovs_types::config::ParameterValue::Float(1.0),
        )]);
        state
            .set_effect_parameters(instance, &values, false)
            .expect("the slot's effect is in the show")
            .expect("something moved");
    }

    #[test]
    fn putting_an_effect_back_is_something_a_client_can_ask_for() {
        // "No override" is an absence, so there is no value a client could send
        // that means it. Without this an operator could never return an effect
        // to the look the show was built with.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show, None);
        state
            .set_effect_parameters(blur, &radius(40.0), false)
            .expect("applies");

        state
            .set_effect_parameters(blur, &ParameterMap::new(), true)
            .expect("applies")
            .expect("putting it back is a change like any other");
        assert!(state.effect(blur).is_none());
    }

    #[test]
    fn re_sending_the_same_effect_value_says_nothing_new() {
        // A client holding a control still, or re-sending what it already sent,
        // must not put an update on the stream that says nothing.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show, None);

        state
            .set_effect_parameters(blur, &radius(40.0), false)
            .expect("applies")
            .expect("something moved");
        let sequence = state.sequence();

        assert_eq!(
            state
                .set_effect_parameters(blur, &radius(40.0), false)
                .expect("applies"),
            None,
            "nothing changed, so nothing was said"
        );
        assert_eq!(state.sequence(), sequence);
    }

    #[test]
    fn a_refused_effect_value_does_not_advance_the_sequence() {
        // Otherwise every client would see a gap and resynchronise over an
        // error that never produced a change.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show, None);
        let sequence = state.sequence();

        assert!(state
            .set_effect_parameters(blur, &radius(4_000.0), false)
            .is_err());
        assert_eq!(state.sequence(), sequence);
    }

    #[test]
    fn a_live_effect_value_is_published_like_every_other_live_change() {
        // A client shows a control as pending until the server says what
        // landed, and never retries. Without this it would wait for ever.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show, None);

        let change = state
            .set_effect_parameters(blur, &radius(40.0), false)
            .expect("applies")
            .expect("something moved");

        let StateChange::EffectChanged {
            effect_instance_id,
            overrides,
        } = change.change
        else {
            panic!("expected an effect change");
        };
        assert_eq!(effect_instance_id, blur);
        assert_eq!(
            overrides.get("radius_px"),
            Some(&ovs_types::config::ParameterValue::Float(40.0))
        );
        assert_eq!(change.sequence, state.sequence());
    }

    #[test]
    fn closing_a_show_forgets_its_live_effect_values() {
        // Otherwise a value set on one show would be waiting for the next one,
        // keyed by an instance identifier nothing in it has.
        let mut state = AuthoritativeState::new();
        let (show, blur) = show_with_a_blur();
        state.load_show(show, None);
        state
            .set_effect_parameters(blur, &radius(40.0), false)
            .expect("applies");

        state.close_show();

        assert!(state.effect_overrides().is_empty());
        assert!(
            state.snapshot().effects.is_empty(),
            "and a client connecting afterwards is told so"
        );
    }

    #[test]
    fn loading_a_different_show_replaces_the_layers_entirely() {
        let mut state = AuthoritativeState::new();
        let first = show();
        let first_layer = first.layers[0].id;
        state.load_show(first, None);

        let second = show();
        let second_layer = second.layers[0].id;
        state.load_show(second, None);

        assert!(
            state.layer(first_layer).is_none(),
            "stale layers must not linger"
        );
        assert!(state.layer(second_layer).is_some());
        assert_eq!(state.snapshot().layers.len(), 2);
    }
}
