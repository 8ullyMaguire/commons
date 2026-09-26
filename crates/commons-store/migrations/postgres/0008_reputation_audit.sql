-- Migration: 0008 reputation_audit
--
-- §8.3's six points need two things 0001 does not have: somewhere to record
-- that a trust tier was granted (stash-box#630 asks for an audit trail, and a
-- tier with no record of who granted it is indistinguishable from a tier nobody
-- granted), and somewhere to record that a coordinated pattern was *flagged*.
--
-- The flag table matters more than it looks. §8.3 says coordinated patterns are
-- flagged for a steward and "never silently punished" — so the flag must be
-- durable enough that a steward can see it, and the penalty must be a separate
-- act. Putting the flag in the same row as the account would make "flagged" and
--"penalised" the same write, which is precisely the conflation §8.3 forbids.
--
-- `reputation_event` is append-only and is the *input* to a reputation
-- recomputation, not a counter. §8.6's rule — scores are recomputed from the
-- accepted-edit set, never maintained as a counter — applies to reputation for
-- the same reason it applies to votes: a counter drifts on every path that
-- forgets to update it, and there is always one.
CREATE TABLE reputation_event (
  id          TEXT PRIMARY KEY,
  account_id  TEXT NOT NULL,
  field       TEXT NOT NULL,
  -- What happened: agreed, disputed, retracted, decayed, granted, revoked.
  kind        TEXT NOT NULL,
  -- The proposal the event is about, when it is about one. NULL for events that
  -- are not (a decay sweep, a steward grant).
  proposal_id TEXT,
  -- Signed contribution. Negative for a dispute. Kept as a number rather than
  -- derived from `kind` so that a dispute's size can be recorded without
  -- inventing a kind per magnitude.
  delta       REAL NOT NULL,
  -- The weight this account held when the event happened, so a recomputation can
  -- be checked against what the system believed at the time rather than only
  -- against its current belief.
  weight_at   REAL NOT NULL,
  created_at  TEXT NOT NULL
);
CREATE INDEX reputation_event_account_idx ON reputation_event (account_id, field);
CREATE INDEX reputation_event_field_idx    ON reputation_event (field, created_at);
-- One event per (account, proposal, kind): re-running a settlement pass must not
-- double-count, and the constraint is what makes that true rather than a
-- property of the caller's loop.
CREATE UNIQUE INDEX reputation_event_uniq
  ON reputation_event (account_id, COALESCE(proposal_id, ''), kind);

-- A steward-granted trust tier (§8.3, stash-box#630).
--
-- Separate from `role`: a role is what an account may *do*, a trust tier is what
-- its word is *worth*. A steward is trusted and may also curate; a trusted
-- contributor is neither. Collapsing them would mean granting vote weight and
-- queue access with the same click.
CREATE TABLE trust_tier (
  account_id  TEXT PRIMARY KEY,
  -- 0 is an ordinary account; higher tiers weigh more. Stored as an integer so
  -- the *order* is in the type and a comparison cannot be a string compare.
  tier        INTEGER NOT NULL DEFAULT 0,
  granted_by  TEXT,
  granted_at  TEXT NOT NULL,
  reason      TEXT,
  revoked_by  TEXT,
  revoked_at  TEXT
);

-- A coordinated pattern, flagged for a steward and not yet acted on (§8.3).
--
-- `status` is 'flagged' until a steward resolves it, and a resolution is a
-- separate row in `audit_log`. So the path from "we noticed" to "we acted" is
-- always visible, and there is no code path that punishes without one.
CREATE TABLE coordination_flag (
  id          TEXT PRIMARY KEY,
  field       TEXT NOT NULL,
  -- The proposal the pattern is centred on.
  proposal_id TEXT,
  -- Why it looked coordinated, in words a steward can act on. Not a score: a
  -- number here would be a verdict, and §8.3 wants a referral.
  reason      TEXT NOT NULL,
  -- The accounts involved, JSON. A relation table would be tidier and would
  -- also be a second write that can half-succeed; a flag is a referral, not a
  -- claim, and a half-written referral that names fewer accounts is still a
  -- true referral.
  accounts    TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'flagged',
  created_at  TEXT NOT NULL
);
CREATE INDEX coordination_flag_field_idx ON coordination_flag (field, status);
CREATE UNIQUE INDEX coordination_flag_uniq
  ON coordination_flag (field, COALESCE(proposal_id, ''));
