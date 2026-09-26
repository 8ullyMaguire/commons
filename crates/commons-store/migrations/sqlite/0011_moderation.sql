-- 0011_moderation.sql - SQLite mirror (Postgres: 0011_moderation.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0011_moderation.sql and re-run that script.
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

CREATE TABLE IF NOT EXISTS moderation_item (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL,
    subject_type  TEXT NOT NULL,
    subject_id    TEXT NOT NULL,
    field         TEXT NOT NULL,
    summary       TEXT NOT NULL,
    raised_by     TEXT,
    raised_at     TEXT NOT NULL,
    -- Who resolved it, and when. NULL while the item is open, which is what
    -- makes `queue` a single filtered read rather than a join.
    resolved_by   TEXT,
    resolved_at   TEXT,
    resolution    TEXT
);

-- The open queue. Partial, because the read that matters is always "what is
-- still open" and a steward should not page through resolved items to find it.
CREATE INDEX IF NOT EXISTS moderation_item_open
    ON moderation_item (raised_at, id)
    WHERE resolved_at IS NULL;

CREATE INDEX IF NOT EXISTS moderation_item_subject
    ON moderation_item (subject_type, subject_id, field);

-- An abuse report: an accusation about an account (stash-box#213, §8.5).
--
-- `dismissed_at` rather than a delete, and the same argument as T-P4-004's
-- history entries: a steward who looks at a queue needs to see that somebody
-- already looked at this one, and a report somebody made and somebody dismissed
-- is a fact about two people rather than one.
CREATE TABLE IF NOT EXISTS abuse_report (
    id            TEXT PRIMARY KEY,
    item_id       TEXT NOT NULL,
    reporter      TEXT NOT NULL,
    accused       TEXT,
    reason        TEXT NOT NULL,
    raised_at     TEXT NOT NULL,
    dismissed_at  TEXT,
    dismissed_by  TEXT
);

CREATE INDEX IF NOT EXISTS abuse_report_item ON abuse_report (item_id);

-- A name-collision warning (stash-box#714, #950).
--
-- The colliding value and the existing holder are both stored, because the
-- question a steward answers is "is this the *same* studio or a different one
-- that happens to share a name" and that cannot be answered from the warning
-- alone.
CREATE TABLE IF NOT EXISTS name_collision (
    id              TEXT PRIMARY KEY,
    item_id         TEXT NOT NULL,
    proposed        TEXT NOT NULL,
    proposed_json   TEXT NOT NULL,
    collides_with   TEXT,
    note            TEXT NOT NULL,
    raised_at       TEXT NOT NULL,
    dismissed_at    TEXT,
    dismissed_by    TEXT
);

CREATE INDEX IF NOT EXISTS name_collision_item ON name_collision (item_id);

-- A pending edit awaiting a steward (stash-box#226 amend, #599 attributed edit).
--
-- `author` and `amended_by` are separate columns rather than one `last_editor`,
-- because stash-box#599 is the whole point: a steward may correct somebody's
-- pending edit and the edit stays theirs. One column would make the amended text
-- unattributable, which is the failure the ticket names.
--
-- `resolved_at` frees the author's pending slot (stash-box#782) -- the limit is
-- on *open* edits, so a contributor who proposes forty things over a year is not
-- locked out forever.
CREATE TABLE IF NOT EXISTS pending_edit (
    id            TEXT PRIMARY KEY,
    item_id       TEXT NOT NULL,
    author        TEXT NOT NULL,
    value         TEXT NOT NULL,          -- JSON
    note          TEXT,
    raised_at     TEXT NOT NULL,
    amended_value TEXT,                   -- JSON, after a steward corrected it
    amended_note  TEXT,
    amended_by    TEXT,
    amended_at    TEXT,
    resolved_at   TEXT,
    resolved_by   TEXT
);

CREATE INDEX IF NOT EXISTS pending_edit_item ON pending_edit (item_id);
CREATE INDEX IF NOT EXISTS pending_edit_open_author
    ON pending_edit (author, raised_at)
    WHERE resolved_at IS NULL;

-- A closed submission (stash-box#570).
--
-- `closed_at` and `reopened_at` rather than a boolean, for the same reason a
-- merge is a record: a submission that was closed and quietly reopened is a
-- different history from one that was never closed, and only the second needs
-- no explanation.
CREATE TABLE IF NOT EXISTS submission (
    id            TEXT PRIMARY KEY,
    subject_type  TEXT NOT NULL,
    subject_id    TEXT NOT NULL,
    field         TEXT NOT NULL,
    closed_at     TEXT,
    closed_by     TEXT,
    close_reason  TEXT,
    reopened_at   TEXT,
    reopened_by   TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS submission_uniq
    ON submission (subject_type, subject_id, field);

-- An ignore-list entry for a studio or a performer (stash-box#787).
--
-- One table for both, with `kind` naming which. Two tables would double the
-- authorization code for what is one relationship, and the two are read
-- together: a UI asks "what is excluded" and does not care whether the answer
-- came from the studio list or the performer list.
--
-- `ON DELETE CASCADE` on the account: an ignore-list is a statement *by* an
-- account, and a statement by an account that no longer exists is not a rule
-- anybody is following.
CREATE TABLE IF NOT EXISTS account_ignore (
    account_id    TEXT NOT NULL,
    target_kind   TEXT NOT NULL,          -- 'studio' | 'performer'
    target        TEXT NOT NULL,
    reason        TEXT,
    created_at    TEXT NOT NULL,
    PRIMARY KEY (account_id, target_kind, target)
);

CREATE INDEX IF NOT EXISTS account_ignore_target ON account_ignore (target_kind, target);

-- A studio that cannot accept new scenes (stash-box#1175).
--
-- Distinct from an ignore-list entry: an ignore is one account's preference and
-- one studio's status is a fact about the studio. Conflating them would mean
-- "this studio is closed" is expressed by N rows, one per account, and a studio
-- with no accounts would look open.
CREATE TABLE IF NOT EXISTS excluded_studio (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    reason        TEXT NOT NULL,
    excluded_by   TEXT,
    excluded_at   TEXT NOT NULL
);

-- A pinned comment (stash-box#700).
--
-- Pinned to a subject, not to a field. A pinned note is usually about the object
-- as a whole ("this is a re-encode, do not merge it with the original"), and
-- pinning it per field would mean pinning it N times and unpinning it N times
-- when the note stops applying to all but one.
CREATE TABLE IF NOT EXISTS pinned_note (
    id            TEXT PRIMARY KEY,
    subject_type  TEXT NOT NULL,
    subject_id    TEXT NOT NULL,
    body          TEXT NOT NULL,
    author        TEXT NOT NULL,
    pinned_at     TEXT NOT NULL,
    unpinned_at   TEXT,
    unpinned_by   TEXT
);

CREATE INDEX IF NOT EXISTS pinned_note_subject
    ON pinned_note (subject_type, subject_id, pinned_at)
    WHERE unpinned_at IS NULL;

-- §8.5's contested-field resolution locks a field, and `field_lock` predates the
-- queue (migration 0001) without a column for *why* it was locked. A lock with no
-- stated reason is indistinguishable from a lock nobody can explain, and the
-- first person to meet one is the next steward -- so the reason is recorded.
--
-- Nullable and added here rather than in 0001: 0001 is applied and immutable,
-- and a lock written before this migration has no reason, which is true rather
-- than a hole.
ALTER TABLE field_lock ADD COLUMN reason TEXT;
