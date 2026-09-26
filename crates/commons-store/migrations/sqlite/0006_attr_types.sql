-- 0006_attr_types.sql - SQLite mirror (Postgres: 0006_attr_types.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0006_attr_types.sql and re-run that script.
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

-- §7.9: status is an enum, not a phrase. "passed away", "RIP" and "Passed" as
-- free text would be three filters for one fact, and a `LIKE` cannot be indexed.
-- `NULL` means unknown, which is the common case and is not the same as active.
-- `custom_field` and `custom_field_value` came from 0001 without a timestamp,
-- which was fine while nothing wrote them. A declared field now has a creation
-- date like every other row, and a value that carries `at` is not a substitute:
-- `at` is the *date the value describes* (a measurement's date, a date
-- attribute) and for a plain text value it is the write time, so the column
-- cannot mean both things. 0001 is immutable, so the columns are added here.
--
-- The pair is added rather than only `custom_field.created_at` because a value
-- row written at a known time is what lets "recently corrected" be a query
-- rather than a guess.
-- The SQLite mirror adds only `custom_field.created_at`, because the
-- `custom_field_value` rebuild further down this migration produces the whole
-- table shape including `recorded_at`. Postgres has no such rebuild, so it needs
-- the column stated. The two engines reach the same shape by different routes,
-- which the parity test is what proves.
ALTER TABLE custom_field
    ADD COLUMN created_at TEXT;

ALTER TABLE custom_field_value
    ADD COLUMN recorded_at TEXT;

-- SQLite form of the statement above (0006_attr_types_alters.sqlite.sql).
-- Nothing to run: on SQLite `custom_field_value`'s shape is produced whole by the
-- table rebuild further down this migration, and `custom_field.created_at` is
-- added above this block by the shared `ALTER TABLE`.
--
-- The block exists to mark the one statement that is *not* portable. The Postgres
-- form adds `recorded_at` to `custom_field_value` by ALTER; SQLite gets the same
-- column by rebuilding the table, because a rebuild is the only way to drop the
-- unnamed `UNIQUE` from 0001. So the Postgres statement has no SQLite equivalent
-- of its own, and the sidecar says so rather than repeating a statement that
-- would fail with "duplicate column name".

-- SQLite form of the statement above (0006_attr_types.sqlite.sql).
-- SQLite has no `ALTER TABLE ... ADD CONSTRAINT`, and it does not enforce
-- CHECK constraints added after a table exists either -- a CHECK in SQLite is
-- part of the column definition in the original CREATE TABLE. So the status
-- vocabulary is enforced by a trigger, which is the one mechanism SQLite has for
-- constraining a write to an existing table.
--
-- The alternative -- the twelve-step table rebuild -- was rejected because it
-- rewrites the whole `performer` table to add a constraint that is four values
-- long, and the rebuild has to be redone correctly for every future constraint.
-- A trigger is one statement and is checked by the parity test on the same terms
-- as any other.
--
-- BEFORE rather than AFTER so the write is refused: an AFTER trigger would let
-- the bad value in and then undo it, which is a row-version bump and an index
-- update wasted on every bad write.
CREATE TRIGGER IF NOT EXISTS performer_status_known
BEFORE INSERT ON performer
FOR EACH ROW
WHEN NEW.status IS NOT NULL
 AND NEW.status NOT IN ('active', 'deceased', 'retired', 'inactive')
BEGIN
    SELECT RAISE(ABORT, 'performer.status is not a known status');
END;

CREATE TRIGGER IF NOT EXISTS performer_status_known_update
BEFORE UPDATE OF status ON performer
FOR EACH ROW
WHEN NEW.status IS NOT NULL
 AND NEW.status NOT IN ('active', 'deceased', 'retired', 'inactive')
BEGIN
    SELECT RAISE(ABORT, 'performer.status is not a known status');
END;

-- §7.7's vocabulary. Eight types, and a ninth value is a migration rather than a
-- typo, because a value nobody has implemented is worse than a rejected one.
--
-- `ON CONFLICT DO NOTHING` rather than `INSERT OR IGNORE`: the latter is a
-- SQLite-ism, the former is portable SQL both engines accept, and a seed that
-- cannot be re-run after a partial failure is a migration that leaves the
-- database in a state nobody can recover from without hand-editing it.
CREATE TABLE IF NOT EXISTS attr_type_vocab (
    name         TEXT PRIMARY KEY,
    multi_valued INTEGER NOT NULL DEFAULT 0,
    needs_date   INTEGER NOT NULL DEFAULT 0
);

INSERT INTO attr_type_vocab (name, multi_valued, needs_date) VALUES
    ('single',      0, 0),
    ('multi',       1, 0),
    ('range',       0, 0),
    ('ordinal',     0, 0),
    ('boolean',     0, 0),
    ('text',        0, 0),
    ('date',        0, 0),
    ('measurement', 1, 1)
ON CONFLICT (name) DO NOTHING;

-- The vocabulary is looked up rather than duplicated in Rust, so the schema is
-- the single statement of which types exist and which are multi-valued. A type
-- that is multi-valued in code and single-valued here would make `add()` accept
-- a second value the schema believes is impossible.
CREATE INDEX IF NOT EXISTS custom_field_type ON custom_field (attr_type);

-- §7.12: "all credited appearances in this object", which is the shape the
-- appear-with graph walks. Without it the graph scans every appearance row.
CREATE INDEX IF NOT EXISTS appearance_by_object
    ON appearance (object_id, appearance_type);

-- §7.11: the span query is `MIN(object.date)` / `MAX(object.date)` over the
-- appearances of one cluster. The index is on the appearance side because that
-- is the selective end -- one cluster's appearances, then the object dates.
CREATE INDEX IF NOT EXISTS appearance_by_cluster
    ON appearance (cluster_id, object_id);

-- `custom_field_value.at` is the measurement's date and is read by the
-- "measurements over time" chart. Indexed because it is the only ordering in the
-- table: the same field and subject, ordered by when.
CREATE INDEX IF NOT EXISTS custom_field_value_timeline
    ON custom_field_value (field_id, subject_type, subject_id, at);

-- 0001's uniqueness on `custom_field_value` is
-- `(field_id, subject_type, subject_id, at)`, which allows exactly one value per
-- subject per date. For a `multi` field that is precisely wrong: two values
-- written on the same day collide, and the second is refused by a constraint
-- whose message names no field and no value. §7.7's multi-select attributes are
-- the whole reason this table has values, so the constraint is dropped and
-- rebuilt with `value_json` in the key.
--
-- The new key also makes `at` optional in practice. A text value has no date of
-- its own, so two text values on a subject would still collide on a NULL `at` --
-- except that this key now includes the value, so they only collide if they are
-- the *same* value, which is the duplicate `add()` reports by name.
--
-- 0001 is immutable, so the constraint is dropped here rather than corrected
-- there. See the SQLite sidecar for why the SQLite form is empty.
--
-- SQLite form of the statement above (0006_attr_types_unique.sqlite.sql).
-- The twelve-step rebuild, because the constraint has to change.
--
-- SQLite cannot `DROP CONSTRAINT`, and 0001's `UNIQUE (field_id, subject_type,
-- subject_id, at)` is a table-level constraint with no name to drop. Dropping it
-- therefore means rebuilding the table, which is the documented procedure and is
-- spelled out here rather than left to be discovered:
--
--   1. create the new table under a temporary name
--   2. copy the rows
--   3. drop the old table
--   4. rename
--   5. recreate the indexes that lived on the old table
--
-- Step 5 is the one that is easy to forget, and forgetting it does not fail here
-- -- it fails later, as a query that scans. The `recorded_at` column is added in
-- the same rebuild because SQLite's `ALTER TABLE ... ADD COLUMN` cannot add a
-- NOT NULL column without a default, and `recorded_at` is nullable so it could
-- be added separately; doing it in the rebuild keeps one copy of the table's
-- shape rather than two places that have to agree.
--
-- `PRAGMA foreign_keys` is off by default in SQLite and sqlx does not turn it on
-- for a migration, so the `REFERENCES` clause is carried over verbatim and the
-- rebuild is safe. The `ON DELETE CASCADE` behaviour is a property of the
-- schema, not of the rebuild: recreating the table with the same clause
-- restores it.
CREATE TABLE custom_field_value_new (
  id          TEXT PRIMARY KEY,
  field_id    TEXT NOT NULL REFERENCES custom_field(id) ON DELETE CASCADE,
  subject_type TEXT NOT NULL,
  subject_id  TEXT NOT NULL,
  value_json  TEXT NOT NULL,
  at          TEXT,
  recorded_at TEXT,
  -- `value_json` is in the key because §7.7's `multi` fields hold several values
  -- per subject, and a key without it allows exactly one. See the Postgres form
  -- for the full argument.
  UNIQUE (field_id, subject_type, subject_id, at, value_json)
);

INSERT INTO custom_field_value_new
    (id, field_id, subject_type, subject_id, value_json, at)
SELECT id, field_id, subject_type, subject_id, value_json, at
FROM custom_field_value;

DROP TABLE custom_field_value;

ALTER TABLE custom_field_value_new RENAME TO custom_field_value;

-- 0001's index, recreated because step 3 dropped the table it lived on.
CREATE INDEX IF NOT EXISTS custom_field_value_subject_idx
    ON custom_field_value (subject_type, subject_id);

CREATE UNIQUE INDEX IF NOT EXISTS custom_field_value_unique_value
    ON custom_field_value (field_id, subject_type, subject_id, at, value_json);


-- 0002's uniqueness on `appearance` is `(object_id, appearance_type)`, partial on
-- `ambiguous`. That was a correct fix for a real problem -- in SQL a NULL
-- compares unequal to every other NULL, so with `cluster_id` nullable the
-- original constraint stopped constraining the NULL rows at all, and an
-- ambiguous appearance could be re-recorded on every scan forever. The fix kept
-- the intent and lost the key: dropping `cluster_id` from the unique key means
-- **one appearance per object per type**, so a scene with two performers cannot
-- be recorded at all.
--
-- That is not a rare shape. §7.12 is about scenes with several people in them, and
-- the appear-with graph is built by joining appearances through `object_id` -- a
-- graph whose every object holds one person has no edges to find. The whole
-- feature is unbuildable against this constraint.
--
-- So `cluster_id` goes back in the key, and the NULL problem is solved the way
-- 0002 meant to solve it: `COALESCE(cluster_id, '')` as an *expression* index
-- column rather than a sentinel written into the data. The `''` exists only
-- inside the index, so the column stays genuinely NULL in the table -- which is
-- the property 0002 was protecting -- while every ambiguous row of one object
-- still collides with every other and is constrained.
--
-- The `(object_id, appearance_type)` index added above for the appear-with
-- graph becomes redundant once this exists with `COALESCE`, since the expression
-- index cannot serve a plain equality lookup on the first two columns. It is kept
-- because it is *not* redundant for the graph query in practice: the graph joins
-- on `object_id` and filters on `appearance_type` as a value list, which the
-- partial index serves without evaluating `COALESCE` on every row.
DROP INDEX IF EXISTS appearance_unique_member;
DROP INDEX IF EXISTS appearance_unique_ambiguous;

CREATE UNIQUE INDEX appearance_unique_member
  ON appearance (object_id, appearance_type, COALESCE(cluster_id, ''))
  WHERE ambiguous = 0;
CREATE UNIQUE INDEX appearance_unique_ambiguous
  ON appearance (object_id, appearance_type, COALESCE(cluster_id, ''))
  WHERE ambiguous = 1;
