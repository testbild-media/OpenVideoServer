//! What a client needs in order to draw a control.
//!
//! Generators and effects both declare their parameters the same way, so a
//! client draws a slider for a blur radius with the code that draws one for a
//! grid size. One description rather than two is what makes that true, and what
//! stops the two catalogues drifting apart as modules are added.
//!
//! This is a rendering of the manifests in `ovs-types`, not a second source of
//! truth.

use ovs_types::config::manifest::ParameterKind;
use serde::{Deserialize, Serialize};

/// What a parameter takes, and where it starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParameterType {
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
    /// A client draws a text field, no longer than `max_chars`.
    Text {
        /// What it says when nobody has set it.
        default: String,
        /// The most characters it may hold, counted as `char`s rather than
        /// bytes.
        max_chars: usize,
    },
    /// One of a fixed list of named choices.
    ///
    /// A client draws a menu. The value a show stores is the choice's `id`,
    /// never its `name` and never its position — a name is a thing that gets
    /// reworded and a position is a thing that moves.
    Choice {
        /// Every choice, in the order to offer them.
        choices: Vec<ParameterChoice>,
        /// The `id` of the one it is when nobody has set it.
        default: String,
    },
    /// A picture from the media library.
    ///
    /// A client offers the library here — the still images in it — rather than
    /// a text box, and stores the item's identifier. The value is carried as
    /// text, and empty text is "none chosen": that is what it starts as, and
    /// what it goes back to when the picture it named is deleted.
    ///
    /// No default, because there is nothing that could be one. Every other
    /// parameter type publishes a default that is part of the manifest
    /// forever; a picture is a row in a library that an operator can empty.
    Image,
}

/// One entry in a [`ParameterType::Choice`] list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ParameterChoice {
    /// The stable identifier a show stores. Never shown to an operator.
    pub id: String,
    /// What to show beside it.
    pub name: String,
}

impl From<ParameterKind> for ParameterType {
    /// Renders a manifest's declaration as something a client can read.
    fn from(kind: ParameterKind) -> Self {
        match kind {
            ParameterKind::Number { min, max, default } => Self::Number { min, max, default },
            ParameterKind::Colour { default } => Self::Colour { default },
            ParameterKind::Toggle { default } => Self::Toggle { default },
            ParameterKind::Text { default, max_chars } => Self::Text {
                default: default.to_owned(),
                max_chars,
            },
            ParameterKind::Choice { choices, default } => Self::Choice {
                choices: choices
                    .iter()
                    .map(|choice| ParameterChoice {
                        id: choice.id.to_owned(),
                        name: choice.name.to_owned(),
                    })
                    .collect(),
                default: default.to_owned(),
            },
            ParameterKind::Image => Self::Image,
        }
    }
}

/// One parameter a generator or effect declares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModuleParameter {
    /// The key it is stored under in a show.
    pub key: String,
    /// What to show beside it.
    pub name: String,
    /// What it takes.
    pub parameter_type: ParameterType,
}

impl From<&ovs_types::config::manifest::ParameterDefinition> for ModuleParameter {
    /// Renders one declared parameter.
    fn from(definition: &ovs_types::config::manifest::ParameterDefinition) -> Self {
        Self {
            key: definition.key.to_owned(),
            name: definition.name.to_owned(),
            parameter_type: definition.kind.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parameter_carries_what_a_client_needs_to_draw_a_control() {
        // Without the range and the default there is nothing to draw: a slider
        // with no ends and no position is not a control.
        let definition = ovs_types::config::manifest::ParameterDefinition {
            key: "radius_px",
            name: "Radius",
            kind: ParameterKind::Number {
                min: 0.0,
                max: 64.0,
                default: 0.0,
            },
            dmx: None,
        };
        let rendered = ModuleParameter::from(&definition);

        assert_eq!(rendered.key, "radius_px");
        assert_eq!(rendered.name, "Radius");
        assert_eq!(
            rendered.parameter_type,
            ParameterType::Number {
                min: 0.0,
                max: 64.0,
                default: 0.0
            }
        );
    }
}
