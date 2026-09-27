//! Pieces: rectangles of the composition mapped onto physical outputs.
//!
//! Deliberately simple rectangles for the first releases. Mesh warping, polygons,
//! edge blending and projector calibration are explicit non-goals, but the
//! model keeps them addable without changing what a
//! piece means.

use serde::{Deserialize, Serialize};

use crate::config::layer::Route;
use crate::error::{ValidationError, ValidationErrors};
use crate::geometry::{AnchorPoint, RectPx, SizePx};
use crate::id::{OutputId, PieceId};

/// Which picture a piece takes its rectangle out of.
///
/// The composition is the show as it leaves the server, and is what every piece
/// took until the owner asked for a choice (2026-09-23). The three buses are
/// what it is made of: A and B are the two sides of the crossfade, and M is the
/// master path that goes over them.
///
/// A wall fed from one bus is a monitor — it shows the look coming up rather
/// than the look going out, which is the thing an operator is judging a
/// crossfade against and cannot see on the wall itself.
///
/// **One layer's own picture is not here yet.** A layer composites straight
/// into its bus, so there is no image of one on its own to sample, and giving
/// this a variant the renderer cannot honour would be a control that does
/// nothing.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PieceSource {
    /// The composition: the show as it leaves the server.
    #[default]
    Composition,
    /// One of the three buses, before the combine.
    Bus {
        /// Which one: A and B are the crossfade's sides, M is the master path.
        route: Route,
    },
}

/// Whether an angle is zero, for leaving it out of a serialised document.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde asks for a reference"
)]
fn is_zero(angle: &f32) -> bool {
    *angle == 0.0
}

/// One rectangular mapping from the composition to an output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Piece {
    /// Stable identifier.
    pub id: PieceId,
    /// Operator-visible name, for example `Wall Left Upper`.
    pub name: String,
    /// Whether this piece is drawn.
    pub enabled: bool,
    /// Draw order within its output; lower values draw first.
    pub order: u32,
    /// Which output this piece draws to.
    pub output_id: OutputId,
    /// Region of the composition this piece takes, in composition pixels.
    pub source_rect: RectPx,
    /// Clockwise rotation of the *taken* region, in degrees.
    ///
    /// About [`source_anchor`](Self::source_anchor), and in composition pixels
    /// rather than in texture coordinates — a forty-five degree turn of a 16:9
    /// region is a turn and not a shear.
    ///
    /// Both ends of a piece turn (owner, 2026-09-23): the output rotation puts
    /// a rectangle on a wall at an angle, and this one takes a rectangle out of
    /// the composition at an angle, which is what a wall hung on the diagonal
    /// needs fed to it.
    ///
    /// Zero unless somebody says otherwise, which is what every document
    /// written before this field existed reads as.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub source_rotation_deg: f32,
    /// The point in `source_rect` that rotation happens about.
    ///
    /// The centre unless somebody says otherwise, for the same reason
    /// [`anchor`](Self::anchor) is.
    #[serde(default, skip_serializing_if = "AnchorPoint::is_centre")]
    pub source_anchor: AnchorPoint,
    /// Region of the output this piece fills, in output pixels.
    ///
    /// Scaling is the relationship between `source_rect` and this rectangle, so
    /// there is no separate scale factor that could disagree with the geometry.
    pub dest_rect: RectPx,
    /// Clockwise rotation of the *placed* rectangle, in degrees.
    ///
    /// About [`anchor`](Self::anchor), in output pixels.
    pub rotation_deg: f32,
    /// Which picture this piece takes its rectangle out of.
    ///
    /// The composition unless somebody says otherwise, which is what every
    /// document written before this field existed reads as — so nothing already
    /// mapped changes when it is loaded.
    ///
    /// The owner asked for a choice here (2026-09-23): a wall showing the
    /// incoming look rather than the outgoing one is a monitor, and a wall
    /// showing one bus is what a crossfade is judged against.
    #[serde(default, skip_serializing_if = "PieceSource::is_composition")]
    pub source: PieceSource,
    /// The point in `dest_rect` the rotation happens about.
    ///
    /// The centre unless somebody says otherwise, which is what every
    /// document written before this field existed reads as — so nothing
    /// already mapped moves.
    #[serde(default, skip_serializing_if = "AnchorPoint::is_centre")]
    pub anchor: AnchorPoint,
}

impl PieceSource {
    /// Returns true for the default, so a document written before this existed
    /// round-trips without gaining a field.
    #[must_use]
    pub const fn is_composition(&self) -> bool {
        matches!(self, Self::Composition)
    }
}

