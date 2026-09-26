-- 0017_search_index_any_kind.sql - SQLite mirror (Postgres: 0017_search_index_any_kind.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0017_search_index_any_kind.sql and re-run that script.
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

-- `search_fuzzy` has the same constraint and the same need, for the same
-- reason: a tag's fuzzy keys are written by `create_tag`, so a tag that could
-- not be indexed exactly could not be indexed fuzzily either. 0015 wrote it
-- with the constraint because at the time the only thing being indexed was an
-- object; this migration widens both indexes together so the two cannot end up
-- disagreeing about what is indexable.
-- SQLite form of the statement above (../sqlite-forms/0017_search_index_any_kind.sql).
-- The SQLite form of `ALTER TABLE search_term DROP CONSTRAINT ...` in
-- 0017_search_index_any_kind.sql.
--
-- SQLite has no `ALTER TABLE ... DROP CONSTRAINT`, and a column constraint
-- cannot be removed any other way, so the table is rebuilt.
--
-- The rebuild is the whole reason this file exists and the reason 0017 is worth
-- being a migration at all. `search_term` is derived data, but "rebuild it"
-- still means a window in which the index does not exist — a search in that
-- window returns nothing rather than everything, which is the safe direction to
-- fail. The three steps run inside the migration's own transaction, so the
-- window is not a separate committed state: either the old table is there or the
-- new one is, and never neither.
--
-- `PRAGMA foreign_keys = OFF` is *not* used. It is a no-op inside a transaction
-- -- SQLite ignores it for the duration of one, deliberately, because a
-- `PRAGMA` that took effect mid-transaction would change what an already-parsed
-- statement means. The rebuild therefore does not need it: the new table
-- declares no foreign key, and the rows are copied before the old table is
-- dropped, so no constraint is ever violated.
--
-- That is worth spelling out because the obvious version of this file turns the
-- pragma on and off around the rebuild, and it would do nothing at all. A
-- migration that appears to disable a safety check and does not is worse than
-- one that never mentions it.

CREATE TABLE search_term_new (
  -- The name is unchanged and now slightly a misnomer: the column holds a tag
  -- id for some rows, because 0017 widened the index past `object`. Renaming it
  -- in an applied migration would be a second migration, and the name is right
  -- for the overwhelming majority of rows.
  object_id  TEXT NOT NULL,
  field      TEXT NOT NULL,
  term       TEXT NOT NULL,
  UNIQUE (object_id, field, term)
);

INSERT INTO search_term_new (object_id, field, term)
     SELECT object_id, field, term FROM search_term;

DROP TABLE search_term;

ALTER TABLE search_term_new RENAME TO search_term;

-- SQLite form of the statement above (../sqlite-forms/0017_search_fuzzy_index.sql).
-- The SQLite form of `ALTER TABLE search_fuzzy DROP CONSTRAINT ...` in
-- 0017_search_index_any_kind.sql.
--
-- A separate sidecar from the `search_term` one because `scripts/sync-migrations.py`
-- substitutes a `--:sqlite` block with the *whole* sidecar file, and one file
-- holding both rebuilds would run `search_term`'s twice for the second block.
-- `search_fuzzy` is widened the same way, in the same migration, for the same
-- reason. One migration rather than two: a schema in which the exact index
-- accepts a tag and the fuzzy index does not is a schema where a tag is findable
-- by its name and not by a typo of it, which is a bug nobody would think to
-- look for.
--
-- The rows are copied the same way, and the UNIQUE is kept whole: 0015 put it on
-- all four columns precisely because one term generates many keys.
CREATE TABLE search_fuzzy_new (
  object_id  TEXT NOT NULL,
  field      TEXT NOT NULL,
  term       TEXT NOT NULL,
  key        TEXT NOT NULL,
  UNIQUE (object_id, field, term, key)
);

INSERT INTO search_fuzzy_new (object_id, field, term, key)
     SELECT object_id, field, term, key FROM search_fuzzy;

DROP TABLE search_fuzzy;

ALTER TABLE search_fuzzy_new RENAME TO search_fuzzy;

-- The index already exists: 0014 created `search_term_object_id_idx` for the
-- same column, and the rebuild in the SQLite sidecar drops it along with the
-- table it was on. So the constraint is all that is dropped, and adding a
-- second index here would be a name collision rather than an optimisation.
