//! Blend modes as Vulkan blend state.
//!
//! One pipeline per mode, differing only in the blend state, because a blend
//! mode *is* blend state: the GPU's fixed-function blender is exactly the piece
//! of hardware this feature is asking for, and doing it in a shader would mean
//! reading the destination, which needs either an extension or a pass per layer.
//!
//! # Premultiplied, and why
//!
//! The fragment shader hands over premultiplied colour, so the source is already
//! scaled by its own alpha before the blender sees it. That is what makes most
//! of these modes exact at partial opacity rather than only at full: fading a
//! layer down has to fade its *contribution*, and with straight alpha that is
//! not something the fixed-function factors can express.
//!
//! [`identity`] is the other half of the same idea. Fading out means tending
//! towards the mode's identity — black for anything additive, white for a
//! minimum — so the shader mixes towards it rather than towards zero.
//!
//! # Alpha is never written
//!
//! Every mode writes RGB only. The composition is opaque by construction: it is
//! cleared opaque and nothing may change that, because an output samples it and
//! a composition that had gone half-transparent would show as a half-dark wall.

use ash::vk;
use ovs_types::config::BlendMode;

/// How many blend modes there are, for sizing the pipeline table.
pub(crate) const BLEND_MODE_COUNT: usize = 8;

/// Every blend mode, in the order [`index_of`] gives them.
pub(crate) const ALL_BLEND_MODES: [BlendMode; BLEND_MODE_COUNT] = [
    BlendMode::Normal,
    BlendMode::Add,
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::Lighten,
    BlendMode::Darken,
    BlendMode::Difference,
];

/// Returns a mode's position in the pipeline table.
pub(crate) const fn index_of(mode: BlendMode) -> usize {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Add => 1,
        BlendMode::Multiply => 2,
        BlendMode::Screen => 3,
        BlendMode::Overlay => 4,
        BlendMode::Lighten => 5,
        BlendMode::Darken => 6,
        BlendMode::Difference => 7,
    }
}

/// What the shader mixes towards as a layer fades out.
///
/// Black for every mode whose identity is "add nothing"; white for [`Darken`],
/// whose identity is "let everything through" because it takes a minimum.
///
/// [`Darken`]: BlendMode::Darken
pub(crate) const fn identity(mode: BlendMode) -> f32 {
    match mode {
        BlendMode::Darken => 1.0,
        _ => 0.0,
    }
}

/// Returns true when this mode draws as configured.
///
/// [`BlendMode::Overlay`] and [`BlendMode::Difference`] cannot be expressed with
/// core Vulkan blending: one needs the destination in the shader and the other
/// needs an absolute difference, and the fixed-function blender offers neither.
/// `VK_EXT_blend_operation_advanced` has both as hardware equations, so on a
/// device that offers it they draw as themselves; on one that does not they are
/// drawn as [`BlendMode::Normal`] and the renderer says so, rather than showing
/// something subtly wrong in silence.
pub(crate) const fn is_supported(mode: BlendMode, advanced: bool) -> bool {
    advanced || !matches!(mode, BlendMode::Overlay | BlendMode::Difference)
}

/// Returns the advanced operation a mode needs, if it needs one.
///
/// The source the shader hands over is premultiplied and the composition is
/// opaque, which is what makes these exact: with a destination alpha of one the
/// advanced equation reduces to "blend, then fade by the source's alpha", which
/// is what a blend mode at partial opacity means.
pub(crate) const fn advanced_op(mode: BlendMode) -> Option<vk::BlendOp> {
    match mode {
        BlendMode::Overlay => Some(vk::BlendOp::OVERLAY_EXT),
        BlendMode::Difference => Some(vk::BlendOp::DIFFERENCE_EXT),
        _ => None,
    }
}

/// The blend state for one mode.
///
/// A mode that cannot be expressed gets [`BlendMode::Normal`]'s state, which is
/// the honest fallback: it composites, it looks like something, and the log says
/// what happened.
pub(crate) fn attachment_for(
    mode: BlendMode,
    advanced: bool,
) -> vk::PipelineColorBlendAttachmentState {
    attachment_with(mode, advanced, false)
}

/// The blend state for one mode, drawing into a bus.
///
/// The same colour arithmetic, plus coverage. A bus is a picture *with* an alpha
/// channel — the crossfade has to know where its layers are and where they are
/// not — so alpha accumulates source-over whatever the colour mode does: a layer
/// drawn with `Add` still covers what it is drawn over.
pub(crate) fn attachment_for_bus(
    mode: BlendMode,
    advanced: bool,
) -> vk::PipelineColorBlendAttachmentState {
    attachment_with(mode, advanced, true)
}

