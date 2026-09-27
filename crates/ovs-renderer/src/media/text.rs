//! Setting one line of characters into a picture.
//!
//! The strokes come from [`crate::pattern::font`], which was drawn for the
//! service test card. Reusing it rather than adding a font is the same decision
//! that module already argues for, and it holds twice over here: a clock is
//! digits and a colon, which is exactly what a test card's lettering already
//! is, and two tables of the same thirty-eight characters would be two sets of
//! letters that could drift apart.
//!
//! # What this does that the test card's own lettering does not
//!
//! The caption on a test card is always white, blended over a disc, and never
//! allowed to punch a hole in it. A clock is whatever colour an operator chose,
//! over whatever background they chose, filling a wall. The two blends are
//! different rules, so the drawing is written twice and the *letters* are
//! written once.
//!
//! # Nothing here allocates
//!
//! A line is passed as bytes the caller already has, and the strokes are written
//! straight into the picture. That matters because the caller is redrawing into
//! a buffer it holds rather than making a new one (house rule §5).

use ovs_types::geometry::SizePx;

use crate::pattern::font;

/// What to draw for a character the font has not got.
///
/// The font has thirty-eight characters and no lower case, and which of these
/// is right depends entirely on what is being set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Missing {
    /// Nothing at all.
    ///
    /// What a test card wants. Every string it sets is one this renderer built
    /// out of characters the font has, so a box could only ever be a fault the
    /// card invented — and a test card that invents faults is worse than no
    /// test card.
    #[default]
    Nothing,
    /// A hollow box.
    ///
    /// What a caption wants. The words are whatever an operator typed, so a
    /// character the font has not got is *their* mistake to see, and seeing it
    /// in the preview is the whole difference between a caption that is wrong
    /// and a caption that is wrong at the get-in.
    Box,
}

/// How a line is set.
#[derive(Debug, Clone, Copy)]
pub struct Pen {
    /// Cap height, in pixels.
    pub cap_px: f32,
    /// Stroke width, as a fraction of the cap height.
    pub weight: f32,
    /// Extra space after each character, as a fraction of the cap height.
    pub tracking: f32,
    /// The RGBA the strokes are drawn in.
    pub colour: [u8; 4],
    /// What a character the font has not got looks like.
    pub missing: Missing,
}

/// How wide a line of `count` characters is, in cap heights.
///
/// The font is monospaced, so this is arithmetic rather than a measurement —
/// which is the property a clock depends on: the digits do not move sideways
/// when a `1` becomes a `2`.
#[must_use]
pub fn line_width(count: usize, tracking: f32) -> f32 {
    ((font::ADVANCE + tracking) * count as f32 - tracking).max(f32::EPSILON)
}

/// Writes one line of ASCII, centred on a point in the picture.
///
/// The line is centred on its own box rather than on its ink, so a string whose
/// first character is a space — which is how a countdown reserves room for the
/// minus sign it may need — keeps its digits in exactly the same columns as one
/// whose first character is a digit.
pub fn write_centred(pixels: &mut [u8], size: SizePx, text: &[u8], pen: &Pen, centre: (f32, f32)) {
    let cap = pen.cap_px;
    if cap <= 0.0 || text.is_empty() {
        return;
    }

    let advance = (font::ADVANCE + pen.tracking) * cap;
    let mut left = centre.0 - line_width(text.len(), pen.tracking) * cap / 2.0;
    // `y = 0` is the cap line in a glyph's own box and `y = 1` is the baseline,
    // so centring the box vertically means putting the cap line half a cap
    // height above the middle.
    let top = centre.1 - cap / 2.0;
    let stroke = pen.weight * cap;

    for byte in text {
        let strokes = match (font::glyph(char::from(*byte)), pen.missing) {
            ([], Missing::Box) if *byte != b' ' => font::NOTDEF,
            (strokes, _) => strokes,
        };
        for polyline in strokes {
            for pair in polyline.windows(2) {
                let from = (left + pair[0].0 * cap, top + pair[0].1 * cap);
                let to = (left + pair[1].0 * cap, top + pair[1].1 * cap);
                stroke_segment(pixels, size, from, to, stroke, pen.colour);
            }
        }
        left += advance;
    }
}

