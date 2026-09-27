//! Pieces: mapping rectangles of the composition onto physical outputs.
//!
//! The right-hand half of the render path:
//!
//! ```text
//! Composition → Pieces → Output Canvases → Physical GPU Outputs
//! ```
//!
//! A piece takes a rectangle of the composition and puts it somewhere on an
//! output, at whatever size and rotation the mapping needs. That is how one
//! composition drives several LED processors, and how an operator splits a wide
//! canvas across the four inputs a wall actually has.
//!
//! Deliberately rectangles. Mesh warping, polygons, edge blending and projector
//! calibration are explicit non-goals for these releases, and a
//! rectangle is what an LED wall wants anyway.
//!
//! # Why this is pure
//!
//! An output is a physical thing that cannot be checked in CI, but *which*
//! rectangle of the composition lands *where* on it is arithmetic. Getting that
//! wrong is the failure an operator sees as content on the wrong wall or half a
//! wall dark, so it is worth being able to test on a machine with no GPU.

use ovs_types::config::{BlendMode, Piece, PieceSource, Route, Show};
use ovs_types::geometry::{AnchorPoint, RectPx, SizePx};
use ovs_types::id::{OutputId, PieceId};

use crate::scene::{Quad, Scene, SourceRect, SourceTurn, TextureRef};

/// One rectangle of the composition, placed on one output.
///
/// Both rectangles are already in normalised coordinates: the source in
/// composition texture space, where `0.0` is left and top; the destination in
/// the output's normalised device coordinates, where `+Y` is up, matching the
/// shaders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MappedPiece {
    /// Which piece this came from, for diagnostics and tests.
    pub piece_id: PieceId,
    /// The picture this rectangle is taken out of.
    ///
    /// The composition, or one of the three buses. Resolved here rather than in
    /// the scene so that the one place that reads a `Piece` is the one place
    /// that knows what its source means.
    pub texture: TextureRef,
    /// Left edge of the source rectangle, `0.0` to `1.0` across the composition.
    pub source_left: f32,
    /// Top edge of the source rectangle, `0.0` to `1.0` down the composition.
    pub source_top: f32,
    /// Right edge of the source rectangle.
    pub source_right: f32,
    /// Bottom edge of the source rectangle.
    pub source_bottom: f32,
    /// Left edge on the output, in NDC.
    pub dest_left: f32,
    /// Bottom edge on the output, in NDC.
    pub dest_bottom: f32,
    /// Right edge on the output, in NDC.
    pub dest_right: f32,
    /// Top edge on the output, in NDC.
    pub dest_top: f32,
    /// Clockwise rotation in degrees about [`anchor`](Self::anchor).
    pub rotation_deg: f32,
    /// The point in the destination the turn happens about.
    pub anchor: AnchorPoint,
    /// How the source rectangle is turned before it is sampled.
    ///
    /// The other end of the same idea (owner, 2026-09-23): the rotation above
    /// puts the rectangle on the wall at an angle, and this takes it out of the
    /// composition at one. Worked out here rather than in the shader, because
    /// correcting for the composition's shape needs the composition's size.
    pub source_turn: SourceTurn,
}

impl MappedPiece {
    /// Returns true when this piece would put nothing on the output.
    #[must_use]
    pub fn is_invisible(&self) -> bool {
        self.dest_right <= self.dest_left
            || self.dest_top <= self.dest_bottom
            || self.source_right <= self.source_left
            || self.source_bottom <= self.source_top
    }

    /// Returns true when the piece lies entirely outside its output.
    ///
    /// Not an error. A piece may hang off the edge deliberately, and one that is
    /// entirely off is worth skipping rather than drawing invisibly.
    #[must_use]
    pub fn is_off_screen(&self) -> bool {
        self.dest_right <= -1.0
            || self.dest_left >= 1.0
            || self.dest_top <= -1.0
            || self.dest_bottom >= 1.0
    }
}

/// The pieces that make up one output's image.
///
/// Reused between frames, like the scene, so mapping allocates nothing after the
/// first few frames.
#[derive(Debug, Default)]
pub struct OutputMapping {
    output_id: Option<OutputId>,
    pieces: Vec<MappedPiece>,
}

impl OutputMapping {
    /// Creates an empty mapping.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns which output this maps onto, once it has been filled.
    #[must_use]
    pub const fn output_id(&self) -> Option<OutputId> {
        self.output_id
    }

