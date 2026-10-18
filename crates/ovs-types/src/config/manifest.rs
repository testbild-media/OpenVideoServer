//! What a generator or an effect declares about itself.
//!
//! Generators and effects are different things — one makes pixels, the other
//! changes them — but what an operator sets on either is the same: a handful of
//! named values with types, ranges and defaults. That part lives here so the two
//! catalogues cannot drift apart, because a client draws both from the same
//! description and a show stores both in the same [`ParameterMap`].
//!
//! # A manifest is code, not data
//!
//! Not serialisable, deliberately, exactly like a fixture personality. A
//! manifest describes what this build can actually do; a show stores a type
//! and expects the same thing back. What a client reads is the catalogue
//! `ovs-proto` renders from these, which owns its strings.
//!
//! # One of each
//!
//! There is one manifest of each type and no versions of it (owner,
//! 2026-10-18: "Es gibt kein Invert V2.. nirgendwo"). A parameter added later
//! goes on the end with a default that changes nothing, because a show saved
//! last year must open looking the way it was left.

use crate::config::slot::{ParameterMap, ParameterValue};

/// What kind of value a parameter takes, and where it starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParameterKind {
    /// A number in a closed range.
    Number {
        /// Lowest value.
        min: f64,
        /// Highest value.
        max: f64,
        /// What it is when nobody has set it.
        default: f64,
    },
    /// A colour, as linear RGBA.
    Colour {
        /// What it is when nobody has set it.
        default: [f32; 4],
    },
    /// On or off.
    Toggle {
        /// What it is when nobody has set it.
        default: bool,
    },
    /// Words an operator types.
    ///
    /// For a caption, a name, a label — anything whose value is the text itself
    /// rather than a number or a switch. The desktop client draws a text field.
    ///
    /// # Why there is a length at all
    ///
    /// A show is a document that is validated, exported, hashed and sent
    /// between servers, and a parameter with no bound is a way to put a
    /// megabyte into one. The limit is per parameter and generous enough that
    /// nobody typing a caption meets it; a value past it is refused on save
    /// with the length in the message, like every other bound here.
    Text {
        /// What it says when nobody has set it.
        default: &'static str,
        /// The most characters it may hold, counted as `char`s.
        ///
        /// Characters rather than bytes: somebody who typed thirty letters has
        /// typed thirty letters whether or not any of them was an umlaut, and a
        /// limit that said otherwise would depend on the language they write in.
        max_chars: usize,
    },
    /// One of a fixed list of named choices.
    ///
    /// For a parameter whose values mean something rather than measure
    /// something: which shape, which blend mode, which pattern. The
    /// alternative — a number whose whole values are the list, with the names
    /// written into the label — was what the shape generator had to do, and it
    /// gives an operator a fader labelled "Shape (0 ellipse, 1 rectangle, 2
    /// triangle)" to guess at (owner decision Q17).
    Choice {
        /// Every choice, in the order a client offers them.
        choices: &'static [Choice],
        /// The identifier of the one it is when nobody has set it.
        ///
        /// Must name one of `choices`; a manifest that does not is refused by
        /// the catalogue's own test rather than by a show that opens wrong.
        default: &'static str,
    },
    /// A picture from the media library.
    ///
    /// The first parameter that names something outside itself. Its value is
    /// the identifier of a media item, as text, and empty text is "none
    /// chosen" — which is what it starts as, because a published default may
    /// not name anything that could be deleted, and an effect has to be
    /// neutral at its defaults.
    ///
    /// Text rather than an identifier of its own on the wire, for the reason
    /// [`ParameterKind::Choice`] is text: the kind says what the string means,
    /// and a client that reads the catalogue knows to offer the library here
    /// rather than a text box. What it costs is a parse where the value is
    /// read; what it saves is a second shape of value in every show file, every
    /// schema and every control path.
    Image,
}

