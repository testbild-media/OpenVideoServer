//! Sessions in flight.
//!
//! Tokens are held in memory rather than in the database. A server restart
//! signing everyone out is correct behaviour: the show does not depend on a
//! client being connected, so there is nothing to protect by making a session
//! outlive the process that issued it.

use std::collections::HashMap;

use ovs_types::id::AccountId;
use ovs_types::time::Timestamp;

use crate::auth::{issue_token, AuthError, SessionToken};

/// The live set of session tokens.
#[derive(Debug, Default)]
pub struct Sessions {
    by_token: HashMap<String, SessionToken>,
}

impl Sessions {
    /// Creates an empty session store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Issues a token for an account that has already proven its password.
    ///
    /// This deliberately does not check the password: verification belongs to
    /// [`crate::auth`], and keeping them apart means this type cannot be the
    /// place a credential check is accidentally skipped.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Entropy`] if the system random source fails.
    pub fn sign_in(
        &mut self,
        account_id: AccountId,
        now: Timestamp,
    ) -> Result<SessionToken, AuthError> {
        let token = issue_token(account_id, now)?;
        self.by_token.insert(token.token.clone(), token.clone());
        Ok(token)
    }

    /// Returns the account a token belongs to, if it is valid right now.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::InvalidToken`] for an unknown or expired token. The
    /// two are one error on purpose: an attacker learns nothing from the
    /// difference.
    pub fn account_for(&self, token: &str, now: Timestamp) -> Result<AccountId, AuthError> {
        let session = self.by_token.get(token).ok_or(AuthError::InvalidToken)?;
        if session.is_valid_at(now) {
            Ok(session.account_id)
        } else {
            Err(AuthError::InvalidToken)
        }
    }

    /// Ends one session.
    ///
    /// Returns true when a session was ended.
    pub fn sign_out(&mut self, token: &str) -> bool {
        self.by_token.remove(token).is_some()
    }

    /// Ends every session for an account.
    ///
    /// Used when a password changes: an old token must stop working.
    pub fn sign_out_account(&mut self, account_id: AccountId) -> usize {
        let before = self.by_token.len();
        self.by_token
            .retain(|_, session| session.account_id != account_id);
        before - self.by_token.len()
    }

    /// Drops expired sessions and returns how many were removed.
    ///
    /// Expired tokens are already refused by [`Sessions::account_for`]; this just
    /// keeps the map from growing for the lifetime of the process.
    pub fn purge_expired(&mut self, now: Timestamp) -> usize {
        let before = self.by_token.len();
        self.by_token.retain(|_, session| session.is_valid_at(now));
        before - self.by_token.len()
    }

    /// Returns how many sessions are currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_token.len()
    }

    /// Returns true when no sessions are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_token.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use crate::auth::TOKEN_LIFETIME_MS;

    use super::*;

    fn later(now: Timestamp, millis: i64) -> Timestamp {
        Timestamp::from_millis(now.as_millis() + millis)
    }

    #[test]
    fn a_signed_in_token_resolves_to_its_account() {
        let mut sessions = Sessions::new();
        let account = AccountId::new();
        let now = Timestamp::now();

        let token = sessions.sign_in(account, now).expect("issues");
        assert_eq!(
            sessions.account_for(&token.token, now).expect("valid"),
            account
        );
    }

    #[test]
    fn an_unknown_token_is_refused() {
        let sessions = Sessions::new();
        let error = sessions
            .account_for("not a real token", Timestamp::now())
            .expect_err("refused");
        assert!(matches!(error, AuthError::InvalidToken));
    }

    #[test]
    fn an_expired_token_is_refused_the_same_way_an_unknown_one_is() {
        // Same message, so the difference reveals nothing.
        let mut sessions = Sessions::new();
        let now = Timestamp::from_millis(1_000_000);
        let token = sessions.sign_in(AccountId::new(), now).expect("issues");

        let expired = sessions
            .account_for(&token.token, later(now, TOKEN_LIFETIME_MS))
            .expect_err("expired");
        let unknown = sessions.account_for("nope", now).expect_err("unknown");
        assert_eq!(expired.to_string(), unknown.to_string());
    }

    #[test]
    fn a_token_is_valid_right_up_to_its_expiry() {
        let mut sessions = Sessions::new();
        let now = Timestamp::from_millis(1_000_000);
        let token = sessions.sign_in(AccountId::new(), now).expect("issues");

        assert!(sessions
            .account_for(&token.token, later(now, TOKEN_LIFETIME_MS - 1))
            .is_ok());
        assert!(sessions
            .account_for(&token.token, later(now, TOKEN_LIFETIME_MS))
            .is_err());
    }

    #[test]
    fn signing_out_ends_the_session_immediately() {
        let mut sessions = Sessions::new();
        let now = Timestamp::now();
        let token = sessions.sign_in(AccountId::new(), now).expect("issues");

        assert!(sessions.sign_out(&token.token));
        assert!(sessions.account_for(&token.token, now).is_err());
        assert!(
            !sessions.sign_out(&token.token),
            "signing out twice is not an error"
        );
        assert!(sessions.is_empty());
    }

    #[test]
    fn changing_a_password_can_invalidate_every_token_for_that_account() {
        let mut sessions = Sessions::new();
        let account = AccountId::new();
        let other = AccountId::new();
        let now = Timestamp::now();

        let first = sessions.sign_in(account, now).expect("issues");
        let second = sessions.sign_in(account, now).expect("issues");
        let untouched = sessions.sign_in(other, now).expect("issues");

        assert_eq!(sessions.sign_out_account(account), 2);
        assert!(sessions.account_for(&first.token, now).is_err());
        assert!(sessions.account_for(&second.token, now).is_err());
        assert!(sessions.account_for(&untouched.token, now).is_ok());
    }

    #[test]
    fn purging_removes_only_expired_sessions() {
        let mut sessions = Sessions::new();
        let early = Timestamp::from_millis(1_000_000);
        let old = sessions.sign_in(AccountId::new(), early).expect("issues");

        let late = later(early, TOKEN_LIFETIME_MS / 2);
        let fresh = sessions.sign_in(AccountId::new(), late).expect("issues");

        let at = later(early, TOKEN_LIFETIME_MS);
        assert_eq!(sessions.purge_expired(at), 1);
        assert_eq!(sessions.len(), 1);
        assert!(sessions.account_for(&fresh.token, at).is_ok());
        assert!(sessions.account_for(&old.token, at).is_err());
    }

    #[test]
    fn several_sessions_can_exist_for_one_account() {
        // One operator on a laptop and a tablet is an ordinary situation, even
        // with a single account.
        let mut sessions = Sessions::new();
        let account = AccountId::new();
        let now = Timestamp::now();

        let laptop = sessions.sign_in(account, now).expect("issues");
        let tablet = sessions.sign_in(account, now).expect("issues");

        assert_ne!(laptop.token, tablet.token);
        assert_eq!(
            sessions.account_for(&laptop.token, now).expect("valid"),
            account
        );
        assert_eq!(
            sessions.account_for(&tablet.token, now).expect("valid"),
            account
        );

        sessions.sign_out(&laptop.token);
        assert!(
            sessions.account_for(&tablet.token, now).is_ok(),
            "one sign-out is not all"
        );
    }
}
