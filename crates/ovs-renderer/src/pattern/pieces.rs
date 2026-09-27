//! The pieces of a show, drawn on the wall they land on, at their own pixels.
//!
//! The owner, 2026-09-25: "Der Knopf sollte dann auf dem Physikalien Output
//! einmal Black machen und nur noch die Pieces anzeigen lassen. [...] So kann
//! man schnell erkennen bei LED Wänden, ob es wirklich passt und ob es
//! Pixelgenau richtig sitzt."
//!
//! # What it answers that the other two patterns do not
//!
//! `COMPOSITION` tests the mapping and `OUTPUTS` tests the cabling and the
//! mode. This one tests the arithmetic: the rectangle an operator drew in the
//! editor, put on the wall at the exact pixels it was given. On an LED wall a
//! piece one pixel out is invisible in a preview scaled to fit a laptop panel,
//! and unmistakable here.
//!
//! So nothing is scaled and nothing is anti-aliased at the edges: a border is
//! two pixels because two pixels is what was asked for, and a rectangle starts
//! on the pixel its `x` names. A softened edge would be a picture of a
//! rectangle rather than the rectangle.

use ovs_types::geometry::{RectPx, SizePx};

use crate::media::DecodedImage;

use super::font;

/// The widest surface this will draw, in pixels each way.
const MAX_SIDE: u32 = 8192;

/// How thick the border round a piece is, in pixels.
///
/// The owner asked for two. It is not scaled with the surface: what this is
/// for is judging pixels, and a border that grew with the wall would be the
/// one thing on screen that lies about its size.
const BORDER_PX: i64 = 2;

/// Orange, the same one the editor draws a selection in.
const ORANGE: [u8; 3] = [0xff, 0x99, 0x00];
/// Blue, for the pieces that are not the chosen one.
const BLUE: [u8; 3] = [0x1d, 0x4e, 0xd8];

/// One piece to draw, as the render loop hands it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawnPiece {
    /// Where it lands on this output, in that output's pixels.
    pub rect: RectPx,
    /// What it is called.
    pub name: String,
    /// Whether it is the one the operator is working on.
    pub chosen: bool,
}

/// A wall showing its pieces and nothing else.
pub struct PiecePattern {
    size: SizePx,
    pieces: Vec<DrawnPiece>,
    image: DecodedImage,
}

impl std::fmt::Debug for PiecePattern {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PiecePattern")
            .field("size", &self.size)
            .field("pieces", &self.pieces.len())
            .finish_non_exhaustive()
    }
}

impl PiecePattern {
    /// Draws the pieces onto black, once.
    ///
    /// Nothing here moves, so the picture is built in the constructor and
    /// handed out unchanged every frame. That is the whole cost of this mode:
    /// one buffer per wall, written when the mapping changes.
    #[must_use]
    pub fn new(size: SizePx, pieces: Vec<DrawnPiece>) -> Self {
        let size = SizePx::new(
            size.width.clamp(1, MAX_SIDE),
            size.height.clamp(1, MAX_SIDE),
        );
        let width = size.width as usize;
        let height = size.height as usize;

        // Black, and opaque. A transparent background would let whatever the
        // compositor had last put there show through, which is the one thing
        // this mode exists to remove.
        let mut pixels = vec![0_u8; width * height * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 0xff;
        }

        for piece in &pieces {
            let fill = if piece.chosen { ORANGE } else { BLUE };
            draw_piece(&mut pixels, size, piece, fill);
        }

        let image = DecodedImage::new(pixels, size).expect("a buffer the size it was made for");

        Self {
            size,
            pieces,
            image,
        }
    }

    /// Whether this is still the right picture for a wall.
    ///
    /// The pieces are compared by value, so moving one rebuilds and nothing
    /// else does. A rebuild is a few milliseconds of writing bytes; doing it
    /// per frame would be a few milliseconds per frame.
    #[must_use]
    pub fn matches(&self, size: SizePx, pieces: &[DrawnPiece]) -> bool {
        self.size == size && self.pieces == pieces
    }

