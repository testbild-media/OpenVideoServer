//! A stroke font, for the words on a test card.
//!
//! The renderer has to write four things on a wall — what the pattern is, which
//! output this is, and what it is running at — and it has to do it at any size
//! from a 1024x600 panel to a 4K wall. That is the whole requirement, and it is
//! a long way short of text layout.
//!
//! # Why not a real font
//!
//! A font file means a font engine, a licence, and a megabyte of glyph outlines
//! shipped for the sake of thirty-eight characters that are always upper case.
//! Strokes cost a table and thirty lines of drawing, scale to any size because
//! they are described in a unit box, and look like what they are: the lettering
//! on a test card, which has been drawn this way since test cards were drawn by
//! hand.
//!
//! # How a glyph is described
//!
//! Each is one or more polylines in a box that is `ADVANCE` wide and one high,
//! with `y = 0` at the cap line and `y = 1` on the baseline. A descender goes
//! past 1, which is why `p` reaches 1.3. Nothing is closed and nothing is
//! filled: a glyph is a path a pen takes.

/// The width of a glyph's box, as a fraction of its height.
///
/// Every glyph is this wide, because the strings this draws are technical — a
/// resolution, a rate, an output number — and technical strings are read in
/// columns. Proportional spacing would make `1920x1080` wander.
pub const ADVANCE: f32 = 0.62;

/// One straight run of a pen.
type Polyline = &'static [(f32, f32)];

/// What this font can make of a string somebody typed.
///
/// **Nothing calls the glyph table directly with an operator's text.** The
/// strokes are upper case, plus the three lower-case letters a video format needs
/// — and until 2026-09-26 a name went through unchanged, so every other
/// lower-case letter drew nothing at all. A piece called `Piece 1` was written
/// `Pi   1` and one renamed to `Screen C` came out `S    C`, which is not a
/// legible fault: it looks like a spacing bug, and the owner reported it as one.
///
/// Three things happen, in this order.
///
/// **Upper case**, which is what fixes that, and what test-card lettering is
/// anyway. `char::to_uppercase` rather than the ASCII one, so `ß` becomes `SS`:
/// it has no single capital to become, and dropping it would lose a letter out of
/// the middle of a German word.
///
/// **Accents folded to the letter underneath**, because these rooms are
/// German-speaking and a wall called `Bühne` must not lose one either. The base
/// letter rather than the `UE` spelling: this is approximating a glyph, not
/// transliterating a word, and one character in gives one character out — which
/// is what keeps the size a caller computed from the length honest.
///
/// **Anything still unwritable is passed through unchanged**, so what becomes of
/// it stays the caller's decision rather than this function's.
#[must_use]
pub fn writable(text: &str) -> String {
    text.chars()
        .flat_map(char::to_uppercase)
        .map(fold)
        .collect()
}

/// The letter under an accent, or the character unchanged.
///
/// The Latin-1 letters and the few beyond it that turn up in European venue and
/// equipment names. Not a general Unicode decomposition: that is a table and a
/// dependency, and what it would buy over this is scripts this font has no
/// letters for at all.
const fn fold(character: char) -> char {
    match character {
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => 'A',
        'Ç' | 'Ć' | 'Č' => 'C',
        'Ď' | 'Đ' => 'D',
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ė' | 'Ę' | 'Ě' => 'E',
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ī' | 'Į' => 'I',
        'Ł' => 'L',
        'Ñ' | 'Ń' | 'Ň' => 'N',
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ő' => 'O',
        'Ŕ' | 'Ř' => 'R',
        'Ś' | 'Š' | 'Ş' => 'S',
        'Ť' | 'Ţ' => 'T',
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ū' | 'Ů' | 'Ű' => 'U',
        'Ý' | 'Ÿ' => 'Y',
        'Ź' | 'Ž' | 'Ż' => 'Z',
        other => other,
    }
}