impl Piece {
    /// Creates a piece mapping a composition region onto an output region.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        output_id: OutputId,
        source_rect: RectPx,
        dest_rect: RectPx,
    ) -> Self {
        Self {
            id: PieceId::new(),
            name: name.into(),
            enabled: true,
            order: 0,
            output_id,
            source_rect,
            source: PieceSource::Composition,
            source_rotation_deg: 0.0,
            source_anchor: AnchorPoint::CENTRE,
            dest_rect,
            rotation_deg: 0.0,
            anchor: AnchorPoint::CENTRE,
        }
    }

    /// Returns true when this piece maps its source region without resampling.
    #[must_use]
    pub const fn is_pixel_exact(&self) -> bool {
        self.source_rect.width == self.dest_rect.width
            && self.source_rect.height == self.dest_rect.height
            && self.rotation_deg == 0.0
            && self.source_rotation_deg == 0.0
    }

    pub(crate) fn validate(
        &self,
        path: &str,
        composition: SizePx,
        known_outputs: &[OutputId],
        out: &mut ValidationErrors,
    ) {
        if self.name.trim().is_empty() {
            out.push(ValidationError::new(
                format!("{path}.name"),
                "must not be empty",
            ));
        }
        self.source_rect
            .validate(&format!("{path}.source_rect"), out);
        self.dest_rect.validate(&format!("{path}.dest_rect"), out);

        self.anchor.validate(&format!("{path}.anchor"), out);
        self.source_anchor
            .validate(&format!("{path}.source_anchor"), out);

        for (angle, field) in [
            (self.rotation_deg, "rotation_deg"),
            (self.source_rotation_deg, "source_rotation_deg"),
        ] {
            if !angle.is_finite() {
                out.push(ValidationError::new(
                    format!("{path}.{field}"),
                    "must be a finite number",
                ));
            }
        }

        if !self.source_rect.fits_within(composition) {
            out.push(ValidationError::new(
                format!("{path}.source_rect"),
                format!(
                    "must lie within the {}x{} composition",
                    composition.width, composition.height
                ),
            ));
        }

        if !known_outputs.contains(&self.output_id) {
            out.push(ValidationError::new(
                format!("{path}.output_id"),
                format!(
                    "references output {} which is not configured",
                    self.output_id
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composition() -> SizePx {
        SizePx::new(1920, 1080)
    }

    #[test]
    fn a_full_frame_piece_validates() {
        let output = OutputId::new();
        let piece = Piece::new(
            "Wall",
            output,
            RectPx::covering(composition()),
            RectPx::new(0, 0, 1920, 1080),
        );
        let mut errors = ValidationErrors::new();
        piece.validate("pieces[0]", composition(), &[output], &mut errors);
        assert!(errors.is_empty(), "{errors}");
        assert!(piece.is_pixel_exact());
    }

    #[test]
    fn a_source_rect_outside_the_composition_is_rejected() {
        let output = OutputId::new();
        let piece = Piece::new(
            "Wall",
            output,
            RectPx::new(1000, 0, 1920, 1080),
            RectPx::new(0, 0, 1920, 1080),
        );
        let mut errors = ValidationErrors::new();
        piece.validate("pieces[0]", composition(), &[output], &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors
            .to_string()
            .contains("must lie within the 1920x1080 composition"));
    }

    #[test]
    fn a_piece_pointing_at_an_unconfigured_output_is_rejected() {
        let piece = Piece::new(
            "Wall",
            OutputId::new(),
            RectPx::covering(composition()),
            RectPx::new(0, 0, 1920, 1080),
        );
        let mut errors = ValidationErrors::new();
        piece.validate("pieces[0]", composition(), &[], &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors.to_string().contains("is not configured"));
    }

    #[test]
    fn a_piece_may_hang_off_the_edge_of_its_output() {
        // Positioning a piece partly outside the output is a legitimate mapping
        // choice, so only the source side is constrained.
        let output = OutputId::new();
        let piece = Piece::new(
            "Overhang",
            output,
            RectPx::new(0, 0, 400, 400),
            RectPx::new(-100, -100, 400, 400),
        );
        let mut errors = ValidationErrors::new();
        piece.validate("pieces[0]", composition(), &[output], &mut errors);
        assert!(errors.is_empty(), "{errors}");
    }

    #[test]
    fn scaling_or_rotation_makes_a_piece_non_pixel_exact() {
        let output = OutputId::new();
        let scaled = Piece::new(
            "Scaled",
            output,
            RectPx::new(0, 0, 400, 400),
            RectPx::new(0, 0, 800, 800),
        );
        assert!(!scaled.is_pixel_exact());

        let rotated = Piece {
            rotation_deg: 90.0,
            ..Piece::new(
                "Rotated",
                output,
                RectPx::new(0, 0, 400, 400),
                RectPx::new(0, 0, 400, 400),
            )
        };
        assert!(!rotated.is_pixel_exact());
    }
}
