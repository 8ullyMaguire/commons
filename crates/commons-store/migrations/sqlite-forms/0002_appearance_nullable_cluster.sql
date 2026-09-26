-- SQLite form of: ALTER TABLE appearance ALTER COLUMN cluster_id DROP NOT NULL;
--
-- SQLite cannot alter a column's nullability in place. The supported way is
-- the table rebuild: create the new table, copy, drop, rename, and recreate the
-- indexes. The steps below are in that order deliberately -- `DROP TABLE
-- appearance_old` has to come after the copy, and the rename has to come after
-- the drop, or the foreign key from nothing to it is left dangling.
--
-- The rebuild preserves the `UNIQUE (object_id, cluster_id, appearance_type)`
-- constraint. That constraint has a subtlety worth recording now that
-- `cluster_id` is nullable: in SQL, `NULL` never equals `NULL` for the purpose
-- of a unique constraint, so two ambiguous appearances of the *same object* are
-- both allowed. That is the desired behaviour -- a face that fits two people
-- gets recorded once per doubt, and a re-run that is still ambiguous records
-- another -- so no change is needed. What it does mean is that a unique
-- constraint is not the way to make a face recorded once, and nothing should
-- rely on that.
--
-- `PRAGMA foreign_keys` is NOT changed here. Turning it off to do the rebuild
-- would be shorter, but it makes the rebuild's correctness depend on no other
-- connection being mid-write, and the twelve steps do not need it.
--
-- The two partial indexes are created after this block, from the shared
-- migration body: SQLite has supported partial indexes since 3.8.0, so they are
-- the same statements on both engines.

CREATE TABLE appearance_new (
  id              TEXT PRIMARY KEY,
  object_id       TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  cluster_id      TEXT REFERENCES person_cluster(id) ON DELETE CASCADE,
  appearance_type TEXT NOT NULL DEFAULT 'primary',
  face_id         TEXT,
  -- Composite score components are stored, not just the result: §7.4
  -- requires the UI to be able to say *why* two items were linked, and one
  -- opaque float makes that impossible.
  distance        REAL,
  face_score      REAL,
  body_score      REAL,
  ambiguous       INTEGER NOT NULL DEFAULT 0,
  source          TEXT NOT NULL,
  created_at      TEXT NOT NULL,
  UNIQUE (object_id, cluster_id, appearance_type)
);

INSERT INTO appearance_new
  (id, object_id, cluster_id, appearance_type, face_id,
   distance, face_score, body_score, ambiguous, source, created_at)
SELECT
  id, object_id, cluster_id, appearance_type, face_id,
  distance, face_score, body_score, ambiguous, source, created_at
FROM appearance;

DROP TABLE appearance;

ALTER TABLE appearance_new RENAME TO appearance;
