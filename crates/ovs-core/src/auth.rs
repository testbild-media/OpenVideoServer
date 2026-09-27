//! Authentication.
//!
//! What is required here: secure password hashing, expiring session
//! tokens, an authenticated API.
//!
//! V0.x has one operator account (owner decision Q7), but every token carries the
//! [`AccountId`] it was issued for, so nothing here has to be reshaped when there
//! are several.

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use ovs_types::id::AccountId;
use ovs_types::time::Timestamp;
use rand::rand_core::TryRng;
use rand::rngs::SysRng;

/// How long an issued token stays valid.
///
/// Long enough to cover a show without an operator being signed out mid-cue,
/// short enough that a token left on a laptop does not stay useful for weeks.
pub const TOKEN_LIFETIME_MS: i64 = 12 * 60 * 60 * 1000;

/// Minimum password length accepted at setup.
pub const MIN_PASSWORD_LENGTH: usize = 12;

/// Something went wrong authenticating.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The password could not be hashed.
    #[error("could not hash password: {0}")]
    Hashing(String),

    /// The stored hash is not a valid Argon2 hash string.
    #[error("the stored password hash is malformed")]
    MalformedHash,

    /// The username or password did not match.
    ///
    /// Deliberately one variant for both: distinguishing them tells an attacker
    /// which usernames exist.
    #[error("invalid username or password")]
    InvalidCredentials,

    /// The password does not meet the minimum requirements.
    #[error("a password must be at least {MIN_PASSWORD_LENGTH} characters")]
    PasswordTooShort,

    /// The token is unknown, malformed or expired.
    #[error("the session has expired; sign in again")]
    InvalidToken,

    /// The system random source failed.
    #[error("could not generate a secure token: {0}")]
    Entropy(String),
}

/// Hashes a password for storage.
///
/// Argon2id with the crate defaults, which are the current OWASP-recommended
/// parameters. A fresh random salt is generated per password by the hasher, so
/// the same password never produces the same stored hash twice.
///
/// # Errors
///
/// Returns [`AuthError::PasswordTooShort`] for a password below the minimum, and
/// [`AuthError::Hashing`] if the hasher fails.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    if password.chars().count() < MIN_PASSWORD_LENGTH {
        return Err(AuthError::PasswordTooShort);
    }

    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|error| AuthError::Hashing(error.to_string()))
}

/// Checks a password against a stored hash.
///
/// # Errors
///
/// Returns [`AuthError::InvalidCredentials`] when the password does not match,
/// and [`AuthError::MalformedHash`] when the stored hash cannot be parsed.
pub fn verify_password(password: &str, stored_hash: &str) -> Result<(), AuthError> {
    let parsed = PasswordHash::new(stored_hash).map_err(|_| AuthError::MalformedHash)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| AuthError::InvalidCredentials)
}

/// An issued session token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToken {
    /// The opaque token string the client presents.
    pub token: String,
    /// Which account it was issued for.
    pub account_id: AccountId,
    /// When it stops being accepted.
    pub expires_at: Timestamp,
}

impl SessionToken {
    /// Returns true when this token is still valid at the given moment.
    #[must_use]
    pub fn is_valid_at(&self, now: Timestamp) -> bool {
        now < self.expires_at
    }
}

/// Issues a session token.
///
/// The token is 32 bytes of system entropy, hex encoded. It carries no claims:
/// an opaque token that the server looks up cannot be forged by an attacker who
/// understands the format, and revoking one is a delete rather than a blocklist.
///
/// # Errors
///
/// Returns [`AuthError::Entropy`] if the system random source fails, which is
/// worth failing loudly over rather than falling back to something weaker.
pub fn issue_token(account_id: AccountId, now: Timestamp) -> Result<SessionToken, AuthError> {
    let token = issue_opaque_token()?;

    Ok(SessionToken {
        token,
        account_id,
        expires_at: Timestamp::from_millis(now.as_millis().saturating_add(TOKEN_LIFETIME_MS)),
    })
}