    /// Returns the pieces, in draw order.
    #[must_use]
    pub fn pieces(&self) -> &[MappedPiece] {
        &self.pieces
    }

    /// Returns true when this output has nothing mapped onto it.
    ///
    /// An ordinary state: an output with no pieces shows black, which is what an
    /// unmapped wall should show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Returns how many pieces are mapped.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    /// Fills `into` with one quad per mapped piece, in draw order.
    ///
    /// Each samples the composition: a piece is a rectangle of the composition
    /// put somewhere on an output, which is the same drawing operation a layer
    /// uses with a different rectangle and its own rotation.
    pub fn fill_scene(&self, into: &mut Scene) {
        into.clear();
        for piece in &self.pieces {
            into.push(Quad {
                left: piece.dest_left,
                bottom: piece.dest_bottom,
                right: piece.dest_right,
                top: piece.dest_top,
                // White: the composition already carries every colour decision
                // the show made, and white is the identity for the shader's
                // multiply.
                colour: [1.0, 1.0, 1.0, 1.0],
                rotation_deg: piece.rotation_deg,
                anchor: piece.anchor,
                layer_id: None,
                source: SourceRect {
                    left: piece.source_left,
                    top: piece.source_top,
                    right: piece.source_right,
                    bottom: piece.source_bottom,
                },
                source_turn: piece.source_turn,
                texture: piece.texture,
                // A piece is not on a bus and does not blend with anything: it
                // is the last thing drawn before the wall.
                route: Route::M,
                blend: BlendMode::Normal,
            });
        }
    }

    /// Fills this mapping for one output, reusing its capacity.
    ///
    /// Pieces are drawn in their configured order, lowest first, so a later
    /// piece overlaps an earlier one. Disabled pieces, pieces belonging to other
    /// outputs and pieces that would draw nothing are all skipped.
    pub fn build(&mut self, show: &Show, output_id: OutputId, output: SizePx) {
        self.pieces.clear();
        self.output_id = Some(output_id);

        let composition = show.composition.size;
        if composition.width == 0 || composition.height == 0 {
            return;
        }
        if output.width == 0 || output.height == 0 {
            return;
        }

        let mut ordered: Vec<&Piece> = show
            .pieces
            .iter()
            .filter(|piece| piece.enabled && piece.output_id == output_id)
            .collect();
        // Stable within an order value, so two pieces at the same order keep
        // the order the show lists them in rather than swapping between runs.
        ordered.sort_by_key(|piece| piece.order);

        for piece in ordered {
            let mapped = map_piece(piece, composition, output);
            // A turned piece is skipped only for being empty, never for being
            // off screen: its rectangle is the unturned one, and a piece placed
            // just past an edge can turn back into view.
            let off_screen = mapped.rotation_deg == 0.0 && mapped.is_off_screen();
            if !mapped.is_invisible() && !off_screen {
                self.pieces.push(mapped);
            }
        }
    }
}

/// Converts one piece's pixel rectangles into normalised coordinates.
fn map_piece(piece: &Piece, composition: SizePx, output: SizePx) -> MappedPiece {
    let source = normalise_source(piece.source_rect, composition);
    let dest = normalise_dest(piece.dest_rect, output);

    MappedPiece {
        piece_id: piece.id,
        // Which picture this rectangle is taken out of. The composition unless
        // the piece says otherwise, which is what every document written before
        // pieces had a source says (owner, 2026-09-23).
        texture: match piece.source {
            PieceSource::Composition => TextureRef::Composition,
            PieceSource::Bus { route } => TextureRef::Bus(route),
        },
        source_left: source.0,
        source_top: source.1,
        source_right: source.2,
        source_bottom: source.3,
        dest_left: dest.0,
        dest_bottom: dest.1,
        dest_right: dest.2,
        dest_top: dest.3,
        anchor: piece.anchor,
        rotation_deg: if piece.rotation_deg.is_finite() {
            piece.rotation_deg
        } else {
            0.0
        },
        source_turn: SourceTurn::about(
            SourceRect {
                left: source.0,
                top: source.1,
                right: source.2,
                bottom: source.3,
            },
            piece.source_anchor,
            if piece.source_rotation_deg.is_finite() {
                piece.source_rotation_deg
            } else {
                0.0
            },
            composition,
        ),
    }
}

