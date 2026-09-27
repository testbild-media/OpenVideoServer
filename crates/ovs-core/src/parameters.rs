//! The one way a control source changes a show.
//!
//! Art-Net, sACN, the HTTP API and anything that comes later all arrive here
//!. A transport produces a [`Parameter`] and a value; this
//! decides what that means. Nothing else is allowed to reach into the state, and
//! nothing at all reaches into the renderer — the renderer consumes state
//! changes, it is not called.
//!
//! # Live only
//!
//! Every parameter here writes live state. A value that arrived on a wire never
//! reaches a show file, which is the leak that matters: a console
//! sending a dimmer at 44 Hz must not slowly rewrite what the show opens with
//! tomorrow. Where a control has both — a layer's grid, its rate — the
//! configuration holds the default and the live state holds what the console is
//! doing to it.
//!
//! # Out of range is clamped, not refused
//!
//! A console with a badly scaled fader must not be able to brighten a show past
//! full, and must not be able to stop it working either. Nonsense is held at the
//! last good value by the live state itself (house rule §17).
//!
//! # Effects are not here yet, and why
//!
//! The protocol lists `effect.<instance>.parameter.<name>` beside the master
//! and layer parameters, and an effect instance's parameters are live: they are
//! moved through `PUT /live/effects/{id}`, they reach the renderer, and since
//! 2026-09-23 a console can move them too.
//!
//! # A plugin's channels are the plugin's
//!
//! There is no Effect Fixture personality, because there is no one shape an
//! effect has. An effect is a plugin; it declares which of its own parameters a
//! console may drive and how finely (`ParameterDefinition::dmx`), and a
//! personality is read off that declaration. An effect that declares none has
//! no personality and cannot be patched, which is the plugin saying it does not
//! want to be driven that way rather than a gap.
//!
//! So what arrives here is [`Parameter::EffectParameter`], carrying the
//! manifest's own key. This module does not know what `brightness` means and
//! does not need to: the manifest says what the value may be and the renderer
//! knows what to do with it.
//!
//! [`Parameter`]: ovs_types::config::Parameter

use ovs_types::config::slot::{ParameterMap, ParameterValue};
use ovs_types::config::{ChannelValue, ColourPart, Parameter};
use ovs_types::id::{EffectInstanceId, GeneratorInstanceId};

use crate::state::{AuthoritativeState, SequencedChange};

