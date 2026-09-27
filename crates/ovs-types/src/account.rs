//! Who may do what.
//!
//! Q7 settled on one operator/admin account for V0.x and said the model must
//! not make more accounts impossible later. The owner asked for the rest on
//! 2026-09-24: "Mir fehlt noch für den Login eine Benutzerverwaltung… Admin und
//! Operator getrennt."
//!
//! Two roles and no more. The question a role answers here is the one a venue
//! actually asks — *may this person change the machine, or only drive the show*
//! — and a guest VJ at the desk is the case it exists for.

use serde::{Deserialize, Serialize};

/// What an account is allowed to do.
///
/// Ordered by authority, so `>=` is a meaningful test and
/// [`Role::may_administer`] is the only place that decides what that means.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Drives the show: everything Live Mode and the show editor can reach.
    ///
    /// The default, because it is the safe half. An account created without a
    /// role said is an account nobody decided to trust with the machine.
    #[default]
    Operator,
    /// Everything an operator may do, and the machine itself.
    ///
    /// Accounts, passwords other than their own, and the system settings — the
    /// Service Display's output among them. The first account a server has is
    /// this, because somebody has to be able to make the second.
    Admin,
}

impl Role {
    /// Whether this role may change the machine and the accounts on it.
    ///
    /// The single place that answers it. A handler that asked
    /// `role == Role::Admin` directly would be a second answer to go out of
    /// step with this one the day a third role exists.
    #[must_use]
    pub const fn may_administer(self) -> bool {
        matches!(self, Self::Admin)
    }

    /// Every role, for interfaces that offer a choice.
    pub const ALL: [Self; 2] = [Self::Operator, Self::Admin];

    /// The name this role is stored and sent under.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Admin => "admin",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(self.as_str())
    }
}

impl std::str::FromStr for Role {
    type Err = UnknownRole;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "operator" => Ok(Self::Operator),
            "admin" => Ok(Self::Admin),
            other => Err(UnknownRole(other.to_owned())),
        }
    }
}

/// A role name that is not one of ours.
///
/// What a database written by a newer build looks like from here. It is an
/// error rather than a silent fall back to `Operator`: a row this build cannot
/// read is not a row it should guess the authority of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownRole(pub String);

impl std::fmt::Display for UnknownRole {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "unknown role: {}", self.0)
    }
}

impl std::error::Error for UnknownRole {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_survives_the_round_trip_it_is_stored_through() {
        for role in Role::ALL {
            assert_eq!(role.as_str().parse::<Role>().expect("reads back"), role);
            assert_eq!(
                serde_json::from_str::<Role>(&serde_json::to_string(&role).expect("writes"))
                    .expect("reads"),
                role
            );
        }
    }

    #[test]
    fn only_an_admin_administers() {
        assert!(Role::Admin.may_administer());
        assert!(!Role::Operator.may_administer());
    }

    #[test]
    fn an_account_with_no_role_said_is_the_safe_half() {
        // What a row written before roles existed, or a request that left the
        // field out, has to mean. Trusting it with the machine would be a
        // default that hands out authority nobody asked to give.
        assert_eq!(Role::default(), Role::Operator);
    }

    #[test]
    fn a_role_this_build_does_not_know_is_refused_rather_than_guessed() {
        assert!("superuser".parse::<Role>().is_err());
        assert!("".parse::<Role>().is_err());
    }
}