/// Composition pixels to texture coordinates: `0.0` is left and top.
fn normalise_source(rect: RectPx, composition: SizePx) -> (f32, f32, f32, f32) {
    let width = composition.width as f32;
    let height = composition.height as f32;

    let left = rect.x as f32 / width;
    let top = rect.y as f32 / height;
    let right = rect.right() as f32 / width;
    let bottom = rect.bottom() as f32 / height;

    (left, top, right, bottom)
}

/// Output pixels to NDC: `-1.0` is left and bottom, matching the shaders.
///
/// The Y flip happens here and nowhere else. A piece's destination is given in
/// output pixels with the origin at the top-left, because that is how an
/// operator reads an LED processor's documentation; the shaders want `+Y` up.
fn normalise_dest(rect: RectPx, output: SizePx) -> (f32, f32, f32, f32) {
    let width = output.width as f32;
    let height = output.height as f32;

    let left = rect.x as f32 / width * 2.0 - 1.0;
    let right = rect.right() as f32 / width * 2.0 - 1.0;

    // Top in pixels is the larger NDC value.
    let top = 1.0 - rect.y as f32 / height * 2.0;
    let bottom = 1.0 - rect.bottom() as f32 / height * 2.0;

    (left, bottom, right, top)
}

#[cfg(test)]
mod tests {
    use ovs_types::config::{DisplayMode, Show};
    use ovs_types::geometry::FrameRate;

    use super::*;

    fn output_of(width: u32, height: u32) -> SizePx {
        SizePx::new(width, height)
    }

    /// A show whose composition is 1920x1080 and which owns one output.
    fn show_with_output() -> (Show, OutputId) {
        let mut show = Show::new("Show");
        let output = ovs_types::config::OutputConfig::new(
            "Wall",
            DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
        );
        let id = output.id;
        show.outputs.push(output);
        (show, id)
    }

    fn piece(show: &mut Show, output: OutputId, source: RectPx, dest: RectPx) -> PieceId {
        let piece = Piece::new("Piece", output, source, dest);
        let id = piece.id;
        show.pieces.push(piece);
        id
    }

