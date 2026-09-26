-- Migration: 0009 file_signal
--
-- T-P4-002 has nine proposers, and seven of them read a signal that no table
-- holds. The candidates exist as arguments in the caller's hand — the scanner
-- knows the phash it just computed and throws it away, a sidecar reader knows
-- the caption it just parsed — and §8.2 wants them to be *retained*, because
-- a proposal whose justification is "the same perceptual hash as item X" is
-- only checkable if the hash is still there to check against.
--
-- So the signals get a home. This is deliberately not a wide table of nullable
-- columns per format: the nine proposers read from genuinely different places
-- (a filesystem path, a container's tag atoms, a hash index, an ASR output, a
-- sidecar file, two model outputs, a peer's response, an upstream scrape), and
-- a table with a column per source is a table whose every row is 90% NULL and
-- whose column set changes every time a format is added.
--
-- `file_signal` is key/value per file, with the value kept as text because the
-- consumers differ: a phash is 16 raw bytes, an ASR transcript is kilobytes of
-- prose, and a container tag is a vocab term. What they share is that they were
-- *extracted*, by *something*, on a *date* — and that is the row.

CREATE TABLE file_signal (
  id          TEXT PRIMARY KEY,
  file_id     TEXT NOT NULL REFERENCES file(id) ON DELETE CASCADE,
  -- Which signal this is. Not a foreign key to a table of names: the set of
  -- signals is a property of the extractors, which are plugins, and a plugin
  -- that ships a new signal must not need a migration to record it.
  kind        TEXT NOT NULL,
  value       TEXT NOT NULL,
  -- Where the signal came from, when that is not the extractor: a container
  -- atom's namespace, the sidecar file's path, the model that produced a
  -- caption. Nullable because most signals have exactly one origin.
  origin      TEXT,
  -- How sure the extractor is, when it says. NULL is not "unsure", it is
  -- "this kind of signal has no confidence" — a phash is a hash, not a guess.
  confidence  REAL,
  extracted_at TEXT NOT NULL
);
-- One signal per (file, kind, origin, value). A rescan re-extracts everything,
-- and without this the second run doubles every row.
--
-- The *value* is in the key, and that is a correction rather than a detail. The
-- first version keyed on (file, kind, origin), which looks right until you
-- notice that a tagger produces many `ml:tag` signals for one file from one
-- model: `pose:doggy`, `scenario:outdoors`, `outfit:swimsuit` are three
-- proposals, and a key without the value lets only the first one be stored. The
-- bug was found by `ml_tagger_proposes_confident_tags_in_its_namespace`, which
-- asserts two tags from one model and could not insert the second.
--
-- `COALESCE` on the nullable origin so two signals of the same kind from
-- different origins -- two containers' title atoms, say -- are distinct rows
-- rather than one silently overwriting the other.
--
-- An index and not a table-level `UNIQUE (...)`, for the reason 0008 documents:
-- the two are identical in Postgres and only the index form is legal in SQLite,
-- so a table constraint here would pass every Postgres test and fail the first
-- SQLite one.
CREATE UNIQUE INDEX file_signal_uniq
  ON file_signal (file_id, kind, COALESCE(origin, ''), value);

-- The phash index, separately, because it is the one signal that is *queried*
-- rather than read: `phash_match` is a lookup, not a scan, and a lookup over a
-- key/value table is a full table scan wearing a hat.
--
-- Split out so the index is on the column and the distance is a query-time
-- comparison. 64 bits is the standard choice: 32 collides visibly in a library
-- of any size, and 256 is wasted storage for a signal that only ever reports a
-- boolean "same or not".
CREATE TABLE file_phash (
  file_id  TEXT PRIMARY KEY REFERENCES file(id) ON DELETE CASCADE,
  phash    TEXT NOT NULL,
  -- The algorithm, because pHash has three incompatible definitions in common
  -- use and two files with the same 64 bits from different algorithms are not
  -- a match. Stored so a re-hash with a different implementation cannot be
  -- mistaken for a change of content.
  algorithm TEXT NOT NULL DEFAULT 'phash64',
  created_at TEXT NOT NULL
);

-- `phash_match` finds "an item with the same phash as a *described* item", so
-- the query is over described items. `object.organized` is the closest thing to
-- "described" the schema has, and indexing on it directly rather than
-- interpreting `organized` at query time keeps the two from drifting.
CREATE INDEX file_phash_lookup_idx ON file_phash (phash);

-- A described object's settled phash, so the proposer is a join rather than a
-- scan. This is a cache of "the phash of an object with a title", derived, and
-- like every other derived thing here it is written by the code that owns it —
-- the phash proposer — and by nothing else.
CREATE TABLE object_phash (
  object_id  TEXT PRIMARY KEY REFERENCES object(id) ON DELETE CASCADE,
  phash      TEXT NOT NULL,
  algorithm  TEXT NOT NULL DEFAULT 'phash64',
  created_at TEXT NOT NULL
);
CREATE INDEX object_phash_lookup_idx ON object_phash (phash);

-- `peer:<id>` is a first-class proposer in §8.2, and it needs two things the
-- `peer` table from 0001 has neither of.
--
-- `peer.name`: the justification for a peer's proposal has to say *which* peer,
-- because "proposed by a peer" is not something a user can act on. The id is a
-- UUID, which a UI cannot show. Nullable, because a peer added before it
-- introduced a name has none, and a missing name is better than a generated one
-- that looks like something the operator chose.
--
-- `peer_value`: a federated index's settled value for one of our objects. This
-- is the *input* to the `peer:<id>` proposer and the reason it exists: §13's
-- promise is that a settled value travels, and without a table to land in it
-- travels nowhere. Keyed on (peer, subject, field) so a resync of the same peer
-- is an update rather than a duplicate pile.
ALTER TABLE peer ADD COLUMN name TEXT;

CREATE TABLE peer_value (
  id          TEXT PRIMARY KEY,
  peer_id     TEXT NOT NULL REFERENCES peer(id) ON DELETE CASCADE,
  subject_id  TEXT NOT NULL,
  field       TEXT NOT NULL,
  -- The value as JSON, because a peer's `tags` is an array and its `title` is a
  -- string, and storing both as text would mean every reader parses.
  value_json  TEXT NOT NULL,
  -- How much the peer's own community trusts this, which is a different number
  -- from how much *we* do. Kept apart on purpose: collapsing them would let a
  -- peer's confidence become our confidence, and §13's whole design is that a
  -- settled value is evidence rather than an answer.
  confidence  REAL,
  -- The peer's own resolve output, if it has one. Nullable: a peer's
  -- contribution may be a value it has not itself settled.
  margin      REAL,
  created_at  TEXT NOT NULL,
  UNIQUE (peer_id, subject_id, field)
);
CREATE INDEX peer_value_subject_idx ON peer_value (subject_id, field);
