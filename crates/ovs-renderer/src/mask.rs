//! The composition with everything no wall receives taken out.
//!
//! A second way of looking at the same picture. The composition surface shows
//! what the show *is*; this shows the part of it that is actually mapped onto a
//! piece and therefore actually reaches an LED processor. On a rig whose
//! composition is larger than the sum of its walls — which is the normal case,
//! because a composition is authored at a convenient size and the walls are
//! whatever the venue has — the difference between the two is the content
//! nobody in the room will ever see.
//!
//! Scoped by the owner (2026-09-23): the composition, the composition masked by
//! pieces, and the outputs, in that order of importance. There is deliberately
//! no per-piece and no per-layer preview: every extra surface is another
//! readback and another encode on a machine that has to hold 60 fps first.
//!
//! # Why this is pure
//!
//! Which part of a picture survives the mapping is arithmetic, and getting it
//! wrong is the failure an operator reads as "my content is being cut off" when
//! it is not, or misses when it is. A GPU cannot be checked in CI; this can.

use ovs_types::geometry::SizePx;

/// One shape of the composition that reaches a wall.
///
/// Four corners rather than four edges, because a piece's input end can be
/// taken at an angle (owner, 2026-09-23) and a turned rectangle is not a
/// rectangle. Drawn as the axis-aligned box it came from, this surface showed
/// horizontal bars where the mapping has diagonal ones — which is the one
/// thing it exists to be trusted about.
///
/// In composition texture space, where `0.0` is left and top and `1.0` is
/// right and bottom — the same coordinates
/// [`MappedPiece`](crate::pieces::MappedPiece) already carries, so the mask is
/// built from what the renderer really composed rather than from a second
/// reading of the show that could disagree with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    /// The corners, going round the shape: top-left, top-right, bottom-right,
    /// bottom-left of the rectangle this was before it was turned.
    pub corners: [[f32; 2]; 4],
}

impl Region {
    /// The whole composition.
    ///
    /// What an output with no piece mapping shows, which the renderer treats as
    /// "everything" rather than "nothing" (`rebuild_mappings`). The mask has to
    /// agree with that, or a single-wall setup nobody has reached the mapping
    /// screen for would preview as entirely black while the wall shows the show.
    pub const WHOLE: Self = Self {
        corners: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
    };

    /// An upright rectangle.
    #[must_use]
    pub const fn rectangle(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            corners: [[left, top], [right, top], [right, bottom], [left, bottom]],
        }
    }

    /// A rectangle put through the turn its piece takes it at.
    ///
    /// `matrix` is row-major `[m00, m01, m10, m11]` and `pivot` the point it
    /// turns about, both in texture space — which is exactly what
    /// [`SourceTurn`](crate::scene::SourceTurn) hands the shader. Taken as the
    /// numbers rather than as the type so that this module stays arithmetic
    /// with nothing in it that needs a GPU; passing the *same* numbers is what
    /// makes the mask and the wall agree by construction rather than by two
    /// implementations happening to match.
    #[must_use]
    pub fn turned(
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        matrix: [f32; 4],
        pivot: [f32; 2],
    ) -> Self {
        let turn = |x: f32, y: f32| {
            let dx = x - pivot[0];
            let dy = y - pivot[1];
            [
                pivot[0] + matrix[0] * dx + matrix[1] * dy,
                pivot[1] + matrix[2] * dx + matrix[3] * dy,
            ]
        };
        Self {
            corners: [
                turn(left, top),
                turn(right, top),
                turn(right, bottom),
                turn(left, bottom),
            ],
        }
    }

    /// The corners in pixels of a picture of this size.
    ///
    /// Brought into a finite range on the way. A piece may hang off its output
    /// deliberately (`pieces.rs`) and a show file is under no obligation to
    /// be sensible, so a coordinate can arrive as an infinity — and an infinite
    /// corner makes the arithmetic that finds where an edge crosses a line
    /// produce a NaN rather than a very large number. One picture's width
    /// either side is as far as any of this can matter; beyond that the span is
    /// clamped to the picture anyway.
    fn in_pixels(self, size: SizePx) -> [[f32; 2]; 4] {
        let width = size.width as f32;
        let height = size.height as f32;
        let finite = |value: f32, extent: f32| {
            if value.is_nan() {
                // No nearer edge to clamp to, and the old arithmetic read one
                // as the origin.
                0.0
            } else {
                value.clamp(-extent, extent * 2.0)
            }
        };
        self.corners.map(|corner| {
            [
                finite(corner[0] * width, width),
                finite(corner[1] * height, height),
            ]
        })
    }
}

