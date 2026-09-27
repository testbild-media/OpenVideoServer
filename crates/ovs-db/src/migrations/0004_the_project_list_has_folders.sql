-- Folders for projects, the same idea as the library's (owner, 2026-09-23).
--
-- "Projekte sollten ebenfalls Ordner und Unterordner haben können." A shop with
-- forty shows on one server has forty rows in one list, and the year, the
-- client and the venue are the three ways anybody looks for one.
--
-- A folder is a name a project is filed under and nothing else. The document is
-- untouched, and so is its identity: a project plays whatever it played,
-- wherever it is filed. Folders are a table of their own for the same reason
-- the library's are — an operator makes one in order to put things in it, and
-- one derived from the projects in it could not exist while it was empty.
--
-- **Not on the document.** Where a project is filed is a fact about this
-- server's list rather than about the show, so a project exported and carried
-- to another machine arrives at the root of whatever that machine has. The
-- alternative would be a folder path travelling between two operators'
-- filing systems, which is a folder nobody made.

CREATE TABLE project_folders (
    -- The whole path, slash-separated, for example '2026/Summer tour'. The path
    -- is the identity: nothing points at a folder except the projects filed
    -- under it, and those name it the same way.
    path       TEXT    PRIMARY KEY,
    created_at INTEGER NOT NULL
) STRICT;

-- Where a project is filed. Empty is the root, which is where every project
-- that already exists is.
ALTER TABLE projects ADD COLUMN folder TEXT NOT NULL DEFAULT '';

CREATE INDEX projects_folder ON projects (folder);
