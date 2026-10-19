//! Server settings and accounts.
//!
//! Settings describe the machine, not the show: the startup policy, connector
//! roles, network configuration. They are deliberately not part of any show,
//! because moving a show between servers must not move the hardware
//! configuration with it.

use std::collections::BTreeMap;

use ovs_types::account::Role;
use ovs_types::config::{ConnectorRole, DmxProtocols, StartupPolicy};
use ovs_types::id::AccountId;
use ovs_types::time::Timestamp;
use rusqlite::{params, OptionalExtension};

use crate::database::Database;
use crate::error::{DbError, Result};

/// Key under which the startup policy is stored.
const STARTUP_POLICY: &str = "startup_policy";

/// The key previews are stored under.
const PREVIEWS_ENABLED: &str = "previews_enabled";

/// The key the DMX switch in Live is stored under.
const DMX_ENABLED: &str = "dmx_enabled";

/// The key connector roles are stored under.
const CONNECTOR_ROLES: &str = "connector_roles";

/// Returns the key one connector's role is stored under.
///
/// The GPU as well as the connector, because `DP-1` exists on every card in the
/// machine and a two-GPU server would otherwise have one role for two sockets.
#[must_use]
pub fn connector_key(gpu_pci_address: &str, connector: &str) -> String {
    format!("{gpu_pci_address}/{connector}")
}

/// An account somebody signs in with.
///
/// The password hash is opaque here: hashing and verification belong to the
/// server core, which owns the algorithm choice and its parameters. Storage only
/// keeps the string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Stable identifier.
    pub id: AccountId,
    /// Login name.
    pub username: String,
    /// Opaque password hash, produced and verified by the server core.
    pub password_hash: String,
    /// What this account is allowed to do.
    pub role: Role,
    /// When the account was created.
    pub created_at: Timestamp,
    /// When the account last signed in.
    pub last_login_at: Option<Timestamp>,
}

/// Turns one stored row into an account.
///
/// One function rather than three copies of the same six columns: they are read
/// by identifier, by username and all at once, and a role parsed one way in two
/// of those and another way in the third is exactly the kind of difference
/// nobody finds until somebody cannot sign in.
fn account_from(
    (id, username, password_hash, role, created_at, last_login_at): (
        String,
        String,
        String,
        String,
        i64,
        Option<i64>,
    ),
) -> Result<Account> {
    Ok(Account {
        id: id
            .parse()
            .map_err(|_| DbError::Conflict(format!("malformed account id {id}")))?,
        username,
        password_hash,
        // Refused rather than read as the lesser role. A row written by a newer
        // build is not a row whose authority this one should guess, in either
        // direction: guessing low locks somebody out of their own server and
        // guessing high hands out the machine.
        role: role
            .parse()
            .map_err(|_| DbError::Conflict(format!("unknown account role {role}")))?,
        created_at: Timestamp::from_millis(created_at),
        last_login_at: last_login_at.map(Timestamp::from_millis),
    })
}

impl Database {
    /// Stores a server setting as a JSON value.
    ///
    /// # Errors
    ///
    /// Returns an error if the value cannot be encoded or the write fails.
    pub fn set_setting<T: serde::Serialize>(&mut self, key: &str, value: &T) -> Result<()> {
        let encoded = serde_json::to_string(value).map_err(|source| DbError::Encode {
            entity: "server setting",
            source,
        })?;
        self.connection().execute(
            "INSERT INTO server_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, encoded, Timestamp::now().as_millis()],
        )?;
        Ok(())
    }

    /// Reads a server setting, or `None` when it has never been set.
    ///
    /// # Errors
    ///
    /// Returns an error if the stored value cannot be decoded.
    pub fn setting<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let encoded: Option<String> = self
            .connection()
            .query_row(
                "SELECT value FROM server_settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;

        encoded
            .map(|encoded| {
                serde_json::from_str(&encoded).map_err(|source| DbError::Decode {
                    entity: "server setting",
                    source,
                })
            })
            .transpose()
    }

    /// Returns what this server opens on start.
    ///
    /// Defaults to [`StartupPolicy::LastActive`] on a server that has never been
    /// configured (owner decision Q6).
    ///
    /// # Errors
    ///
    /// Returns an error if the stored value cannot be decoded.
    pub fn startup_policy(&self) -> Result<StartupPolicy> {
        Ok(self.setting(STARTUP_POLICY)?.unwrap_or_default())
    }