/// Everything this can write.
///
/// Upper case only, with the handful of lower-case letters that appear in a
/// video format (`1920x1080p59.94`) and nothing else. A character that is not
/// here draws nothing rather than a box: a missing glyph on a test card should
/// not look like a graphics fault. Anything an operator typed should have been
/// through [`writable`] before it reaches here.
#[must_use]
pub fn glyph(character: char) -> &'static [Polyline] {
    match character {
        'A' => &[
            &[(0.0, 1.0), (0.29, 0.0), (0.58, 1.0)],
            &[(0.11, 0.62), (0.47, 0.62)],
        ],
        'B' => &[
            &[(0.0, 0.0), (0.0, 1.0)],
            &[
                (0.0, 0.0),
                (0.36, 0.0),
                (0.5, 0.1),
                (0.54, 0.24),
                (0.48, 0.4),
                (0.34, 0.48),
                (0.0, 0.48),
            ],
            &[
                (0.34, 0.48),
                (0.5, 0.55),
                (0.57, 0.7),
                (0.52, 0.88),
                (0.36, 1.0),
                (0.0, 1.0),
            ],
        ],
        'C' => &[&[
            (0.55, 0.15),
            (0.44, 0.04),
            (0.29, 0.0),
            (0.14, 0.05),
            (0.04, 0.2),
            (0.0, 0.5),
            (0.04, 0.8),
            (0.14, 0.95),
            (0.29, 1.0),
            (0.44, 0.96),
            (0.55, 0.85),
        ]],
        'D' => &[
            &[(0.0, 0.0), (0.0, 1.0)],
            &[
                (0.0, 0.0),
                (0.3, 0.0),
                (0.48, 0.12),
                (0.56, 0.35),
                (0.56, 0.65),
                (0.48, 0.88),
                (0.3, 1.0),
                (0.0, 1.0),
            ],
        ],
        'E' => &[
            &[(0.58, 0.0), (0.0, 0.0), (0.0, 1.0), (0.58, 1.0)],
            &[(0.0, 0.5), (0.44, 0.5)],
        ],
        'F' => &[
            &[(0.58, 0.0), (0.0, 0.0), (0.0, 1.0)],
            &[(0.0, 0.5), (0.44, 0.5)],
        ],
        'G' => &[&[
            (0.55, 0.15),
            (0.44, 0.04),
            (0.29, 0.0),
            (0.14, 0.05),
            (0.04, 0.2),
            (0.0, 0.5),
            (0.04, 0.8),
            (0.14, 0.95),
            (0.29, 1.0),
            (0.44, 0.96),
            (0.55, 0.85),
            (0.55, 0.56),
            (0.33, 0.56),
        ]],
        'H' => &[
            &[(0.0, 0.0), (0.0, 1.0)],
            &[(0.58, 0.0), (0.58, 1.0)],
            &[(0.0, 0.5), (0.58, 0.5)],
        ],
        'I' => &[
            &[(0.1, 0.0), (0.48, 0.0)],
            &[(0.29, 0.0), (0.29, 1.0)],
            &[(0.1, 1.0), (0.48, 1.0)],
        ],
        'J' => &[&[
            (0.5, 0.0),
            (0.5, 0.76),
            (0.36, 0.98),
            (0.16, 0.98),
            (0.02, 0.8),
        ]],
        'K' => &[
            &[(0.0, 0.0), (0.0, 1.0)],
            &[(0.55, 0.0), (0.04, 0.55)],
            &[(0.19, 0.42), (0.58, 1.0)],
        ],
        'L' => &[&[(0.0, 0.0), (0.0, 1.0), (0.54, 1.0)]],
        'M' => &[&[
            (0.0, 1.0),
            (0.0, 0.0),
            (0.29, 0.46),
            (0.58, 0.0),
            (0.58, 1.0),
        ]],
        'N' => &[&[(0.0, 1.0), (0.0, 0.0), (0.58, 1.0), (0.58, 0.0)]],
        'O' => &[&[
            (0.29, 0.0),
            (0.43, 0.07),
            (0.54, 0.25),
            (0.58, 0.5),
            (0.54, 0.75),
            (0.43, 0.93),
            (0.29, 1.0),
            (0.14, 0.93),
            (0.04, 0.75),
            (0.0, 0.5),
            (0.04, 0.25),
            (0.14, 0.07),
            (0.29, 0.0),
        ]],
        'P' => &[&[
            (0.0, 1.0),
            (0.0, 0.0),
            (0.36, 0.0),
            (0.52, 0.1),
            (0.57, 0.27),
            (0.5, 0.45),
            (0.34, 0.54),
            (0.0, 0.54),
        ]],
        'Q' => &[
            &[
                (0.29, 0.0),
                (0.43, 0.07),
                (0.54, 0.25),
                (0.58, 0.5),
                (0.54, 0.75),
                (0.43, 0.93),
                (0.29, 1.0),
                (0.14, 0.93),
                (0.04, 0.75),
                (0.0, 0.5),
                (0.04, 0.25),
                (0.14, 0.07),
                (0.29, 0.0),
            ],
            &[(0.34, 0.72), (0.6, 1.08)],
        ],
        'R' => &[
            &[
                (0.0, 1.0),
                (0.0, 0.0),
                (0.36, 0.0),
                (0.52, 0.1),
                (0.57, 0.27),
                (0.5, 0.45),
                (0.34, 0.54),
                (0.0, 0.54),
            ],
            &[(0.3, 0.54), (0.58, 1.0)],
        ],
        'S' => &[&[
            (0.55, 0.15),
            (0.42, 0.02),
            (0.2, 0.0),
            (0.06, 0.12),
            (0.04, 0.3),
            (0.18, 0.44),
            (0.42, 0.54),
            (0.55, 0.68),
            (0.53, 0.86),
            (0.38, 0.99),
            (0.16, 1.0),
            (0.03, 0.87),
        ]],
        'T' => &[&[(0.0, 0.0), (0.58, 0.0)], &[(0.29, 0.0), (0.29, 1.0)]],
        'U' => &[&[
            (0.0, 0.0),
            (0.0, 0.62),
            (0.03, 0.78),
            (0.12, 0.92),
            (0.29, 0.97),
            (0.46, 0.92),
            (0.55, 0.78),
            (0.58, 0.62),
            (0.58, 0.0),
        ]],
        'V' => &[&[(0.0, 0.0), (0.29, 1.0), (0.58, 0.0)]],
        'W' => &[&[
            (0.0, 0.0),
            (0.12, 1.0),
            (0.29, 0.36),
            (0.46, 1.0),
            (0.58, 0.0),
        ]],
        'X' => &[&[(0.0, 0.0), (0.58, 1.0)], &[(0.58, 0.0), (0.0, 1.0)]],
        'Y' => &[
            &[(0.0, 0.0), (0.29, 0.5), (0.58, 0.0)],
            &[(0.29, 0.5), (0.29, 1.0)],
        ],
        'Z' => &[&[(0.0, 0.0), (0.58, 0.0), (0.0, 1.0), (0.58, 1.0)]],

        '0' => &[
            &[
                (0.29, 0.0),
                (0.43, 0.07),
                (0.54, 0.25),
                (0.58, 0.5),
                (0.54, 0.75),
                (0.43, 0.93),
                (0.29, 1.0),
                (0.14, 0.93),
                (0.04, 0.75),
                (0.0, 0.5),
                (0.04, 0.25),
                (0.14, 0.07),
                (0.29, 0.0),
            ],
            &[(0.13, 0.85), (0.45, 0.15)],
        ],
        '1' => &[
            &[(0.08, 0.2), (0.29, 0.0)],
            &[(0.29, 0.0), (0.29, 1.0)],
            &[(0.08, 1.0), (0.5, 1.0)],
        ],
        '2' => &[&[
            (0.03, 0.18),
            (0.16, 0.03),
            (0.36, 0.0),
            (0.52, 0.1),
            (0.56, 0.28),
            (0.44, 0.48),
            (0.02, 1.0),
            (0.58, 1.0),
        ]],
        '3' => &[&[
            (0.04, 0.14),
            (0.18, 0.02),
            (0.38, 0.02),
            (0.53, 0.14),
            (0.5, 0.34),
            (0.33, 0.46),
            (0.52, 0.56),
            (0.56, 0.76),
            (0.42, 0.95),
            (0.2, 1.0),
            (0.04, 0.88),
        ]],
        '4' => &[&[(0.42, 1.0), (0.42, 0.0), (0.0, 0.7), (0.58, 0.7)]],
        '5' => &[&[
            (0.53, 0.02),
            (0.12, 0.02),
            (0.07, 0.38),
            (0.2, 0.33),
            (0.36, 0.35),
            (0.5, 0.46),
            (0.55, 0.65),
            (0.49, 0.85),
            (0.34, 0.98),
            (0.16, 0.98),
            (0.03, 0.88),
        ]],
        '6' => &[&[
            (0.5, 0.05),
            (0.35, 0.0),
            (0.19, 0.06),
            (0.07, 0.24),
            (0.02, 0.5),
            (0.02, 0.72),
            (0.1, 0.9),
            (0.25, 0.99),
            (0.42, 0.95),
            (0.53, 0.8),
            (0.5, 0.62),
            (0.36, 0.51),
            (0.2, 0.52),
            (0.07, 0.62),
        ]],
        '7' => &[&[(0.0, 0.0), (0.58, 0.0), (0.22, 1.0)]],
        '8' => &[&[
            (0.29, 0.47),
            (0.13, 0.4),
            (0.09, 0.25),
            (0.15, 0.08),
            (0.29, 0.02),
            (0.43, 0.08),
            (0.49, 0.25),
            (0.45, 0.4),
            (0.29, 0.47),
            (0.11, 0.56),
            (0.05, 0.72),
            (0.1, 0.9),
            (0.29, 0.99),
            (0.48, 0.9),
            (0.53, 0.72),
            (0.47, 0.56),
            (0.29, 0.47),
        ]],
        '9' => &[&[
            (0.08, 0.95),
            (0.23, 1.0),
            (0.39, 0.94),
            (0.51, 0.76),
            (0.56, 0.5),
            (0.56, 0.28),
            (0.48, 0.1),
            (0.33, 0.01),
            (0.16, 0.05),
            (0.05, 0.2),
            (0.08, 0.38),
            (0.22, 0.49),
            (0.38, 0.48),
            (0.51, 0.38),
        ]],
        // The lower-case letters a video format is written with, at x-height.
        'x' => &[&[(0.06, 0.38), (0.52, 1.0)], &[(0.52, 0.38), (0.06, 1.0)]],
        'p' => &[
            &[(0.06, 0.38), (0.06, 1.32)],
            &[
                (0.06, 0.38),
                (0.4, 0.38),
                (0.54, 0.56),
                (0.54, 0.74),
                (0.4, 0.92),
                (0.06, 0.92),
            ],
        ],
        'i' => &[&[(0.29, 0.38), (0.29, 1.0)], &[(0.29, 0.16), (0.29, 0.2)]],

        '.' => &[&[(0.26, 0.98), (0.32, 0.98)]],
        ',' => &[&[(0.3, 0.94), (0.22, 1.12)]],
        ':' => &[&[(0.29, 0.36), (0.29, 0.4)], &[(0.29, 0.94), (0.29, 0.98)]],
        '-' => &[&[(0.08, 0.56), (0.5, 0.56)]],
        '/' => &[&[(0.5, 0.0), (0.08, 1.0)]],
        '(' => &[&[(0.42, 0.0), (0.2, 0.28), (0.2, 0.72), (0.42, 1.0)]],
        ')' => &[&[(0.16, 0.0), (0.38, 0.28), (0.38, 0.72), (0.16, 1.0)]],

        // A space, and anything this does not know how to write.
        // A space is not a missing character. Without this arm it would be one
        // of them, and every gap in a caption would become a box.
        ' ' => &[],
        _ => &[],
    }
}

