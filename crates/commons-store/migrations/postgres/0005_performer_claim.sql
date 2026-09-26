-- §7.5's self-service performer claim.
--
-- # Why this is not the `claim` table
--
-- `claim` already exists and carries a signed, content-hashed assertion from a
-- peer (§14.1). A performer claim is a different thing: an unverified human
-- saying "that cluster is me", carrying no signature, going to moderation, and
-- being able to be wrong. Folding the two together would make an unsigned
-- self-service row look like a signed federation claim to every reader, and the
-- consent chain §7.5 exists to make trustworthy would inherit the least
-- trustworthy input. So the name is `performer_claim` and the two never share a
-- table.
--
-- # One pending claim per account, one per cluster
--
-- Both uniqueness constraints, because both abuses are real and each is a queue
-- full of duplicates a moderator cannot act on:
--
--   * one pending claim per account -- otherwise a person claims every cluster
--     in the library and the queue becomes their list
--   * one pending claim per cluster -- otherwise a contested cluster accumulates
--     one row per person who clicked
--
-- The constraints are partial -- `WHERE state = 'queued'` -- because a
-- *rejected* claim must not block a later one. A moderator rejecting a bad claim
-- is a normal outcome and the person is allowed to try again with better
-- evidence, so "one pending claim" is the rule and "one claim ever" is not.
--
-- Both engines have partial indexes (SQLite since 3.8, Postgres since 9.0), so
-- this needs no `--:sqlite` block.
--
-- # `decided_by` is not optional
--
-- §7.5's claim is the mechanism by which a takedown request reaches a person.
-- If the claim's own approval were anonymous, the consent chain would have a
-- link with no owner, and the platform's answer to "who agreed to this?" would
-- be a shrug. Every state change names a moderator.

CREATE TABLE IF NOT EXISTS performer_claim (
    id          TEXT PRIMARY KEY,
    cluster_id  TEXT NOT NULL REFERENCES person_cluster(id) ON DELETE CASCADE,
    account     TEXT NOT NULL,
    -- §7.5: a person "privately verifies" a cluster. The evidence is for a
    -- moderator, never published.
    evidence    TEXT NOT NULL,
    state       TEXT NOT NULL DEFAULT 'queued',
    decided_by  TEXT,
    decided_at  TEXT,
    created_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS performer_claim_cluster ON performer_claim (cluster_id, state);
CREATE INDEX IF NOT EXISTS performer_claim_account ON performer_claim (account, state);

CREATE UNIQUE INDEX IF NOT EXISTS performer_claim_one_pending_per_cluster
    ON performer_claim (cluster_id) WHERE state = 'queued';
CREATE UNIQUE INDEX IF NOT EXISTS performer_claim_one_pending_per_account
    ON performer_claim (account) WHERE state = 'queued';

-- A verified claim: one cluster, one performer. The composite primary key is the
-- scope rule made structural rather than procedural -- §7.5's whole guarantee
-- is that a verified performer edits their own record and no other, and here
-- that is a constraint the database enforces rather than a check some caller
-- might forget.
--
-- `performer_id` is nullable because §7.5 lets a person verify a cluster
-- *before* there is a performer record to attach to (the claim is often how the
-- record gets created). NULL means "verified, no record yet" and is a legitimate
-- state, not a half-finished one.
CREATE TABLE IF NOT EXISTS performer_verification (
    cluster_id    TEXT NOT NULL REFERENCES person_cluster(id) ON DELETE CASCADE,
    performer_id  TEXT,
    account       TEXT NOT NULL,
    claim_id      TEXT NOT NULL REFERENCES performer_claim(id) ON DELETE CASCADE,
    verified_at   TEXT NOT NULL,
    PRIMARY KEY (cluster_id)
);

-- A cluster may be verified by at most one performer, but a performer may hold
-- several verifications (one per person they are: a performer who appears as
-- more than one identity is the case §7.5 exists for). So the index is on the
-- performer, not unique.
CREATE INDEX IF NOT EXISTS performer_verification_performer
    ON performer_verification (performer_id);

-- §7.5's third clause: a takedown request reaches the person in the content
-- through their verified claim. A request with no recipient is not dropped --
-- it goes to the stewards, which is why `to_stewards` exists rather than the
-- row simply having no recipients.
--
-- `cluster_id` rather than the performer: the request is about content, and the
-- performer is derived from the cluster at the moment it is read. A takedown
-- outliving a claim change then reaches whoever holds the cluster now, which is
-- the right behaviour -- the content is still there.
CREATE TABLE IF NOT EXISTS takedown_request (
    id            TEXT PRIMARY KEY,
    cluster_id    TEXT NOT NULL REFERENCES person_cluster(id) ON DELETE CASCADE,
    requested_by  TEXT NOT NULL,
    reason        TEXT NOT NULL,
    to_stewards  INTEGER NOT NULL DEFAULT 0,
    state         TEXT NOT NULL DEFAULT 'open',
    created_at    TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS takedown_cluster ON takedown_request (cluster_id, state);
