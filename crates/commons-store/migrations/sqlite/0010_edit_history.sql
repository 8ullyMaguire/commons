-- Migration: 0010 edit_history
--
-- Mirrors postgres/0010. The differences are the ones SQLite forces and nothing
-- this migration chose:
--
--   * `IF NOT EXISTS` on indexes, as everywhere in the sqlite tree.
--   * `REAL` is a real type in both, and `BIGINT` is `INTEGER` here.
--   * No expression appears in a `UNIQUE (...)` constraint: SQLite rejects one,
--     which 0008 and 0009 both learned the hard way. The partial unique index
--     below has no expression, so it is legal as written.
--
-- `field_edit.value_json` is TEXT holding a JSON document, 'null' included. A
-- deliberate null is a row whose value is the JSON literal `null`; an unset
-- field is no row. That distinction is stash-box#9 and it is the reason this is
-- a table with a value column rather than a nullable column on the subject.

CREATE TABLE IF NOT EXISTS field_edit (
    id              TEXT PRIMARY KEY,
    proposal_id     TEXT,
    subject_type    TEXT NOT NULL,
    subject_id      TEXT NOT NULL,
    field           TEXT NOT NULL,
    value_json      TEXT NOT NULL,
    author          TEXT,
    weight          REAL NOT NULL,
    accepted_at     TEXT,
    retracted_at    TEXT,
    removed_at      TEXT,
    justification   TEXT,
    created_at      TEXT NOT NULL,
    -- A monotonic sequence, assigned by the writer on both backends.
    --
    -- SQLite cannot declare two primary keys, and `INTEGER PRIMARY KEY` has to
    -- be the declared one, so an autoincrement is not available alongside a
    -- text `id`. Rather than have SQLite's sequence and Postgres's BIGSERIAL
    -- mean subtly different things -- one of them silently not existing -- the
    -- column is written explicitly, from the same source on both.
    --
    -- Why a sequence at all: a history's order is a fact about time, and
    -- `accepted_at` ties whenever two edits are written in the same millisecond,
    -- which is most pairs of edits in a real scan. The fallback was a uuid, so
    -- the tie was broken at random. See `field_history`.
    seq             INTEGER NOT NULL
);

-- The `seq` order is the history order, so this index carries it. Named
-- `field_edit_order` because `field_edit_seq` is the sequence table below, and
-- the first version of this migration gave both the same name. SQLite rejects
-- that; Postgres does not, because a table and an index are in different
-- namespaces there.
CREATE INDEX IF NOT EXISTS field_edit_order ON field_edit (seq);

CREATE INDEX IF NOT EXISTS field_edit_live
    ON field_edit (subject_id, field, accepted_at)
    WHERE retracted_at IS NULL;

CREATE INDEX IF NOT EXISTS field_edit_proposal ON field_edit (proposal_id);

CREATE INDEX IF NOT EXISTS field_edit_subject
    ON field_edit (subject_id, created_at, id);

CREATE UNIQUE INDEX IF NOT EXISTS field_edit_uniq_proposal
    ON field_edit (proposal_id)
    WHERE accepted_at IS NOT NULL AND retracted_at IS NULL;

CREATE TABLE IF NOT EXISTS history_report (
    id          TEXT PRIMARY KEY,
    entry_id    TEXT NOT NULL,
    reporter    TEXT NOT NULL,
    reason      TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS history_report_entry ON history_report (entry_id);

CREATE TABLE IF NOT EXISTS object_merge (
    id            TEXT PRIMARY KEY,
    winner_id     TEXT NOT NULL,
    loser_id      TEXT NOT NULL,
    moved_edits   BIGINT NOT NULL DEFAULT 0,
    actor         TEXT,
    created_at    TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS object_merge_winner ON object_merge (winner_id);
CREATE INDEX IF NOT EXISTS object_merge_loser ON object_merge (loser_id);


-- The sequence itself.
--
-- One counter for the whole table rather than a per-subject one, because the
-- ordering has to be *total*: two edits of different fields have to be
-- orderable too, or a merge that moved one of them and a merge that moved the
-- other could produce two histories that disagree about which came first.
--
-- A row that holds the next value, updated under a transaction. This is the
-- slowest possible way to count and it is chosen anyway, because the two
-- backends cannot both do it fast: `nextval` exists in Postgres and not in
-- SQLite, and SQLite's `AUTOINCREMENT` is not available here. A contention
-- hotspot on one row is the honest cost of one implementation that means the
-- same thing on both, and the write is a single indexed update inside the same
-- transaction as the insert.
CREATE TABLE IF NOT EXISTS field_edit_seq (
    id      INTEGER PRIMARY KEY,
    next    BIGINT NOT NULL
);
