-- Migration: 0019 undo records
--
-- §10.7 (undo for destructive actions, #3221) and §8.6 (revert). The storage
-- half of a feature whose other half is the command registry from T-P5-006
-- item 5.
--
-- # What this table holds, and what it deliberately does not
--
-- A record of a write, with the state that write replaced. Not a counter, not a
-- log line, and not a re-derivation: the point is that the inverse of a write
-- cannot be computed after the fact.
--
-- Reconstructing it later fails in three ways, each of which is a bug rather
-- than an inconvenience:
--
--   * *Which objects.* The undo must restore exactly the objects the write
--     touched. Re-running the same selection later can return a different set,
--     because between the write and the undo an object may have been added,
--     removed, or matched by something else. A `Filter` target is explicitly
--     unbounded at request time.
--   * *What was there before.* `object_tag`'s `confidence`, `source` and
--     `created_at` were all NULL before 0016 added them, so "the row did not
--     exist" and "the row existed with NULLs" are different states. The write
--     uses `ON CONFLICT DO UPDATE`, so an object that already carried the tag
--     has its values replaced rather than gaining a second row. Without the
--     prior state, undo cannot tell a no-op from a change, and would delete a
--     tag the user set deliberately.
--   * *Whether undo is still allowed.* A write that has been superseded must
--     not be undoable into a state nobody was ever in. That is a comparison
--     against the recorded prior state, so it needs the record.
--
-- # Why `before` and `after` are separate columns and not a diff
--
-- Because a diff cannot represent the NULL-versus-absent case, and that case is
-- the whole of stash-box#9, which §8.6 makes a correctness rule: a deliberate
-- null is a row with a null value and an unset field is no row. Storing both
-- sides as written makes the inverse mechanical and the staleness check exact.
--
-- `row_existed` is redundant with `before_confidence IS NULL AND
-- before_source IS NULL AND before_created_at IS NULL`, and is kept anyway
-- because the redundancy is what makes the column single-valued on the *insert*
-- path. Every current column of `object_tag` is nullable, so three NULLs cannot
-- distinguish "absent" from "all null" — and that is not a hypothetical, it is
-- the state every row written before 0016 is in.

CREATE TABLE undo_record (
    id            TEXT PRIMARY KEY,
    -- Who may undo this. An undo is a write, so it passes through the same
    -- consent clause as the write it reverses; a record another caller could
    -- replay would be a way to write without the clause.
    caller        TEXT NOT NULL,
    -- `bulk.tag.add` or `bulk.tag.remove`. A free-form string rather than an
    -- enum because the client renders it in the toast ("Undo adding beach to
    -- 40 objects"), and a new action should not need a migration.
    action        TEXT NOT NULL,
    -- What the write was about, and the reason the inverse needs no per-entry
    -- copy of it. One tag per record, because every bulk action today acts on
    -- exactly one; a record that reversed a whole batch of different tags
    -- would need a second table to say which entry belonged to which, and that
    -- is a shape to reach for when an action needs it rather than to pay for in
    -- every record written now.
    tag_id        TEXT NOT NULL REFERENCES tag(id) ON DELETE CASCADE,
    -- What the write covered, for the confirmation text. `ids` is the count the
    -- caller named; `matched` is the count the write actually reached. Both,
    -- because the difference between them is the number of objects the consent
    -- filter hid, and that is exactly what a user needs to know before undoing.
    requested     INTEGER NOT NULL,
    matched       INTEGER NOT NULL,
    created_at    TEXT NOT NULL,
    expires_at    TEXT NOT NULL,
    -- Set when the record is consumed. A row is never deleted: the history of
    -- what was done and undone is itself history, and §8.6 is about not losing
    -- it. Nullability is the state -- undone and not-undone are different
    -- states, the same rule as everywhere else in this schema.
    undone_at     TEXT
);

-- One row per object the write changed.
--
-- Separate from `undo_record` rather than a JSON blob, for the same reason
-- `field_edit` is a table and not a column: the staleness check is a per-object
-- query, and it has to be able to name the object that diverged in the error.
CREATE TABLE undo_entry (
    record_id      TEXT NOT NULL REFERENCES undo_record(id) ON DELETE CASCADE,
    object_id      TEXT NOT NULL,
    -- Whether the tag row existed before the write. See the header: the
    -- columns below cannot express this on their own.
    row_existed    INTEGER NOT NULL,
    before_confidence  DOUBLE PRECISION,
    before_source      TEXT,
    before_created_at  TEXT,
    after_confidence   DOUBLE PRECISION,
    after_source       TEXT,
    after_created_at   TEXT,
    PRIMARY KEY (record_id, object_id)
);

-- The read path: "what may this caller undo, newest first". Indexed on the
-- caller because that is the predicate, and on `created_at` because the list is
-- ordered by it. `undone_at` is not in the index: an index over a column that is
-- NULL for every row in the common case stores nothing useful, and the
-- `undone_at IS NULL` filter is applied after the caller has already narrowed to
-- their own handful of records.
CREATE INDEX undo_record_caller_created
    ON undo_record(caller, created_at);

-- The staleness check's lookup. `ON DELETE CASCADE` on the record is above; the
-- object is deliberately not a foreign key, because an undo whose object has
-- been deleted is not an error -- the write that created the tag is already
-- undone by the deletion, and a row that outlived its subject is the correct
-- record of that.
CREATE INDEX undo_entry_object ON undo_entry(object_id);
