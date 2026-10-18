//! A configured effect chain, as passes the GPU runs.
//!
//! An effect consumes a picture and produces one. A chain is an
//! ordered list of them, and this is the step that turns the list a show
//! stores into the list the compositor records: what each pass computes, and
//! what it was set to.
//!
//! Pure, like [`crate::scene`]. No Vulkan and no GPU, so the rules an operator
//! notices when they are wrong — that a disabled effect does nothing, that an
//! untouched one is neutral, that two of them run in the order they were listed
//! — are testable on a machine with no graphics hardware.
//!
//! # Every number comes from the manifest
//!
//! [`EffectValues`] answers with what a control source has moved it to, or what
//! the show set, or what the effect was published with, in that order.
//! Nothing here carries a default of its own: a default written twice is two
//! defaults, and the second one is wrong the day the first one changes.
//!
//! # Live values are looked up, not merged
//!
//! An effect an operator is playing with during a show has live values on top of
//! its configured ones. They arrive as a separate map and stay
//! separate: [`EffectValues`] holds a borrow of each and asks them in order, so
//! building a chain costs no allocation however many parameters are being
//! driven. Merging them into one map per effect per frame is the obvious
//! implementation and is exactly what the realtime rules forbid (house rule §5).
//!
//! # One effect is not one pass
//!
//! A separable blur is a pass across and a pass down, and a block average is the
//! same shape of thing. So an effect is asked how many passes it is rather than
//! being assumed to be one, and the compositor runs a flat list — which is why
//! the blur needed no special case anywhere downstream of here.

use std::sync::atomic::{AtomicBool, Ordering};

use ovs_types::config::effects::{self, EffectValues};
use ovs_types::config::manifest::ParameterKind;
use ovs_types::config::EffectInstance;
use ovs_types::id::MediaItemId;
use ovs_types::live::EffectOverrides;

/// What one full-screen pass computes.
///
/// The value the shader switches on. The numbers are part of the contract
/// between this and `shaders/effect.wgsl`, which is why they are written down
/// once, here, rather than being taken from the order of the variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassKind {
    /// Brightness, contrast, saturation and gamma, in that order.
    ColourCorrect,
    /// A mix towards one colour.
    Tint,
    /// The across half of a block average.
    PixelateAcross,
    /// The down half of a block average.
    PixelateDown,
    /// The across half of a separable Gaussian.
    BlurAcross,
    /// The down half of a separable Gaussian.
    BlurDown,
    /// Four edges cleared, in composition pixels.
    Crop,
    /// The white point: temperature in kelvin, then the green–magenta axis.
    WhiteBalance,
    /// One half of the picture folded onto the other, on either axis.
    Mirror,
    /// The brightness kept and one colour painted over it.
    Colorize,
    /// A black level and a white level, per channel.
    RgbCorrect,
    /// The channels pulled apart along a line.
    RgbSplit,
    /// Line jumps, snow and a rolling error. The one pass that reads the clock.
    TvGlitch,
    /// A picture from the library, multiplied in or cut out. The one pass that
    /// samples a second texture.
    Mask,
    /// Every colour swapped for its opposite, or nothing at all.
    Invert,
}

impl PassKind {
    /// Returns the number the shader's branch is selected by.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::ColourCorrect => 0,
            Self::Tint => 1,
            Self::PixelateAcross => 2,
            Self::PixelateDown => 3,
            Self::BlurAcross => 4,
            Self::BlurDown => 5,
            Self::Crop => 6,
            Self::WhiteBalance => 7,
            Self::Mirror => 8,
            Self::Colorize => 9,
            Self::RgbCorrect => 10,
            Self::RgbSplit => 11,
            Self::TvGlitch => 12,
            Self::Mask => 13,
            Self::Invert => 14,
        }
    }
}

/// One full-screen pass, with everything its shader reads.
///
/// Copy and fixed-size: a chain is built into a reused buffer every frame, so a
/// pass may not own anything that would have to be allocated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectPass {
    /// Which branch of the effect shader runs.
    pub kind: PassKind,
    /// The pass's numbers, in the order its own branch reads them.
    pub values: [f32; 4],
    /// The pass's colour, where it has one.
    pub colour: [f32; 4],
    /// The picture this pass samples besides the one it is given, where it has
    /// one.
    ///
    /// **The only thing in a pass that is not a number.** Everything else here
    /// is pushed to the shader as constants; this is a texture the compositor
    /// has to bind. A pass naming no picture, and one naming a picture that is
    /// not on the GPU, both get the white default bound instead — and white is
    /// a mask that does nothing, so neither changes the layer. Still `Copy`: an
    /// identifier is sixteen bytes and a chain is rebuilt into a reused buffer
    /// every frame.
    pub mask: Option<MediaItemId>,
}

