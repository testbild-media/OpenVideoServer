//! Geometry primitives shared by the composition, layer and piece models.
//!
//! Units are stated explicitly on every type, because an ambiguous coordinate
//! space is the fastest way to make a mapped LED wall wrong in a way nobody
//! notices until the show.

use serde::{Deserialize, Serialize};

use crate::error::{ValidationError, ValidationErrors};

/// A size in whole pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SizePx {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl SizePx {
    /// Creates a size.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Returns the pixel count, widened so large canvases cannot overflow.
    #[must_use]
    pub const fn pixel_count(self) -> u64 {
        self.width as u64 * self.height as u64
    }

    pub(crate) fn validate(self, path: &str, out: &mut ValidationErrors) {
        if self.width == 0 || self.height == 0 {
            out.push(ValidationError::new(
                path,
                "width and height must both be greater than zero",
            ));
        }
    }
}

/// An axis-aligned rectangle in whole pixels.
///
/// The origin is the top-left corner of the surrounding surface, with `x` growing
/// right and `y` growing down. Offsets are signed so a piece may be positioned
/// partly outside its output, which is a legitimate mapping choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RectPx {
    /// Left edge in pixels, may be negative.
    pub x: i32,
    /// Top edge in pixels, may be negative.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl RectPx {
    /// Creates a rectangle.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Creates a rectangle covering a whole surface of the given size.
    #[must_use]
    pub const fn covering(size: SizePx) -> Self {
        Self {
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
        }
    }

    /// Returns the size of this rectangle.
    #[must_use]
    pub const fn size(self) -> SizePx {
        SizePx::new(self.width, self.height)
    }

    /// Returns the exclusive right edge, widened to avoid overflow at the extremes.
    #[must_use]
    pub const fn right(self) -> i64 {
        self.x as i64 + self.width as i64
    }

    /// Returns the exclusive bottom edge, widened to avoid overflow at the extremes.
    #[must_use]
    pub const fn bottom(self) -> i64 {
        self.y as i64 + self.height as i64
    }

    /// Returns true when this rectangle lies entirely within a surface of `size`.
    #[must_use]
    pub const fn fits_within(self, size: SizePx) -> bool {
        self.x >= 0
            && self.y >= 0
            && self.right() <= size.width as i64
            && self.bottom() <= size.height as i64
    }

    pub(crate) fn validate(self, path: &str, out: &mut ValidationErrors) {
        if self.width == 0 || self.height == 0 {
            out.push(ValidationError::new(
                path,
                "width and height must both be greater than zero",
            ));
        }
    }
}

/// Where in a rectangle a turn happens.
///
/// A fraction of the rectangle's own size, measured from its top-left corner:
/// `(0.0, 0.0)` is that corner, `(0.5, 0.5)` the centre, `(1.0, 1.0)` the
/// bottom-right. Fractions rather than pixels so that resizing the rectangle
/// does not move the point the operator picked.
///
/// The owner asked for an anchor point per piece (2026-09-23). Rotation used to
/// be about the centre and nothing else, which is right for a wall hung square
/// and wrong for one hinged at a corner: turning it about its centre moves the
/// hinge, and the operator then has to put the rectangle back by hand.
///
/// The centre is the default, and the default is what every document written
/// before this field existed reads as — so nothing already mapped moves.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnchorPoint {
    /// Across, as a fraction of the width from the left edge.
    pub x: f32,
    /// Down, as a fraction of the height from the top edge.
    pub y: f32,
}

impl AnchorPoint {
    /// The middle, which is where a turn happens unless somebody says otherwise.
    pub const CENTRE: Self = Self { x: 0.5, y: 0.5 };

    /// The nine points a corner, edge or centre handle stands for.
    ///
    /// Published as a list so the client can draw the handles it offers from the
    /// model rather than from nine numbers written into a screen.
    pub const HANDLES: [(&'static str, Self); 9] = [
        ("Top left", Self { x: 0.0, y: 0.0 }),
        ("Top", Self { x: 0.5, y: 0.0 }),
        ("Top right", Self { x: 1.0, y: 0.0 }),
        ("Left", Self { x: 0.0, y: 0.5 }),
        ("Centre", Self::CENTRE),
        ("Right", Self { x: 1.0, y: 0.5 }),
        ("Bottom left", Self { x: 0.0, y: 1.0 }),
        ("Bottom", Self { x: 0.5, y: 1.0 }),
        ("Bottom right", Self { x: 1.0, y: 1.0 }),
    ];

    /// Whether this is the middle, which is the one that is left off the wire.
    #[must_use]
    pub fn is_centre(&self) -> bool {
        *self == Self::CENTRE
    }

    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        for (name, value) in [("x", self.x), ("y", self.y)] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                out.push(ValidationError::new(
                    format!("{path}.{name}"),
                    "must be between 0.0 and 1.0",
                ));
            }
        }
    }
}

impl Default for AnchorPoint {
    fn default() -> Self {
        Self::CENTRE
    }
}

