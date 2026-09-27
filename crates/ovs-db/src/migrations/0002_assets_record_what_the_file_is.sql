-- The internal playback profile is removed.
--
-- The server no longer prepares media to a format of its own choosing, so an
-- asset has nothing to record a profile of. What it records instead is what the
-- file turned out to be, in the `codec` and `container` columns that were
-- already there and described as diagnostics.
--
-- Deduplication was by (sha256, profile_version), which asked "have we got
-- these bytes, prepared this way". Without profiles the question is the one
-- content addressing was for in the first place: have we got these bytes.

-- Two assets could hold the same bytes under different profile versions. In
-- practice none do — no released build ever wrote a profile_version but 1 —
-- but the unique index below would fail to build if any did, and a migration
-- that cannot run is a server that cannot start. The row kept is the one that
-- would have been chosen for playback: the old query ordered by profile_version
-- descending, and rowid follows insertion order, so the last one in wins.
DELETE FROM media_assets
WHERE rowid NOT IN (SELECT MAX(rowid) FROM media_assets GROUP BY sha256);

DROP INDEX media_assets_content;

ALTER TABLE media_assets DROP COLUMN profile;
ALTER TABLE media_assets DROP COLUMN profile_version;

-- The same bytes are one asset, however many times they are imported
--.
CREATE UNIQUE INDEX media_assets_content ON media_assets (sha256);
