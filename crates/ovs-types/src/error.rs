//! Validation errors for the domain model.
//!
//! Validation collects every problem instead of failing on the first one, so a
//! client can show an operator everything that is wrong with a show in one
//! pass rather than one error per save attempt.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A single validation problem, located by a dotted path into the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ValidationError {
    /// Dotted path to the offending value, for example `layers[2].slots[0].index`.
    pub path: String,
    /// Human-readable description of what is wrong.
    pub message: String,
}

impl ValidationError {
    /// Creates a validation error.
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// An accumulated set of validation problems.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct ValidationErrors(Vec<ValidationError>);

impl ValidationErrors {
    /// Creates an empty error set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a problem.
    pub fn push(&mut self, error: ValidationError) {
        self.0.push(error);
    }

    /// Returns true when nothing is wrong.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the number of recorded problems.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns the recorded problems.
    #[must_use]
    pub fn as_slice(&self) -> &[ValidationError] {
        &self.0
    }

    /// Converts into `Ok(())` when empty, otherwise into `Err(self)`.
    ///
    /// # Errors
    ///
    /// Returns the accumulated problems when any were recorded.
    pub fn into_result(self) -> Result<(), Self> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(self)
        }
    }
}

impl IntoIterator for ValidationErrors {
    type Item = ValidationError;
    type IntoIter = std::vec::IntoIter<ValidationError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.len() {
            0 => write!(f, "no validation errors"),
            1 => write!(f, "1 validation error: {}", self.0[0]),
            n => {
                write!(f, "{n} validation errors:")?;
                for error in &self.0 {
                    write!(f, "\n  - {error}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ValidationErrors {}

/// Reasons a stored show cannot be loaded at all.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// The show was written by a newer version of OpenVideoServer.
    #[error(
        "show schema version {found} is newer than the supported version {supported}; \
         update OpenVideoServer to open this show"
    )]
    TooNew {
        /// Version found in the stored show.
        found: u32,
        /// Highest version this build understands.
        supported: u32,
    },
    /// No migration path exists from the stored version.
    #[error("no migration path from show schema version {found}")]
    Unmigratable {
        /// Version found in the stored show.
        found: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_errors_convert_to_ok() {
        assert!(ValidationErrors::new().into_result().is_ok());
    }

    #[test]
    fn recorded_errors_convert_to_err() {
        let mut errors = ValidationErrors::new();
        errors.push(ValidationError::new(
            "layers[0].opacity",
            "must be between 0 and 1",
        ));
        let result = errors.into_result();
        assert!(result.is_err());
        assert_eq!(result.expect_err("has errors").len(), 1);
    }

    #[test]
    fn display_lists_every_problem() {
        let mut errors = ValidationErrors::new();
        errors.push(ValidationError::new("a", "first"));
        errors.push(ValidationError::new("b", "second"));
        let text = errors.to_string();
        assert!(text.contains("2 validation errors"));
        assert!(text.contains("a: first"));
        assert!(text.contains("b: second"));
    }
}