/// Applies one parameter, returning the change it made.
///
/// Returns `None` when nothing changed, which is the common case: a console
/// re-sends every channel forty-four times a second and almost none of them are
/// different from last time.
pub fn apply(
    state: &mut AuthoritativeState,
    parameter: Parameter,
    value: ChannelValue,
) -> Option<SequencedChange> {
    match parameter {
        Parameter::EffectParameter {
            effect_instance_id,
            key,
            component,
        } => {
            return apply_plugin(
                state,
                Plugin::Effect(effect_instance_id),
                key,
                component,
                value,
            )
        }
        Parameter::GeneratorParameter {
            generator_instance_id,
            key,
            component,
        } => {
            return apply_plugin(
                state,
                Plugin::Generator(generator_instance_id),
                key,
                component,
                value,
            )
        }
        _ => {}
    }

    match parameter {
        Parameter::MasterDimmer => {
            let level = number(value)?;
            state.update_master(|master| master.set_dimmer(level as f32))
        }
        Parameter::MasterCrossfader => {
            let position = number(value)?;
            state.update_master(|master| master.set_crossfader(position as f32))
        }
        Parameter::MasterBpm => {
            let bpm = number(value)?;
            state.update_master(|master| master.set_bpm(bpm))
        }
        Parameter::MasterSpeedScale => {
            let scale = number(value)?;
            state.update_master(|master| master.set_speed_scale(scale))
        }
        Parameter::MasterFreeze => {
            let frozen = flag(value)?;
            state.update_master(|master| master.freeze = frozen)
        }

        // The two the master fixture fires rather than sets. A transport turns
        // a channel crossing its threshold into one of these arriving at all,
        // so reaching here means "it happened", not "it is on".
        Parameter::MasterTapTempo => state.tap_tempo(std::time::Instant::now()),
        Parameter::MasterResetPhase => {
            state.reset_phase(std::time::Instant::now());
            None
        }

        Parameter::LayerOpacity { layer_id } => {
            let level = number(value)?;
            state
                .update_layer(layer_id, |live| live.set_opacity(level as f32))
                .ok()
                .flatten()
        }
        Parameter::LayerSlot { layer_id } => {
            let ChannelValue::Index(index) = value else {
                return None;
            };
            // A slot the layer does not have is refused by the state, which is
            // the right answer: a console patched against a bigger show must not
            // be able to put a layer on a slot that is not there.
            state.select_slot(layer_id, index).ok().flatten()
        }
        Parameter::LayerTrigger { layer_id } => {
            // Selecting the slot that is already running, which is exactly what
            // a retrigger is.
            let current = state.layer(layer_id)?.active_slot;
            state.select_slot(layer_id, current).ok().flatten()
        }

        Parameter::LayerPositionX { layer_id } => {
            let fraction = number(value)?;
            let half = state.composition_size()?.width as f64 / 2.0;
            state
                .update_layer(layer_id, |live| {
                    live.transform.x = (fraction * half) as f32;
                })
                .ok()
                .flatten()
        }
        Parameter::LayerPositionY { layer_id } => {
            let fraction = number(value)?;
            let half = state.composition_size()?.height as f64 / 2.0;
            state
                .update_layer(layer_id, |live| {
                    live.transform.y = (fraction * half) as f32;
                })
                .ok()
                .flatten()
        }
        Parameter::LayerScaleX { layer_id } => {
            let scale = number(value)? as f32;
            state
                .update_layer(layer_id, |live| live.transform.scale_x = scale)
                .ok()
                .flatten()
        }
        Parameter::LayerScaleY { layer_id } => {
            let scale = number(value)? as f32;
            state
                .update_layer(layer_id, |live| live.transform.scale_y = scale)
                .ok()
                .flatten()
        }
        Parameter::LayerRotation { layer_id } => {
            let degrees = number(value)? as f32;
            state
                .update_layer(layer_id, |live| live.transform.rotation_deg = degrees)
                .ok()
                .flatten()
        }
        // Handled above, before this match: the two variants whose meaning
        // comes from a manifest rather than from this crate.
        Parameter::EffectParameter { .. } | Parameter::GeneratorParameter { .. } => None,
    }
}

/// Which kind of module a channel is driving, and which instance of it.
///
/// One type rather than two code paths: an effect and a generator are both
/// plugins with declared parameters, and the owner's instruction (2026-09-23)
/// is that they use one model. What differs is the identity and which state
/// method takes it, which is what this carries.
#[derive(Debug, Clone, Copy)]
enum Plugin {
    /// One effect instance in one chain.
    Effect(EffectInstanceId),
    /// One generator instance in one slot.
    Generator(GeneratorInstanceId),
}