/// A 2D transform applied to a slot source or to a whole layer.
///
/// `x` and `y` are an offset in composition pixels from the centre of the
/// composition, chosen over normalised units because LED operators work in
/// pixels and because a non-square composition makes normalised offsets behave
/// differently on each axis.
///
/// The layer transform is intended for live Art-Net/sACN control; the
/// slot transform is configuration only. Both use this type, but they live in
/// different places in the model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Transform2D {
    /// Horizontal offset from the composition centre, in pixels.
    pub x: f32,
    /// Vertical offset from the composition centre, in pixels.
    pub y: f32,
    /// Horizontal scale multiplier, `1.0` meaning unscaled.
    pub scale_x: f32,
    /// Vertical scale multiplier, `1.0` meaning unscaled.
    pub scale_y: f32,
    /// Clockwise rotation in degrees about the transform anchor.
    pub rotation_deg: f32,
}

impl Transform2D {
    /// The neutral transform: no offset, no scaling, no rotation.
    pub const IDENTITY: Self = Self {
        x: 0.0,
        y: 0.0,
        scale_x: 1.0,
        scale_y: 1.0,
        rotation_deg: 0.0,
    };

    /// Returns true when this transform leaves the source untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        for (name, value) in [
            ("x", self.x),
            ("y", self.y),
            ("scale_x", self.scale_x),
            ("scale_y", self.scale_y),
            ("rotation_deg", self.rotation_deg),
        ] {
            if !value.is_finite() {
                out.push(ValidationError::new(
                    format!("{path}.{name}"),
                    "must be a finite number",
                ));
            }
        }
    }
}

impl Default for Transform2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// The greatest common divisor, for reducing a rate to its smallest terms.
///
/// A rate carried as 60000/1000 rather than 60/1 is the same rate and a
/// different pair of numbers, and two of those compare unequal — which would
/// make a show written by one build fail to match a mode reported by
/// another.
const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    // Never zero, so a caller can divide by it. Only reachable when both were
    // zero, which is not a rate.
    if a == 0 {
        1
    } else {
        a
    }
}

/// An exact frame rate expressed as a rational number of frames per second.
///
/// Rational rather than floating point so that 60000/1001 stays exact and the
/// scheduler cannot accumulate drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FrameRate {
    /// Numerator, for example `60000`.
    pub numerator: u32,
    /// Denominator, for example `1001`.
    pub denominator: u32,
}

impl FrameRate {
    /// 60 frames per second, the reference target.
    pub const FPS_60: Self = Self {
        numerator: 60,
        denominator: 1,
    };
    /// 50 frames per second.
    pub const FPS_50: Self = Self {
        numerator: 50,
        denominator: 1,
    };
    /// 30000/1001 frames per second, commonly written 29.97.
    pub const FPS_29_97: Self = Self {
        numerator: 30000,
        denominator: 1001,
    };
    /// 60000/1001 frames per second, commonly written 59.94.
    pub const FPS_59_94: Self = Self {
        numerator: 60000,
        denominator: 1001,
    };