/// How wide a channel a parameter asks for, when it asks for one at all.
///
/// # Why a plugin decides this and nobody else
///
/// An effect or a generator is a plugin, and third-party ones are expected
///. **DMX is optional and per parameter:** a plugin may bring
/// channels, may bring none, and may bring them for some of its parameters and
/// not others. Whoever writes it knows which of its knobs somebody would want on
/// a fader; a fixed layout decided here would be wrong for every plugin that
/// did not exist when it was written.
///
/// So this is not a channel number. The order is the order the parameters are
/// declared in, and the numbers fall out of that — which means a plugin author
/// writes one line per parameter and gets a personality a console can be
/// programmed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmxChannel {
    /// One channel. 256 steps across whatever the parameter's range is.
    ///
    /// Right for a switch, for a choice, and for a number an operator sets
    /// rather than rides.
    Coarse,
    /// Two channels, coarse then fine.
    ///
    /// For anything a hand moves: 256 steps across a fade is visible banding in
    /// the last few per cent, which is the same reason the layer's transform is
    /// sixteen bits (`ovs.layer`).
    ///
    /// Only means anything for a number. A colour is four eight-bit channels
    /// either way, because that is what a desk sends colour as; a switch and a
    /// choice are one.
    Fine,
}

/// One entry in a [`ParameterKind::Choice`] list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// The stable identifier a show stores. Never shown.
    pub id: &'static str,
    /// What a client shows beside it.
    pub name: &'static str,
}

/// One parameter a generator or effect declares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParameterDefinition {
    /// The key it is stored under.
    pub key: &'static str,
    /// What a client shows beside it.
    pub name: &'static str,
    /// What it takes.
    pub kind: ParameterKind,
    /// Whether a lighting console may drive it, and how finely.
    ///
    /// `None` means this parameter is the client's and the API's only, which is
    /// the right answer for most of them: a caption's words cannot be sent down
    /// a DMX cable at all, and a parameter nobody would ride during a show is a
    /// channel a patch has to find room for and an operator has to program.
    ///
    /// A plugin that declares none of these has no personality and cannot be
    /// patched, which is not a gap — it is the plugin saying it does not want
    /// to be driven that way.
    pub dmx: Option<DmxChannel>,
}

impl ParameterDefinition {
    /// How many DMX channels this parameter takes, or zero when it takes none.
    ///
    /// Text is always zero, whatever it declares: a desk sends numbers, and a
    /// parameter whose value *is* words has nothing to send. The catalogue's own
    /// test refuses a manifest that asks for one.
    #[must_use]
    pub const fn dmx_width(&self) -> u16 {
        let Some(width) = self.dmx else {
            return 0;
        };
        match self.kind {
            // Four: red, green, blue and alpha, a byte each, which is what a
            // desk's colour fader set sends.
            ParameterKind::Colour { .. } => 4,
            ParameterKind::Number { .. } => match width {
                DmxChannel::Coarse => 1,
                DmxChannel::Fine => 2,
            },
            ParameterKind::Toggle { .. } | ParameterKind::Choice { .. } => 1,
            // Neither is anything a desk can send: one is words, the other
            // names a file in the library.
            ParameterKind::Text { .. } | ParameterKind::Image => 0,
        }
    }

    /// Returns the value to use when a show does not set this parameter.
    #[must_use]
    pub fn default_value(&self) -> ParameterValue {
        match &self.kind {
            ParameterKind::Number { default, .. } => ParameterValue::Float(*default),
            ParameterKind::Colour { default } => ParameterValue::Color(*default),
            ParameterKind::Toggle { default } => ParameterValue::Bool(*default),
            ParameterKind::Choice { default, .. } => ParameterValue::Choice((*default).to_owned()),
            ParameterKind::Text { default, .. } => ParameterValue::Text((*default).to_owned()),
            // No picture. See `ParameterKind::Image`: a default that named one
            // would be a default that can be deleted from the library.
            ParameterKind::Image => ParameterValue::Text(String::new()),
        }
    }