    /// Which DMX protocols reach the show.
    ///
    /// Both when nothing has been stored, which is what a server nobody has
    /// configured should do: a desk that is plugged in and sending should reach
    /// the wall.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn dmx_protocols(&self) -> Result<DmxProtocols> {
        Ok(self.setting("dmx_protocols")?.unwrap_or_default())
    }

    /// Records which DMX protocols reach the show.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn set_dmx_protocols(&mut self, protocols: DmxProtocols) -> Result<()> {
        self.set_setting("dmx_protocols", &protocols)
    }

    /// Whether the DMX switch in Live was on when it was last pressed.
    ///
    /// The one switch between the desk and the client (owner, 2026-10-19), and
    /// the server's rather than a show's: a restart brings it back as it was
    /// left. On when it has never been pressed, because a server is normally
    /// driven from the desk.
    ///
    /// # Errors
    ///
    /// Returns an error if the stored value cannot be decoded.
    pub fn dmx_enabled(&self) -> Result<bool> {
        Ok(self.setting(DMX_ENABLED)?.unwrap_or(true))
    }

    /// Records the DMX switch.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn set_dmx_enabled(&mut self, enabled: bool) -> Result<()> {
        self.set_setting(DMX_ENABLED, &enabled)
    }

    /// Returns whether the renderer should produce previews.
    ///
    /// On unless somebody turned them off. A preview costs a readback from the
    /// GPU and an encode, which is small but not nothing, and on a machine at
    /// its limit an operator needs to be able to take that back (
    /// §26).
    ///
    /// # Errors
    ///
    /// Returns an error if the stored value cannot be decoded.
    pub fn previews_enabled(&self) -> Result<bool> {
        Ok(self.setting(PREVIEWS_ENABLED)?.unwrap_or(true))
    }

    /// Sets whether the renderer should produce previews.
    ///
    /// Persisted, because an operator who turned previews off to get a show
    /// through the night should not find them back on after a reboot.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn set_previews_enabled(&mut self, enabled: bool) -> Result<()> {
        self.set_setting(PREVIEWS_ENABLED, &enabled)
    }

    /// Returns what each connector is used for, keyed by `gpu/connector`.
    ///
    /// A connector nobody has said anything about is absent rather than
    /// `Unused`: "nothing has been decided" and "decided to leave it alone" are
    /// different states, and a server that treated the first as the second would
    /// silently refuse to drive a wall an operator has just plugged in.
    ///
    /// # Errors
    ///
    /// Returns an error if the stored value cannot be decoded.
    pub fn connector_roles(&self) -> Result<BTreeMap<String, ConnectorRole>> {
        Ok(self.setting(CONNECTOR_ROLES)?.unwrap_or_default())
    }

    /// Sets what one connector is used for.
    ///
    /// Machine configuration, not show data: moving a show between servers
    /// must not move the wiring with it.
    ///
    /// # Errors
    ///
    /// Returns an error if the read or the write fails.
    pub fn set_connector_role(
        &mut self,
        gpu_pci_address: &str,
        connector: &str,
        role: ConnectorRole,
    ) -> Result<()> {
        let mut roles = self.connector_roles()?;
        roles.insert(connector_key(gpu_pci_address, connector), role);
        self.set_setting(CONNECTOR_ROLES, &roles)
    }

    /// Sets what this server opens on start.
    ///
    /// # Errors
    ///
    /// Returns an error if the pinned show does not exist, or if the write fails.
    pub fn set_startup_policy(&mut self, policy: StartupPolicy) -> Result<()> {
        if let StartupPolicy::Pinned { show_id } = policy {
            if !self.show_exists(show_id)? {
                return Err(DbError::NotFound {
                    entity: "show",
                    id: show_id.to_string(),
                });
            }
        }
        self.set_setting(STARTUP_POLICY, &policy)
    }

    /// Creates or replaces an account.
    ///
    /// # Errors
    ///
    /// Returns an error if the username is taken by a different account, or if
    /// the change would leave the server with no administrator.
    pub fn upsert_account(&mut self, account: &Account) -> Result<()> {
        if account.username.trim().is_empty() {
            return Err(DbError::Conflict("an account needs a username".to_owned()));
        }
        if account.password_hash.is_empty() {
            return Err(DbError::Conflict(
                "an account needs a password hash".to_owned(),
            ));
        }

        // **A server always has somebody who can administer it.** Checked here
        // rather than in the route, because this is the only door: a machine
        // whose last admin has just been demoted is a machine nobody can create
        // an account on, change a password on or point the Service Display at,
        // and the only way back is a database editor.
        if !account.role.may_administer() && self.is_last_admin(account.id)? {
            return Err(DbError::Conflict(
                "this is the last account that can administer this server".to_owned(),
            ));
        }

        self.connection().execute(
            "INSERT INTO accounts (id, username, password_hash, role, created_at, last_login_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                 username      = excluded.username,
                 password_hash = excluded.password_hash,
                 role          = excluded.role,
                 last_login_at = excluded.last_login_at",
            params![
                account.id.to_string(),
                account.username,
                account.password_hash,
                account.role.as_str(),
                account.created_at.as_millis(),
                account.last_login_at.map(Timestamp::as_millis),
            ],
        )?;
        Ok(())
    }

    /// Whether this account is the only administrator the server has.
    ///
    /// False for an account that does not exist yet, which is what a new admin
    /// being created looks like from here.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn is_last_admin(&self, id: AccountId) -> Result<bool> {
        let admins: u32 = self.connection().query_row(
            "SELECT COUNT(*) FROM accounts WHERE role = ?1",
            params![Role::Admin.as_str()],
            |row| row.get(0),
        )?;
        if admins > 1 {
            return Ok(false);
        }
        let is_admin: u32 = self.connection().query_row(
            "SELECT COUNT(*) FROM accounts WHERE id = ?1 AND role = ?2",
            params![id.to_string(), Role::Admin.as_str()],
            |row| row.get(0),
        )?;
        Ok(is_admin == 1)
    }

    /// Every account, oldest first.
    ///
    /// The list a user management page draws. Hashes come with them because the
    /// type carries one; nothing outside this crate has a reason to look, and
    /// the API type that leaves the server does not have the field at all.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or a stored row is malformed.
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT id, username, password_hash, role, created_at, last_login_at
             FROM accounts ORDER BY created_at, username",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })?;

        let mut accounts = Vec::new();
        for row in rows {
            accounts.push(account_from(row?)?);
        }
        Ok(accounts)
    }

    /// Looks an account up by its identifier.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or the stored row is malformed.
    pub fn account(&self, id: AccountId) -> Result<Option<Account>> {
        self.connection()
            .query_row(
                "SELECT id, username, password_hash, role, created_at, last_login_at
                 FROM accounts WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?
            .map(account_from)
            .transpose()
    }

    /// Removes an account.
    ///
    /// Returns whether there was one to remove.
    ///
    /// # Errors
    ///
    /// Returns an error if this is the last account that can administer the
    /// server, or if the write fails.
    pub fn delete_account(&mut self, id: AccountId) -> Result<bool> {
        if self.is_last_admin(id)? {
            return Err(DbError::Conflict(
                "this is the last account that can administer this server".to_owned(),
            ));
        }
        let removed = self.connection().execute(
            "DELETE FROM accounts WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(removed > 0)
    }

    /// Looks an account up by login name.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or a stored row is malformed.
    pub fn account_by_username(&self, username: &str) -> Result<Option<Account>> {
        self.connection()
            .query_row(
                "SELECT id, username, password_hash, role, created_at, last_login_at
                 FROM accounts WHERE username = ?1",
                params![username],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?
            .map(account_from)
            .transpose()
    }

    /// Returns how many accounts exist.
    ///
    /// The server uses this to decide whether first-run setup is needed.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn account_count(&self) -> Result<u32> {
        Ok(self
            .connection()
            .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))?)
    }

    /// Records a successful sign-in.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn record_login(&mut self, id: AccountId, at: Timestamp) -> Result<()> {
        self.connection().execute(
            "UPDATE accounts SET last_login_at = ?1 WHERE id = ?2",
            params![at.as_millis(), id.to_string()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::Show;
    use ovs_types::id::ShowId;

    use super::*;

    fn account() -> Account {
        admin("operator")
    }

    /// An account that can administer the server.
    fn admin(username: &str) -> Account {
        Account {
            id: AccountId::new(),
            username: username.to_owned(),
            password_hash: "$argon2id$placeholder".to_owned(),
            role: Role::Admin,
            created_at: Timestamp::now(),
            last_login_at: None,
        }
    }

    /// An account that can drive the show and nothing else.
    fn operator(username: &str) -> Account {
        Account {
            role: Role::Operator,
            ..admin(username)
        }
    }

    #[test]
    fn the_dmx_switch_comes_back_as_it_was_left() {
        let mut database = Database::open_in_memory().expect("opens");
        assert!(
            database.dmx_enabled().expect("reads"),
            "a server nobody has touched is driven from the desk"
        );
        database.set_dmx_enabled(false).expect("writes");
        assert!(!database.dmx_enabled().expect("reads"));
        database.set_dmx_enabled(true).expect("writes");
        assert!(database.dmx_enabled().expect("reads"));
    }

    #[test]
    fn a_server_cannot_be_left_with_nobody_who_can_administer_it() {
        // **The rule this whole model hangs on.** A machine whose last admin has
        // been demoted or deleted is a machine nobody can create an account on,
        // change a password on or point the Service Display at — and the only
        // way back is a database editor, in a rack, during a show.
        let mut database = Database::open_in_memory().expect("opens");
        let first = admin("chief");
        database.upsert_account(&first).expect("the first admin");

        // Demoting the only one.
        let demoted = Account {
            role: Role::Operator,
            ..first.clone()
        };
        assert!(
            database.upsert_account(&demoted).is_err(),
            "the last admin was demoted"
        );
        assert!(database.delete_account(first.id).is_err(), "and deleted");

        // With a second admin, either is allowed, and the one that is left
        // cannot then be removed in turn.
        let second = admin("stefan");
        database.upsert_account(&second).expect("a second admin");
        database
            .upsert_account(&demoted)
            .expect("now it may be demoted");
        assert!(
            database.delete_account(second.id).is_err(),
            "which makes the second one the last"
        );

        // An operator is not load-bearing and goes either way.
        let guest = operator("guest");
        database.upsert_account(&guest).expect("an operator");
        assert!(database.delete_account(guest.id).expect("removable"));
        assert!(
            !database.delete_account(guest.id).expect("already gone"),
            "removing one twice is not an error, it is a no-op"
        );
    }

    #[test]
    fn an_account_keeps_its_role_through_storage() {
        let mut database = Database::open_in_memory().expect("opens");
        database.upsert_account(&admin("chief")).expect("stores");
        let guest = operator("guest");
        database.upsert_account(&guest).expect("stores");

        let read = database
            .account_by_username("guest")
            .expect("reads")
            .expect("there");
        assert_eq!(read.role, Role::Operator);
        assert_eq!(
            database.account(guest.id).expect("reads").expect("there"),
            read,
            "by identifier and by name are the same account"
        );

        let listed = database.accounts().expect("lists");
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed
                .iter()
                .filter(|each| each.role.may_administer())
                .count(),
            1
        );
    }

    #[test]
    fn a_role_this_build_cannot_read_is_refused_rather_than_guessed() {
        // What a database written by a newer build looks like from here.
        // Reading it as the lesser role locks somebody out of their own server;
        // reading it as the greater hands out the machine. Neither is a guess
        // worth making.
        let mut database = Database::open_in_memory().expect("opens");
        database.upsert_account(&admin("chief")).expect("stores");
        database
            .connection_for_test()
            .execute(
                "UPDATE accounts SET role = 'superuser' WHERE username = 'chief'",
                [],
            )
            .expect("writes");

        assert!(database.account_by_username("chief").is_err());
        assert!(database.accounts().is_err());
    }

    #[test]
    fn an_account_that_predates_roles_can_still_administer_its_server() {
        // Migration 5's whole job. The one account a server was set up with is
        // the only thing that can create the second, so a machine that upgrades
        // must not wake up unable to administer itself.
        let database = Database::open_in_memory().expect("opens");
        database
            .connection_for_test()
            .execute(
                "INSERT INTO accounts (id, username, password_hash, created_at, last_login_at)
                 VALUES (?1, 'chief', '$argon2id$placeholder', 0, NULL)",
                params![AccountId::new().to_string()],
            )
            .expect("a row written the way the old build wrote them");

        let account = database
            .account_by_username("chief")
            .expect("reads")
            .expect("there");
        assert_eq!(account.role, Role::Admin);
    }

    #[test]
    fn an_unconfigured_server_opens_the_last_active_show() {
        let database = Database::open_in_memory().expect("opens");
        assert_eq!(
            database.startup_policy().expect("reads"),
            StartupPolicy::LastActive
        );
    }

    #[test]
    fn the_startup_policy_round_trips() {
        let mut database = Database::open_in_memory().expect("opens");
        let show = Show::new("Main Show");
        database.save_show(&show).expect("saves");

        database
            .set_startup_policy(StartupPolicy::Pinned { show_id: show.id })
            .expect("pins");
        assert_eq!(
            database.startup_policy().expect("reads"),
            StartupPolicy::Pinned { show_id: show.id }
        );

        database
            .set_startup_policy(StartupPolicy::None)
            .expect("clears");
        assert_eq!(
            database.startup_policy().expect("reads"),
            StartupPolicy::None
        );
    }

    #[test]
    fn pinning_a_show_that_does_not_exist_is_refused() {
        // Otherwise the server comes up black after a reboot with no explanation.
        let mut database = Database::open_in_memory().expect("opens");
        let error = database
            .set_startup_policy(StartupPolicy::Pinned {
                show_id: ShowId::new(),
            })
            .expect_err("no such show");
        assert!(matches!(error, DbError::NotFound { entity: "show", .. }));
        assert_eq!(
            database.startup_policy().expect("reads"),
            StartupPolicy::LastActive
        );
    }

    #[test]
    fn settings_round_trip_arbitrary_values() {
        let mut database = Database::open_in_memory().expect("opens");
        database
            .set_setting("network.interface", &"eth0")
            .expect("writes");
        assert_eq!(
            database
                .setting::<String>("network.interface")
                .expect("reads"),
            Some("eth0".to_owned())
        );
        assert_eq!(
            database.setting::<String>("never.set").expect("reads"),
            None
        );
    }

    #[test]
    fn writing_a_setting_twice_replaces_it() {
        let mut database = Database::open_in_memory().expect("opens");
        database.set_setting("key", &1_u32).expect("writes");
        database.set_setting("key", &2_u32).expect("writes");
        assert_eq!(database.setting::<u32>("key").expect("reads"), Some(2));
    }

    #[test]
    fn an_account_round_trips() {
        let mut database = Database::open_in_memory().expect("opens");
        assert_eq!(database.account_count().expect("counts"), 0);

        let account = account();
        database.upsert_account(&account).expect("creates");

        assert_eq!(database.account_count().expect("counts"), 1);
        assert_eq!(
            database.account_by_username("operator").expect("reads"),
            Some(account)
        );
        assert_eq!(database.account_by_username("nobody").expect("reads"), None);
    }

    #[test]
    fn an_account_needs_a_username_and_a_hash() {
        let mut database = Database::open_in_memory().expect("opens");
        assert!(database
            .upsert_account(&Account {
                username: "  ".to_owned(),
                ..account()
            })
            .is_err());
        assert!(database
            .upsert_account(&Account {
                password_hash: String::new(),
                ..account()
            })
            .is_err());
        assert_eq!(database.account_count().expect("counts"), 0);
    }

    #[test]
    fn two_accounts_cannot_share_a_username() {
        // V0.x ships one account, but the constraint has to hold from the start
        // or adding accounts later becomes a migration.
        let mut database = Database::open_in_memory().expect("opens");
        database.upsert_account(&account()).expect("creates");

        let clash = Account {
            id: AccountId::new(),
            ..account()
        };
        assert!(database.upsert_account(&clash).is_err());
    }

    #[test]
    fn signing_in_is_recorded() {
        let mut database = Database::open_in_memory().expect("opens");
        let account = account();
        database.upsert_account(&account).expect("creates");

        let at = Timestamp::from_millis(1_700_000_000_000);
        database.record_login(account.id, at).expect("records");

        let reloaded = database
            .account_by_username("operator")
            .expect("reads")
            .expect("exists");
        assert_eq!(reloaded.last_login_at, Some(at));
    }
}
