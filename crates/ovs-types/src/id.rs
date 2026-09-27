//! Stable entity identifiers.
//!
//! Every entity that a show, the API or the renderer can refer to carries a
//! ULID. Array position is never identity.
//!
//! Each entity gets its own Rust type through [`define_id`], so a `LayerId` can
//! never be passed where a `MediaItemId` is expected. On the wire and in the
//! database an ID is a 26-character Crockford base32 string.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// An identifier string that could not be parsed as a ULID.
#[derive(Debug, thiserror::Error)]
#[error("invalid identifier {value:?}: expected a 26-character ULID")]
pub struct InvalidId {
    /// The rejected input.
    pub value: String,
}

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(Ulid);

        impl $name {
            /// Creates a new random identifier.
            ///
            /// ULIDs carry a millisecond timestamp in their high bits, so they
            /// sort chronologically at millisecond resolution and keep database
            /// index inserts local. Two identifiers minted within the same
            /// millisecond have an arbitrary relative order: this generator is
            /// not monotonic, and nothing may depend on it being so.
            #[must_use]
            pub fn new() -> Self {
                Self(Ulid::new())
            }

            /// Returns the underlying ULID.
            #[must_use]
            pub fn as_ulid(self) -> Ulid {
                self.0
            }

            /// The identity nothing has.
            ///
            /// A `serde` default has to be the same value every time it is
            /// asked for, because the schema generator writes it into the
            /// published schema and a schema that changes on every build can
            /// never be checked against a committed one. A freshly minted
            /// identity is the opposite of that.
            ///
            /// So a field that mints one on load takes this instead, and is
            /// given a real identity when the show is read
            /// (`Show::assign_missing_ids`).
            #[must_use]
            pub fn nil() -> Self {
                Self(Ulid::nil())
            }

            /// Whether this is [`nil`](Self::nil) — an identity not yet given.
            ///
            /// By reference rather than by value, although this is `Copy`, so
            /// that `serde`'s `skip_serializing_if` can name it: an identity
            /// nothing has is left out of the wire form entirely rather than
            /// written as twenty-six zeroes, which would read as an identity
            /// like any other.
            #[must_use]
            pub fn is_nil(&self) -> bool {
                self.0.is_nil()
            }

            /// Builds an identifier from a raw ULID.
            #[must_use]
            pub fn from_ulid(ulid: Ulid) -> Self {
                Self(ulid)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = InvalidId;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s)
                    .map(Self)
                    .map_err(|_| InvalidId { value: s.to_owned() })
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidId;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                value.parse()
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.to_string()
            }
        }

        impl schemars::JsonSchema for $name {
            fn schema_name() -> String {
                stringify!($name).to_owned()
            }

            fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
                let mut schema = <String as schemars::JsonSchema>::json_schema(gen).into_object();
                schema.string().pattern = Some("^[0-9A-HJKMNP-TV-Z]{26}$".to_owned());
                schema.metadata().description =
                    Some(concat!(stringify!($name), " (ULID, 26 characters)").to_owned());
                schema.into()
            }
        }
    };
}

define_id!(
    /// Identifies a show.
    ShowId
);
define_id!(
    /// Identifies a layer within a show.
    LayerId
);
define_id!(
    /// Identifies a rectangular piece of the composition mapped to an output.
    PieceId
);
define_id!(
    /// Identifies a logical output, independent of transient OS display numbering.
    OutputId
);
define_id!(
    /// Identifies a logical media item, the only media identity a show ever stores.
    MediaItemId
);
define_id!(
    /// Identifies one physical representation of a media item.
    MediaAssetId
);
define_id!(
    /// Identifies one effect instance in one chain.
    EffectInstanceId
);
define_id!(
    /// Identifies one generator instance in one slot.
    ///
    /// A generator is a source rather than a chain entry, so it has no place
    /// in a chain to be found by — and a patched fixture has to keep pointing
    /// at the same one across a save and a reload. Minted when the slot is
    /// built and carried in the show from then on.
    GeneratorInstanceId
);
define_id!(
    /// Identifies a patched fixture instance.
    FixtureInstanceId
);
define_id!(
    /// Identifies an operator account.
    ///
    /// V0.x has exactly one account per server and no roles. The identifier
    /// exists anyway so that authenticated requests carry a subject from the
    /// start: adding accounts later stays additive rather than requiring every
    /// endpoint and record to be reshaped.
    AccountId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_string() {
        let id = LayerId::new();
        let text = id.to_string();
        assert_eq!(text.len(), 26);
        assert_eq!(text.parse::<LayerId>().expect("parses"), id);
    }

    #[test]
    fn round_trips_through_json_as_a_plain_string() {
        let id = MediaItemId::new();
        let json = serde_json::to_string(&id).expect("serialises");
        assert_eq!(json, format!("\"{id}\""));
        let back: MediaItemId = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, id);
    }

    #[test]
    fn rejects_malformed_identifiers() {
        assert!("not-a-ulid".parse::<ShowId>().is_err());
        assert!("".parse::<ShowId>().is_err());
        assert!(serde_json::from_str::<ShowId>("\"short\"").is_err());
    }

    #[test]
    fn distinct_entities_have_distinct_types() {
        // This test documents intent: the line below must not compile.
        //   let _: LayerId = MediaItemId::new();
        let layer = LayerId::new();
        let media = MediaItemId::new();
        assert_ne!(layer.to_string(), media.to_string());
    }

    #[test]
    fn ids_minted_in_different_milliseconds_sort_chronologically() {
        let earlier = ShowId::from_ulid(Ulid::from_parts(1_700_000_000_000, u128::MAX));
        let later = ShowId::from_ulid(Ulid::from_parts(1_700_000_000_001, 0));
        assert!(
            earlier < later,
            "the timestamp must dominate the random component"
        );
        assert!(
            earlier.to_string() < later.to_string(),
            "string form must sort the same way, because the database sorts strings"
        );
    }

    #[test]
    fn ids_minted_in_the_same_millisecond_are_not_ordered() {
        // Documents the limit of the guarantee above. Nothing may rely on the
        // relative order of two identifiers created back to back.
        let a = ShowId::from_ulid(Ulid::from_parts(1_700_000_000_000, 5));
        let b = ShowId::from_ulid(Ulid::from_parts(1_700_000_000_000, 3));
        assert!(
            b < a,
            "within one millisecond only the random component decides"
        );
    }
}