/// Moves one parameter of one plugin instance.
///
/// # Why a colour is four calls and one value
///
/// A colour is four channels on a desk, and they arrive as four separate
/// updates in whatever order the mapper produced them. Each one reads the
/// colour the instance has now — its live value if it has one, otherwise what
/// the show was built with — replaces its own part, and writes the whole
/// colour back. Four channels held still therefore converge on the colour they
/// describe, and one channel moved alone moves only its own part.
fn apply_plugin(
    state: &mut AuthoritativeState,
    plugin: Plugin,
    key: &str,
    component: Option<ColourPart>,
    value: ChannelValue,
) -> Option<SequencedChange> {
    let wanted = match component {
        None => match value {
            ChannelValue::Number(number) => ParameterValue::Float(number),
            ChannelValue::Flag(flag) => ParameterValue::Bool(flag),
            // A band's number, which for a choice channel is its place in the
            // manifest's own list.
            ChannelValue::Index(index) => match choice_at(state, plugin, key, index) {
                Some(choice) => choice,
                None => ParameterValue::Int(i64::from(index)),
            },
        },
        Some(part) => {
            let mut colour = match current(state, plugin, key) {
                Some(ParameterValue::Color(colour)) => colour,
                // Not a colour, or an instance this show does not have.
                // Either way there is nothing to put a part into.
                _ => return None,
            };
            colour[part.index()] = number(value)? as f32;
            ParameterValue::Color(colour)
        }
    };

    let mut overrides = ParameterMap::new();
    overrides.insert(key.to_owned(), wanted);
    // A value the manifest refuses changes nothing and says nothing: a console
    // sends a whole universe forty-four times a second, and a log line per
    // refused frame would be a hundred and fifty thousand an hour.
    let moved = match plugin {
        Plugin::Effect(id) => state.set_effect_parameters(id, &overrides, false),
        Plugin::Generator(id) => state.set_generator_parameters(id, &overrides, false),
    };
    moved.ok().flatten()
}

/// What one parameter of one plugin instance is worth now.
fn current(state: &AuthoritativeState, plugin: Plugin, key: &str) -> Option<ParameterValue> {
    match plugin {
        Plugin::Effect(id) => state.effect_value(id, key),
        Plugin::Generator(id) => state.generator_value(id, key),
    }
}

/// The choice a band number selects, when the parameter is a choice at all.
///
/// A [`ChannelEncoding::Choices`] channel answers with a band, because a
/// fixture definition holds no plugin's vocabulary. This is where the
/// vocabulary is: the manifest's list, in the order the bands were built from.
fn choice_at(
    state: &AuthoritativeState,
    plugin: Plugin,
    key: &str,
    index: u16,
) -> Option<ParameterValue> {
    let choices = match plugin {
        Plugin::Effect(id) => state.effect_choices(id, key),
        Plugin::Generator(id) => state.generator_choices(id, key),
    }?;
    choices
        .get(usize::from(index))
        .map(|choice| ParameterValue::Choice(choice.id.to_owned()))
}

/// Reads a number out of a channel value, or nothing when it is not one.
///
/// A channel decoded as the wrong shape is a patch that disagrees with the
/// personality it names. Dropping it is better than coercing: a flag read as a
/// number would put a layer at zero or full and nothing in between.
const fn number(value: ChannelValue) -> Option<f64> {
    match value {
        ChannelValue::Number(number) => Some(number),
        _ => None,
    }
}

