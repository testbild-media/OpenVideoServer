//! Versions of the persistent and external contracts.
//!
//! Every contract that can outlive a single build carries a version, from the
//! first commit rather than from the first time it changes.
//! Existing shows and lighting-console patches must not silently break after
//! an update (house rule §13).

use crate::error::SchemaError;

/// Schema version of the show model in this build.
///
/// Increment this in the same commit that changes the show model, and add the
/// matching migration. The golden schema test fails until both are done.
///
/// Version 1 has not been released, so no stored show uses it yet and the
/// model may still change under it. The first release freezes it; from that point
/// every change to a persisted type needs a new version and a migration.
pub const SHOW_SCHEMA_VERSION: u32 = 1;

/// Lowest show schema version this build can still migrate from.
pub const MIN_SUPPORTED_SHOW_SCHEMA_VERSION: u32 = 1;

/// Major version of the HTTP API, exposed as the `/api/v1` path prefix.
///
/// Version 1 has not been released and the only client that speaks it is in this
/// repository, shipped from the same tree as the server. So a request shape may
/// still change under it — as one did when the internal media profile was
/// removed, which rewrote the upload request without a new prefix.
/// The first release freezes it; from that point a breaking change to any
/// request or response is `/api/v2`.
pub const API_VERSION: u32 = 1;

/// Version of the realtime state protocol, agreed during the WebSocket handshake.
///
/// A client that does not understand this version refuses the connection with a
/// clear message instead of half-processing an unknown message stream.
///
/// Version 1 has not been released and no client exists outside this repository,
/// so the message set may still change under it, exactly as the show model
/// may. The first release freezes it; from that point every change to a message
/// a client can receive needs a new version.
pub const REALTIME_PROTOCOL_VERSION: u32 = 1;

/// Version of the show export package format.
///
/// A package written by a newer build is refused rather than half-understood: an
/// import that dropped a field it did not know about would produce a show that
/// looks right and is missing something nobody can name.
pub const PACKAGE_VERSION: u32 = 1;

/// Compile-time coherence checks on the version constants.
///
/// A build with a nonsensical supported range must not exist at all, so these
/// are const assertions rather than tests.
const _: () = {
    assert!(MIN_SUPPORTED_SHOW_SCHEMA_VERSION >= 1);
    assert!(MIN_SUPPORTED_SHOW_SCHEMA_VERSION <= SHOW_SCHEMA_VERSION);
    assert!(API_VERSION >= 1);
    assert!(REALTIME_PROTOCOL_VERSION >= 1);
};

/// Checks whether a stored show schema version can be loaded by this build.
///
/// # Errors
///
/// Returns [`SchemaError::TooNew`] for a show written by a newer build, and
/// [`SchemaError::Unmigratable`] for one older than any available migration.
pub const fn check_show_schema_version(found: u32) -> Result<(), SchemaError> {
    if found > SHOW_SCHEMA_VERSION {
        return Err(SchemaError::TooNew {
            found,
            supported: SHOW_SCHEMA_VERSION,
        });
    }
    if found < MIN_SUPPORTED_SHOW_SCHEMA_VERSION {
        return Err(SchemaError::Unmigratable { found });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_version_loads() {
        assert!(check_show_schema_version(SHOW_SCHEMA_VERSION).is_ok());
    }

    #[test]
    fn a_newer_show_is_refused_with_an_explanation() {
        let error =
            check_show_schema_version(SHOW_SCHEMA_VERSION + 1).expect_err("newer than this build");
        assert!(matches!(error, SchemaError::TooNew { .. }));
        assert!(error.to_string().contains("update OpenVideoServer"));
    }

    #[test]
    fn a_show_older_than_any_migration_is_refused() {
        assert!(matches!(
            check_show_schema_version(0),
            Err(SchemaError::Unmigratable { found: 0 })
        ));
    }

    #[test]
    fn every_version_in_the_supported_range_loads() {
        for version in MIN_SUPPORTED_SHOW_SCHEMA_VERSION..=SHOW_SCHEMA_VERSION {
            assert!(
                check_show_schema_version(version).is_ok(),
                "version {version} is inside the supported range but was refused"
            );
        }
    }
}