    /// The picture, which is the same one every time.
    #[must_use]
    pub fn frame(&self) -> &DecodedImage {
        &self.image
    }
}

/// Fills one piece, borders it, and writes its name across the middle.
fn draw_piece(pixels: &mut [u8], size: SizePx, piece: &DrawnPiece, fill: [u8; 3]) {
    let rect = piece.rect;
    let left = i64::from(rect.x);
    let top = i64::from(rect.y);
    let right = left + i64::from(rect.width);
    let bottom = top + i64::from(rect.height);

    // A piece can be placed partly or wholly off its wall -- the editor allows
    // it, because a wall can be re-cut later -- so every write is clipped
    // rather than assumed to land.
    for y in top.max(0)..bottom.min(i64::from(size.height)) {
        for x in left.max(0)..right.min(i64::from(size.width)) {
            let edge = x - left < BORDER_PX
                || right - x <= BORDER_PX
                || y - top < BORDER_PX
                || bottom - y <= BORDER_PX;
            let colour = if edge { ORANGE } else { fill };
            put(pixels, size, x, y, colour);
        }
    }

    write_name(pixels, size, piece);
}

/// Writes a piece's name across the middle of it.
///
/// Bigger than the caption on the other patterns, and centred, because the
/// owner asked for it and because it is read from across a room rather than
/// from in front of a laptop. Sized to the piece so a small one is still
/// readable and a large one does not shout.
fn write_name(pixels: &mut [u8], size: SizePx, piece: &DrawnPiece) {
    // **Folded to what the strokes can write, before it is measured.** The size
    // below is computed from the length, and `ß` becomes two letters — so
    // measuring the typed name and drawing the folded one would centre it wrong.
    let name = font::writable(&piece.name);
    let characters = name.chars().count();
    if characters == 0 || piece.rect.width == 0 || piece.rect.height == 0 {
        return;
    }

    let advance = font::ADVANCE + 0.14;
    // Whichever is the binding constraint: the height of the piece, or the
    // width once the whole name is laid out along it.
    let by_height = f64::from(piece.rect.height) * 0.42;
    let by_width = f64::from(piece.rect.width) * 0.82 / (advance as f64 * characters as f64);
    let height = by_height.min(by_width);
    if height < 5.0 {
        // Too small to be read is worse than absent: it draws as a smear over
        // the fill and hides the thing being judged.
        return;
    }

    let step = advance as f64 * height;
    let line = step * characters as f64 - (0.14 * height);
    let centre_x = f64::from(piece.rect.x) + f64::from(piece.rect.width) / 2.0;
    let centre_y = f64::from(piece.rect.y) + f64::from(piece.rect.height) / 2.0;

    let mut pen = centre_x - line / 2.0;
    let baseline = centre_y + height / 2.0;
    let stroke = (height * 0.14).max(1.5);

    for character in name.chars() {
        for polyline in font::glyph(character) {
            for pair in polyline.windows(2) {
                let from = (
                    pen + f64::from(pair[0].0) * height,
                    baseline - height + f64::from(pair[0].1) * height,
                );
                let to = (
                    pen + f64::from(pair[1].0) * height,
                    baseline - height + f64::from(pair[1].1) * height,
                );
                stroke_segment(pixels, size, from, to, stroke);
            }
        }
        pen += step;
    }
}

/// Draws one straight white stroke, clipped to the surface.
fn stroke_segment(pixels: &mut [u8], size: SizePx, from: (f64, f64), to: (f64, f64), width: f64) {
    let half = width / 2.0;
    let left = (from.0.min(to.0) - half - 1.0).floor().max(0.0) as i64;
    let right = (from.0.max(to.0) + half + 1.0)
        .ceil()
        .min(f64::from(size.width)) as i64;
    let top = (from.1.min(to.1) - half - 1.0).floor().max(0.0) as i64;
    let bottom = (from.1.max(to.1) + half + 1.0)
        .ceil()
        .min(f64::from(size.height)) as i64;

    for y in top..bottom {
        for x in left..right {
            let distance = distance_to_segment((x as f64 + 0.5, y as f64 + 0.5), from, to);
            let alpha = (half + 0.5 - distance).clamp(0.0, 1.0);
            if alpha <= 0.0 {
                continue;
            }
            blend_white(pixels, size, x, y, alpha);
        }
    }
}

