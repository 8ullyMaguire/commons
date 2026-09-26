-- 0002_appearance_nullable_cluster.sql - SQLite mirror (Postgres: 0002_appearance_nullable_cluster.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0002_appearance_nullable_cluster.sql and re-run that script.
-- T-P0-007's parity test fails if the two files' table sets ever diverge.
--
-- Portable-SQL rules in force (plan section 0.4):
--   * ids are TEXT
--   * timestamps are ISO-8601 UTC TEXT, so comparison and sort need no
--     timezone function and both engines agree
--   * no vector columns; embeddings live in a sidecar ANN file
--   * booleans are INTEGER 0|1 here and BOOLEAN in Postgres; the store crate
--     hides the difference and no query writes a literal
--   * foreign keys need `PRAGMA foreign_keys = ON` per connection, which the
--     store crate sets at open time
-- SQLite form of the statement above (../sqlite-forms/0002_appearance_nullable_cluster.sql).
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

-- The clustering engine's hot query is "every non-ambiguous member of this
-- cluster". A plain index on cluster_id would also carry every ambiguous row,
-- which is noise for this query and makes the index larger for no benefit.
CREATE INDEX appearance_cluster_idx ON appearance (cluster_id) WHERE ambiguous = 0;

-- The other hot query: "everything the user needs to review". Deliberately
-- partial on `ambiguous = 1`, so it stays small no matter how large the library
-- is, and ordered by creation so the review queue is in the order the doubts
-- arose.
CREATE INDEX appearance_ambiguous_idx ON appearance (created_at, id) WHERE ambiguous = 1;

-- 0001's `UNIQUE (object_id, cluster_id, appearance_type)` does NOT survive
-- this migration intact, and nothing about the schema says so.
--
-- In SQL a NULL compares unequal to every other NULL, so a uniqueness
-- constraint containing a nullable column constrains the NULL rows not at all.
-- With `cluster_id` now nullable, the same object could be recorded as an
-- ambiguous appearance any number of times -- one row per re-scan, forever --
-- and the constraint that was supposed to prevent exactly that would sit there
-- looking like it was preventing it.
--
-- So the constraint is restated as two partial ones, which is what it was
-- doing before. `COALESCE(cluster_id, '')` would also work and is less SQL to
-- read, but it puts a sentinel value back into the column this migration exists
-- to keep free of one, and a reader would have to know that to be surprised.
-- Partial indexes say plainly which rows are covered, and the empty predicate
-- is itself the documentation.
CREATE UNIQUE INDEX appearance_unique_member
  ON appearance (object_id, appearance_type) WHERE ambiguous = 0;
CREATE UNIQUE INDEX appearance_unique_ambiguous
  ON appearance (object_id, appearance_type) WHERE ambiguous = 1;