/// The blend state for one mode, with or without an alpha channel.
fn attachment_with(
    mode: BlendMode,
    advanced: bool,
    writes_alpha: bool,
) -> vk::PipelineColorBlendAttachmentState {
    let mut mask =
        vk::ColorComponentFlags::R | vk::ColorComponentFlags::G | vk::ColorComponentFlags::B;
    if writes_alpha {
        mask |= vk::ColorComponentFlags::A;
    }

    let base = vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(mask)
        .blend_enable(true)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(if writes_alpha {
            // Coverage accumulates source-over: what is already there shows
            // through whatever the new layer does not cover.
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA
        } else {
            vk::BlendFactor::ZERO
        })
        .alpha_blend_op(vk::BlendOp::ADD);

    // The hardware's own equation, where there is one. The factors are
    // ignored for an advanced operation, and the alpha operation has to match
    // the colour one.
    if advanced {
        if let Some(operation) = advanced_op(mode) {
            return base.color_blend_op(operation).alpha_blend_op(operation);
        }
    }

    match mode {
        // Source over destination. `src + dst * (1 - a)` — the premultiplied
        // form of ordinary alpha compositing.
        BlendMode::Normal | BlendMode::Overlay | BlendMode::Difference => base
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD),

        // `src + dst`, and since the source is premultiplied, fading the layer
        // fades what it adds.
        BlendMode::Add => base
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::ADD),

        // `dst * src + dst * (1 - a)` = `dst * mix(1, src, a)`: at full opacity
        // a true multiply, and at zero the destination untouched.
        BlendMode::Multiply => base
            .src_color_blend_factor(vk::BlendFactor::DST_COLOR)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD),

        // `src * (1 - dst) + dst` = `src + dst - src * dst`, which is the
        // inverse multiply, exact with a premultiplied source.
        BlendMode::Screen => base
            .src_color_blend_factor(vk::BlendFactor::ONE_MINUS_DST_COLOR)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::ADD),

        // Per-channel maximum. The source is premultiplied, so a layer fading
        // out contributes less and finally nothing.
        BlendMode::Lighten => base
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::MAX),

        // Per-channel minimum, against a source the shader mixed towards white
        // as it faded. Premultiplying towards black here would make a layer at
        // zero opacity darken everything to nothing.
        BlendMode::Darken => base
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::MIN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_a_place_in_the_table() {
        let mut seen = [false; BLEND_MODE_COUNT];
        for mode in ALL_BLEND_MODES {
            let index = index_of(mode);
            assert!(!seen[index], "{mode:?} shares an index");
            seen[index] = true;
        }
        assert!(seen.iter().all(|&taken| taken));
    }

    #[test]
    fn the_table_is_in_index_order() {
        // The pipeline table is built by iterating this list, so a list that
        // disagreed with the indices would give every mode somebody else's
        // blend state — and every one of them still looks like *a* blend.
        for (index, mode) in ALL_BLEND_MODES.into_iter().enumerate() {
            assert_eq!(index_of(mode), index, "{mode:?}");
        }
    }

    #[test]
    fn no_mode_writes_alpha() {
        // An output samples the composition, so a composition that went
        // half-transparent would show as a half-dark wall.
        for mode in ALL_BLEND_MODES {
            for advanced in [false, true] {
                let state = attachment_for(mode, advanced);
                assert!(
                    !state.color_write_mask.contains(vk::ColorComponentFlags::A),
                    "{mode:?} writes alpha"
                );
            }
        }
    }

    #[test]
    fn only_darken_fades_towards_white() {
        for mode in ALL_BLEND_MODES {
            let expected = if mode == BlendMode::Darken { 1.0 } else { 0.0 };
            assert!((identity(mode) - expected).abs() < f32::EPSILON, "{mode:?}");
        }
    }

    #[test]
    fn without_the_extension_the_two_hard_modes_fall_back_to_normal() {
        // Rather than to nothing. It composites, it looks like something, and
        // the log says what happened.
        let normal = attachment_for(BlendMode::Normal, false);
        for mode in [BlendMode::Overlay, BlendMode::Difference] {
            assert!(!is_supported(mode, false));
            let state = attachment_for(mode, false);
            assert_eq!(state.src_color_blend_factor, normal.src_color_blend_factor);
            assert_eq!(state.dst_color_blend_factor, normal.dst_color_blend_factor);
            assert_eq!(state.color_blend_op, normal.color_blend_op);
        }
    }

    #[test]
    fn with_the_extension_they_are_the_hardware_equations() {
        // And the alpha operation matches the colour one, which the advanced
        // blend specification requires: a pipeline that disagreed with itself
        // would be refused at creation, on a machine nobody is standing next to.
        for (mode, operation) in [
            (BlendMode::Overlay, vk::BlendOp::OVERLAY_EXT),
            (BlendMode::Difference, vk::BlendOp::DIFFERENCE_EXT),
        ] {
            assert!(is_supported(mode, true));
            let state = attachment_for(mode, true);
            assert_eq!(state.color_blend_op, operation, "{mode:?}");
            assert_eq!(state.alpha_blend_op, operation, "{mode:?}");
        }
    }

    #[test]
    fn no_other_mode_asks_for_an_advanced_operation() {
        // The other six are exact with the fixed-function blender, which every
        // device has. Using an extension for them would make a working mode
        // depend on a driver feature for nothing.
        for mode in ALL_BLEND_MODES {
            if matches!(mode, BlendMode::Overlay | BlendMode::Difference) {
                continue;
            }
            assert!(advanced_op(mode).is_none(), "{mode:?}");
            assert!(is_supported(mode, false), "{mode:?}");
            assert_eq!(
                attachment_for(mode, true).color_blend_op,
                attachment_for(mode, false).color_blend_op,
                "{mode:?} draws differently for no reason"
            );
        }
    }
}
