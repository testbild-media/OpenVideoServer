-- The stored document is a show (owner, 2026-09-24).
--
-- "Show fände ich als richtigen begriff. projekt hatte ich damals nur von dir
-- übernommen." The word was mine and it was the wrong one: the specification
-- uses *show* for the thing a server holds and runs, and the only place
-- "project" ever came from is me.
--
-- So the tables say what the rows are. Nothing about the rows themselves
-- changes: same identifiers, same documents, same timestamps. A server that
-- upgrades keeps the show it had open and every show it had stored.
--
-- `live_snapshots.project_id` is renamed as well. SQLite carries the foreign
-- key across a table rename on its own — `legacy_alter_table` is off by default
-- since 3.25 — so the reference to `shows (id)` is right afterwards without
-- being restated.

ALTER TABLE projects RENAME TO shows;
ALTER TABLE project_folders RENAME TO show_folders;
ALTER TABLE live_snapshots RENAME COLUMN project_id TO show_id;

-- Indexes keep the name they were created with, which would leave two of them
-- called `projects_*` on a table called `shows`. Renamed so that somebody
-- reading the schema in a year is not told a story about a table that is not
-- there.
DROP INDEX IF EXISTS projects_updated_at;
DROP INDEX IF EXISTS projects_folder;
CREATE INDEX shows_updated_at ON shows (updated_at DESC);
CREATE INDEX shows_folder ON shows (folder);
