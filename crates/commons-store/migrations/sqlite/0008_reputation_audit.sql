-- Migration: 0008 reputation_audit
--
-- Mirrors postgres/0008. `COALESCE` in the unique indexes is the same
-- expression-index technique 0006 used for `appearance`: it makes a NULL
-- `proposal_id` comparable to another NULL without writing a sentinel into the
-- data, so "one event per account per kind for a non-proposal event" holds
-- without every writer having to remember to substitute a string.
--
-- The expression goes in a `CREATE UNIQUE INDEX`, never in a table-level
-- `UNIQUE (...)` constraint. SQLite rejects an expression in the latter and
-- accepts it in the former; Postgres accepts both. The version that had it as a
-- table constraint therefore passed every Postgres test and failed the first
-- SQLite test that touched it.
CREATE TABLE reputation_event (
  id          TEXT PRIMARY KEY,
  account_id  TEXT NOT NULL,
  field       TEXT NOT NULL,
  kind        TEXT NOT NULL,
  proposal_id TEXT,
  delta       REAL NOT NULL,
  weight_at   REAL NOT NULL,
  created_at  TEXT NOT NULL
);
CREATE INDEX reputation_event_account_idx ON reputation_event (account_id, field);
CREATE INDEX reputation_event_field_idx    ON reputation_event (field, created_at);
CREATE UNIQUE INDEX reputation_event_uniq
  ON reputation_event (account_id, COALESCE(proposal_id, ''), kind);

CREATE TABLE trust_tier (
  account_id  TEXT PRIMARY KEY,
  tier        INTEGER NOT NULL DEFAULT 0,
  granted_by  TEXT,
  granted_at  TEXT NOT NULL,
  reason      TEXT,
  revoked_by  TEXT,
  revoked_at  TEXT
);

CREATE TABLE coordination_flag (
  id          TEXT PRIMARY KEY,
  field       TEXT NOT NULL,
  proposal_id TEXT,
  reason      TEXT NOT NULL,
  accounts    TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'flagged',
  created_at  TEXT NOT NULL
);
CREATE INDEX coordination_flag_field_idx ON coordination_flag (field, status);
CREATE UNIQUE INDEX coordination_flag_uniq
  ON coordination_flag (field, COALESCE(proposal_id, ''));