    /// Checks a value against this definition.
    ///
    /// Returns why it is not usable, or `None` when it is. A number outside its
    /// range is refused rather than clamped: clamping is what a live control
    /// does with a fader that has gone too far, and this is a show being
    /// saved, where the right answer is to say what is wrong (house rule §17).
    #[must_use]
    pub fn refuse(&self, value: &ParameterValue) -> Option<String> {
        match (&self.kind, value) {
            (ParameterKind::Number { min, max, .. }, ParameterValue::Float(number)) => {
                (!(number.is_finite() && number >= min && number <= max))
                    .then(|| format!("must be a number between {min} and {max}"))
            }
            (ParameterKind::Number { min, max, .. }, ParameterValue::Int(number)) => {
                let number = *number as f64;
                (!(number >= *min && number <= *max))
                    .then(|| format!("must be a number between {min} and {max}"))
            }
            (ParameterKind::Colour { .. }, ParameterValue::Color(colour)) => colour
                .iter()
                .any(|channel| !(channel.is_finite() && (0.0..=1.0).contains(channel)))
                .then(|| "must be four channels between 0 and 1".to_owned()),
            (ParameterKind::Toggle { .. }, ParameterValue::Bool(_)) => None,
            (ParameterKind::Text { max_chars, .. }, ParameterValue::Text(words)) => {
                let length = words.chars().count();
                (length > *max_chars)
                    .then(|| format!("must be at most {max_chars} characters, not {length}"))
            }
            (ParameterKind::Choice { choices, .. }, ParameterValue::Choice(chosen)) => choices
                .iter()
                .all(|choice| choice.id != chosen)
                .then(|| format!("must be one of {}", ids(choices))),
            // Whether the library has it is not asked here. This crate has no
            // library to ask, and the answer would be a different kind of
            // refusal in any case: a show naming a picture that was deleted is
            // a show with a gap in it, not a show with a bad value.
            (ParameterKind::Image, ParameterValue::Text(name)) => (!name.is_empty()
                && name.parse::<crate::id::MediaItemId>().is_err())
            .then(|| "must be a picture from the library, or nothing".to_owned()),
            (ParameterKind::Number { .. }, _) => Some("must be a number".to_owned()),
            (ParameterKind::Colour { .. }, _) => Some("must be a colour".to_owned()),
            (ParameterKind::Toggle { .. }, _) => Some("must be on or off".to_owned()),
            (ParameterKind::Text { .. }, _) => Some("must be text".to_owned()),
            (ParameterKind::Image, _) => {
                Some("must be a picture from the library, or nothing".to_owned())
            }
            (ParameterKind::Choice { choices, .. }, _) => {
                Some(format!("must be one of {}", ids(choices)))
            }
        }
    }
}

/// The identifiers of a choice list, for a message that has to name them.
fn ids(choices: &[Choice]) -> String {
    choices
        .iter()
        .map(|choice| choice.id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Returns a parameter by key.
#[must_use]
pub fn parameter<'a>(
    parameters: &'a [ParameterDefinition],
    key: &str,
) -> Option<&'a ParameterDefinition> {
    parameters.iter().find(|parameter| parameter.key == key)
}

/// Returns the value for a key, falling back to the declared default.
///
/// A show that leaves a parameter out is not incomplete: it is a show that
/// wants what the module was published with, which is exactly what a default is
/// for.
#[must_use]
pub fn value(
    parameters: &[ParameterDefinition],
    values: &ParameterMap,
    key: &str,
) -> Option<ParameterValue> {
    if let Some(value) = values.get(key) {
        return Some(value.clone());
    }
    parameter(parameters, key).map(ParameterDefinition::default_value)
}