/// Blacks out every pixel of an RGBA picture that no region keeps.
///
/// The picture is the composition already shrunk to preview size, so the
/// regions are scaled into it rather than the other way round: masking costs
/// one pass over a preview-sized buffer and no GPU work at all.
///
/// Black rather than dimmed, because the owner asked for the pieces to be the
/// only thing visible. A dimmed surround would be easier to orient in and would
/// also be a picture in which "mapped" and "not mapped" differ by a brightness,
/// which is exactly the judgement an operator is using this surface to avoid
/// having to make.
///
/// Alpha is left opaque. What comes out of here is encoded as H.264, which has
/// no alpha, and saved as a PNG, which has — and a masked area that arrived as
/// transparent would be drawn over whatever the client happened to put behind
/// it.
///
/// Does nothing to a picture whose length does not match its stated size: a
/// buffer that does not describe itself correctly is one this must not index
/// into.
pub fn keep_only(pixels: &mut [u8], size: SizePx, regions: &[Region]) {
    let width = size.width as usize;
    let height = size.height as usize;
    if pixels.len() != width.saturating_mul(height).saturating_mul(4) || width == 0 || height == 0 {
        return;
    }

    // The corners in pixels, once. Each row then asks each shape where it
    // crosses that line, which is the same question an upright rectangle
    // answers with its own two edges — so there is one path here rather than
    // one for turned pieces and one for the rest.
    let shapes: Vec<[[f32; 2]; 4]> = regions
        .iter()
        .map(|region| region.in_pixels(size))
        .collect();

    // One allocation for the whole picture rather than one per row: a preview
    // is 720 rows and this runs several times a second per watcher.
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(shapes.len());

    for y in 0..height {
        let row = &mut pixels[y * width * 4..(y + 1) * width * 4];

        // The middle of the row, so a shape that starts halfway down a pixel
        // takes that pixel or not by where most of it lies.
        let line = y as f32 + 0.5;
        spans.clear();
        spans.extend(
            shapes
                .iter()
                .filter_map(|corners| span_at(corners, line, width)),
        );

        if spans.is_empty() {
            black(row, 0, width);
            continue;
        }

        // Sorted so that overlapping pieces — two walls sharing a seam, which
        // is a normal mapping — merge into one kept span instead of the second
        // one blacking out part of the first.
        spans.sort_unstable();
        let mut cursor = 0;
        for &(left, right) in &spans {
            if left > cursor {
                black(row, cursor, left);
            }
            cursor = cursor.max(right);
        }
        if cursor < width {
            black(row, cursor, width);
        }
    }
}

/// Where one convex shape crosses one horizontal line, in whole pixels.
///
/// A convex shape meets a line in at most one interval, so this is the same
/// `(left, right)` an upright rectangle contributed before and merges with its
/// neighbours the same way.
///
/// The edge test is half-open in `y`, which is what keeps a corner shared by
/// two edges from being counted twice and leaves a horizontal edge out
/// altogether — both of which would otherwise widen the span by whatever that
/// edge spans.
///
/// Returns `None` for a line the shape does not reach, and for one where it is
/// too thin to round to a pixel: an empty span every row is work for nothing.
fn span_at(corners: &[[f32; 2]; 4], line: f32, width: usize) -> Option<(usize, usize)> {
    let mut lowest = f32::INFINITY;
    let mut highest = f32::NEG_INFINITY;

    for edge in 0..4 {
        let from = corners[edge];
        let to = corners[(edge + 1) % 4];
        if (from[1] <= line) == (to[1] <= line) {
            continue;
        }
        let along = (line - from[1]) / (to[1] - from[1]);
        let x = from[0] + (to[0] - from[0]) * along;
        lowest = lowest.min(x);
        highest = highest.max(x);
    }

    if !lowest.is_finite() || !highest.is_finite() {
        return None;
    }

    let left = to_pixel(lowest, width);
    let right = to_pixel(highest, width);
    (right > left).then_some((left, right))
}

/// Rounds a position in pixels to a whole one inside the picture.
///
/// Rounded rather than truncated, and the same way for every edge, so two
/// pieces that abut in the composition abut in the mask as well instead of
/// leaving a one-pixel black line between them.
///
/// Anything outside the picture is clamped to the nearer edge, which includes
/// an infinity: a piece may hang off its output deliberately, and a show
/// file is under no obligation to be sensible. A NaN has no nearer edge, so it
/// is taken as zero.
fn to_pixel(value: f32, extent: usize) -> usize {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if value >= extent as f32 {
        return extent;
    }
    // `as` truncates, and the value is known to be inside the picture here, so
    // rounding first is what puts an edge on the nearer pixel boundary.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rounded = f64::from(value).round() as usize;
    rounded.min(extent)
}