/// How far a point is from a line segment.
fn distance_to_segment(point: (f64, f64), from: (f64, f64), to: (f64, f64)) -> f64 {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx * dx + dy * dy;
    let along = if length <= f64::EPSILON {
        0.0
    } else {
        (((point.0 - from.0) * dx + (point.1 - from.1) * dy) / length).clamp(0.0, 1.0)
    };
    let nearest = (from.0 + along * dx, from.1 + along * dy);
    ((point.0 - nearest.0).powi(2) + (point.1 - nearest.1).powi(2)).sqrt()
}

/// Writes one opaque pixel, if it is on the surface.
fn put(pixels: &mut [u8], size: SizePx, x: i64, y: i64, colour: [u8; 3]) {
    let Some(at) = offset(size, x, y) else {
        return;
    };
    pixels[at] = colour[0];
    pixels[at + 1] = colour[1];
    pixels[at + 2] = colour[2];
    pixels[at + 3] = 0xff;
}

/// Mixes white into one pixel, if it is on the surface.
fn blend_white(pixels: &mut [u8], size: SizePx, x: i64, y: i64, alpha: f64) {
    let Some(at) = offset(size, x, y) else {
        return;
    };
    for channel in 0..3 {
        let under = f64::from(pixels[at + channel]);
        pixels[at + channel] = (under + (255.0 - under) * alpha) as u8;
    }
    pixels[at + 3] = 0xff;
}