/// Returns every problem with a set of values.
///
/// A value for something the module does not declare is a problem too: it is
/// nearly always a typo or a show written against another version, and
/// silently ignoring it means an operator setting something and seeing nothing
/// happen.
///
/// `what` names the module in the message, so the reader of a refused save is
/// told which of a chain's effects they need to look at rather than only that
/// one of them is wrong.
#[must_use]
pub fn problems(
    parameters: &[ParameterDefinition],
    what: &str,
    values: &ParameterMap,
) -> Vec<(String, String)> {
    let mut problems = Vec::new();
    for (key, value) in values {
        match parameter(parameters, key) {
            Some(definition) => {
                if let Some(reason) = definition.refuse(value) {
                    problems.push((key.clone(), reason));
                }
            }
            None => problems.push((key.clone(), format!("{what} has no such parameter"))),
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLOUR: ParameterDefinition = ParameterDefinition {
        key: "colour",
        name: "Colour",
        kind: ParameterKind::Colour {
            default: [1.0, 1.0, 1.0, 1.0],
        },
        dmx: None,
    };

    const AMOUNT: ParameterDefinition = ParameterDefinition {
        key: "amount",
        name: "Amount",
        kind: ParameterKind::Number {
            min: 0.0,
            max: 1.0,
            default: 0.5,
        },
        dmx: None,
    };

    const SHAPE: ParameterDefinition = ParameterDefinition {
        key: "shape",
        name: "Shape",
        kind: ParameterKind::Choice {
            choices: &[
                Choice {
                    id: "ellipse",
                    name: "Ellipse",
                },
                Choice {
                    id: "rectangle",
                    name: "Rectangle",
                },
            ],
            default: "ellipse",
        },
        dmx: None,
    };

    const CAPTION: ParameterDefinition = ParameterDefinition {
        key: "text",
        name: "Text",
        kind: ParameterKind::Text {
            default: "OPENVIDEOSERVER",
            max_chars: 8,
        },
        dmx: None,
    };

    #[test]
    fn text_nobody_set_is_what_it_was_published_with() {
        assert_eq!(
            CAPTION.default_value(),
            ParameterValue::Text("OPENVIDEOSERVER".to_owned())
        );
    }

    #[test]
    fn text_past_its_length_is_refused_and_says_how_long_it_was() {
        // A show is validated, exported, hashed and sent between servers. A
        // parameter with no bound is a way to put a megabyte into one.
        let refusal = CAPTION
            .refuse(&ParameterValue::Text("NINE CHAR".to_owned()))
            .expect("refused");
        assert!(refusal.contains('8') && refusal.contains('9'), "{refusal}");

        assert!(CAPTION
            .refuse(&ParameterValue::Text("EIGHT CH".to_owned()))
            .is_none());
        assert!(
            CAPTION
                .refuse(&ParameterValue::Text(String::new()))
                .is_none(),
            "a caption somebody cleared is a caption"
        );
    }

    #[test]
    fn a_text_length_counts_characters_rather_than_bytes() {
        // Eight letters is eight letters whether or not any of them is an
        // umlaut. Counting bytes would be a limit that depends on the language
        // somebody writes in.
        assert!(CAPTION
            .refuse(&ParameterValue::Text("GRUSSE ✓".to_owned()))
            .is_none());
    }

    #[test]
    fn a_choice_nobody_set_is_the_one_it_was_published_with() {
        assert_eq!(
            SHAPE.default_value(),
            ParameterValue::Choice("ellipse".to_owned())
        );
    }

    #[test]
    fn a_choice_the_manifest_does_not_declare_is_refused_and_says_what_it_could_be() {
        let refusal = SHAPE
            .refuse(&ParameterValue::Choice("star".to_owned()))
            .expect("refused");
        assert!(refusal.contains("ellipse"), "{refusal}");
        assert!(refusal.contains("rectangle"), "{refusal}");

        assert!(SHAPE
            .refuse(&ParameterValue::Choice("rectangle".to_owned()))
            .is_none());
    }

    #[test]
    fn a_choice_is_never_a_number_however_plausible_the_number_looks() {
        // The whole point of the kind. A show that stored 1 for "rectangle"
        // was a show whose meaning depended on the order of a list, and
        // inserting a shape at the front changed every show that used one.
        assert!(SHAPE.refuse(&ParameterValue::Float(1.0)).is_some());
        assert!(SHAPE.refuse(&ParameterValue::Int(1)).is_some());
        assert!(SHAPE
            .refuse(&ParameterValue::Text("rectangle".to_owned()))
            .is_some());
    }

    #[test]
    fn a_parameter_nobody_set_is_the_one_it_was_published_with() {
        let values = ParameterMap::new();
        assert_eq!(
            value(&[COLOUR, AMOUNT], &values, "amount"),
            Some(ParameterValue::Float(0.5))
        );
    }

    #[test]
    fn a_number_outside_its_range_is_refused_rather_than_clamped() {
        // Clamping is what a live control does with a fader that has gone too
        // far. A show being saved is told what is wrong instead.
        assert!(AMOUNT.refuse(&ParameterValue::Float(1.5)).is_some());
        assert!(AMOUNT.refuse(&ParameterValue::Float(0.25)).is_none());
    }

    #[test]
    fn a_value_for_something_that_was_never_declared_says_which_module() {
        // Nearly always a typo or a show written against another version.
        // Silently ignoring it means somebody sets a control and sees nothing.
        let values = ParameterMap::from([("radius".to_owned(), ParameterValue::Float(4.0))]);
        let problems = problems(&[COLOUR, AMOUNT], "ovs.tint version 1", &values);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].0, "radius");
        assert!(problems[0].1.contains("ovs.tint version 1"), "{problems:?}");
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused() {
        let values = ParameterMap::from([("colour".to_owned(), ParameterValue::Float(0.5))]);
        let problems = problems(&[COLOUR, AMOUNT], "ovs.tint version 1", &values);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("colour"));
    }
}
