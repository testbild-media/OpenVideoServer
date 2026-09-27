//! Three bit depths that are never allowed to become one number.
//!
//! A show can decode ten-bit material, composite it in eight bits, and send
//! eight bits down the cable. Those are three separate facts about three
//! separate stages, and the owner's instruction of 2026-09-23 is that they stay
//! separate permanently — in the model, in the telemetry and on the output
//! display:
//!
//! > However, preserve the distinction between: Source/Pipeline Decode Depth,
//! > Composition Depth, Physical Output Depth, because these are three separate
//! > capabilities.
//!
//! > Do not describe the current renderer as having a complete end-to-end
//! > 10-bit pipeline if the Composition stage quantizes to 8-bit.
//!
//! **As built, that reads: Source Decode 10-bit, Composition 8-bit, Output
//! 8-bit.** Nothing anywhere may shorten that to "10-bit". The native ten-bit
//! source path is worth having for what it actually does — no CPU-side 10-to-8
//! conversion, and an efficient decode and upload — and not for a number.
//!
//! # Why three types rather than one enum and three fields
//!
//! Because the failure this prevents is assigning one stage's depth to another
//! stage's label. Three types with no conversion between them and no way to
//! compare one to another means `output_depth: composition.into()` does not
//! compile, and neither does `decode == composition`. That is the whole design;
//! the rest is three small enums.
//!
//! The asymmetry between them is deliberate too. Only [`OutputDepth`] has an
//! unknown case: the renderer decodes the media and creates the surfaces, so it
//! cannot be unsure about those two, while a backend with no cable — headless, a
//! composition rendered for a stream — has no honest answer about a cable and
//! saying so is better than defaulting to eight and being believed.

use serde::{Deserialize, Serialize};

/// How many bits a sample of the material being decoded carries.
///
/// A property of the clip and its decoder, reported per playing layer, because
/// that is where it is a fact: two layers of one show can be playing an
/// eight-bit clip and a Main 10 clip at the same time and both are right.
///
/// Where the ten bits *sit* inside their element is a separate matter again and
/// does not belong on a wire — a card's P010 and a processor's `yuv420p10le` are
/// sixty-four times apart and the renderer's own `SampleDepth` keeps them apart.
/// An operator asking what is playing is asking how many bits, not which end of
/// a `u16` they are at.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DecodeDepth {
    /// Eight bits a sample. The efficient path, and most material.
    #[default]
    Eight,
    /// Ten bits a sample, decoded and uploaded as ten with no conversion.
    Ten,
}

impl DecodeDepth {
    /// How many bits of precision a sample carries.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::Eight => 8,
            Self::Ten => 10,
        }
    }

    /// What an operator reads.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Eight => "8-bit",
            Self::Ten => "10-bit",
        }
    }
}

/// How many bits a colour channel carries inside the compositor.
///
/// What the layer buses and the composition surface are made of, which is what
/// decides whether blending twenty layers bands. **This is the stage that
/// quantises**, and it is eight bits: the owner's decision of 2026-09-23 is to
/// leave it there until banding is measured as a real problem, because widening
/// it doubles the bandwidth and memory of every bus of every output.
///
/// It is not a function of the material. A Main 10 clip is decoded at ten bits
/// and composited into this, and this still says eight.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CompositionDepth {
    /// Eight bits a channel. What this renderer composites in.
    #[default]
    Eight,
    /// Ten bits or more a channel.
    ///
    /// Not reachable yet, and reserved rather than aspirational: the format to
    /// investigate first is `A2B10G10R10_UNORM_PACK32`, which is ten-bit RGB at
    /// the same bandwidth as eight-bit RGBA, and whether it is usable depends on
    /// whether two bits of alpha survive alpha video, soft edges, fades, masks,
    /// A/B/M bus compositing and effects that need continuous alpha
    /// (owner decision Q20).
    Ten,
}

impl CompositionDepth {
    /// How many bits of precision a channel carries.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::Eight => 8,
            Self::Ten => 10,
        }
    }

    /// What an operator reads.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Eight => "8-bit",
            Self::Ten => "10-bit",
        }
    }
}

/// How many bits a colour channel carries on the cable.
///
/// What the display engine is scanning out, read off the format the swapchain
/// was actually created with rather than off anything that was asked for. A
/// driver that offers no ten-bit surface format gets [`Self::Eight`] however the
/// show is configured, and that is the number reported.
///
/// [`Self::Unknown`] is not a failure. A backend that is not a panel — a
/// headless one in a test, a composition rendered for a stream — has no cable
/// and no honest answer, and saying so is better than defaulting to eight and
/// being believed.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OutputDepth {
    /// Eight bits a channel leave this socket.
    Eight,
    /// Ten bits a channel leave this socket.
    Ten,
    /// Nothing leaves this socket, or the driver will not say what does.
    #[default]
    Unknown,
}

impl OutputDepth {
    /// How many bits of precision a channel carries, when that is known.
    #[must_use]
    pub const fn bits(self) -> Option<u32> {
        match self {
            Self::Eight => Some(8),
            Self::Ten => Some(10),
            Self::Unknown => None,
        }
    }

    /// What an operator reads.
    ///
    /// Never a number this cannot stand behind: an output whose depth is not
    /// known says so rather than borrowing another stage's.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Eight => "8-bit",
            Self::Ten => "10-bit",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_depths_are_written_the_same_way_on_the_wire() {
        assert_eq!(
            serde_json::to_string(&DecodeDepth::Ten).expect("serialises"),
            r#""ten""#
        );
        assert_eq!(
            serde_json::to_string(&CompositionDepth::Eight).expect("serialises"),
            r#""eight""#
        );
        assert_eq!(
            serde_json::to_string(&OutputDepth::Eight).expect("serialises"),
            r#""eight""#
        );
    }

    #[test]
    fn an_output_that_does_not_know_its_depth_says_so_rather_than_guessing() {
        // The default matters: a backend that never sets this reports
        // `unknown`, not `eight`. Eight would be a claim, and a wrong one on a
        // machine that has no output at all.
        assert_eq!(OutputDepth::default(), OutputDepth::Unknown);
        assert_eq!(OutputDepth::default().bits(), None);
        assert_eq!(OutputDepth::default().label(), "unknown");
    }

    #[test]
    fn the_two_stages_the_renderer_owns_are_never_unsure() {
        // The asymmetry is the point. The renderer opens the decoder and creates
        // the surfaces, so it cannot be unsure about either; the driver owns the
        // cable and can decline to say. An unknown case here would be inventing
        // something that cannot happen and then having to render it.
        assert_eq!(DecodeDepth::default(), DecodeDepth::Eight);
        assert_eq!(CompositionDepth::default(), CompositionDepth::Eight);
        assert_eq!(DecodeDepth::Ten.bits(), 10);
        assert_eq!(CompositionDepth::Eight.bits(), 8);
    }

    #[test]
    fn what_this_renderer_actually_is_reads_as_three_different_numbers() {
        // The owner's own wording, as a test. Ten-bit source, eight-bit
        // composition, eight-bit output — and no way to write any one of those
        // three into another's field, which is what the three types are for.
        assert_eq!(DecodeDepth::Ten.label(), "10-bit");
        assert_eq!(CompositionDepth::Eight.label(), "8-bit");
        assert_eq!(OutputDepth::Eight.label(), "8-bit");
    }
}
