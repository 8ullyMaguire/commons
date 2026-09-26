-- §7.7's typed attribute layer, and §7.9's status enum.
--
-- # Why this is a migration and not just code
--
-- `custom_field` and `custom_field_value` already exist (0001) with an
-- `attr_type` column, but nothing wrote or read them: the type was a string with
-- no vocabulary and the value was opaque JSON. That is enough to store a field
-- and not enough to *query* one, and §7.7's questions are queries -- "which
-- performers have a measurement", "does this field take more than one value".
--
-- So the change here is a vocabulary and a shape, not a new table:
--
--   * `attr_type` is constrained to §7.7's eight types. A CHECK rather than an
--     enum, because the two engines differ on enums and the sync script already
--     has to translate between them; the vocabulary is short and fixed, so a
--     CHECK does the same job.
--
--   * `at` is NOT NULL *for measurement fields*. It cannot be expressed as a
--     column constraint, because whether a date is required depends on the
--     field's declared type, which lives in another table. So it is enforced in
--     code -- `AttributeError::MeasurementNeedsDate` -- and the reason is written
--     here: an undated measurement cannot be placed on a timeline, so it cannot
--     contribute to §7.11's career span or an age-at-scene, and storing one
--     means the derivation silently skips a row that looks present.
--
-- # Why career span needs no column
--
-- §7.11's span is derived from item dates. `object.date` already holds the item
-- date and `appearance` already links an object to a person, so the span is a
-- query over existing rows and needs nothing stored. A `career_start` column
-- would have to be maintained on every insert, every edit and every delete of an
-- item, would drift, and could not explain itself. The indexes below are the only
-- thing added for it.
--
-- # The appear-with index
--
-- §7.12's graph joins appearances to other appearances through the object. The
-- existing `appearance` unique index is on `(object_id, cluster_id,
-- appearance_type)`, which is the wrong shape for it: the graph needs "all
-- appearances in this object", so `(object_id, appearance_type)` is what turns a
-- per-object scan into a lookup.

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

--:sqlite 0006_attr_types_alters.sqlite.sql
ALTER TABLE custom_field
    ADD COLUMN created_at TEXT;
--:end

--:sqlite 0006_attr_types.sqlite.sql
ALTER TABLE performer
    ADD CONSTRAINT performer_status_known
    CHECK (status IS NULL OR status IN ('active', 'deceased', 'retired', 'inactive'));
--:end

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
--:sqlite 0006_attr_types_unique.sqlite.sql
ALTER TABLE custom_field_value
    DROP CONSTRAINT IF EXISTS custom_field_value_field_subject_at_key;
--:end

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
