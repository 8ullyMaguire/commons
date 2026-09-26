-- Migration: 0014 search term index
--
-- §9.2's breadth and §9.3's "one behaviour across both engines".
--
-- The surprising thing about this table is that it is not an FTS table. §9.3
-- asks for "the same tokenizer ... in both Postgres and the embedded store",
-- and the only way to get that is to own the tokenizer: SQLite's FTS5
-- `unicode61` and Postgres's `to_tsvector('english', ...)` disagree about
-- stemming, stop words and hyphenation, and two native implementations have two
-- tokenizers, not one. So the terms are produced by `search::tokenize` in Rust
-- and stored as ordinary rows, and neither engine gets an opinion about them.
--
-- The table is deliberately an ordinary table rather than an FTS virtual table
-- or a `tsvector` column. It costs a scan per term, which for a personal
-- library is the right trade, and a hosted index can add a native index
-- *alongside* these rows -- keyed on the same terms -- to get the same answers
-- faster. What it cannot do is answer differently, which is the property that
-- actually matters.
--
-- `field` is part of the key rather than a weight column. A weight per row is a
-- number that has to be kept in step with the weighting table in
-- `search::Field::weight` when the weighting changes; a field name is resolved
-- through the same enum in both places, so there is one table and no rebuild.
--
-- One row per (object, field, term): a title and a description holding the
-- same word are two rows, so a title hit is distinguishable from a description
-- hit and a title ranks above a description without a second index.

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