/// Issues one opaque credential: 32 bytes of system entropy, hex encoded.
///
/// Shared by sessions and by the tickets that let somebody watch one preview,
/// because the property that matters is the same for both — it carries no
/// claims, so an attacker who understands the format is no closer to forging
/// one, and withdrawing one is a delete rather than a blocklist.
///
/// # Errors
///
/// Returns [`AuthError::Entropy`] if the system random source fails, which is
/// worth failing loudly over rather than falling back to something weaker.
pub fn issue_opaque_token() -> Result<String, AuthError> {
    let mut bytes = [0_u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| AuthError::Entropy(error.to_string()))?;

    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }

    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_PASSWORD: &str = "correct horse battery staple";

    #[test]
    fn a_password_verifies_against_its_own_hash() {
        let hash = hash_password(GOOD_PASSWORD).expect("hashes");
        assert!(verify_password(GOOD_PASSWORD, &hash).is_ok());
    }

    #[test]
    fn a_wrong_password_is_refused() {
        let hash = hash_password(GOOD_PASSWORD).expect("hashes");
        let error = verify_password("wrong password entirely", &hash).expect_err("refused");
        assert!(matches!(error, AuthError::InvalidCredentials));
    }

    #[test]
    fn the_hash_is_argon2id_and_not_the_password() {
        let hash = hash_password(GOOD_PASSWORD).expect("hashes");
        assert!(hash.starts_with("$argon2id$"), "got {hash}");
        assert!(!hash.contains(GOOD_PASSWORD));
    }

    #[test]
    fn the_same_password_hashes_differently_every_time() {
        // A shared salt would let one rainbow table cover every server.
        let first = hash_password(GOOD_PASSWORD).expect("hashes");
        let second = hash_password(GOOD_PASSWORD).expect("hashes");
        assert_ne!(first, second);
        assert!(verify_password(GOOD_PASSWORD, &first).is_ok());
        assert!(verify_password(GOOD_PASSWORD, &second).is_ok());
    }

    #[test]
    fn a_short_password_is_refused_at_setup() {
        assert!(matches!(
            hash_password("short"),
            Err(AuthError::PasswordTooShort)
        ));
        assert!(matches!(
            hash_password(""),
            Err(AuthError::PasswordTooShort)
        ));
        // Exactly at the boundary is accepted.
        assert!(hash_password(&"a".repeat(MIN_PASSWORD_LENGTH)).is_ok());
    }

    #[test]
    fn a_malformed_stored_hash_is_reported_as_such() {
        // Distinguishable from a wrong password, because it means a corrupt row
        // rather than a failed sign-in attempt.
        let error = verify_password(GOOD_PASSWORD, "not a hash").expect_err("malformed");
        assert!(matches!(error, AuthError::MalformedHash));
    }

    #[test]
    fn a_failed_sign_in_does_not_reveal_whether_the_user_exists() {
        // One message for both cases, by construction: there is only one variant.
        let hash = hash_password(GOOD_PASSWORD).expect("hashes");
        let wrong_password = verify_password("nope nope nope", &hash).expect_err("refused");
        assert_eq!(
            wrong_password.to_string(),
            AuthError::InvalidCredentials.to_string()
        );
        assert_eq!(wrong_password.to_string(), "invalid username or password");
    }

    #[test]
    fn a_token_is_long_random_hex() {
        let token = issue_token(AccountId::new(), Timestamp::now()).expect("issues");
        assert_eq!(token.token.len(), 64, "32 bytes of entropy");
        assert!(token.token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn two_tokens_are_never_the_same() {
        let account = AccountId::new();
        let now = Timestamp::now();
        let first = issue_token(account, now).expect("issues");
        let second = issue_token(account, now).expect("issues");
        assert_ne!(first.token, second.token);
    }

    #[test]
    fn a_token_expires() {
        let issued_at = Timestamp::from_millis(1_000_000);
        let token = issue_token(AccountId::new(), issued_at).expect("issues");

        assert!(token.is_valid_at(issued_at));
        assert!(token.is_valid_at(Timestamp::from_millis(
            issued_at.as_millis() + TOKEN_LIFETIME_MS - 1
        )));
        assert!(!token.is_valid_at(Timestamp::from_millis(
            issued_at.as_millis() + TOKEN_LIFETIME_MS
        )));
        assert!(!token.is_valid_at(Timestamp::from_millis(issued_at.as_millis() + 999_999_999)));
    }

    #[test]
    fn a_token_carries_the_account_it_was_issued_for() {
        // V0.x has one account, but nothing here assumes that.
        let account = AccountId::new();
        let token = issue_token(account, Timestamp::now()).expect("issues");
        assert_eq!(token.account_id, account);
    }

    #[test]
    fn an_expiry_far_in_the_future_does_not_overflow() {
        let token =
            issue_token(AccountId::new(), Timestamp::from_millis(i64::MAX)).expect("issues");
        assert_eq!(token.expires_at.as_millis(), i64::MAX);
    }
}
