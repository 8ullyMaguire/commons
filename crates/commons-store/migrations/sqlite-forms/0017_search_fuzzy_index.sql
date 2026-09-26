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