    /// Creates a frame rate.
    #[must_use]
    pub const fn new(numerator: u32, denominator: u32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// Returns the rate as a floating point value, for display only.
    ///
    /// Scheduling maths must use the rational form.
    #[must_use]
    pub fn as_f64(self) -> f64 {
        if self.denominator == 0 {
            return 0.0;
        }
        f64::from(self.numerator) / f64::from(self.denominator)
    }

    /// Returns the rate in millihertz, rounded to nearest.
    ///
    /// The unit display hardware reports its modes in, so this is how an exact
    /// rational rate is matched against a panel's mode list. 60000/1001 becomes
    /// 59 940, which is what a driver calls that mode.
    ///
    /// `None` for a rate that is not one, or one too large to express — neither
    /// of which a display can have.
    #[must_use]
    pub const fn millihertz(self) -> Option<u32> {
        if self.numerator == 0 || self.denominator == 0 {
            return None;
        }
        let scaled = self.numerator as u64 * 1000;
        let denominator = self.denominator as u64;
        let value = (scaled + denominator / 2) / denominator;
        if value > u32::MAX as u64 {
            None
        } else {
            Some(value as u32)
        }
    }

    /// Returns the rate a display driver means by a number of millihertz.
    ///
    /// The inverse of [`millihertz`](Self::millihertz), and not simply
    /// `n / 1000`. A driver reporting 59 940 means 60000/1001 — the rate every
    /// piece of broadcast equipment in the building calls 59.94 — and
    /// 59940/1000 is a *different* rate that happens to round to the same three
    /// decimal places. They differ by a part in a million, which is one frame
    /// every four and a half hours: exactly long enough to be found during a
    /// show rather than during a rehearsal.
    ///
    /// So the six rates the 1001 family actually has are named, and everything
    /// else is the exact thousandth it says it is, reduced.
    ///
    /// `None` for zero, which is not a rate a display can have.
    #[must_use]
    pub fn from_millihertz(millihertz: u32) -> Option<Self> {
        /// What a driver reports, and what it means by it.
        const FAMILY: [(u32, FrameRate); 6] = [
            (23_976, FrameRate::new(24_000, 1001)),
            (29_970, FrameRate::new(30_000, 1001)),
            (47_952, FrameRate::new(48_000, 1001)),
            (59_940, FrameRate::new(60_000, 1001)),
            (119_880, FrameRate::new(120_000, 1001)),
            (239_760, FrameRate::new(240_000, 1001)),
        ];

        if millihertz == 0 {
            return None;
        }

        for (reported, rate) in FAMILY {
            if reported == millihertz {
                return Some(rate);
            }
        }

        let divisor = gcd(millihertz, 1000);
        Some(Self::new(millihertz / divisor, 1000 / divisor))
    }

    /// Returns the exact frame duration in nanoseconds, rounded to nearest.
    #[must_use]
    pub const fn frame_duration_nanos(self) -> Option<u64> {
        if self.numerator == 0 || self.denominator == 0 {
            return None;
        }
        let numerator = 1_000_000_000u128 * self.denominator as u128;
        let denominator = self.numerator as u128;
        Some(((numerator + denominator / 2) / denominator) as u64)
    }

    pub(crate) fn validate(self, path: &str, out: &mut ValidationErrors) {
        if self.numerator == 0 || self.denominator == 0 {
            out.push(ValidationError::new(
                path,
                "numerator and denominator must both be greater than zero",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_a_driver_reports_comes_back_as_the_rate_it_meant() {
        // 59 940 is 60000/1001, not 59940/1000. The two round to the same three
        // decimals and differ by a frame every four and a half hours, which is
        // long enough to be found during a show rather than a rehearsal.
        assert_eq!(
            FrameRate::from_millihertz(59_940),
            Some(FrameRate::new(60_000, 1001))
        );
        assert_eq!(
            FrameRate::from_millihertz(29_970),
            Some(FrameRate::new(30_000, 1001))
        );
        assert_eq!(
            FrameRate::from_millihertz(23_976),
            Some(FrameRate::new(24_000, 1001))
        );
    }

    #[test]
    fn a_whole_rate_comes_back_in_its_smallest_terms() {
        // 60000/1000 is the same rate as 60/1 and a different pair of numbers,
        // and two of those compare unequal — which would make a show written
        // by one build fail to match a mode reported by another.
        assert_eq!(FrameRate::from_millihertz(60_000), Some(FrameRate::FPS_60));
        assert_eq!(FrameRate::from_millihertz(50_000), Some(FrameRate::FPS_50));
        assert_eq!(
            FrameRate::from_millihertz(144_000),
            Some(FrameRate::new(144, 1))
        );
    }

    #[test]
    fn every_rate_a_driver_can_report_goes_out_and_comes_back_the_same() {
        // The property the two directions owe each other: a mode reported,
        // stored in a show and matched against the panel again has to find
        // itself.
        for millihertz in 1_000..=240_000 {
            let rate = FrameRate::from_millihertz(millihertz).expect("a rate");
            assert_eq!(
                rate.millihertz(),
                Some(millihertz),
                "{millihertz} mHz came back as {rate:?}"
            );
        }
    }

    #[test]
    fn nothing_is_not_a_rate() {
        assert_eq!(FrameRate::from_millihertz(0), None);
    }

    #[test]
    fn frame_duration_is_exact_for_integer_rates() {
        assert_eq!(FrameRate::FPS_60.frame_duration_nanos(), Some(16_666_667));
        assert_eq!(FrameRate::FPS_50.frame_duration_nanos(), Some(20_000_000));
    }

    #[test]
    fn frame_duration_handles_fractional_rates() {
        // 1001/30000 s = 33.3666... ms
        assert_eq!(
            FrameRate::FPS_29_97.frame_duration_nanos(),
            Some(33_366_667)
        );
    }

    #[test]
    fn frame_duration_rejects_degenerate_rates() {
        assert_eq!(FrameRate::new(0, 1).frame_duration_nanos(), None);
        assert_eq!(FrameRate::new(60, 0).frame_duration_nanos(), None);
    }

    #[test]
    fn rect_containment_uses_exclusive_edges() {
        let canvas = SizePx::new(1920, 1080);
        assert!(RectPx::new(0, 0, 1920, 1080).fits_within(canvas));
        assert!(!RectPx::new(1, 0, 1920, 1080).fits_within(canvas));
        assert!(!RectPx::new(-1, 0, 100, 100).fits_within(canvas));
    }

    #[test]
    fn large_canvases_do_not_overflow() {
        let wide = SizePx::new(u32::MAX, u32::MAX);
        assert_eq!(
            wide.pixel_count(),
            u64::from(u32::MAX) * u64::from(u32::MAX)
        );
        let rect = RectPx::new(i32::MAX, i32::MAX, u32::MAX, u32::MAX);
        assert_eq!(rect.right(), i64::from(i32::MAX) + i64::from(u32::MAX));
    }

    #[test]
    fn identity_transform_is_neutral() {
        assert!(Transform2D::default().is_identity());
        assert!(!Transform2D {
            x: 1.0,
            ..Transform2D::IDENTITY
        }
        .is_identity());
    }
}
