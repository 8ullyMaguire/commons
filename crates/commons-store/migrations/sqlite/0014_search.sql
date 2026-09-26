-- 0014_search.sql - SQLite mirror (Postgres: 0014_search.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0014_search.sql and re-run that script.
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

CREATE TABLE search_term (
  object_id  TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  field      TEXT NOT NULL,
  term       TEXT NOT NULL,
  -- The UNIQUE constraint, not just an index. `index_object` relies on
  -- `ON CONFLICT DO NOTHING` to be idempotent, and without this the clause
  -- fails at *runtime* with "does not match any PRIMARY KEY or UNIQUE
  -- constraint" -- on one engine, after the other has already worked. It was
  -- missing here and the tests caught it, which is the argument for writing
  -- the conflict clause against a constraint that has to exist.
  UNIQUE (object_id, field, term)
);

-- The lookup is "which objects have this term", so the term leads.
CREATE INDEX search_term_term_idx  ON search_term (term);
-- "reindex this object" and the per-object field lookup for a result.
CREATE INDEX search_term_object_idx ON search_term (object_id, field);

-- §9.3's synonym table, per-vocabulary. Per-vocabulary and not global because
-- a global table cannot express "ass" meaning two things, which §9.3 names as
-- the case synonyms exist for.
--
-- `expands_to` is a list because a term can mean more than one thing and the
-- table has to hold both directions ("cunt" -> the tag, "ass" -> both its
-- meanings). Storing one synonym per row would put a list in a scalar column on
-- one engine and a text[] on the other -- a schema difference the portability
-- rule of §15.2 exists to prevent. The separator is a space, which is safe
-- because a term is a run of alphanumerics: it cannot contain one.
CREATE TABLE search_synonym (
  vocabulary  TEXT NOT NULL,
  term        TEXT NOT NULL,
  expands_to  TEXT NOT NULL,
  PRIMARY KEY (vocabulary, term)
);