/// Appends the passes a chain runs at its configured values, in chain order.
///
/// For callers with no live state to apply: the whole show has some, but a test
/// of what one effect draws at given values has none, and neither does anything
/// asking what a chain would look like on its own.
pub fn passes_into(chain: &[EffectInstance], into: &mut Vec<EffectPass>) {
    live_passes_into(chain, &EffectOverrides::new(), into);
}

/// Appends the passes a chain runs, in chain order, live values on top.
///
/// A disabled effect contributes nothing, so a chain whose effects are all
/// disabled appends nothing at all — which is what lets the compositor take a
/// layer with such a chain down exactly the path it takes with no chain.
///
/// `into` is appended to rather than replaced, because a frame's chains live
/// end to end in one buffer that is reused rather than reallocated.
pub fn live_passes_into(
    chain: &[EffectInstance],
    live: &EffectOverrides,
    into: &mut Vec<EffectPass>,
) {
    for instance in chain {
        if !instance.enabled {
            continue;
        }

        let Some(manifest) = effects::manifest(&instance.type_id, instance.version) else {
            // Unreachable on a show that was saved by this build: validation
            // refuses an effect nobody publishes. Reachable on one that arrives
            // from somewhere else, and the answer is the layer without the
            // effect rather than a layer drawn with something else's maths.
            warn_once(&instance.type_id, instance.version);
            continue;
        };

        // A lookup per effect per frame, against a map holding only the effects
        // somebody has actually touched. Nothing is built and nothing is
        // copied; an effect nobody is driving finds nothing and reads the
        // show's values exactly as before.
        passes_of(
            manifest.live_over(&instance.parameters, live.get(&instance.id)),
            into,
        );
    }
}

/// Appends the pictures a chain's effects name, live values on top.
///
/// Which pictures have to be on the GPU is decided from the show rather than
/// from the scene, the same way a slot's clip is: this is what the residency is
/// told to keep, and a layer faded to nothing keeps its mask instead of losing
/// it and having to decode again the moment it is faded up.
///
/// Appends rather than replaces, and skips what is already in the list, because
/// the caller is collecting every layer's into one buffer it reuses.
pub fn media_wanted(chain: &[EffectInstance], live: &EffectOverrides, into: &mut Vec<MediaItemId>) {
    for instance in chain {
        // A disabled effect is not drawn, but its picture is kept: an operator
        // switching a mask off and on again during a show must not wait for a
        // decode, and the list this feeds is also what keeps it from being
        // evicted. It is the same reason a faded layer keeps its clip.
        let Some(manifest) = effects::manifest(&instance.type_id, instance.version) else {
            continue;
        };
        let values = manifest.live_over(&instance.parameters, live.get(&instance.id));
        for parameter in manifest.parameters {
            if !matches!(parameter.kind, ParameterKind::Image) {
                continue;
            }
            if let Some(media_item_id) = values.media(parameter.key) {
                if !into.contains(&media_item_id) {
                    into.push(media_item_id);
                }
            }
        }
    }
}