/// Returns the flag a value carries, if it carries one.
const fn flag(value: ChannelValue) -> Option<bool> {
    match value {
        ChannelValue::Flag(flag) => Some(flag),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::{Layer, Show, Slot};
    use ovs_types::id::{LayerId, MediaItemId};

    use super::*;

    use ovs_types::config::fixtures::definition;
    use ovs_types::config::patch::FixtureTarget;
    use ovs_types::config::slot::SourceRef;

    /// A show with one generator in one slot, and that generator's identity.
    fn show_with(type_id: &str) -> (AuthoritativeState, GeneratorInstanceId) {
        let instance_id = GeneratorInstanceId::new();
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        let mut slot = Slot::media(1, MediaItemId::new());
        slot.source = SourceRef::Generator {
            instance_id,
            type_id: type_id.to_owned(),
            version: 1,
            parameters: ParameterMap::new(),
        };
        layer.slots.push(slot);
        show.layers.push(layer);

        let mut state = AuthoritativeState::new();
        state.load_show(show, None);
        (state, instance_id)
    }

    #[test]
    fn a_console_moves_a_generators_number() {
        // The whole point of the owner's instruction: a generator is reached
        // the same way an effect is, through the same call, with no separate
        // path of its own.
        let (mut state, instance_id) = show_with("ovs.gradient");

        let moved = apply(
            &mut state,
            Parameter::GeneratorParameter {
                generator_instance_id: instance_id,
                key: "angle_deg",
                component: None,
            },
            ChannelValue::Number(180.0),
        );

        assert!(moved.is_some(), "the show changed");
        assert_eq!(
            state.generator_value(instance_id, "angle_deg"),
            Some(ParameterValue::Float(180.0))
        );
    }

    #[test]
    fn four_channels_of_colour_converge_on_one_colour() {
        // A desk sends a colour as four faders, in whatever order the mapper
        // produced them, and each one has to leave the other three alone.
        let (mut state, instance_id) = show_with("ovs.gradient");

        for (part, level) in [
            (ColourPart::Red, 1.0),
            (ColourPart::Green, 0.5),
            (ColourPart::Blue, 0.0),
            (ColourPart::Alpha, 1.0),
        ] {
            apply(
                &mut state,
                Parameter::GeneratorParameter {
                    generator_instance_id: instance_id,
                    key: "colour_a",
                    component: Some(part),
                },
                ChannelValue::Number(level),
            );
        }

        let Some(ParameterValue::Color(colour)) = state.generator_value(instance_id, "colour_a")
        else {
            panic!("a colour");
        };
        assert!((colour[0] - 1.0).abs() < 0.01, "red: {colour:?}");
        assert!((colour[1] - 0.5).abs() < 0.01, "green: {colour:?}");
        assert!((colour[2] - 0.0).abs() < 0.01, "blue: {colour:?}");
        assert!((colour[3] - 1.0).abs() < 0.01, "alpha: {colour:?}");
    }

    #[test]
    fn a_choice_channel_selects_by_name_rather_than_by_number() {
        // A choice channel answers with a band, because a fixture definition
        // holds no plugin's vocabulary. This is where the vocabulary is, and a
        // band has to arrive as the manifest's own identifier rather than as
        // the number of the band: a show stores the name.
        let (mut state, instance_id) = show_with("ovs.shape");

        apply(
            &mut state,
            Parameter::GeneratorParameter {
                generator_instance_id: instance_id,
                key: "shape",
                component: None,
            },
            ChannelValue::Index(2),
        );

        assert_eq!(
            state.generator_value(instance_id, "shape"),
            Some(ParameterValue::Choice("triangle".to_owned())),
            "the third band is the third choice"
        );
    }

    #[test]
    fn a_generated_personality_binds_every_channel_to_its_instance() {
        // What the mapper does with a patch entry: every channel of the
        // generated personality has to produce a parameter naming this
        // instance. A personality half of which bound would be patched
        // without complaint and then do half of nothing.
        let instance_id = GeneratorInstanceId::new();
        let personality = definition("ovs.gradient", 1).expect("a generated personality");
        let target = FixtureTarget::Generator {
            generator_instance_id: instance_id,
        };

        assert!(personality.drives(target));
        assert!(!personality.channels.is_empty(), "channels to bind");
        for channel in personality.channels {
            let Some(Parameter::GeneratorParameter {
                generator_instance_id,
                ..
            }) = channel.parameter.bind(target)
            else {
                panic!("channel {} is not a generator parameter", channel.offset);
            };
            assert_eq!(generator_instance_id, instance_id);
        }
    }

    #[test]
    fn a_generated_personality_refuses_a_target_of_the_wrong_kind() {
        // The other half of the same rule: a generator personality patched
        // onto the master, or onto an effect, must bind nothing at all rather
        // than bind the channels the two happen to share.
        let personality = definition("ovs.gradient", 1).expect("a generated personality");

        assert!(!personality.drives(FixtureTarget::Master));
        assert!(!personality.drives(FixtureTarget::Effect {
            effect_instance_id: EffectInstanceId::new(),
        }));
    }

    #[test]
    fn a_value_the_manifest_refuses_changes_nothing_and_says_nothing() {
        // A console sends a whole universe forty-four times a second. A
        // refused frame that logged would be a hundred and fifty thousand
        // lines an hour.
        let (mut state, instance_id) = show_with("ovs.gradient");

        let moved = apply(
            &mut state,
            Parameter::GeneratorParameter {
                generator_instance_id: instance_id,
                key: "angle_deg",
                component: None,
            },
            // A flag where the manifest declares a number.
            ChannelValue::Flag(true),
        );

        assert!(moved.is_none());
        assert_eq!(
            state.generator_value(instance_id, "angle_deg"),
            Some(ParameterValue::Float(90.0)),
            "still what the manifest declares"
        );
    }

    #[test]
    fn a_generator_this_show_does_not_have_changes_nothing() {
        // A patch built against a bigger show, pointed at this one.
        let (mut state, _) = show_with("ovs.gradient");

        let moved = apply(
            &mut state,
            Parameter::GeneratorParameter {
                generator_instance_id: GeneratorInstanceId::new(),
                key: "angle_deg",
                component: None,
            },
            ChannelValue::Number(180.0),
        );

        assert!(moved.is_none());
    }

    fn show() -> (AuthoritativeState, LayerId) {
        let mut state = AuthoritativeState::new();
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        layer.slots.push(Slot::media(2, MediaItemId::new()));
        let layer_id = layer.id;
        show.layers.push(layer);
        state.load_show(show, None);
        (state, layer_id)
    }

    #[test]
    fn a_master_channel_moves_the_master() {
        let (mut state, _) = show();

        apply(
            &mut state,
            Parameter::MasterDimmer,
            ChannelValue::Number(0.5),
        );
        apply(
            &mut state,
            Parameter::MasterCrossfader,
            ChannelValue::Number(1.0),
        );
        apply(
            &mut state,
            Parameter::MasterBpm,
            ChannelValue::Number(174.0),
        );
        apply(
            &mut state,
            Parameter::MasterFreeze,
            ChannelValue::Flag(true),
        );

        let master = state.master();
        assert!((master.dimmer - 0.5).abs() < f32::EPSILON);
        assert!((master.crossfader - 1.0).abs() < f32::EPSILON);
        assert!((master.bpm - 174.0).abs() < f64::EPSILON);
        assert!(master.freeze);
    }

    #[test]
    fn a_value_out_of_range_is_held_rather_than_obeyed() {
        // A console with a badly scaled fader must not be able to brighten a
        // show past full, and must not be able to stop it working either.
        let (mut state, _) = show();

        apply(
            &mut state,
            Parameter::MasterDimmer,
            ChannelValue::Number(5.0),
        );
        assert!((state.master().dimmer - 1.0).abs() < f32::EPSILON);

        apply(
            &mut state,
            Parameter::MasterBpm,
            ChannelValue::Number(f64::NAN),
        );
        assert!(state.master().bpm.is_finite(), "the last good value holds");
    }

    #[test]
    fn a_slot_channel_selects_a_slot_and_a_missing_one_changes_nothing() {
        let (mut state, layer_id) = show();

        apply(
            &mut state,
            Parameter::LayerSlot { layer_id },
            ChannelValue::Index(2),
        );
        assert_eq!(state.layer(layer_id).expect("loaded").active_slot, 2);

        // A console sends slot 9, which this layer has no clip in. It goes
        // there and shows nothing, because an empty pad is black (owner,
        // 2026-09-23) — the same thing a press on that pad does, which is what
        // makes a console and this window agree about what slot 9 means.
        apply(
            &mut state,
            Parameter::LayerSlot { layer_id },
            ChannelValue::Index(9),
        );
        assert_eq!(state.layer(layer_id).expect("loaded").active_slot, 9);

        // Past the banks the layer shows, though, nothing moves: that is a
        // console patched against another show, not an operator's choice.
        apply(
            &mut state,
            Parameter::LayerSlot { layer_id },
            ChannelValue::Index(40),
        );
        assert_eq!(state.layer(layer_id).expect("loaded").active_slot, 9);
    }

    #[test]
    fn a_trigger_restarts_the_slot_that_is_already_running() {
        let (mut state, layer_id) = show();
        apply(
            &mut state,
            Parameter::LayerSlot { layer_id },
            ChannelValue::Index(1),
        );
        let before = state.layer(layer_id).expect("loaded").trigger;

        apply(
            &mut state,
            Parameter::LayerTrigger { layer_id },
            ChannelValue::Flag(true),
        );

        let after = state.layer(layer_id).expect("loaded");
        assert_eq!(after.active_slot, 1, "the same slot");
        assert_eq!(after.trigger, before + 1, "started again");
    }

    #[test]
    fn position_is_a_fraction_of_the_composition_rather_than_pixels() {
        // A fixture cannot know how big the canvas is, and an operator moving a
        // layer to the edge should not have to. Full travel is half the
        // composition, so centre is centre on any canvas.
        let (mut state, layer_id) = show();
        let width = state.composition_size().expect("a show is open").width;

        apply(
            &mut state,
            Parameter::LayerPositionX { layer_id },
            ChannelValue::Number(1.0),
        );

        let live = state.layer(layer_id).expect("loaded");
        assert!((live.transform.x - width as f32 / 2.0).abs() < 1.0);
    }

    #[test]
    fn each_scale_axis_moves_on_its_own() {
        // The owner's layer fixture carries both (2026-09-22). The earlier one
        // had a single uniform scale, so a console could not squash a layer to
        // fit a letterbox without editing the show.
        let (mut state, layer_id) = show();

        apply(
            &mut state,
            Parameter::LayerScaleX { layer_id },
            ChannelValue::Number(2.0),
        );

        let live = state.layer(layer_id).expect("loaded");
        assert!((live.transform.scale_x - 2.0).abs() < f32::EPSILON);
        assert!(
            (live.transform.scale_y - 1.0).abs() < f32::EPSILON,
            "the other axis did not move"
        );

        apply(
            &mut state,
            Parameter::LayerScaleY { layer_id },
            ChannelValue::Number(0.5),
        );

        let live = state.layer(layer_id).expect("loaded");
        assert!((live.transform.scale_x - 2.0).abs() < f32::EPSILON);
        assert!((live.transform.scale_y - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn a_layer_that_is_not_loaded_changes_nothing() {
        let (mut state, _) = show();
        let stranger = LayerId::new();

        assert!(apply(
            &mut state,
            Parameter::LayerOpacity { layer_id: stranger },
            ChannelValue::Number(0.5)
        )
        .is_none());
        assert!(apply(
            &mut state,
            Parameter::LayerTrigger { layer_id: stranger },
            ChannelValue::Flag(true)
        )
        .is_none());
    }

    #[test]
    fn a_value_of_the_wrong_shape_is_dropped_rather_than_coerced() {
        // A patch that disagrees with the personality it names. A flag read as a
        // number would put a layer at zero or full and nothing in between.
        let (mut state, layer_id) = show();
        let before = state.layer(layer_id).expect("loaded").opacity;

        apply(
            &mut state,
            Parameter::LayerOpacity { layer_id },
            ChannelValue::Flag(true),
        );

        assert!((state.layer(layer_id).expect("loaded").opacity - before).abs() < f32::EPSILON);
    }

    #[test]
    fn re_sending_the_same_value_says_nothing() {
        // A console sends every channel forty-four times a second. Almost none
        // of them are different from last time, and the update stream must not
        // carry them.
        let (mut state, _) = show();
        apply(
            &mut state,
            Parameter::MasterDimmer,
            ChannelValue::Number(0.25),
        );

        assert!(
            apply(
                &mut state,
                Parameter::MasterDimmer,
                ChannelValue::Number(0.25)
            )
            .is_none(),
            "nothing changed, so nothing was said"
        );
    }
}