/// A box, for a caller that wants a missing character to be visible.
///
/// Not what [`glyph`] returns: on a test card a box reads as a fault, which is
/// the last thing a test card should invent, and the test beside it says so.
/// A caption is the other way round — the strings are whatever an operator
/// typed (owner decision Q11), and a character that drew a gap would be a
/// mistake discovered at the get-in rather than in the preview.
///
/// So the font draws nothing and says what nothing could have been, and the
/// caller decides which it wants. `crate::media::text::Missing` is where that
/// decision is written down.
pub const NOTDEF: &[&[(f32, f32)]] = &[&[
    (0.10, 0.10),
    (0.52, 0.10),
    (0.52, 0.90),
    (0.10, 0.90),
    (0.10, 0.10),
]];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_a_test_card_says_can_be_written() {
        // The exact strings this font exists for. A missing glyph would leave a
        // hole in the middle of the one thing an engineer is standing there to
        // read.
        for character in "TESTPATTERN GENERATOR COMPOSITION OUTPUT 1920x1080p59.94".chars() {
            if character == ' ' {
                continue;
            }
            assert!(
                !glyph(character).is_empty(),
                "nothing is drawn for {character:?}"
            );
        }
    }

    #[test]
    fn a_name_an_operator_typed_can_be_written() {
        // **The reported fault, both halves of it.** Before `writable` existed
        // these came out as "Pi   1" and "S    C", because every lower-case
        // letter but x, p and i draws nothing.
        assert_eq!(writable("Piece 1"), "PIECE 1");
        assert_eq!(writable("Screen C"), "SCREEN C");

        // German, which is what these rooms are named in. One character in, one
        // out, except `ß` — which has no single capital and would otherwise be a
        // letter missing from the middle of a word.
        assert_eq!(writable("Bühne"), "BUHNE");
        assert_eq!(writable("Straße"), "STRASSE");
        assert_eq!(writable("Öffentlich"), "OFFENTLICH");

        // And whatever comes out, this font can draw all of it.
        for text in ["Piece 1", "Screen C", "Bühne", "Straße", "LED Wall Left"] {
            for character in writable(text).chars() {
                assert!(
                    character == ' ' || !glyph(character).is_empty(),
                    "{character:?} came out of writable({text:?}) and draws nothing"
                );
            }
        }
    }

    #[test]
    fn a_character_it_does_not_know_draws_nothing() {
        // Rather than a box, which on a test card reads as a fault. A caller
        // that wants the box asks for `NOTDEF` and says so — see
        // `crate::media::text::Missing`.
        assert!(glyph('\u{20ac}').is_empty());
        assert!(
            glyph(' ').is_empty(),
            "and a space is not a missing character"
        );
    }

    #[test]
    fn the_box_for_a_missing_character_stays_inside_its_own_box() {
        for polyline in NOTDEF {
            for (x, y) in *polyline {
                assert!((0.0..=ADVANCE).contains(x), "{x} is outside the advance");
                assert!((0.0..=1.0).contains(y), "{y} is outside the cap box");
            }
        }
    }

    #[test]
    fn every_glyph_stays_inside_its_box() {
        // Descenders are allowed below the baseline and nothing else is
        // allowed anywhere: a stroke outside the box would collide with the
        // next letter.
        for character in ('A'..='Z')
            .chain('0'..='9')
            .chain(['x', 'p', 'i', '.', '-', '/'])
        {
            for polyline in glyph(character) {
                for &(x, y) in *polyline {
                    assert!((0.0..=ADVANCE).contains(&x), "{character:?} at x={x}");
                    assert!((0.0..=1.4).contains(&y), "{character:?} at y={y}");
                }
            }
        }
    }
}
