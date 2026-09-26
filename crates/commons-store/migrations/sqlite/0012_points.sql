-- 0012_points.sql - SQLite mirror (Postgres: 0012_points.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0012_points.sql and re-run that script.
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

CREATE TABLE IF NOT EXISTS points_award (
    id           TEXT PRIMARY KEY,
    account_id   TEXT NOT NULL,
    -- The proposal that earned it. The tie to §8.1 is this column: points are
    -- paid for a *proposal* winning, so the award can be withdrawn when that
    -- stops being true.
    proposal_id  TEXT NOT NULL,
    points       BIGINT NOT NULL,
    awarded_at   TEXT NOT NULL,
    withdrawn_at TEXT,
    withdraw_reason TEXT,
    -- Why this award exists when it is not a proposal award: an invite reward
    -- gets a ledger row too, keyed on a derived id, and this is what
    -- distinguishes the two. Nullable, so a proposal award needs no value.
    note TEXT
);

-- The idempotency key. Partial-unique on the live rows would also work, but a
-- full unique index is simpler to reason about and the conflict target in the
-- ON CONFLICT clause is what `reconcile` actually needs.
CREATE UNIQUE INDEX IF NOT EXISTS points_award_uniq
    ON points_award (account_id, proposal_id);

-- The leaderboard's only index: live rows, by account, for the SUM.
CREATE INDEX IF NOT EXISTS points_award_live
    ON points_award (account_id)
    WHERE withdrawn_at IS NULL;

CREATE INDEX IF NOT EXISTS points_award_proposal
    ON points_award (proposal_id);

-- Invite keys (stash-box#551: the count is configurable).
--
-- `max_uses` is a column rather than a boolean so a key can be single-use (the
-- default) or an open link a steward chose to widen. `used_at` and
-- `used_by` make redemption exactly-once even if two people click at the same
-- moment: the UPDATE is conditional on `used_at IS NULL`, so the loser of the
-- race updates zero rows and gets an error.
CREATE TABLE IF NOT EXISTS invite_key (
    key         TEXT PRIMARY KEY,
    created_by  TEXT NOT NULL,
    max_uses    INTEGER NOT NULL DEFAULT 1,
    uses        INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    expires_at  TEXT
);

-- One row per redemption, so "who came in through which key" is answerable and
-- not a guess from an accounts table's `created_at`.
CREATE TABLE IF NOT EXISTS invite_redeem (
    key         TEXT NOT NULL,
    account_id  TEXT NOT NULL,
    redeemed_at TEXT NOT NULL,
    PRIMARY KEY (key, account_id)
);

CREATE INDEX IF NOT EXISTS invite_redeem_account ON invite_redeem (account_id);