/// Appends the passes one effect runs.
fn passes_of(values: EffectValues<'_>, into: &mut Vec<EffectPass>) {
    let manifest = values.manifest();
    let number = |key: &str| values.number(key) as f32;

    match (manifest.type_id, manifest.version) {
        // **Two passes, because the white point comes after.** A pass carries
        // four numbers and version two has six, and splitting them is not a
        // workaround for that: the owner put temperature and tint after
        // saturation and gamma, so they belong in a pass that runs after the
        // one which does those. Appended even at their defaults, for the same
        // reason every other effect is — the renderer proves an effect neutral
        // by rendering it.
        ("ovs.colour_correct", 2) => {
            into.push(EffectPass {
                kind: PassKind::ColourCorrect,
                values: [
                    number("brightness"),
                    number("contrast"),
                    number("saturation"),
                    number("gamma"),
                ],
                colour: [0.0; 4],
                mask: None,
            });
            into.push(EffectPass {
                kind: PassKind::WhiteBalance,
                values: [number("temperature_k"), number("tint"), 0.0, 0.0],
                colour: [0.0; 4],
                mask: None,
            });
        }
        ("ovs.colour_correct", 1) => into.push(EffectPass {
            kind: PassKind::ColourCorrect,
            values: [
                number("brightness"),
                number("contrast"),
                number("saturation"),
                number("gamma"),
            ],
            colour: [0.0; 4],
            mask: None,
        }),
        // A switch, and the pass runs either way: an effect that appended
        // nothing when it was off would never be drawn, and the renderer
        // proves every published effect neutral at its defaults by drawing it.
        ("ovs.invert", 1) => into.push(EffectPass {
            kind: PassKind::Invert,
            // Zero in the rate slot, which the shader reads as "do not flash".
            // Version one has no rate and must keep behaving as it was saved.
            values: [f32::from(u8::from(values.flag("on"))), 0.0, 0.0, 0.0],
            colour: [0.0; 4],
            mask: None,
        }),
        // The same pass with a rate. The flashing is decided in the shader
        // rather than here: the pass is built once per frame and the clock it
        // would be compared against is the one already on the card, so working
        // it out on this side would mean reading a second clock that is a
        // fraction of a frame away from the first.
        ("ovs.invert", 2) => into.push(EffectPass {
            kind: PassKind::Invert,
            values: [
                f32::from(u8::from(values.flag("on"))),
                number("hz"),
                0.0,
                0.0,
            ],
            colour: [0.0; 4],
            mask: None,
        }),
        ("ovs.tint", 1) => into.push(EffectPass {
            kind: PassKind::Tint,
            values: [
                number("amount"),
                f32::from(u8::from(values.flag("preserve_luma"))),
                0.0,
                0.0,
            ],
            colour: values.colour("colour"),
            mask: None,
        }),
        ("ovs.pixelate", 1) => {
            let block = number("block_px");
            for kind in [PassKind::PixelateAcross, PassKind::PixelateDown] {
                into.push(EffectPass {
                    kind,
                    values: [block, 0.0, 0.0, 0.0],
                    colour: [0.0; 4],
                    mask: None,
                });
            }
        }
        ("ovs.blur", 1) => {
            let radius = number("radius_px");
            let amount = number("amount");
            for kind in [PassKind::BlurAcross, PassKind::BlurDown] {
                into.push(EffectPass {
                    kind,
                    values: [radius, amount, 0.0, 0.0],
                    colour: [0.0; 4],
                    mask: None,
                });
            }
        }
        // One pass for all four edges: the test is a rectangle and there is no
        // kernel, so there is nothing to separate the way the blur and the
        // pixelate are. Appended even when nothing is cut, which is what lets
        // the renderer prove it neutral at its defaults by rendering it.
        ("ovs.crop", 1) => into.push(EffectPass {
            kind: PassKind::Crop,
            values: [
                number("left_px"),
                number("right_px"),
                number("top_px"),
                number("bottom_px"),
            ],
            colour: [0.0; 4],
            mask: None,
        }),
        // **Appended with no picture as well**, like every other effect here,
        // because that is what lets the renderer prove it neutral by rendering
        // it. A pass naming no picture binds the white default texture, and
        // white is a mask that does nothing — which is also what a mask that is
        // still decoding and a mask whose picture was deleted from the library
        // get, so all three leave the layer exactly as it was.
        ("ovs.mask", 1) => into.push(EffectPass {
            kind: PassKind::Mask,
            values: [
                number("amount"),
                // Which of the two things it does, as a code. Multiplying
                // darkens the picture and keeps the layer covering what is
                // under it; cutting out lowers the layer's own alpha, so what
                // is underneath comes through the hole.
                f32::from(u8::from(values.choice("mode") == "cut_out")),
                f32::from(u8::from(values.flag("invert"))),
                0.0,
            ],
            colour: [0.0; 4],
            mask: values.media("image"),
        }),
        ("ovs.mirror", 1) => into.push(EffectPass {
            kind: PassKind::Mirror,
            // Which half each axis keeps, as a number the shader branches on:
            // nothing, the low side, or the high side. A choice crosses as a
            // code rather than as a string because a pass is four floats and a
            // colour, and this is the one place the two vocabularies meet.
            values: [
                fold(&values.choice("horizontal")),
                fold(&values.choice("vertical")),
                0.0,
                0.0,
            ],
            colour: [0.0; 4],
            mask: None,
        }),
        ("ovs.colorize", 1) => into.push(EffectPass {
            kind: PassKind::Colorize,
            values: [number("hue"), number("saturation"), number("amount"), 0.0],
            colour: [0.0; 4],
            mask: None,
        }),
        // **The lift travels in the colour slot, because it is one.** A pass
        // carries four numbers and this effect has six, and a black level per
        // channel is an RGB triple in every sense that matters. The
        // alternative is a second full-screen read and write for arithmetic
        // that fits in one.
        ("ovs.rgb_correct", 1) => into.push(EffectPass {
            kind: PassKind::RgbCorrect,
            values: [number("gain_r"), number("gain_g"), number("gain_b"), 0.0],
            colour: [number("lift_r"), number("lift_g"), number("lift_b"), 0.0],
            mask: None,
        }),
        ("ovs.rgb_split", 1) => into.push(EffectPass {
            kind: PassKind::RgbSplit,
            values: [number("offset_px"), number("angle_deg"), 0.0, 0.0],
            colour: [0.0; 4],
            mask: None,
        }),
        // **The fifth number travels in the colour slot, because a pass has four
        // and this effect has five.** The same thing RGB Correct does with its
        // lift, and the reason version one is untouched by this: a version one
        // pass leaves the slot at nought, and at nought the shader does what it
        // always did.
        ("ovs.tv_glitch", 2) => into.push(EffectPass {
            kind: PassKind::TvGlitch,
            values: [
                number("amount"),
                number("tear"),
                number("noise"),
                number("roll"),
            ],
            // Scaled here rather than in the shader: how far out of hold the
            // picture may go is the owner's number and lives beside the
            // manifest, so what crosses is a fraction of the height.
            colour: [
                number("vertical_hold") * effects::GLITCH_OUT_OF_SYNC as f32,
                0.0,
                0.0,
                0.0,
            ],
            mask: None,
        }),
        ("ovs.tv_glitch", 1) => into.push(EffectPass {
            kind: PassKind::TvGlitch,
            values: [
                number("amount"),
                number("tear"),
                number("noise"),
                number("roll"),
            ],
            colour: [0.0; 4],
            mask: None,
        }),
        // A manifest this build publishes but the renderer cannot draw. Nothing
        // is in this state today — the four published effects are the four
        // above — and the day one is, it is a layer drawn without it and a line
        // in the log rather than a guess.
        _ => warn_once(manifest.type_id, manifest.version),
    }
}

