-- The SQLite form of `ALTER TABLE tag ALTER COLUMN importance TYPE
-- DOUBLE PRECISION` in 0016_tag_attributes.sql.
--
-- SQLite has no data types to widen -- it has storage classes -- so the
-- twelve-step rebuild is the only route, the same shape 0002, 0006 and 0017 use.
-- The value is carried across untouched; SQLite has been storing it as a 64-bit
-- real all along, which is exactly why the Postgres side is the one that had a
-- problem and this side does not.
--
-- The definition below is 0001's `tag` verbatim. That is the cost of rebuilding
-- an applied table, and the reason it is worth being explicit about: this file
-- now owns `tag`'s shape, so a column added by a later migration has to be added
-- here too or the rebuild drops it. Every other rebuild in this set carries the
-- same warning, and the honest fix -- a generator that reads the live schema --
-- is a bigger piece of work than this ticket.
--
-- One thing that is easy to get wrong and is not wrong here: the self-reference
-- `parent_id TEXT REFERENCES tag(id) ON DELETE SET NULL` is preserved as it was,
-- including the action. Dropping `ON DELETE SET NULL` would turn a delete of a
-- parent into a constraint failure instead of orphaning the children.
CREATE TABLE tag_new (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  parent_id   TEXT REFERENCES tag(id) ON DELETE SET NULL,
  namespace   TEXT NOT NULL DEFAULT 'canonical',
  color       TEXT,
  -- 0001's, widened. See the note in the Postgres file.
  importance  DOUBLE PRECISION NOT NULL DEFAULT 1.0,
  UNIQUE (namespace, name)
);

INSERT INTO tag_new (id, name, parent_id, namespace, color, importance)
     SELECT id, name, parent_id, namespace, color, importance FROM tag;

DROP TABLE tag;

ALTER TABLE tag_new RENAME TO tag;
