-- Migration: 0011 moderation
--
-- Mirrors postgres/0011. One difference, and SQLite forces it:
--
--   * `account_ignore` has no `ON DELETE CASCADE`, because SQLite only honours a
--     foreign key when `PRAGMA foreign_keys = ON` and the rest of this tree's
--     migrations do not turn it on. The Postgres form is therefore stronger than
--     the SQLite one, which is worth knowing rather than assuming: on SQLite an
--     ignore-list outlives the account that made it, and the module's read
--     filters on the account anyway, so the effect is the same. The comment is
--     here because "the two backends differ" is otherwise invisible.
--
-- Everything else is identical, including the partial indexes -- SQLite supports
-- them, and the reason the partial form matters is the same on both: the read
-- that matters is "what is still open", and a partial index is what makes it a
-- filtered index rather than a scan with a WHERE.

CREATE TABLE IF NOT EXISTS moderation_item (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL,
    subject_type  TEXT NOT NULL,
    subject_id    TEXT NOT NULL,
    field         TEXT NOT NULL,
    summary       TEXT NOT NULL,
    raised_by     TEXT,
    raised_at     TEXT NOT NULL,
    resolved_by   TEXT,
    resolved_at   TEXT,
    resolution    TEXT
);

CREATE INDEX IF NOT EXISTS moderation_item_open
    ON moderation_item (raised_at, id)
    WHERE resolved_at IS NULL;

CREATE INDEX IF NOT EXISTS moderation_item_subject
    ON moderation_item (subject_type, subject_id, field);

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

CREATE TABLE IF NOT EXISTS pending_edit (
    id            TEXT PRIMARY KEY,
    item_id       TEXT NOT NULL,
    author        TEXT NOT NULL,
    value         TEXT NOT NULL,
    note          TEXT,
    raised_at     TEXT NOT NULL,
    amended_value TEXT,
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

CREATE TABLE IF NOT EXISTS account_ignore (
    account_id    TEXT NOT NULL,
    target_kind   TEXT NOT NULL,
    target        TEXT NOT NULL,
    reason        TEXT,
    created_at    TEXT NOT NULL,
    PRIMARY KEY (account_id, target_kind, target)
);

CREATE INDEX IF NOT EXISTS account_ignore_target ON account_ignore (target_kind, target);

CREATE TABLE IF NOT EXISTS excluded_studio (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    reason        TEXT NOT NULL,
    excluded_by   TEXT,
    excluded_at   TEXT NOT NULL
);

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
