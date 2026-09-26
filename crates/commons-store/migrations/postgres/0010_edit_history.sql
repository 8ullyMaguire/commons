-- Migration: 0010 edit_history
--
-- §8.6's rule is that scores are *recomputed* from the accepted-edit set, never
-- maintained as a counter. That needs the set to be a thing, and there was no
-- such table: T-P4-001 wrote a field lock holding the winning value, which says
-- what a field is and not who proposed it or when.
--
-- `field_edit` is that set. One row per accepted edit, and a field's score is
-- `max(weight)` over its rows. Nothing increments.
--
-- The four states of a row matter and are the reason this is a table rather than
-- a column on `field_proposal`:
--
--   * `accepted_at` NULL  -> not in the set
--   * `retracted_at` set  -> was in the set, no longer is
--   * `author` NULL       -> the author asked to be unlinked (stash-box#656);
--                            the row survives, because "nobody proposed this"
--                            and "this was proposed and the author asked to be
--                            unlinked" must not look the same to the next reader
--   * `justification`     -> set on a revert, so a reader can tell a revert from
--                            somebody having proposed the earlier value first
--
-- `value_json` holds a JSON `null` for a deliberate null and the row's existence
-- for "set". That is the whole of stash-box#9: the two are different states, and
-- they are different here because a deliberate null is a row with a null value
-- and an unset field is no row.

CREATE TABLE IF NOT EXISTS field_edit (
    id              TEXT PRIMARY KEY,
    proposal_id     TEXT,
    subject_type    TEXT NOT NULL,
    subject_id      TEXT NOT NULL,
    field           TEXT NOT NULL,
    value_json      TEXT NOT NULL,          -- JSON, possibly 'null'
    author          TEXT,                   -- NULL once unlinked (§8.6.5)
    weight          REAL NOT NULL,
    accepted_at     TEXT,
    retracted_at    TEXT,
    removed_at      TEXT,                   -- attribution removed, row kept
    justification   TEXT,
    created_at      TEXT NOT NULL,
    -- A monotonic sequence, assigned by the writer on both backends.
    --
    -- Not a BIGSERIAL here and an autoincrement in SQLite: SQLite cannot declare
    -- two primary keys and `INTEGER PRIMARY KEY` must be the declared one, so the
    -- autoincrement is not available alongside a text `id`. One column, written
    -- the same way on both, rather than two mechanisms that mean subtly
    -- different things.
    --
    -- Why a sequence at all: a history's order is a fact about time, and
    -- `accepted_at` ties whenever two edits are written in the same millisecond,
    -- which is most pairs of edits in a real scan. The fallback was a uuid, so
    -- the tie was broken at random. See `field_history`.
    seq             BIGINT NOT NULL
);

-- The history order. Every per-field read is ordered by this, so it is indexed
-- on its own rather than as a suffix of the field index: an `ORDER BY` over
-- three fields of a filtered index is a sort, and this is the one query that must
-- not sort.
--
-- Named `field_edit_order`, not `field_edit_seq`: the sequence *table* below is
-- `field_edit_seq`, and the first version of this migration gave both the same
-- name. SQLite caught it -- "there is already an index named field_edit_seq" --
-- and Postgres would not have, because a table and an index live in different
-- namespaces there.
CREATE INDEX IF NOT EXISTS field_edit_order ON field_edit (seq);

-- The accepted set for one field. Partial, because the index exists to answer
-- "which rows count", and a retracted row is in none of those queries -- but it
-- is still read for history, so the row is not deleted.
CREATE INDEX IF NOT EXISTS field_edit_live
    ON field_edit (subject_id, field, accepted_at)
    WHERE retracted_at IS NULL;

CREATE INDEX IF NOT EXISTS field_edit_proposal ON field_edit (proposal_id);

-- A subject's edits in order, for the whole-subject history view. Field first
-- because per-field is the common read (§8.6.1) and the whole-subject view is
-- the same query without a filter.
CREATE INDEX IF NOT EXISTS field_edit_subject
    ON field_edit (subject_id, created_at, id);

-- One accepted edit per proposal. Without this, accepting the same proposal
-- twice is two rows and the score is computed over an edit that happened once.
--
-- Partial on accepted_at IS NOT NULL, and that is what lets a rejected-then-
-- accepted-then-retracted proposal be accepted again later: the constraint only
-- applies to rows currently in the set.
CREATE UNIQUE INDEX IF NOT EXISTS field_edit_uniq_proposal
    ON field_edit (proposal_id)
    WHERE accepted_at IS NOT NULL AND retracted_at IS NULL;

-- A report about a history entry (stash-box#656).
--
-- `reporter` is NOT NULL and deliberately kept after the entry's attribution is
-- removed. A report is a fact about the reporter, and an anonymous report is one
-- the moderation queue cannot act on; the reporter asked for their *edit* to be
-- unlinked, not for their report to vanish.
CREATE TABLE IF NOT EXISTS history_report (
    id          TEXT PRIMARY KEY,
    entry_id    TEXT NOT NULL,
    reporter    TEXT NOT NULL,
    reason      TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS history_report_entry ON history_report (entry_id);

-- §8.6.3: an object's merges, so a reader can tell a merge from an object that
-- never had a second identity.
--
-- The same argument as `cluster_merge` in 0004, and the same shape: winner and
-- loser rather than a from/to pair, because the operation is not symmetric -- the
-- winner keeps its identity and the loser is retired. Nothing in the surviving
-- rows says two identities were ever one.
CREATE TABLE IF NOT EXISTS object_merge (
    id                TEXT PRIMARY KEY,
    winner_id         TEXT NOT NULL,
    loser_id          TEXT NOT NULL,
    moved_edits       BIGINT NOT NULL DEFAULT 0,
    actor             TEXT,
    created_at        TEXT NOT NULL
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
