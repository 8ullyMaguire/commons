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
