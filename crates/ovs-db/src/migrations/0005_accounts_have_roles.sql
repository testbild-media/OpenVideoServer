-- Roles on accounts (owner, 2026-09-24).
--
-- "Mir fehlt noch für den Login eine Benutzerverwaltung… Admin und Operator
-- getrennt." Q7 settled V0.x on a single operator/admin account and said in the
-- same breath that the model must not make more accounts impossible later. This
-- is the later.
--
-- **Every account that already exists becomes an admin.** There is at most one
-- of them on any server this migration runs on, it is the account the server
-- was set up with, and it is the only thing that can create the second — so a
-- machine that upgrades must not wake up with nobody able to administer it.
-- That is why the default here is `admin` and not the safer `operator`: it
-- applies to exactly the rows that predate the choice, and `Role::default()` in
-- the model is `operator` for everything created after it.
--
-- Stored as the same text the API sends, so a row can be read by eye. A value
-- this build does not know is an error rather than a fall back to the lesser
-- role: a row written by a newer build is not a row whose authority to guess.

ALTER TABLE accounts ADD COLUMN role TEXT NOT NULL DEFAULT 'admin';
