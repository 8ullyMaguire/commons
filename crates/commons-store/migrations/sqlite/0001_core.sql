-- 0001_core.sql - SQLite mirror (Postgres: 0001_core.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0001_core.sql and re-run that script.
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
CREATE TABLE object (
  id             TEXT PRIMARY KEY,
  kind           TEXT NOT NULL,
  title          TEXT,
  description    TEXT,
  date           TEXT,
  producer_id    TEXT,
  organized      TEXT NOT NULL DEFAULT 'unreviewed',
  rating_sum     BIGINT NOT NULL DEFAULT 0,
  rating_count   BIGINT NOT NULL DEFAULT 0,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL
);
CREATE INDEX object_kind_idx      ON object (kind);
CREATE INDEX object_producer_idx  ON object (producer_id);
CREATE INDEX object_organized_idx ON object (organized);
CREATE INDEX object_date_idx      ON object (date DESC);

-- One file may back N objects via segment (stash#3530), so object:file is 1:n.
CREATE TABLE file (
  id           TEXT PRIMARY KEY,
  object_id    TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  path         TEXT NOT NULL,
  size_bytes   BIGINT NOT NULL DEFAULT 0,
  mtime_ns     BIGINT NOT NULL DEFAULT 0,
  hash_xxh128  TEXT,
  hash_blake3  TEXT,
  state        TEXT NOT NULL DEFAULT 'present',
  -- One path belongs to one object. Without this, a rescan after an
  -- interrupted run produces duplicate rows for the same file.
  UNIQUE (object_id, path)
);
CREATE INDEX file_path_idx   ON file (path);
CREATE INDEX file_hash_idx   ON file (hash_blake3);
CREATE INDEX file_xxh_idx    ON file (hash_xxh128);
CREATE INDEX file_state_idx  ON file (state);
-- Move detection looks up by content hash across the whole library.
CREATE INDEX file_object_state_idx ON file (object_id, state);

CREATE TABLE segment (
  id         TEXT PRIMARY KEY,
  file_id    TEXT NOT NULL REFERENCES file(id) ON DELETE CASCADE,
  object_id  TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  idx        INTEGER NOT NULL,
  start_ms   BIGINT NOT NULL,
  end_ms     BIGINT NOT NULL,
  UNIQUE (file_id, idx)
);
CREATE INDEX segment_object_idx ON segment (object_id);

CREATE TABLE archive (
  id            TEXT PRIMARY KEY,
  object_id     TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  path          TEXT NOT NULL,
  compression   TEXT,
  member_count  BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX archive_object_idx ON archive (object_id);

CREATE TABLE object_relation (
  id          TEXT PRIMARY KEY,
  from_id     TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  to_id       TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  relation    TEXT NOT NULL,
  asserted    INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  UNIQUE (from_id, to_id, relation)
);
CREATE INDEX object_relation_to_idx ON object_relation (to_id, relation);

-- The identity engine (§7.1). `state` is 'anonymous' for the overwhelming
-- majority of rows and that is a valid, browsable condition: it is what
-- links a person across a corpus no scraper can reach.
CREATE TABLE person_cluster (
  id               TEXT PRIMARY KEY,
  handle           TEXT,
  state            TEXT NOT NULL DEFAULT 'anonymous',
  centroid_hex     TEXT,
  appearance_count BIGINT NOT NULL DEFAULT 0,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);
CREATE INDEX person_cluster_state_idx ON person_cluster (state);

CREATE TABLE appearance (
  id              TEXT PRIMARY KEY,
  object_id       TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  cluster_id      TEXT NOT NULL REFERENCES person_cluster(id) ON DELETE CASCADE,
  appearance_type TEXT NOT NULL DEFAULT 'primary',
  face_id         TEXT,
  -- Composite score components are stored, not just the result: §7.4
  -- requires the UI to be able to say *why* two items were linked, and one
  -- opaque float makes that impossible.
  distance        REAL,
  face_score      REAL,
  body_score      REAL,
  ambiguous       INTEGER NOT NULL DEFAULT 0,
  source          TEXT NOT NULL,
  created_at      TEXT NOT NULL,
  UNIQUE (object_id, cluster_id, appearance_type)
);
CREATE INDEX appearance_cluster_idx    ON appearance (cluster_id);
CREATE INDEX appearance_object_idx     ON appearance (object_id);
CREATE INDEX appearance_ambiguous_idx  ON appearance (ambiguous);

CREATE TABLE performer (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  gender      TEXT,
  nationality TEXT,
  birth_date  TEXT,
  status      TEXT,
  claimed_at  TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX performer_name_idx ON performer (name);

-- Aliases are rows, not a delimited string: a comma inside an alias must never
-- split it (stash-box#778, stash#5033), and a name must be selectable as
-- primary (stash-box#610).
CREATE TABLE performer_alias (
  id             TEXT PRIMARY KEY,
  performer_id   TEXT NOT NULL REFERENCES performer(id) ON DELETE CASCADE,
  name           TEXT NOT NULL,
  is_primary     INTEGER NOT NULL DEFAULT 0,
  -- Optional studio/era scope for a stage name (stash#422, stash-box#818).
  producer_id    TEXT REFERENCES producer(id) ON DELETE SET NULL,
  UNIQUE (performer_id, name, producer_id)
);
CREATE INDEX performer_alias_name_idx ON performer_alias (name);

CREATE TABLE producer (
  id            TEXT PRIMARY KEY,
  kind          TEXT NOT NULL DEFAULT 'unknown',
  name          TEXT NOT NULL,
  career_start  TEXT,
  career_end    TEXT,
  defunct       INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL
);
CREATE INDEX producer_kind_idx  ON producer (kind);
CREATE INDEX producer_name_idx ON producer (name);

CREATE TABLE producer_alias (
  id           TEXT PRIMARY KEY,
  producer_id  TEXT NOT NULL REFERENCES producer(id) ON DELETE CASCADE,
  name         TEXT NOT NULL,
  is_primary   INTEGER NOT NULL DEFAULT 0,
  UNIQUE (producer_id, name)
);

CREATE TABLE producer_url (
  id           TEXT PRIMARY KEY,
  producer_id  TEXT NOT NULL REFERENCES producer(id) ON DELETE CASCADE,
  url          TEXT NOT NULL,
  -- 'active' | 'defunct' (stash-box#1161).
  state        TEXT NOT NULL DEFAULT 'active',
  favicon      TEXT,
  -- Sort order when the user overrides the default (stash#5238).
  ord          INTEGER NOT NULL DEFAULT 0,
  UNIQUE (producer_id, url)
);

CREATE TABLE group_obj (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  description  TEXT,
  code         TEXT,
  producer_id  TEXT REFERENCES producer(id) ON DELETE SET NULL,
  created_at   TEXT NOT NULL
);
CREATE TABLE group_member (
  group_id  TEXT NOT NULL REFERENCES group_obj(id) ON DELETE CASCADE,
  object_id TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  ord       INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (group_id, object_id)
);

-- Namespaces are the honesty mechanism for machine tagging (§5.15): an ML
-- tag is always visibly an ML tag.
CREATE TABLE tag (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  parent_id   TEXT REFERENCES tag(id) ON DELETE SET NULL,
  namespace   TEXT NOT NULL DEFAULT 'canonical',
  color       TEXT,
  importance  REAL NOT NULL DEFAULT 1.0,
  UNIQUE (namespace, name)
);
CREATE INDEX tag_parent_idx ON tag (parent_id);
CREATE INDEX tag_ns_idx     ON tag (namespace);

CREATE TABLE object_tag (
  object_id  TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  tag_id     TEXT NOT NULL REFERENCES tag(id) ON DELETE CASCADE,
  PRIMARY KEY (object_id, tag_id)
);
CREATE INDEX object_tag_tag_idx ON object_tag (tag_id);

-- The heart of the design (§8.1). Metadata is not a column per field: it is a
-- set of proposals per (subject, field) that vote to a settled value.
-- `subject_type` is polymorphic so performer and studio names vote by the same
-- machinery as object titles.
CREATE TABLE field_proposal (
  id            TEXT PRIMARY KEY,
  subject_type  TEXT NOT NULL,
  subject_id    TEXT NOT NULL,
  field         TEXT NOT NULL,
  value_json    TEXT NOT NULL,
  source        TEXT NOT NULL,
  proposer_kind TEXT NOT NULL,
  proposer_id   TEXT,
  confidence    REAL,
  created_at    TEXT NOT NULL
);
CREATE INDEX field_proposal_subject_idx ON field_proposal (subject_type, subject_id, field);
CREATE INDEX field_proposal_field_idx    ON field_proposal (field);
-- One proposer may not submit the same value twice for the same field.
CREATE UNIQUE INDEX field_proposal_uniq_idx
  ON field_proposal (subject_type, subject_id, field, value_json, source,
                     COALESCE(proposer_id, ''));

-- Votes are per-field on purpose (§8.3): agreeing about titles says nothing
-- about a voter's judgement on tags. `weight` is the reputation at cast time,
-- so history does not silently re-weight when reputation moves.
CREATE TABLE vote (
  id           TEXT PRIMARY KEY,
  proposal_id  TEXT NOT NULL REFERENCES field_proposal(id) ON DELETE CASCADE,
  account_id   TEXT NOT NULL,
  field        TEXT NOT NULL,
  weight       REAL NOT NULL DEFAULT 1.0,
  retracted    INTEGER NOT NULL DEFAULT 0,
  created_at   TEXT NOT NULL,
  UNIQUE (proposal_id, account_id)
);
CREATE INDEX vote_account_idx ON vote (account_id, field);
CREATE INDEX vote_field_idx   ON vote (field);

-- A locked field pins its value and refuses further proposals
-- (stash-box#213). Stored beside the proposal set, not on the subject, because
-- the lock is per field.
CREATE TABLE field_lock (
  subject_type  TEXT NOT NULL,
  subject_id    TEXT NOT NULL,
  field         TEXT NOT NULL,
  value_json    TEXT,
  locked_by     TEXT,
  locked_at     TEXT NOT NULL,
  PRIMARY KEY (subject_type, subject_id, field)
);

-- The consent record is the reason the rest of the design is allowed to be
-- ambitious (§14.1). `redistribution_permitted` gates P2P locators (§5.18)
-- and is deliberately NOT derivable from tier.
CREATE TABLE consent_record (
  id                        TEXT PRIMARY KEY,
  object_id                 TEXT NOT NULL UNIQUE REFERENCES object(id) ON DELETE CASCADE,
  tier                      TEXT NOT NULL DEFAULT 'unverified',
  redistribution_permitted  INTEGER NOT NULL DEFAULT 0,
  attested_by               TEXT,
  attested_at               TEXT,
  attested_basis            TEXT,
  decided_by                TEXT,
  decided_at                TEXT,
  updated_at                TEXT NOT NULL
);
CREATE INDEX consent_tier_idx ON consent_record (tier);

-- Append-only audit trail. Revocation propagates as a tombstone and is never
-- outvoted by contribution points (§13.2).
CREATE TABLE consent_event (
  id          TEXT PRIMARY KEY,
  object_id   TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  actor_id    TEXT,
  from_tier   TEXT,
  to_tier     TEXT,
  reason      TEXT,
  at          TEXT NOT NULL
);
CREATE INDEX consent_event_object_idx ON consent_event (object_id, at DESC);

-- Content-hash denylist. An accepted takedown adds entries here; they are
-- checked on import, scan, and match, and they propagate to every peer.
CREATE TABLE denied_hash (
  hash       TEXT PRIMARY KEY,
  reason     TEXT,
  added_at   TEXT NOT NULL,
  origin_peer TEXT
);

CREATE TABLE rating (
  id            TEXT PRIMARY KEY,
  subject_type  TEXT NOT NULL,
  subject_id    TEXT NOT NULL,
  account_id    TEXT NOT NULL,
  stars         INTEGER NOT NULL,
  created_at    TEXT NOT NULL,
  UNIQUE (subject_type, subject_id, account_id)
);
CREATE INDEX rating_subject_idx ON rating (subject_type, subject_id);

CREATE TABLE marker (
  id             TEXT PRIMARY KEY,
  object_id      TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  title          TEXT NOT NULL,
  start_ms       BIGINT NOT NULL,
  end_ms         BIGINT,
  primary_tag_id TEXT REFERENCES tag(id) ON DELETE SET NULL,
  rating         INTEGER,
  created_at     TEXT NOT NULL
);
CREATE INDEX marker_object_idx ON marker (object_id, start_ms);

CREATE TABLE subtitle (
  id          TEXT PRIMARY KEY,
  object_id   TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  path        TEXT,
  embedded    INTEGER NOT NULL DEFAULT 0,
  language    TEXT NOT NULL DEFAULT 'und',
  format      TEXT NOT NULL,
  kind        TEXT NOT NULL DEFAULT 'subtitle',
  extracted   TEXT,
  created_at  TEXT NOT NULL
);
CREATE INDEX subtitle_object_idx ON subtitle (object_id);

CREATE TABLE funscript (
  id          TEXT PRIMARY KEY,
  object_id   TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  path        TEXT NOT NULL,
  axis_count  INTEGER NOT NULL DEFAULT 1,
  metadata    TEXT,
  created_at  TEXT NOT NULL,
  UNIQUE (object_id, path)
);

CREATE TABLE list_obj (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  description TEXT,
  -- 'list' | 'playlist' | 'smart_collection' (§5.14).
  kind        TEXT NOT NULL DEFAULT 'list',
  -- For a smart collection: the serialized filter from T-P0-005.
  filter_json TEXT,
  owner_id    TEXT,
  shared      INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL
);
CREATE TABLE list_item (
  list_id   TEXT NOT NULL REFERENCES list_obj(id) ON DELETE CASCADE,
  object_id TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  ord       INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (list_id, object_id)
);

CREATE TABLE account (
  id          TEXT PRIMARY KEY,
  handle      TEXT NOT NULL UNIQUE,
  role        TEXT NOT NULL DEFAULT 'subscriber',
  reputation  REAL NOT NULL DEFAULT 1.0,
  pw_hash     TEXT,
  disabled    INTEGER NOT NULL DEFAULT 0,
  -- Per-field reputation (a JSON map) so title agreement does not raise tag
  -- weight (§8.3).
  field_reputation TEXT,
  created_at  TEXT NOT NULL
);

CREATE TABLE session (
  token      TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
CREATE INDEX session_account_idx ON session (account_id);

CREATE TABLE job (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL,
  state       TEXT NOT NULL DEFAULT 'queued',
  -- Idempotency: a watcher firing fifty times for one file must produce one
  -- job (T-P2-004).
  dedupe_key  TEXT NOT NULL UNIQUE,
  target_id   TEXT,
  attempts    INTEGER NOT NULL DEFAULT 0,
  last_error  TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX job_state_idx ON job (state, kind);

-- Generated files are keyed on every input that can change their contents, so
-- a file replaced at the same path invalidates exactly its own artifacts
-- (stash#7155, stash#2773).
CREATE TABLE artifact (
  id                TEXT PRIMARY KEY,
  file_id           TEXT NOT NULL REFERENCES file(id) ON DELETE CASCADE,
  kind              TEXT NOT NULL,
  path              TEXT NOT NULL,
  mtime_ns          BIGINT NOT NULL,
  size_bytes        BIGINT NOT NULL,
  generator_version INTEGER NOT NULL,
  created_at        TEXT NOT NULL,
  UNIQUE (file_id, kind)
);
CREATE INDEX artifact_kind_idx ON artifact (kind);

CREATE TABLE external_id (
  id          TEXT PRIMARY KEY,
  subject_type TEXT NOT NULL,
  subject_id  TEXT NOT NULL,
  source      TEXT NOT NULL,
  external_id TEXT NOT NULL,
  url         TEXT,
  UNIQUE (source, external_id)
);
CREATE INDEX external_id_subject_idx ON external_id (subject_type, subject_id);

CREATE TABLE custom_field (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE,
  attr_type   TEXT NOT NULL DEFAULT 'single',
  applies_to  TEXT NOT NULL DEFAULT 'object',
  ord         INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE custom_field_value (
  id          TEXT PRIMARY KEY,
  field_id    TEXT NOT NULL REFERENCES custom_field(id) ON DELETE CASCADE,
  subject_type TEXT NOT NULL,
  subject_id  TEXT NOT NULL,
  value_json  TEXT NOT NULL,
  at          TEXT,
  UNIQUE (field_id, subject_type, subject_id, at)
);
CREATE INDEX custom_field_value_subject_idx ON custom_field_value (subject_type, subject_id);

-- Peers are configured explicitly. There is no ambient discovery, which is
-- also what keeps §5.18.1's "no DHT" property true of this layer.
CREATE TABLE peer (
  id          TEXT PRIMARY KEY,
  endpoint    TEXT NOT NULL,
  public_key  TEXT NOT NULL,
  enabled     INTEGER NOT NULL DEFAULT 1,
  added_at    TEXT NOT NULL
);

CREATE TABLE claim (
  id            TEXT PRIMARY KEY,
  peer_id       TEXT NOT NULL,
  kind          TEXT NOT NULL,
  subject_type  TEXT NOT NULL,
  subject_id    TEXT,
  payload_json  TEXT NOT NULL,
  content_hash  TEXT NOT NULL,
  signature     TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  UNIQUE (peer_id, kind, subject_type, subject_id, content_hash)
);
CREATE INDEX claim_subject_idx ON claim (subject_type, subject_id);

-- Locators (§5.18). The table and the tier gate are core; computing ed2k
-- hashes and infohashes is the plugin's job (§5.18.1), reachable only through
-- locator.propose, which re-checks the tier.
CREATE TABLE locator (
  id          TEXT PRIMARY KEY,
  object_id   TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  file_id     TEXT REFERENCES file(id) ON DELETE CASCADE,
  scheme      TEXT NOT NULL,
  uri         TEXT NOT NULL,
  infohash    TEXT,
  size_bytes  BIGINT,
  name        TEXT,
  source      TEXT NOT NULL,
  added_at    TEXT NOT NULL,
  UNIQUE (object_id, scheme, uri)
);
CREATE INDEX locator_object_idx ON locator (object_id);
CREATE INDEX locator_infohash_idx ON locator (infohash);

-- Notifications about metadata activity (§15.4). Deliberately not a social
-- feed (§2): it watches entities, not people.
CREATE TABLE watch (
  id           TEXT PRIMARY KEY,
  account_id   TEXT NOT NULL,
  subject_type TEXT NOT NULL,
  subject_id   TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  UNIQUE (account_id, subject_type, subject_id)
);

CREATE TABLE notification (
  id           TEXT PRIMARY KEY,
  account_id   TEXT NOT NULL,
  kind         TEXT NOT NULL,
  subject_type TEXT,
  subject_id   TEXT,
  body         TEXT NOT NULL,
  read_at      TEXT,
  created_at   TEXT NOT NULL
);
CREATE INDEX notification_account_idx ON notification (account_id, read_at, created_at DESC);