/// Blacks out one run of pixels in a row, leaving them opaque.
fn black(row: &mut [u8], from: usize, to: usize) {
    for pixel in row[from * 4..to * 4].as_chunks_mut::<4>().0 {
        pixel[0] = 0;
        pixel[1] = 0;
        pixel[2] = 0;
        pixel[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture in which every pixel is the same recognisable colour.
    fn picture(size: SizePx) -> Vec<u8> {
        vec![200; (size.width * size.height * 4) as usize]
    }

    /// Every pixel of a picture, as four bytes each.
    fn pixels_of(pixels: &[u8]) -> impl Iterator<Item = &[u8; 4]> {
        pixels.as_chunks::<4>().0.iter()
    }

    /// Whether the pixel at `(x, y)` survived.
    fn kept(pixels: &[u8], size: SizePx, x: u32, y: u32) -> bool {
        let at = ((y * size.width + x) * 4) as usize;
        pixels[at] != 0
    }

    #[test]
    fn nothing_mapped_leaves_nothing_visible() {
        // A rig with no outputs, or every output faulted. Black is the honest
        // answer: none of this composition is reaching a wall.
        let size = SizePx::new(8, 4);
        let mut pixels = picture(size);

        keep_only(&mut pixels, size, &[]);

        assert!(pixels_of(&pixels).all(|pixel| pixel[..3] == [0, 0, 0]));
        assert!(
            pixels_of(&pixels).all(|pixel| pixel[3] == 255),
            "opaque, not transparent"
        );
    }

    #[test]
    fn an_unmapped_output_keeps_the_whole_composition() {
        let size = SizePx::new(8, 4);
        let mut pixels = picture(size);

        keep_only(&mut pixels, size, &[Region::WHOLE]);

        assert!(pixels_of(&pixels).all(|pixel| pixel[0] == 200));
    }

    #[test]
    fn one_piece_keeps_its_own_rectangle_and_nothing_else() {
        // The left half of a picture, which is what one wall of a two-wall rig
        // takes.
        let size = SizePx::new(8, 4);
        let mut pixels = picture(size);

        keep_only(&mut pixels, size, &[Region::rectangle(0.0, 0.0, 0.5, 1.0)]);

        for y in 0..size.height {
            for x in 0..4 {
                assert!(kept(&pixels, size, x, y), "kept at {x},{y}");
            }
            for x in 4..8 {
                assert!(!kept(&pixels, size, x, y), "blacked at {x},{y}");
            }
        }
    }

    #[test]
    fn two_pieces_side_by_side_leave_no_seam_between_them() {
        // The failure this exists to catch: two walls abutting in the
        // composition previewing with a black line down the middle, which an
        // operator would read as a mapping gap that is not there.
        let size = SizePx::new(8, 2);
        let mut pixels = picture(size);

        keep_only(
            &mut pixels,
            size,
            &[
                Region::rectangle(0.0, 0.0, 0.5, 1.0),
                Region::rectangle(0.5, 0.0, 1.0, 1.0),
            ],
        );

        assert!(pixels_of(&pixels).all(|pixel| pixel[0] == 200));
    }

    #[test]
    fn pieces_arriving_out_of_order_still_merge() {
        // The renderer hands these over per output, in whatever order the
        // outputs enumerate. Sorting is not cosmetic: unsorted, the second
        // region would black out the run before it.
        let size = SizePx::new(8, 1);
        let mut pixels = picture(size);

        keep_only(
            &mut pixels,
            size,
            &[
                Region::rectangle(0.5, 0.0, 1.0, 1.0),
                Region::rectangle(0.0, 0.0, 0.5, 1.0),
            ],
        );

        assert!(pixels_of(&pixels).all(|pixel| pixel[0] == 200));
    }

    #[test]
    fn overlapping_pieces_keep_their_union() {
        // Two walls sharing a seam deliberately, which is a normal mapping.
        let size = SizePx::new(8, 1);
        let mut pixels = picture(size);

        keep_only(
            &mut pixels,
            size,
            &[
                Region::rectangle(0.0, 0.0, 0.75, 1.0),
                Region::rectangle(0.25, 0.0, 1.0, 1.0),
            ],
        );

        assert!(pixels_of(&pixels).all(|pixel| pixel[0] == 200));
    }

    #[test]
    fn a_band_keeps_only_its_own_rows() {
        let size = SizePx::new(4, 8);
        let mut pixels = picture(size);

        keep_only(&mut pixels, size, &[Region::rectangle(0.0, 0.5, 1.0, 1.0)]);

        for y in 0..4 {
            assert!(!kept(&pixels, size, 0, y), "blacked at row {y}");
        }
        for y in 4..8 {
            assert!(kept(&pixels, size, 0, y), "kept at row {y}");
        }
    }

    #[test]
    fn a_region_off_the_edge_is_clamped_rather_than_indexed() {
        // A piece may hang off its output deliberately (`pieces.rs`), and a
        // show file is not obliged to be sensible either.
        let size = SizePx::new(4, 4);
        let mut pixels = picture(size);

        keep_only(
            &mut pixels,
            size,
            &[Region::rectangle(-2.0, -2.0, 9.0, f32::INFINITY)],
        );

        assert!(pixels_of(&pixels).all(|pixel| pixel[0] == 200));
    }

    #[test]
    fn a_region_that_rounds_to_nothing_keeps_nothing() {
        let size = SizePx::new(8, 4);
        let mut pixels = picture(size);

        keep_only(&mut pixels, size, &[Region::rectangle(0.5, 0.0, 0.5, 1.0)]);

        assert!(pixels_of(&pixels).all(|pixel| pixel[0] == 0));
    }

    /// A quarter turn clockwise about the middle of the composition.
    ///
    /// The matrix `SourceTurn::about` builds for a square picture, written out
    /// here so the test says what it is turning by rather than calling the
    /// thing it is checking.
    const QUARTER: [f32; 4] = [0.0, -1.0, 1.0, 0.0];

    #[test]
    fn a_turned_piece_keeps_the_shape_it_actually_takes() {
        // The owner's report, 2026-09-23: "Man soll da die Input Slices sehen.
        // Die sind aktuell rotiert und das passiert da gar nicht."
        //
        // A wide band across the middle of the composition, turned a quarter.
        // What it takes is a *tall* band down the middle — and what this
        // surface showed was the wide one it was written as.
        let size = SizePx::new(8, 8);
        let mut pixels = picture(size);

        keep_only(
            &mut pixels,
            size,
            &[Region::turned(0.0, 0.25, 1.0, 0.75, QUARTER, [0.5, 0.5])],
        );

        // Down the middle, kept.
        for y in 0..8 {
            for x in 2..6 {
                assert!(kept(&pixels, size, x, y), "kept at {x},{y}");
            }
        }
        // Either side of it, gone — which is where the unturned band was.
        for y in 0..8 {
            for x in [0, 1, 6, 7] {
                assert!(!kept(&pixels, size, x, y), "blacked at {x},{y}");
            }
        }
    }

    #[test]
    fn a_piece_turned_off_the_square_keeps_a_diamond() {
        // Half a turn is a rectangle again and proves nothing about the
        // scanline. An eighth is the case the old code could not express at
        // all: a shape whose width changes down the picture.
        let size = SizePx::new(16, 16);
        let mut pixels = picture(size);

        // A square in the middle, turned an eighth about its own centre: a
        // diamond, widest across its own middle and pointed at top and bottom.
        let eighth = std::f32::consts::FRAC_PI_4;
        let (sin, cos) = eighth.sin_cos();
        keep_only(
            &mut pixels,
            size,
            &[Region::turned(
                0.25,
                0.25,
                0.75,
                0.75,
                [cos, -sin, sin, cos],
                [0.5, 0.5],
            )],
        );

        // The middle row is the widest part of a diamond and the rows at its
        // points are the narrowest, which is the whole of what a turned shape
        // is and what a box cannot be.
        let across = |y: u32| (0..16).filter(|&x| kept(&pixels, size, x, y)).count();
        assert!(
            across(8) > across(2),
            "a diamond is wider across its middle: {} against {}",
            across(8),
            across(2),
        );
        assert_eq!(across(0), 0, "and reaches neither the top");
        assert_eq!(across(15), 0, "nor the bottom");
    }

    #[test]
    fn an_upright_piece_keeps_exactly_what_it_did_before_shapes_could_turn() {
        // The turn arrived as a matrix every piece carries, and the identity
        // has to come out where the old arithmetic did — to the pixel, or every
        // mapping in the building moves by one.
        let size = SizePx::new(8, 4);
        let mut turned = picture(size);
        let mut upright = picture(size);

        keep_only(&mut upright, size, &[Region::rectangle(0.0, 0.0, 0.5, 1.0)]);
        keep_only(
            &mut turned,
            size,
            &[Region::turned(
                0.0,
                0.0,
                0.5,
                1.0,
                [1.0, 0.0, 0.0, 1.0],
                [0.25, 0.5],
            )],
        );

        assert_eq!(turned, upright);
    }

    #[test]
    fn a_picture_that_does_not_describe_itself_is_left_alone() {
        // Better than panicking in the render loop over a readback that came
        // back short.
        let size = SizePx::new(8, 4);
        let mut pixels = vec![200; 16];

        keep_only(&mut pixels, size, &[]);

        assert!(pixels.iter().all(|byte| *byte == 200));
    }
}
