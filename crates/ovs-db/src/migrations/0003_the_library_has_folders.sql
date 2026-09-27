-- Folders in the library, which never touch the files (owner, 2026-09-23).
--
-- "Virtual folders: organising media must not move the real files." So a folder
-- is a name an item is filed under, and nothing else. Assets keep their object
-- keys, content addressing keeps working, and moving a clip between folders
-- writes one string.
--
-- Folders are a table of their own rather than only a column on the item,
-- because an operator makes a folder in order to put things in it. One derived
-- from the items in it could not exist while it was empty, and would vanish
-- between being made and being filled.

CREATE TABLE media_folders (
    -- The whole path, slash-separated, for example 'Intros/2026'. The path is
    -- the identity: there is no separate id, because nothing points at a folder
    -- except the items filed under it, and those name it the same way.
    path       TEXT    PRIMARY KEY,
    created_at INTEGER NOT NULL
) STRICT;

-- Where an item is filed. Empty is the root, which is where everything that
-- has ever been imported already is.
ALTER TABLE media_items ADD COLUMN folder TEXT NOT NULL DEFAULT '';

CREATE INDEX media_items_folder ON media_items (folder);
