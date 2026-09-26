-- Migration: 0009 file_signal
--
-- Mirrors postgres/0009. The expression unique index on
-- `COALESCE(origin, '')` is the same technique 0006 and 0008 used, so two
-- signals of the same kind from different origins stay distinct rows instead of
-- one silently overwriting the other.
CREATE TABLE file_signal (
  id          TEXT PRIMARY KEY,
  file_id     TEXT NOT NULL REFERENCES file(id) ON DELETE CASCADE,
  kind        TEXT NOT NULL,
  value       TEXT NOT NULL,
  origin      TEXT,
  confidence  REAL,
  extracted_at TEXT NOT NULL
);
-- An index rather than a table constraint, for the reason 0008 documents: SQLite
-- rejects an expression in `UNIQUE (...)` and accepts it here, and Postgres
-- accepts both, so the wrong one passes review and fails on SQLite. The value is
-- in the key because one tagger run produces many `ml:tag` signals for one file.
CREATE UNIQUE INDEX file_signal_uniq
  ON file_signal (file_id, kind, COALESCE(origin, ''), value);

CREATE TABLE file_phash (
  file_id  TEXT PRIMARY KEY REFERENCES file(id) ON DELETE CASCADE,
  phash    TEXT NOT NULL,
  algorithm TEXT NOT NULL DEFAULT 'phash64',
  created_at TEXT NOT NULL
);

CREATE INDEX file_phash_lookup_idx ON file_phash (phash);

CREATE TABLE object_phash (
  object_id  TEXT PRIMARY KEY REFERENCES object(id) ON DELETE CASCADE,
  phash      TEXT NOT NULL,
  algorithm  TEXT NOT NULL DEFAULT 'phash64',
  created_at TEXT NOT NULL
);
CREATE INDEX object_phash_lookup_idx ON object_phash (phash);

ALTER TABLE peer ADD COLUMN name TEXT;

CREATE TABLE peer_value (
  id          TEXT PRIMARY KEY,
  peer_id     TEXT NOT NULL REFERENCES peer(id) ON DELETE CASCADE,
  subject_id  TEXT NOT NULL,
  field       TEXT NOT NULL,
  value_json  TEXT NOT NULL,
  confidence  REAL,
  margin      REAL,
  created_at  TEXT NOT NULL,
  UNIQUE (peer_id, subject_id, field)
);
CREATE INDEX peer_value_subject_idx ON peer_value (subject_id, field);
