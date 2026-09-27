//! The small pieces of geometry that drawing shares.
//!
//! One function so far, and it is here because it had already been written
//! twice: once for the stroke font's letters and once for the shape
//! generator's triangle. Two copies of a distance function drift — one of them
//! acquires a guard against a zero-length segment and the other does not — and
//! the symptom is a line that vanishes or a corner that rounds off, in one
//! generator and not the other, for no reason anybody can see from the code.

/// How far a point is from a line segment.
///
/// The segment, not the infinite line through it: past either end the nearest
/// point is that end. Drawing an outline needs the segment — a letter's stroke
/// and a triangle's edge both stop where they stop, and measuring to the line
/// would round every corner off by up to half the antialiasing width.
///
/// A segment of no length is a point, and the distance to it is the distance to
/// either end. That case is real rather than defensive: a font's stroke can
/// have coincident ends and a shape scaled to nothing has three of them.
pub fn distance_to_segment(point: (f32, f32), from: (f32, f32), to: (f32, f32)) -> f32 {
    let along = (to.0 - from.0, to.1 - from.1);
    let offset = (point.0 - from.0, point.1 - from.1);

    let length_squared = along.0.mul_add(along.0, along.1 * along.1);
    let position = if length_squared > f32::EPSILON {
        (offset.0.mul_add(along.0, offset.1 * along.1) / length_squared).clamp(0.0, 1.0)
    } else {
        0.0
    };

    along
        .0
        .mul_add(-position, offset.0)
        .hypot(along.1.mul_add(-position, offset.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_beside_a_segment_is_as_far_as_its_perpendicular() {
        let distance = distance_to_segment((0.0, 3.0), (-5.0, 0.0), (5.0, 0.0));
        assert!((distance - 3.0).abs() < 1e-5, "{distance}");
    }

    #[test]
    fn a_point_past_the_end_is_measured_to_the_end_and_not_to_the_line() {
        // The difference between a segment and the line through it, and the
        // reason a corner stays a corner rather than rounding off.
        let distance = distance_to_segment((8.0, 0.0), (-5.0, 0.0), (5.0, 0.0));
        assert!((distance - 3.0).abs() < 1e-5, "{distance}");

        let diagonal = distance_to_segment((5.0, 4.0), (-5.0, 0.0), (5.0, 0.0));
        assert!((diagonal - 4.0).abs() < 1e-5, "{diagonal}");
    }

    #[test]
    fn a_point_on_the_segment_is_on_it() {
        assert!(distance_to_segment((1.0, 0.0), (-5.0, 0.0), (5.0, 0.0)) < 1e-5);
    }

    #[test]
    fn a_segment_of_no_length_is_a_point() {
        // A font's stroke can have coincident ends and a shape scaled to
        // nothing has three of them, so this is arithmetic that happens rather
        // than a case that is guarded against.
        let distance = distance_to_segment((3.0, 4.0), (0.0, 0.0), (0.0, 0.0));
        assert!((distance - 5.0).abs() < 1e-5, "{distance}");
    }
}
