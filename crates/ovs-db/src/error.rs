//! Persistence errors.

use ovs_types::SchemaError;

/// Something went wrong talking to the database.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The underlying SQLite call failed.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A stored JSON document could not be read back into its type.
    ///
    /// This means a row exists that this build cannot understand, which is a
    /// migration bug rather than a user error.
    #[error("stored {entity} could not be decoded: {source}")]
    Decode {
        /// What was being decoded, for example `show`.
        entity: &'static str,
        /// The underlying parse failure.
        #[source]
        source: serde_json::Error,
    },

    /// A value could not be written as JSON.
    #[error("could not encode {entity}: {source}")]
    Encode {
        /// What was being encoded.
        entity: &'static str,
        /// The underlying serialisation failure.
        #[source]
        source: serde_json::Error,
    },

    /// The stored schema version is outside what this build supports.
    #[error(transparent)]
    Schema(#[from] SchemaError),

    /// The requested row does not exist.
    #[error("no {entity} with id {id}")]
    NotFound {
        /// What was being looked up.
        entity: &'static str,
        /// The identifier that did not match.
        id: String,
    },

    /// A migration failed, naming which one so the failure is actionable.
    #[error("migration {version} ({name}) failed: {source}")]
    Migration {
        /// Version of the failing migration.
        version: u32,
        /// Name of the failing migration.
        name: &'static str,
        /// The underlying failure.
        #[source]
        source: rusqlite::Error,
    },

    /// The database was created by a newer build.
    #[error(
        "database schema version {found} is newer than this build understands ({supported}); \
         update OpenVideoServer"
    )]
    DatabaseTooNew {
        /// Highest version found in the database.
        found: u32,
        /// Highest version this build ships a migration for.
        supported: u32,
    },

    /// The value violates the domain invariants and was not written.
    ///
    /// A show that cannot be loaded back must never reach storage, so this is
    /// refused at the boundary with every problem listed rather than the first.
    #[error("refused to store an invalid value: {0}")]
    Invalid(#[from] ovs_types::ValidationErrors),

    /// The write would break an invariant the database guards.
    #[error("{0}")]
    Conflict(String),
}

/// Result alias for persistence operations.
pub type Result<T> = std::result::Result<T, DbError>;