/// Draws one straight stroke, softened at its edges.
///
/// Softened because a clock is read from across a room and then photographed for
/// a rehearsal note: hard-edged diagonal strokes crawl when the picture is
/// rescaled, and one pixel of coverage at the edge costs nothing to produce.
fn stroke_segment(
    pixels: &mut [u8],
    size: SizePx,
    from: (f32, f32),
    to: (f32, f32),
    width: f32,
    colour: [u8; 4],
) {
    let canvas_width = size.width as usize;
    let canvas_height = size.height as usize;
    let half = width / 2.0;

    // Only the pixels the stroke can reach, which is what keeps the cost of a
    // line of digits proportional to the digits rather than to the wall.
    let left = (from.0.min(to.0) - half - 1.0).floor().max(0.0) as usize;
    let right = ((from.0.max(to.0) + half + 1.0).ceil().max(0.0) as usize).min(canvas_width);
    let top = (from.1.min(to.1) - half - 1.0).floor().max(0.0) as usize;
    let bottom = ((from.1.max(to.1) + half + 1.0).ceil().max(0.0) as usize).min(canvas_height);

    for y in top..bottom {
        for x in left..right {
            let distance = crate::media::geometry::distance_to_segment(
                (x as f32 + 0.5, y as f32 + 0.5),
                from,
                to,
            );
            let coverage = (half + 0.5 - distance).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }

            let at = (y * canvas_width + x) * 4;
            for channel in 0..4 {
                let under = f32::from(pixels[at + channel]);
                let over = f32::from(colour[channel]);
                pixels[at + channel] = (under + (over - under) * coverage).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(size: SizePx) -> Vec<u8> {
        vec![0; size.width as usize * size.height as usize * 4]
    }

    fn ink(pixels: &[u8]) -> usize {
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] > 0)
            .count()
    }

    fn pen(cap_px: f32) -> Pen {
        Pen {
            missing: Missing::Nothing,
            cap_px,
            weight: 0.12,
            tracking: 0.08,
            colour: [255, 255, 255, 255],
        }
    }

    #[test]
    fn a_line_of_text_puts_ink_on_the_picture() {
        let size = SizePx::new(128, 64);
        let mut pixels = canvas(size);
        write_centred(&mut pixels, size, b"12:34", &pen(24.0), (64.0, 32.0));

        assert!(ink(&pixels) > 0, "nothing was written");
    }

    #[test]
    fn the_same_line_of_text_is_the_same_picture_every_time() {
        // A clock redrawn once a second must not shimmer between two renderings
        // of the same second, which is what a wall reads as a fault.
        let size = SizePx::new(128, 64);
        let mut first = canvas(size);
        let mut second = canvas(size);
        write_centred(&mut first, size, b"12:34", &pen(24.0), (64.0, 32.0));
        write_centred(&mut second, size, b"12:34", &pen(24.0), (64.0, 32.0));

        assert_eq!(first, second);
    }

    /// Every pixel from column `from` rightwards.
    ///
    /// What the tests below compare: two lines that end in the same characters
    /// must put them in exactly the same place, which is the whole reason this
    /// font is monospaced.
    fn right_of(pixels: &[u8], size: SizePx, from: usize) -> Vec<u8> {
        let width = size.width as usize;
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(index, _)| index % width >= from)
            .flat_map(|(_, pixel)| *pixel)
            .collect()
    }

    #[test]
    fn a_digit_stays_in_its_column_whatever_digit_it_is() {
        // The property a clock depends on: the minutes do not shuffle sideways
        // when the hour ticks from 9 to 10.
        let size = SizePx::new(256, 64);
        let minutes = |text: &[u8]| {
            let mut pixels = canvas(size);
            write_centred(&mut pixels, size, text, &pen(24.0), (128.0, 32.0));
            // Past the colon, which is the half of the line that is the same in
            // both and so must be drawn identically in both.
            right_of(&pixels, size, 140)
        };

        assert_eq!(minutes(b"11:11"), minutes(b"88:11"));
    }

    #[test]
    fn a_leading_space_holds_the_column_a_minus_sign_will_need() {
        // How a countdown reserves room for the sign it may acquire at zero: the
        // digits must not jump sideways the moment it appears.
        let size = SizePx::new(256, 64);
        let digits = |text: &[u8]| {
            let mut pixels = canvas(size);
            write_centred(&mut pixels, size, text, &pen(20.0), (128.0, 32.0));
            // Past the sign's own column, which is the only part that differs.
            right_of(&pixels, size, 116)
        };

        assert_eq!(digits(b" 05:00"), digits(b"-05:00"));
    }

    #[test]
    fn a_line_too_wide_for_the_picture_is_clipped_rather_than_written_outside_it() {
        // A composition can be any shape, and writing past the end of the buffer
        // would be a panic on the render thread.
        let size = SizePx::new(32, 32);
        let mut pixels = canvas(size);
        write_centred(&mut pixels, size, b"88:88:88", &pen(200.0), (16.0, 16.0));

        assert_eq!(pixels.len(), 32 * 32 * 4, "the buffer is the size it was");
    }

    #[test]
    fn a_character_the_font_does_not_know_leaves_its_column_empty() {
        // The font's own rule: a missing glyph draws nothing rather than a box.
        let size = SizePx::new(64, 32);
        let mut pixels = canvas(size);
        write_centred(&mut pixels, size, b"   ", &pen(16.0), (32.0, 16.0));

        assert_eq!(ink(&pixels), 0);
    }
}
