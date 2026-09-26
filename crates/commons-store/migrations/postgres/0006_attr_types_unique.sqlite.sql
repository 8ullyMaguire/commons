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
