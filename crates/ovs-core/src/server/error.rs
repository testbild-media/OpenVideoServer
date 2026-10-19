//! Turning internal errors into API responses.
//!
//! One shape for every failure, so a client has one thing to handle. The
//! mapping is deliberately explicit rather than a blanket "anything becomes a
//! 500": a wrong password and a missing show are different to an operator,
//! and a client that cannot tell them apart cannot say anything useful.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ovs_proto::http::ApiError;

use crate::auth::AuthError;
use crate::state::StateError;

/// An error on its way out of a handler.
#[derive(Debug)]
pub struct ApiFailure {
    status: StatusCode,
    error: ApiError,
}

impl ApiFailure {
    /// Builds a failure with an explicit status.
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            error: ApiError::new(code, message),
        }
    }

    /// The request was not understood.
    pub fn bad_request(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// The thing being addressed does not exist.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// The request conflicts with the server's current state.
    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// The request carried no valid credentials.
    pub fn unauthorised(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorised", message)
    }

    /// The credentials were good and do not reach this far.
    ///
    /// Distinct from [`unauthorised`](Self::unauthorised) on purpose: signing in
    /// again is the answer to one of these and not to the other, and a client
    /// that could not tell them apart would send an operator round the sign-in
    /// screen for a thing no password of theirs unlocks.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    /// The request failed validation, with every problem listed.
    pub fn invalid(errors: ovs_types::ValidationErrors) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: ApiError::invalid(errors),
        }
    }

    /// The disk holding the media store has no room for this.
    ///
    /// Separate from [`internal`](ApiFailure::internal) because it is the one
    /// server-side failure an operator can actually fix, and the only thing
    /// standing between them and fixing it is being told which failure it was.
    /// A client showing "something went wrong on the server" sends somebody to
    /// read a log for a fact the server already knew.
    pub fn out_of_space(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INSUFFICIENT_STORAGE, "out_of_space", message)
    }

    /// Something went wrong that the client cannot do anything about.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    /// Turns a failed write into a failure, naming a full disk as one.
    ///
    /// Every write in the upload path goes through here, so there is one place
    /// that decides what a client is told about a write that did not happen.
    pub fn from_write(what: &str, error: &std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::StorageFull {
            return Self::out_of_space(format!("{what}: the disk holding the media store is full"));
        }
        Self::internal(format!("{what}: {error}"))
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        (self.status, Json(self.error)).into_response()
    }
}

impl From<AuthError> for ApiFailure {
    fn from(error: AuthError) -> Self {
        match error {
            // One response for both, so the difference reveals nothing about
            // which usernames exist.
            AuthError::InvalidCredentials | AuthError::MalformedHash => Self::new(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "invalid username or password",
            ),
            AuthError::InvalidToken => {
                Self::new(StatusCode::UNAUTHORIZED, "invalid_token", error.to_string())
            }
            AuthError::PasswordTooShort => {
                Self::bad_request("password_too_short", error.to_string())
            }
            AuthError::Hashing(_) | AuthError::Entropy(_) => {
                Self::internal("the server could not complete the sign-in")
            }
        }
    }
}

impl From<ovs_db::DbError> for ApiFailure {
    fn from(error: ovs_db::DbError) -> Self {
        use ovs_db::DbError;
        match error {
            DbError::NotFound { entity, id } => {
                Self::not_found(format!("no {entity} with id {id}"))
            }
            DbError::Invalid(errors) => Self::invalid(errors),
            DbError::Schema(inner) => Self::conflict("unsupported_schema", inner.to_string()),
            DbError::DatabaseTooNew { .. } => {
                Self::conflict("unsupported_schema", error.to_string())
            }
            DbError::Conflict(message) => Self::conflict("conflict", message),
            // A decode failure means a row this build cannot read, which is a
            // server problem and not something a client can fix.
            DbError::Sqlite(_)
            | DbError::Decode { .. }
            | DbError::Encode { .. }
            | DbError::Migration { .. } => Self::internal(error.to_string()),
        }
    }
}

