-- 0015_fuzzy.sql - SQLite mirror (Postgres: 0015_fuzzy.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0015_fuzzy.sql and re-run that script.
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

CREATE TABLE search_fuzzy (
  object_id  TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  field      TEXT NOT NULL,
  -- The indexed term this key stands for. Stored so a hit can report what was
  -- actually matched rather than the term the caller typed, and so a candidate
  -- can be re-checked against the query term.
  term       TEXT NOT NULL,
  key        TEXT NOT NULL,
  UNIQUE (object_id, field, term, key)
);

-- The query is "which rows share this key", so the key leads. The object index
-- is for deindexing and for the per-object field report.
CREATE INDEX search_fuzzy_key_idx    ON search_fuzzy (key);
CREATE INDEX search_fuzzy_object_idx ON search_fuzzy (object_id, field);

-- §9.3's alias and nickname awareness: an alternate name for one object, which
-- the search must find by.
--
-- Not `performer_alias`. That table (§7.2, T-P3-004) is scoped to a performer and
-- carries studio and era, because a stage name is a *fact about a history* and
-- the same name can be two different things. This is a search aid: whatever the
-- user calls a file, folder, performer or performer set, so that typing it finds
-- the thing. The two tables answer different questions and conflating them would
-- make every alias a claim about a performer's history.
--
-- The alias is a row here and *tokens* in `search_term` like any other field, so
-- an alias is found by an exact search, a fuzzy one, and a synonym expansion
-- without any of those three knowing that aliases exist. The `kind` column is
-- what tells the indexer which field weight an alias deserves: an alias is
-- somebody's name for the thing, so it is titled text, not a tag.
CREATE TABLE object_alias (
  object_id  TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  alias      TEXT NOT NULL,
  -- Free text rather than a foreign key: the spec names the concept, not the
  -- vocabulary, and a nickname has no table to point at.
  kind       TEXT NOT NULL DEFAULT 'nickname',
  -- An alias is a name, and names are case-folded when compared, so an alias
  -- differing from an existing one only by case is a duplicate the user cannot
  -- see. `COLLATE NOCASE` would do this on SQLite alone, so the uniqueness is
  -- stated in terms of a stored, lower-cased form instead and both engines
  -- enforce the same thing.
  alias_key  TEXT NOT NULL,
  PRIMARY KEY (object_id, alias_key)
);

-- The read path is "which objects answer to this alias", so the index leads.
CREATE INDEX object_alias_key ON object_alias (alias_key);