/// Which half an axis of the mirror keeps, as the shader's own number.
///
/// Zero is off, and the two sides are told apart rather than folded together:
/// a logo in the left third and one in the right third survive opposite
/// choices, which is why this is a choice and not a switch.
fn fold(choice: &str) -> f32 {
    match choice {
        "left" | "top" => 1.0,
        "right" | "bottom" => 2.0,
        // Including the empty string, which is what a key that is not a choice
        // answers: off is the one value that cannot be wrong.
        _ => 0.0,
    }
}

/// Says once that an effect cannot be drawn.
///
/// Once per process rather than once per frame: an effect the renderer cannot
/// draw would be sixty lines a second for as long as the show runs, which is a
/// log nobody can read for a fault that does not change.
fn warn_once(type_id: &str, version: u32) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            type_id,
            version,
            "this build cannot draw this effect; the layer is drawn without it"
        );
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::ParameterValue;

    use super::*;

    /// One enabled instance of a published effect, with the given values.
    fn instance(type_id: &str, values: &[(&str, ParameterValue)]) -> EffectInstance {
        let mut instance = EffectInstance::new(type_id, 1);
        for (key, value) in values {
            instance.parameters.insert((*key).to_owned(), value.clone());
        }
        instance
    }

    /// The passes one chain runs.
    fn passes(chain: &[EffectInstance]) -> Vec<EffectPass> {
        let mut passes = Vec::new();
        passes_into(chain, &mut passes);
        passes
    }

    #[test]
    fn a_chain_with_nothing_in_it_runs_nothing() {
        assert!(passes(&[]).is_empty());
    }

    #[test]
    fn a_disabled_effect_contributes_no_passes() {
        // The claim the renderer's fast path rests on: a layer whose every
        // effect is switched off must be a layer with no chain at all, not a
        // layer that renders into a scratch image and copies it back.
        let mut off = instance("ovs.blur", &[]);
        off.enabled = false;
        assert!(passes(&[off]).is_empty());
    }

    #[test]
    fn every_published_effect_has_a_renderer_implementation() {
        // A published effect a client offers and the renderer cannot draw is an
        // operator adding something and seeing nothing happen.
        for manifest in ovs_types::config::EFFECTS {
            let chain = [EffectInstance::new(manifest.type_id, manifest.version)];
            assert!(
                !passes(&chain).is_empty(),
                "{} version {} runs no passes",
                manifest.type_id,
                manifest.version
            );
        }
    }

    #[test]
    fn a_separable_effect_is_two_passes_for_one_instance() {
        // Across then down, in that order, for both of the two that are
        // separable. The compositor runs a flat list and does not know which
        // pass belongs to which effect, so the order has to be right here.
        assert_eq!(
            passes(&[instance("ovs.blur", &[])])
                .iter()
                .map(|pass| pass.kind)
                .collect::<Vec<_>>(),
            vec![PassKind::BlurAcross, PassKind::BlurDown]
        );
        assert_eq!(
            passes(&[instance("ovs.pixelate", &[])])
                .iter()
                .map(|pass| pass.kind)
                .collect::<Vec<_>>(),
            vec![PassKind::PixelateAcross, PassKind::PixelateDown]
        );
    }

    #[test]
    fn a_parameter_nobody_set_is_the_one_the_manifest_publishes() {
        // Not a default written into the renderer. The manifest is the one
        // place a default lives, so a published effect can be changed in one
        // file and be right everywhere.
        let correct = passes(&[instance("ovs.colour_correct", &[])]);
        assert_eq!(correct[0].values, [1.0, 1.0, 1.0, 1.0]);

        let tint = passes(&[instance("ovs.tint", &[])]);
        assert_eq!(tint[0].values[0], 0.0, "amount");
        assert_eq!(tint[0].values[1], 1.0, "keep brightness is on");
        assert_eq!(tint[0].colour, [1.0, 1.0, 1.0, 1.0]);

        assert_eq!(passes(&[instance("ovs.pixelate", &[])])[0].values[0], 1.0);
        assert_eq!(passes(&[instance("ovs.blur", &[])])[0].values[0], 0.0);
        assert_eq!(passes(&[instance("ovs.blur", &[])])[0].values[1], 1.0);
    }

    #[test]
    fn a_parameter_a_show_set_is_the_one_that_reaches_the_shader() {
        let brightened = passes(&[instance(
            "ovs.colour_correct",
            &[("brightness", ParameterValue::Float(2.0))],
        )]);
        assert_eq!(brightened[0].values[0], 2.0);
        assert_eq!(brightened[0].values[1], 1.0, "and the rest stay published");
    }

    #[test]
    fn a_tints_colour_and_switch_reach_the_shader() {
        let tinted = passes(&[instance(
            "ovs.tint",
            &[
                ("colour", ParameterValue::Color([1.0, 0.0, 0.0, 1.0])),
                ("amount", ParameterValue::Float(0.5)),
                ("preserve_luma", ParameterValue::Bool(false)),
            ],
        )]);
        assert_eq!(tinted[0].colour, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(tinted[0].values[0], 0.5);
        assert_eq!(tinted[0].values[1], 0.0, "switched off");
    }

    /// The passes one chain runs with live values on top.
    fn live_passes(chain: &[EffectInstance], live: &EffectOverrides) -> Vec<EffectPass> {
        let mut passes = Vec::new();
        live_passes_into(chain, live, &mut passes);
        passes
    }

    #[test]
    fn a_live_value_is_the_one_that_reaches_the_shader() {
        let configured = instance("ovs.blur", &[("radius_px", ParameterValue::Float(8.0))]);
        let live = EffectOverrides::from([(
            configured.id,
            ovs_types::config::ParameterMap::from([(
                "radius_px".to_owned(),
                ParameterValue::Float(40.0),
            )]),
        )]);

        let passes = live_passes(std::slice::from_ref(&configured), &live);
        assert_eq!(passes[0].values[0], 40.0);
        assert_eq!(
            passes[1].values[0], 40.0,
            "both halves of the separable pair"
        );
        assert_eq!(
            configured.parameters.get("radius_px"),
            Some(&ParameterValue::Float(8.0)),
            "the instance the chain was built from is unchanged"
        );
    }

    #[test]
    fn a_parameter_no_live_value_names_stays_where_the_show_put_it() {
        // An operator moving one control must not quietly return the others to
        // their published defaults.
        let configured = instance(
            "ovs.blur",
            &[
                ("radius_px", ParameterValue::Float(8.0)),
                ("amount", ParameterValue::Float(0.25)),
            ],
        );
        let live = EffectOverrides::from([(
            configured.id,
            ovs_types::config::ParameterMap::from([(
                "radius_px".to_owned(),
                ParameterValue::Float(40.0),
            )]),
        )]);

        let passes = live_passes(std::slice::from_ref(&configured), &live);
        assert_eq!(passes[0].values[0], 40.0, "radius");
        assert_eq!(passes[0].values[1], 0.25, "amount");
    }

    #[test]
    fn a_live_colour_and_a_live_switch_reach_the_shader_too() {
        // Not only the numbers: every kind of parameter a manifest can declare.
        let configured = instance("ovs.tint", &[]);
        let live = EffectOverrides::from([(
            configured.id,
            ovs_types::config::ParameterMap::from([
                (
                    "colour".to_owned(),
                    ParameterValue::Color([0.0, 1.0, 0.0, 1.0]),
                ),
                ("preserve_luma".to_owned(), ParameterValue::Bool(false)),
            ]),
        )]);

        let passes = live_passes(std::slice::from_ref(&configured), &live);
        assert_eq!(passes[0].colour, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(passes[0].values[1], 0.0, "switched off live");
    }

    #[test]
    fn a_live_value_belongs_to_one_instance_and_not_to_the_effect_type() {
        // Two blurs on one layer are two controls. Keying on the type would
        // make moving one move the other.
        let first = instance("ovs.blur", &[]);
        let second = instance("ovs.blur", &[]);
        let live = EffectOverrides::from([(
            second.id,
            ovs_types::config::ParameterMap::from([(
                "radius_px".to_owned(),
                ParameterValue::Float(40.0),
            )]),
        )]);

        let passes = live_passes(&[first, second], &live);
        assert_eq!(passes[0].values[0], 0.0, "the first blur is untouched");
        assert_eq!(passes[2].values[0], 40.0, "the second one moved");
    }

    #[test]
    fn a_disabled_effect_stays_disabled_however_it_is_driven() {
        // Live values decide what an effect looks like, never whether it runs:
        // that is chain configuration, and an operator who switched an effect
        // off must not have it come back because somebody moved a fader.
        let mut off = instance("ovs.blur", &[]);
        off.enabled = false;
        let live = EffectOverrides::from([(
            off.id,
            ovs_types::config::ParameterMap::from([(
                "radius_px".to_owned(),
                ParameterValue::Float(40.0),
            )]),
        )]);

        assert!(live_passes(std::slice::from_ref(&off), &live).is_empty());
    }

    #[test]
    fn a_chain_runs_in_the_order_it_was_listed() {
        let chain = [instance("ovs.tint", &[]), instance("ovs.pixelate", &[])];
        assert_eq!(
            passes(&chain)
                .iter()
                .map(|pass| pass.kind)
                .collect::<Vec<_>>(),
            vec![
                PassKind::Tint,
                PassKind::PixelateAcross,
                PassKind::PixelateDown
            ]
        );
    }

    #[test]
    fn an_effect_this_build_does_not_publish_runs_nothing() {
        // Rather than something else's maths. A show cannot be saved in this
        // state; one that arrives from elsewhere draws the layer without it.
        assert!(passes(&[EffectInstance::new("ovs.kaleidoscope", 1)]).is_empty());
        assert!(passes(&[EffectInstance::new("ovs.blur", 99)]).is_empty());
    }

    #[test]
    fn a_crop_passes_its_four_edges_in_the_order_the_shader_reads_them() {
        // Left, right, top, bottom. Four numbers in one vector and nothing to
        // name them by the time they reach the card, so the order is the whole
        // contract -- and swapping two of them would crop the wrong edges
        // while every other test still passed.
        let chain = [instance(
            "ovs.crop",
            &[
                ("left_px", ParameterValue::Float(1.0)),
                ("right_px", ParameterValue::Float(2.0)),
                ("top_px", ParameterValue::Float(3.0)),
                ("bottom_px", ParameterValue::Float(4.0)),
            ],
        )];
        let passes = passes(&chain);

        assert_eq!(passes.len(), 1, "one pass for all four edges");
        assert_eq!(passes[0].kind, PassKind::Crop);
        assert_eq!(passes[0].values, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn the_white_point_runs_after_the_levels_rather_than_before_them() {
        // The owner's order, 2026-10-12: "beides passiert erst nach Saturation
        // und Gamma". The compositor runs a flat list and does not know which
        // pass belongs to which effect, so the order has to be right here.
        let mut version_two = instance("ovs.colour_correct", &[]);
        version_two.version = 2;
        let passes = passes(&[version_two]);
        assert_eq!(
            passes.iter().map(|pass| pass.kind).collect::<Vec<_>>(),
            vec![PassKind::ColourCorrect, PassKind::WhiteBalance]
        );
    }

    #[test]
    fn a_mirror_tells_the_two_sides_apart() {
        // Off, the low side, the high side. A switch would have collapsed the
        // last two into one and made half the pictures unreachable.
        let fold = |axis: &str, side: &str| {
            let chain = [instance(
                "ovs.mirror",
                &[(axis, ParameterValue::Choice(side.to_owned()))],
            )];
            passes(&chain)[0].values
        };

        assert_eq!(fold("horizontal", "off")[0], 0.0);
        assert_eq!(fold("horizontal", "left")[0], 1.0);
        assert_eq!(fold("horizontal", "right")[0], 2.0);
        assert_eq!(fold("vertical", "top")[1], 1.0);
        assert_eq!(fold("vertical", "bottom")[1], 2.0);
    }

    #[test]
    fn every_pass_kind_has_its_own_code() {
        let kinds = [
            PassKind::ColourCorrect,
            PassKind::Tint,
            PassKind::PixelateAcross,
            PassKind::PixelateDown,
            PassKind::BlurAcross,
            PassKind::BlurDown,
        ];
        for (index, kind) in kinds.iter().enumerate() {
            for other in &kinds[index + 1..] {
                assert_ne!(kind.code(), other.code(), "{kind:?} and {other:?}");
            }
        }
    }

    /// The rate reaches the pass, and version one keeps a rate of nothing.
    ///
    /// The owner asked for an Invert that flashes on its own
    /// ("der selber in einer HZ Zahl wechselt"), which is version two. Version
    /// one has no such parameter and must go on meaning exactly what it meant
    /// when somebody saved a show with it.
    #[test]
    fn an_inverts_rate_reaches_the_pass_and_version_one_has_none() {
        let mut two = EffectInstance::new("ovs.invert", 2);
        two.parameters
            .insert("on".to_owned(), ParameterValue::Bool(true));
        two.parameters
            .insert("hz".to_owned(), ParameterValue::Float(8.0));

        let mut passes = Vec::new();
        passes_into(std::slice::from_ref(&two), &mut passes);
        assert_eq!(passes.len(), 1);
        assert_eq!(passes[0].kind, PassKind::Invert);
        assert_eq!(passes[0].values[0], 1.0, "switched on");
        assert_eq!(passes[0].values[1], 8.0, "eight hertz");

        let mut one = EffectInstance::new("ovs.invert", 1);
        one.parameters
            .insert("on".to_owned(), ParameterValue::Bool(true));
        let mut older = Vec::new();
        passes_into(std::slice::from_ref(&one), &mut older);
        assert_eq!(older[0].values[0], 1.0);
        assert_eq!(
            older[0].values[1], 0.0,
            "version one must carry no rate at all, which the shader reads as steady"
        );
    }

    /// Switched off is off, whatever the rate says.
    ///
    /// A flashing effect somebody has switched off has to go dark rather than
    /// carry on flashing, and it is what keeps the published defaults neutral.
    #[test]
    fn an_invert_switched_off_does_not_flash() {
        let mut off = EffectInstance::new("ovs.invert", 2);
        off.parameters
            .insert("hz".to_owned(), ParameterValue::Float(12.0));

        let mut passes = Vec::new();
        passes_into(std::slice::from_ref(&off), &mut passes);
        assert_eq!(passes.len(), 1, "the pass runs either way");
        assert_eq!(
            passes[0].values[0], 0.0,
            "off at the published default, so the shader returns the picture untouched"
        );
    }
}