/// Turns a state error into something a client can act on.
///
/// A layer, slot or effect that is not there is the client's mistake and gets a
/// 404; no show open is a 409, because the request was well formed and the
/// server is simply not in a state to carry it out. A value an effect's manifest
/// refuses is a 400 quoting the manifest's own words, so an operator is told what
/// the control's range actually is rather than only that they missed it.
impl From<StateError> for ApiFailure {
    fn from(error: StateError) -> Self {
        match error {
            StateError::NoShow => Self::conflict("no_show", error.to_string()),
            StateError::UnknownLayer { .. }
            | StateError::UnknownSlot { .. }
            | StateError::UnknownEffect { .. }
            | StateError::UnknownGenerator { .. } => Self::not_found(error.to_string()),
            // Not the caller's mistake: the show names an effect this build
            // does not have, which is something an operator has to be told about
            // rather than a request they can correct.
            StateError::UnpublishedEffect { .. } => {
                Self::conflict("unpublished_effect", error.to_string())
            }
            StateError::UnpublishedGenerator { .. } => {
                Self::conflict("unpublished_generator", error.to_string())
            }
            StateError::InvalidEffectParameter { .. } => {
                Self::bad_request("invalid_parameter", error.to_string())
            }
        }
    }
}

/// Turns a refused show edit into something a client can act on.
///
/// Each with a code of its own, because a client answers them differently: a
/// locked effect says who can unlock it, and a value the desk holds says how
/// to get it back.
impl From<ovs_types::control::EditRefusal> for ApiFailure {
    fn from(refusal: ovs_types::control::EditRefusal) -> Self {
        use ovs_types::control::EditRefusal;
        match refusal {
            EditRefusal::Locked(message) => Self::conflict("locked", message),
            EditRefusal::HeldByDesk(message) => Self::conflict("held_by_desk", message),
        }
    }
}

/// What every handler returns.
pub type ApiResult<T> = Result<T, ApiFailure>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_password_and_a_corrupt_hash_look_identical_to_a_client() {
        let wrong: ApiFailure = AuthError::InvalidCredentials.into();
        let corrupt: ApiFailure = AuthError::MalformedHash.into();
        assert_eq!(wrong.status, corrupt.status);
        assert_eq!(wrong.error.message, corrupt.error.message);
        assert_eq!(wrong.error.code, corrupt.error.code);
    }

    #[test]
    fn a_missing_row_is_a_not_found_rather_than_a_server_error() {
        let failure: ApiFailure = ovs_db::DbError::NotFound {
            entity: "show",
            id: "abc".to_owned(),
        }
        .into();
        assert_eq!(failure.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn an_invalid_show_carries_every_problem_to_the_client() {
        let mut errors = ovs_types::ValidationErrors::new();
        errors.push(ovs_types::ValidationError::new("name", "must not be empty"));
        errors.push(ovs_types::ValidationError::new(
            "master.default_bpm",
            "out of range",
        ));

        let failure: ApiFailure = ovs_db::DbError::Invalid(errors).into();
        assert_eq!(failure.status, StatusCode::BAD_REQUEST);
        assert_eq!(failure.error.details.len(), 2);
    }

    #[test]
    fn a_database_from_a_newer_build_is_a_conflict_not_a_crash() {
        let failure: ApiFailure = ovs_db::DbError::DatabaseTooNew {
            found: 9,
            supported: 1,
        }
        .into();
        assert_eq!(failure.status, StatusCode::CONFLICT);
        assert!(failure.error.message.contains("update OpenVideoServer"));
    }

    #[test]
    fn selecting_a_slot_that_does_not_exist_is_a_not_found() {
        let failure: ApiFailure = StateError::UnknownSlot {
            layer_id: ovs_types::id::LayerId::new(),
            slot_index: 7,
        }
        .into();
        assert_eq!(failure.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_full_disk_is_told_apart_from_a_server_fault() {
        let full = ApiFailure::from_write(
            "could not write this chunk",
            &std::io::Error::from(std::io::ErrorKind::StorageFull),
        );
        assert_eq!(full.status, StatusCode::INSUFFICIENT_STORAGE);
        assert_eq!(full.error.code, "out_of_space");
        assert!(full.error.message.contains("full"));

        let other = ApiFailure::from_write(
            "could not write this chunk",
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert_eq!(other.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(other.error.code, "internal");
    }

    #[test]
    fn acting_without_a_show_is_a_conflict() {
        let failure: ApiFailure = StateError::NoShow.into();
        assert_eq!(failure.status, StatusCode::CONFLICT);
        assert_eq!(failure.error.code, "no_show");
    }
}