    #[test]
    fn an_output_with_no_pieces_maps_to_nothing() {
        // An unmapped wall shows black, which is correct rather than an error.
        let (show, output) = show_with_output();
        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));

        assert!(mapping.is_empty());
        assert_eq!(mapping.output_id(), Some(output));
    }

    #[test]
    fn a_piece_draws_the_picture_it_names() {
        // The owner, 2026-09-23: a piece may take its rectangle out of one of
        // the buses rather than out of the composition. A wall fed from A shows
        // the look coming up rather than the one going out, which is the thing
        // an operator judges a crossfade against and cannot see on the wall.
        for (chosen, expected) in [
            (PieceSource::Composition, TextureRef::Composition),
            (
                PieceSource::Bus { route: Route::A },
                TextureRef::Bus(Route::A),
            ),
            (
                PieceSource::Bus { route: Route::B },
                TextureRef::Bus(Route::B),
            ),
            (
                PieceSource::Bus { route: Route::M },
                TextureRef::Bus(Route::M),
            ),
        ] {
            let (mut show, output) = show_with_output();
            let id = piece(
                &mut show,
                output,
                RectPx::new(0, 0, 1920, 1080),
                RectPx::new(0, 0, 1920, 1080),
            );
            show.pieces
                .iter_mut()
                .find(|each| each.id == id)
                .expect("the piece")
                .source = chosen;

            let mut mapping = OutputMapping::new();
            mapping.build(&show, output, output_of(1920, 1080));
            let mut scene = Scene::new();
            mapping.fill_scene(&mut scene);

            assert_eq!(
                scene.quads().first().map(|quad| quad.texture),
                Some(expected),
                "a piece naming {chosen:?}"
            );
        }
    }

    #[test]
    fn a_full_frame_piece_covers_the_whole_output() {
        let (mut show, output) = show_with_output();
        piece(
            &mut show,
            output,
            RectPx::new(0, 0, 1920, 1080),
            RectPx::new(0, 0, 1920, 1080),
        );

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));

        let mapped = mapping.pieces()[0];
        assert!((mapped.source_left - 0.0).abs() < 1e-6);
        assert!((mapped.source_top - 0.0).abs() < 1e-6);
        assert!((mapped.source_right - 1.0).abs() < 1e-6);
        assert!((mapped.source_bottom - 1.0).abs() < 1e-6);

        assert!((mapped.dest_left + 1.0).abs() < 1e-6);
        assert!((mapped.dest_bottom + 1.0).abs() < 1e-6);
        assert!((mapped.dest_right - 1.0).abs() < 1e-6);
        assert!((mapped.dest_top - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_destination_y_axis_is_flipped_exactly_once() {
        // A piece's destination is given in output pixels with the origin at
        // the top-left, because that is how an LED processor's documentation
        // reads. The shaders want +Y up. Flipping twice, or not at all, puts
        // the top half of the composition on the bottom half of the wall.
        let (mut show, output) = show_with_output();
        // The top half of a 1080-tall output.
        piece(
            &mut show,
            output,
            RectPx::new(0, 0, 1920, 540),
            RectPx::new(0, 0, 1920, 540),
        );

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));
        let mapped = mapping.pieces()[0];

        assert!(
            (mapped.dest_top - 1.0).abs() < 1e-6,
            "the top of the output"
        );
        assert!(
            (mapped.dest_bottom - 0.0).abs() < 1e-6,
            "down to the middle"
        );
        assert!(
            mapped.dest_top > mapped.dest_bottom,
            "top must be above bottom"
        );
    }

    #[test]
    fn a_wide_composition_splits_across_two_outputs() {
        // The case pieces exist for: one canvas driving two LED processors.
        let mut show = Show::new("Show");
        show.composition.size = SizePx::new(3840, 1080);

        let left_output = ovs_types::config::OutputConfig::new(
            "Left",
            DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
        );
        let right_output = ovs_types::config::OutputConfig::new(
            "Right",
            DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
        );
        let (left_id, right_id) = (left_output.id, right_output.id);
        show.outputs.push(left_output);
        show.outputs.push(right_output);

        piece(
            &mut show,
            left_id,
            RectPx::new(0, 0, 1920, 1080),
            RectPx::new(0, 0, 1920, 1080),
        );
        piece(
            &mut show,
            right_id,
            RectPx::new(1920, 0, 1920, 1080),
            RectPx::new(0, 0, 1920, 1080),
        );

        let mut mapping = OutputMapping::new();

        mapping.build(&show, left_id, output_of(1920, 1080));
        assert_eq!(mapping.len(), 1, "only the left piece");
        assert!((mapping.pieces()[0].source_left - 0.0).abs() < 1e-6);
        assert!((mapping.pieces()[0].source_right - 0.5).abs() < 1e-6);

        mapping.build(&show, right_id, output_of(1920, 1080));
        assert_eq!(mapping.len(), 1, "only the right piece");
        assert!((mapping.pieces()[0].source_left - 0.5).abs() < 1e-6);
        assert!((mapping.pieces()[0].source_right - 1.0).abs() < 1e-6);

        // Both fill their own output completely.
        assert!((mapping.pieces()[0].dest_left + 1.0).abs() < 1e-6);
        assert!((mapping.pieces()[0].dest_right - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_piece_belonging_to_another_output_is_not_drawn() {
        // Content on the wrong wall is the failure this prevents.
        let (mut show, output) = show_with_output();
        let other = OutputId::new();
        piece(
            &mut show,
            other,
            RectPx::new(0, 0, 1920, 1080),
            RectPx::new(0, 0, 1920, 1080),
        );

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));
        assert!(mapping.is_empty());
    }

    #[test]
    fn a_disabled_piece_is_not_drawn() {
        let (mut show, output) = show_with_output();
        piece(
            &mut show,
            output,
            RectPx::new(0, 0, 1920, 1080),
            RectPx::new(0, 0, 1920, 1080),
        );
        show.pieces[0].enabled = false;

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));
        assert!(mapping.is_empty());
    }

    #[test]
    fn pieces_draw_in_their_configured_order() {
        // A later piece overlaps an earlier one, so the order decides what is
        // visible where they meet.
        let (mut show, output) = show_with_output();
        let first = piece(
            &mut show,
            output,
            RectPx::new(0, 0, 100, 100),
            RectPx::new(0, 0, 100, 100),
        );
        let second = piece(
            &mut show,
            output,
            RectPx::new(0, 0, 100, 100),
            RectPx::new(0, 0, 100, 100),
        );
        show.pieces[0].order = 10;
        show.pieces[1].order = 1;

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));

        let drawn: Vec<PieceId> = mapping
            .pieces()
            .iter()
            .map(|piece| piece.piece_id)
            .collect();
        assert_eq!(drawn, vec![second, first], "order 1 draws before order 10");
    }

    #[test]
    fn pieces_at_the_same_order_keep_the_shows_order() {
        // Two pieces that swapped places between runs would make a mapping look
        // correct one night and wrong the next.
        let (mut show, output) = show_with_output();
        let mut expected = Vec::new();
        for _ in 0..8 {
            expected.push(piece(
                &mut show,
                output,
                RectPx::new(0, 0, 100, 100),
                RectPx::new(0, 0, 100, 100),
            ));
        }

        let mut mapping = OutputMapping::new();
        for _ in 0..5 {
            mapping.build(&show, output, output_of(1920, 1080));
            let drawn: Vec<PieceId> = mapping
                .pieces()
                .iter()
                .map(|piece| piece.piece_id)
                .collect();
            assert_eq!(drawn, expected);
        }
    }

    #[test]
    fn a_piece_may_hang_off_the_edge_of_its_output() {
        // A legitimate mapping choice, so it must be drawn, clipped by the GPU.
        let (mut show, output) = show_with_output();
        piece(
            &mut show,
            output,
            RectPx::new(0, 0, 1920, 1080),
            RectPx::new(-500, -200, 1920, 1080),
        );

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));

        assert_eq!(mapping.len(), 1);
        assert!(
            mapping.pieces()[0].dest_left < -1.0,
            "it starts off the left edge"
        );
        assert!(!mapping.pieces()[0].is_off_screen());
    }

    #[test]
    fn a_piece_entirely_off_the_output_is_skipped() {
        // Not an error — an operator may have parked it — but drawing it would
        // cost a whole piece to change nothing.
        let (mut show, output) = show_with_output();
        piece(
            &mut show,
            output,
            RectPx::new(0, 0, 1920, 1080),
            RectPx::new(5000, 0, 100, 100),
        );

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));
        assert!(mapping.is_empty());
    }

    #[test]
    fn scaling_is_the_relationship_between_the_two_rectangles() {
        // There is no separate scale factor that could disagree with the
        // geometry: a 960-wide source in a 1920-wide destination is 2x.
        let (mut show, output) = show_with_output();
        piece(
            &mut show,
            output,
            RectPx::new(0, 0, 960, 540),
            RectPx::new(0, 0, 1920, 1080),
        );

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));
        let mapped = mapping.pieces()[0];

        let source_width = mapped.source_right - mapped.source_left;
        let dest_width = (mapped.dest_right - mapped.dest_left) / 2.0;
        assert!((source_width - 0.5).abs() < 1e-6, "half the composition");
        assert!((dest_width - 1.0).abs() < 1e-6, "all of the output");
    }

    #[test]
    fn rotation_is_carried_through_and_nonsense_is_not() {
        let (mut show, output) = show_with_output();
        piece(
            &mut show,
            output,
            RectPx::new(0, 0, 1920, 1080),
            RectPx::new(0, 0, 1920, 1080),
        );
        show.pieces[0].rotation_deg = 90.0;

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));
        assert_eq!(mapping.pieces()[0].rotation_deg, 90.0);

        show.pieces[0].rotation_deg = f32::NAN;
        mapping.build(&show, output, output_of(1920, 1080));
        assert_eq!(
            mapping.pieces()[0].rotation_deg,
            0.0,
            "NaN must not reach the GPU"
        );
    }

    #[test]
    fn a_degenerate_composition_or_output_maps_to_nothing() {
        let (mut show, output) = show_with_output();
        piece(
            &mut show,
            output,
            RectPx::new(0, 0, 1920, 1080),
            RectPx::new(0, 0, 1920, 1080),
        );

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(0, 1080));
        assert!(mapping.is_empty(), "an output with no pixels");

        show.composition.size = SizePx::new(1920, 0);
        mapping.build(&show, output, output_of(1920, 1080));
        assert!(mapping.is_empty(), "a composition with no pixels");
    }

    #[test]
    fn building_repeatedly_reuses_the_same_allocation() {
        let (mut show, output) = show_with_output();
        for _ in 0..16 {
            piece(
                &mut show,
                output,
                RectPx::new(0, 0, 100, 100),
                RectPx::new(0, 0, 100, 100),
            );
        }

        let mut mapping = OutputMapping::new();
        mapping.build(&show, output, output_of(1920, 1080));
        let capacity = mapping.pieces.capacity();
        assert!(capacity >= 16);

        for _ in 0..1_000 {
            mapping.build(&show, output, output_of(1920, 1080));
        }
        assert_eq!(
            mapping.pieces.capacity(),
            capacity,
            "mapping must not reallocate"
        );
        assert_eq!(mapping.len(), 16);
    }
}
