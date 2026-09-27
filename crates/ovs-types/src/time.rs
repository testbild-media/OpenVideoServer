//! Wall-clock timestamps for stored records.
//!
//! Deliberately a plain integer rather than a date-time library type. This crate
//! is the shared schema and is linked into the renderer, so it stays free of
//! dependencies that carry timezone databases or allocation.
//!
//! Show timing does not use this type. Presentation timing comes from the
//! monotonic master clock; a wall clock can step backwards.

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch, UTC.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(0);

    /// Creates a timestamp from milliseconds since the Unix epoch.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// Returns milliseconds since the Unix epoch.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// Returns the current time, or [`Timestamp::EPOCH`] if the system clock is
    /// set before 1970.
    #[must_use]
    pub fn now() -> Self {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(Self::EPOCH, |elapsed| Self(elapsed.as_millis() as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_as_a_bare_integer() {
        assert_eq!(
            serde_json::to_string(&Timestamp::from_millis(1_700_000_000_000)).expect("ok"),
            "1700000000000"
        );
    }

    #[test]
    fn now_is_after_the_epoch() {
        assert!(Timestamp::now() > Timestamp::EPOCH);
    }

    #[test]
    fn timestamps_order_chronologically() {
        assert!(Timestamp::from_millis(1) < Timestamp::from_millis(2));
    }
}
