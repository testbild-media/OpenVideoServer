-- Initial schema.
--
-- Two storage strategies, chosen deliberately:
--
--   Projects are documents. A project is loaded and saved whole, by the client
--   and by the renderer, and nothing queries inside one. Storing it as JSON with
--   its schema version beside it keeps the migration story in the type system,
--   where it is already tested, instead of spreading it across a dozen tables.
--
--   Media is relational. The library is queried, filtered, counted and
--   deduplicated, so it earns real columns and real indexes.
--
-- STRICT tables throughout: SQLite otherwise accepts a string where an integer
-- was declared, which would let a corrupt write through unnoticed.

CREATE TABLE projects (
    id             TEXT    PRIMARY KEY,
    name           TEXT    NOT NULL,
    schema_version INTEGER NOT NULL,
    document       TEXT    NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
) STRICT;

CREATE INDEX projects_updated_at ON projects (updated_at DESC);

-- A logical media item: what an operator sees and what a project references.
-- Carries no physical location at all; that lives on media_assets.
CREATE TABLE media_items (
    id                TEXT    PRIMARY KEY,
    display_name      TEXT    NOT NULL,
    original_filename TEXT    NOT NULL,
    kind              TEXT    NOT NULL,
    state             TEXT    NOT NULL,
    duration_ms       INTEGER,
    width             INTEGER,
    height            INTEGER,
    fps_numerator     INTEGER,
    fps_denominator   INTEGER,
    has_alpha         INTEGER NOT NULL,
    failure_reason    TEXT,
    created_at        INTEGER NOT NULL
) STRICT;

CREATE INDEX media_items_state ON media_items (state);
CREATE INDEX media_items_created_at ON media_items (created_at DESC);

-- One physical representation of a media item.
CREATE TABLE media_assets (
    id                TEXT    PRIMARY KEY,
    media_item_id     TEXT    NOT NULL REFERENCES media_items (id) ON DELETE CASCADE,
    profile           TEXT    NOT NULL,
    profile_version   INTEGER NOT NULL,
    object_key        TEXT    NOT NULL,
    sha256            TEXT    NOT NULL,
    byte_size         INTEGER NOT NULL,
    container         TEXT    NOT NULL,
    codec             TEXT    NOT NULL,
    validation_state  TEXT    NOT NULL,
    validated_at      INTEGER,
    validation_reason TEXT,
    created_at        INTEGER NOT NULL
) STRICT;

-- Deduplication: the same bytes prepared for the same profile version are one
-- asset, however many times they are imported.
CREATE UNIQUE INDEX media_assets_content ON media_assets (sha256, profile_version);

CREATE INDEX media_assets_item ON media_assets (media_item_id);

-- The small amount of live state that survives a restart.
-- Separate from the project on purpose: restoring it must never write live
-- values into project configuration.
CREATE TABLE live_snapshots (
    project_id TEXT    PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    document   TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- Server configuration: startup policy, connector roles, network settings.
-- Describes the machine, not the show, so it is not part of any project.
CREATE TABLE server_settings (
    key        TEXT    PRIMARY KEY,
    value      TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- V0.x has one operator account per server and no roles. The table is shaped so
-- that a second account is an INSERT rather than a migration.
CREATE TABLE accounts (
    id            TEXT    PRIMARY KEY,
    username      TEXT    NOT NULL,
    password_hash TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER
) STRICT;

CREATE UNIQUE INDEX accounts_username ON accounts (username);
