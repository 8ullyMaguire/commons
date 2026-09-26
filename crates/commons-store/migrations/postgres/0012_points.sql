-- Migration: 0012 points, badges, invites
--
-- §8.4's economy. Three tables, and the shape of the first one is the whole
-- design decision in this ticket.
--
-- `points_award` is a LEDGER, not a balance. There is no `points_total` column
-- anywhere, deliberately, for the reason T-P4-004 gives about field scores: a
-- stored number has to be kept in step with the evidence, and the moment two
-- writers touch it they disagree. A ledger answers "what is this account worth"
-- by summing rows, answers "why" by reading them, and undoes itself by marking
-- one withdrawn rather than by decrementing a remembered amount.
--
-- The unique key is (account_id, proposal_id), NOT an autoincrement. That is what
-- makes `reconcile` idempotent: re-running it over an unchanged field cannot pay
-- twice, and a proposal that stops winning and then wins again returns to *the
-- same row* rather than forking an account's history in two.
--
-- `withdrawn_at` rather than a delete, for T-P4-004's reason: a leaderboard that
-- deletes the evidence of a withdrawn award cannot explain a balance that moved.

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
