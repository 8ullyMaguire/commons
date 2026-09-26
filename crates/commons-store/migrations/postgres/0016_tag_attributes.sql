-- Migration: 0016 tag attributes, groups, and ML confidence
--
-- §5.15 and §9.4, and the three things 0001's `tag` table could not hold.
--
-- 0001 was written with the right columns for a *name* -- a parent, a
-- namespace, a color, an importance weight -- and nothing for a tag that says
-- something about its subject. A pose tag that is true of half its objects and
-- false of the other half, an ML tag's confidence, a tag that belongs to a
-- set, and a typed attribute are four different things and 0001 has no column
-- for any of them. 0001 is immutable, so they are added here.

-- ---------------------------------------------------------------------------
-- Typed attributes (#3400)
--
-- An attribute is a name, a declared type, and a value. The type is declared
-- on the *name* rather than stored per value, so "a pose tag takes one of
-- these" is a fact about the taxonomy and not a fact every row has to repeat
-- and agree on.
--
-- Four kinds, not one TEXT column. The reason is the filter: §5.16 wants
-- `is null`, `between` and typed operators, and a value stored as text cannot
-- answer `between` without a parse per row whose failure mode is silent -- the
-- row that does not parse is simply not in the range, which looks like an
-- absent value rather than a broken one. Four columns means a `between` is a
-- comparison and a `is null` is an `IS NULL` on the right one.
--
-- `value_text` / `value_num` / `value_date` / `value_ref` with exactly one
-- non-NULL, enforced by the CHECK below rather than by convention. SQLite has
-- no generated columns in the version floor and no domain types, and a
-- Postgres `ENUM`/`DOMAIN` cannot be mirrored into SQLite's type system -- so
-- the constraint is a CHECK, which both engines read the same way.
--
-- `value_date` is ISO-8601 UTC TEXT, like every other timestamp here (§15.2):
-- lexicographic order is chronological order for that format, so `between` on
-- dates is a string comparison on both engines and needs no date function.
CREATE TABLE tag_attribute_type (
  name  TEXT PRIMARY KEY,
  -- The declared type, and the fourth kind (`bool`) is `value_num` holding 0
  -- or 1: a boolean is a number that happens to have two values, and a
  -- separate column would make every aggregate over it engine-specific.
  kind  TEXT NOT NULL
);

-- A declared value, for an attribute whose values come from a fixed set (a
-- pose taxonomy, a position, a studio's own vocabulary). `NULL` in
-- `value_text` means the set is unbounded -- an *open* enumeration, which is
-- the common case and is not the same as an empty one.
CREATE TABLE tag_attribute_option (
  attribute_name TEXT NOT NULL REFERENCES tag_attribute_type(name) ON DELETE CASCADE,
  option_text   TEXT NOT NULL,
  PRIMARY KEY (attribute_name, option_text)
);

-- The values themselves, one row per (tag, attribute).
--
-- A separate table rather than three nullable columns on `tag`, because a tag
-- has *many* attributes and a nullable column per attribute would need a
-- migration per attribute anyone ever invents. The cost is a join; the benefit
-- is that adding an attribute to the taxonomy is data, not a schema change.
CREATE TABLE tag_attribute (
  tag_id        TEXT NOT NULL REFERENCES tag(id) ON DELETE CASCADE,
  attribute     TEXT NOT NULL REFERENCES tag_attribute_type(name),
  value_text    TEXT,
  -- DOUBLE PRECISION, not REAL: `REAL` is FLOAT4 on Postgres and FLOAT8 on SQLite, so a
  -- `f64` decode works on one engine and fails on the other with `mismatched
  -- types; Rust type f64 (as SQL type FLOAT8) is not compatible with SQL type
  -- FLOAT4`. Both spellings are 64-bit doubles; the names are not, and the
  -- portable spelling is the explicit one. Plan section 0.4 lists float widths
  -- as a portability rule and this is the first migration to trip it.
  value_num     DOUBLE PRECISION,
  value_date    TEXT,
  value_ref     TEXT,
  PRIMARY KEY (tag_id, attribute),
  -- Exactly one value column is set. Enforced here rather than by the writer
  -- because a row with two set is not a state any reader can interpret, and
  -- every reader would have to guess which one it meant.
  --
  -- Written as a sum of `(x IS NULL)` *comparisons* rather than a sum of the
  -- booleans. SQLite evaluates `(x IS NULL) + (y IS NULL)` as integer addition,
  -- because SQLite's booleans are integers; Postgres has a real `boolean` type
  -- and has no `+` for it, so the same expression is
  -- `operator does not exist: boolean + boolean` and the whole migration fails
  -- to apply. `CASE WHEN ... THEN 1 ELSE 0 END` is the portable spelling of an
  -- integer-typed `IS NULL` and reads the same on both.
  --
  -- This is the third time this class of divergence has appeared in this
  -- migration, and the first two were the same shape: a construct that is valid
  -- SQLite and invalid Postgres, invisible until something applied the Postgres
  -- tree. The parity test compares table *sets*, so a CHECK constraint is
  -- invisible to it by construction.
  CHECK (
    (CASE WHEN value_text IS NULL THEN 1 ELSE 0 END)
  + (CASE WHEN value_num  IS NULL THEN 1 ELSE 0 END)
  + (CASE WHEN value_date IS NULL THEN 1 ELSE 0 END)
  + (CASE WHEN value_ref  IS NULL THEN 1 ELSE 0 END) = 3
  )
);

-- The read is "every attribute of this tag, in a stable order", and the write
-- is "these tags' attributes for this name", so the index leads with the
-- attribute: a filter over `pose` should not scan the whole table.
CREATE INDEX tag_attribute_name_idx ON tag_attribute (attribute, tag_id);

-- ---------------------------------------------------------------------------
-- Groups (#3469)
--
-- A group is a set of tags, and a tag is in a set of groups. That is a join
-- table, and the interesting part is that membership is *not* inherited: a
-- group's members do not become members of the group's parent group. An
-- inherited membership is a tree walk per read, and a group tree would then
-- need cycle rules the tree does not have. §9.4 asks for groups alongside the
-- tree, not instead of it.
CREATE TABLE tag_group (
  id    TEXT PRIMARY KEY,
  name  TEXT NOT NULL UNIQUE,
  color TEXT
);

CREATE TABLE tag_group_member (
  group_id TEXT NOT NULL REFERENCES tag_group(id) ON DELETE CASCADE,
  tag_id   TEXT NOT NULL REFERENCES tag(id) ON DELETE CASCADE,
  PRIMARY KEY (group_id, tag_id)
);

-- The read is "which groups is this tag in" as often as it is the reverse.
CREATE INDEX tag_group_member_tag_idx ON tag_group_member (tag_id);

-- ---------------------------------------------------------------------------
-- ML confidence (§8.2: "the confidence is visible")
--
-- 0001's `tag` has no confidence, and a confidence per *tag* is wrong: the
-- same tag proposed for two objects by the same model has two confidences,
-- and one number on the tag row would have to be whichever was written last.
-- So confidence belongs to the (object, tag) pair -- the same place the
-- application is, which is `object_tag`.
--
-- The three columns on `object_tag` rather than a separate table, because every
-- read of a tag application wants them and a second table would need a join on
-- the hottest path in the UI. `NULL` confidence means "not a machine's opinion"
-- -- a tag a person applied is not scored, and inventing a 1.0 would be a claim
-- that the tag is certainly true, which is not what anybody knows.

-- `DOUBLE PRECISION`, not `REAL`, and the reason is a portability rule from plan
-- section 0.4 that no earlier migration in this set had tripped: `REAL` is
-- FLOAT4 on Postgres and FLOAT8 on SQLite, so the identical schema produces a
-- column Rust decodes as `f64` on one engine and refuses on the other --
-- `mismatched types; Rust type f64 (as SQL type FLOAT8) is not compatible with
-- SQL type FLOAT4`. Seven significant digits is also not enough to round-trip a
-- model probability, and a confidence that comes back a different number than it
-- went in is a number nobody can show to a user in a consent prompt.
--
-- `tag.importance` gets the same treatment, for the same reason, and it is the
-- one that actually bit: `load_tag` decodes it as `f64`, `0001` declared it
-- `REAL`, and so every tag read on Postgres failed with `mismatched types; Rust
-- type f64 (as SQL type FLOAT8) is not compatible with SQL type FLOAT4`. It is
-- widened here because this migration is the one that makes tags readable
-- through a typed API, and a type that cannot be decoded is not a type the
-- ticket can claim to have delivered.
--
-- `0001` writes `REAL` for `field_proposal.confidence` and `vote.weight` too,
-- and those are left alone: nothing in this ticket reads them, they are applied,
-- and widening a column nobody is reading is how a migration grows a blast
-- radius. They are noted rather than silently skipped — the next ticket that
-- reads one will hit the same error, and the comment is where it should look.
ALTER TABLE object_tag ADD COLUMN confidence DOUBLE PRECISION;
--:sqlite ../sqlite-forms/0016_tag_importance_width.sql
ALTER TABLE tag ALTER COLUMN importance TYPE DOUBLE PRECISION;
--:end
ALTER TABLE object_tag ADD COLUMN source     TEXT;
ALTER TABLE object_tag ADD COLUMN created_at TEXT;

-- The tagger's view: "show me everything the model proposed, newest first"
-- (stash #2305's dedicated tagger list). It leads with the source so the
-- filter is an index range rather than a scan.
CREATE INDEX object_tag_source_idx ON object_tag (source, created_at);