/// Where a pixel is in the buffer, or `None` when it is off the surface.
fn offset(size: SizePx, x: i64, y: i64) -> Option<usize> {
    if x < 0 || y < 0 || x >= i64::from(size.width) || y >= i64::from(size.height) {
        return None;
    }
    Some((y as usize * size.width as usize + x as usize) * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall() -> SizePx {
        SizePx::new(64, 32)
    }

    fn at(pixels: &[u8], size: SizePx, x: u32, y: u32) -> [u8; 3] {
        let at = (y as usize * size.width as usize + x as usize) * 4;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    }

    fn drawn(rect: RectPx, chosen: bool) -> DrawnPiece {
        DrawnPiece {
            rect,
            name: String::new(),
            chosen,
        }
    }

    #[test]
    fn a_wall_with_no_pieces_is_black() {
        let pattern = PiecePattern::new(wall(), Vec::new());
        let pixels = pattern.frame().pixels();
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[..3] == [0, 0, 0]),
            "the point of this mode is that everything the show was drawing is gone"
        );
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 0xff),
            "a transparent background would show whatever the compositor had last"
        );
    }

    #[test]
    fn a_piece_starts_on_the_pixel_it_names() {
        let rect = RectPx::new(10, 4, 20, 12);
        let pattern = PiecePattern::new(wall(), vec![drawn(rect, false)]);
        let pixels = pattern.frame().pixels();

        assert_eq!(
            at(pixels, wall(), 9, 4),
            [0, 0, 0],
            "the pixel before it is not part of it"
        );
        assert_eq!(
            at(pixels, wall(), 10, 4),
            ORANGE,
            "the pixel it names is its border"
        );
        assert_eq!(
            at(pixels, wall(), 29, 4),
            ORANGE,
            "and the last pixel inside its width is too"
        );
        assert_eq!(
            at(pixels, wall(), 30, 4),
            [0, 0, 0],
            "one past the end is off it"
        );
    }

    #[test]
    fn the_border_is_two_pixels_whatever_the_piece_is() {
        let pattern = PiecePattern::new(wall(), vec![drawn(RectPx::new(4, 4, 40, 20), false)]);
        let pixels = pattern.frame().pixels();

        assert_eq!(at(pixels, wall(), 4, 12), ORANGE, "first border pixel");
        assert_eq!(at(pixels, wall(), 5, 12), ORANGE, "second border pixel");
        assert_eq!(at(pixels, wall(), 6, 12), BLUE, "and then the fill");
    }

    #[test]
    fn the_chosen_piece_is_orange_through_and_the_rest_are_blue() {
        let pattern = PiecePattern::new(
            wall(),
            vec![
                drawn(RectPx::new(2, 2, 12, 12), false),
                drawn(RectPx::new(40, 2, 12, 12), true),
            ],
        );
        let pixels = pattern.frame().pixels();

        assert_eq!(
            at(pixels, wall(), 8, 8),
            BLUE,
            "not the one being worked on"
        );
        assert_eq!(at(pixels, wall(), 46, 8), ORANGE, "the one that is");
    }

    #[test]
    fn a_piece_hanging_off_the_wall_is_clipped_rather_than_refused() {
        // The editor allows it, because a wall can be re-cut later. What must
        // not happen is a panic or a write past the buffer.
        let pattern = PiecePattern::new(
            wall(),
            vec![
                drawn(RectPx::new(-10, -10, 30, 30), false),
                drawn(RectPx::new(60, 28, 40, 40), true),
            ],
        );
        assert_eq!(pattern.frame().pixels().len(), 64 * 32 * 4);
    }

    #[test]
    fn a_piece_name_is_written_in_letters_the_font_has() {
        // **The reported fault.** A piece called `Piece 1` was drawn as
        // `Pi   1`: the stroke font is upper case, and `e` and `c` drew nothing.
        // Counted as ink rather than compared to a picture, because what matters
        // is that the letters arrive — a stroke font's exact pixels are its own
        // business and would make this a test of the drawing.
        // Its own surface, not `wall()`: that one is 64x32, and a piece big
        // enough to write a name inside would be entirely off it.
        let size = SizePx::new(1000, 400);
        let rect = RectPx {
            x: 40,
            y: 40,
            width: 900,
            height: 300,
        };

        let ink = |name: &str| {
            let mut pixels = vec![0_u8; size.width as usize * size.height as usize * 4];
            let piece = DrawnPiece {
                rect,
                name: name.to_owned(),
                chosen: false,
            };
            super::draw_piece(&mut pixels, size, &piece, BLUE);
            // White strokes over a blue fill, so a white pixel is a letter.
            (0..pixels.len() / 4)
                .filter(|at| {
                    let from = at * 4;
                    pixels[from] > 200 && pixels[from + 1] > 200 && pixels[from + 2] > 200
                })
                .count()
        };

        let two_letters = ink("Pi");
        let whole_word = ink("Piece 1");
        assert!(
            whole_word > two_letters * 2,
            "'Piece 1' drew {whole_word} pixels of letter and 'Pi' drew {two_letters}: \
             the lower-case letters are still being dropped"
        );

        // A name in German, and one long enough that the size has to give way.
        assert!(ink("Bühne") > 0, "an umlaut lost the whole name");
        assert!(
            ink("A very long piece name that has to be made smaller to fit") > 0,
            "a long name drew nothing at all rather than being set smaller"
        );
    }

    #[test]
    fn a_wall_is_redrawn_when_a_piece_moves_and_not_otherwise() {
        let one = vec![drawn(RectPx::new(2, 2, 10, 10), false)];
        let pattern = PiecePattern::new(wall(), one.clone());

        assert!(pattern.matches(wall(), &one), "nothing changed");
        assert!(
            !pattern.matches(wall(), &[drawn(RectPx::new(3, 2, 10, 10), false)]),
            "a piece that moved one pixel is a different picture, which is the whole point"
        );
        assert!(
            !pattern.matches(SizePx::new(32, 32), &one),
            "a wall that changed mode needs a new buffer"
        );
    }
}
