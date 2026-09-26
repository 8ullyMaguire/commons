-- 0013_takedown.sql - SQLite mirror (Postgres: 0013_takedown.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0013_takedown.sql and re-run that script.
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

CREATE TABLE content_blocklist (
    content_hash  TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('quarantined', 'denied')),
    object_id     TEXT,
    -- §14.1: a takedown is *about* content and the reason travels with it, so a
    -- peer receiving the tombstone can show the person whose content it is why
    -- it is blocked and who decided.
    reason        TEXT NOT NULL,
    -- Which local instance made the call. A blocklist propagated from a peer is
    -- still a row somebody has to be able to point at.
    source        TEXT NOT NULL DEFAULT 'local',
    created_at    TEXT NOT NULL,
    PRIMARY KEY (content_hash, kind)
);

-- §14.1: "Permanently blocked by hash across all peers." Permanently is a
-- database guarantee here, not a convention: a denied hash cannot be denied
-- twice, so there is no row to update and no second decision to take. The
-- partial index is what makes `INSERT ... ON CONFLICT DO NOTHING` mean
-- "already denied" rather than "inserted a rival row".
CREATE UNIQUE INDEX content_blocklist_one_denial
    ON content_blocklist (content_hash) WHERE kind = 'denied';

-- The quarantine index, for the "what is under review" question. Not unique:
-- several peers may quarantine the same content and a local instance may
-- re-report it.
CREATE INDEX content_blocklist_quarantine
    ON content_blocklist (kind, created_at);

-- Tombstones owed to peers (§13.2: revocation travels as a tombstone, not a
-- vote). `delivered_at IS NULL` is the queue, and `delivered_at` is when it
-- went -- not a boolean, because a peer that retries after a dropped connection
-- needs to know the queue is not empty and a boolean that never clears would be
-- the wrong answer forever.
--
-- `id` is the tombstone's own identity and is what makes redelivery a
-- duplicate rather than a second denial. A peer that receives the same
-- tombstone twice must be able to tell.
CREATE TABLE tombstone_outbox (
    id            TEXT PRIMARY KEY,
    content_hash  TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('quarantined', 'denied')),
    object_id     TEXT,
    reason        TEXT NOT NULL,
    -- The signer's key id, and the signature over the canonical form. Verified
    -- on receipt (§14.1: the blocklist propagates to *every* peer, and an
    -- unsigned row claiming to be a takedown is a way to deny somebody else's
    -- library wholesale).
    signer        TEXT NOT NULL,
    signature     TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    delivered_at  TEXT
);

CREATE INDEX tombstone_outbox_pending ON tombstone_outbox (delivered_at, created_at);

-- Tombstones received from peers, kept as their own rows rather than only as
-- blocklist entries. Two reasons, both about auditability: a peer asking "did
-- you get my takedown?" needs an answer, and an operator looking at a blocked
-- file needs to know it came from a peer rather than from their own moderator.
--
-- The primary key is the *peer's* tombstone id, so redelivery is a conflict
-- rather than a second row.
CREATE TABLE tombstone_inbox (
    peer_tombstone_id TEXT PRIMARY KEY,
    peer              TEXT NOT NULL,
    content_hash      TEXT NOT NULL,
    kind              TEXT NOT NULL CHECK (kind IN ('quarantined', 'denied')),
    object_id         TEXT,
    reason            TEXT NOT NULL,
    received_at       TEXT NOT NULL
);

-- Redaction of history on request (stash-box#656). A *record* that a person's
-- identifying data was removed, held independently of the rows it describes, so
-- the removal itself survives the deletion it records. Deleting the note along
-- with the data would leave no way to answer "was this ever redacted?" -- which
-- is the only question an audit asks.
CREATE TABLE redaction_log (
    id           TEXT PRIMARY KEY,
    subject_type TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    -- What was removed, as a description rather than the data: the log is
    -- retained *after* the redaction, so it must not contain what it describes.
    what         TEXT NOT NULL,
    rows_removed INTEGER NOT NULL,
    requested_by TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE INDEX redaction_subject ON redaction_log (subject_type, subject_id);
