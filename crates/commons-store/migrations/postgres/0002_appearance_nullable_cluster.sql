-- 0002_appearance_nullable_cluster.sql — let an appearance belong to no cluster.
--
-- GENERATED for the SQLite mirror by scripts/sync-migrations.py.
--
-- Why this exists
-- ---------------
-- §7.1 requires a third state for a face embedding: `ambiguous`. A face that
-- fits two people equally well is attached to neither, because "silently
-- guessing is the failure mode that would make people distrust the links".
--
-- Schema 0001 cannot express that. `appearance.cluster_id` is declared
--
--     cluster_id TEXT NOT NULL REFERENCES person_cluster(id) ON DELETE CASCADE
--
-- so every appearance must name a cluster, and a cluster id is not an
-- optional value: an empty string is either a foreign-key violation or, on a
-- connection with constraints off, a dangling reference that every later read
-- has to defend against. A sentinel "ambiguous" cluster is worse: it is a
-- person-shaped row that is not a person, and it inflates every cluster count.
--
-- So the column becomes nullable. That is the only one of the three options
-- that is honest, and it has a cost worth naming: a nullable `cluster_id` is a
-- three-state value, and every read has to handle all three. The partial index
-- below is what keeps that cheap — the non-ambiguous case, which is the vast
-- majority of rows and the only one a query usually wants, stays fully indexed
-- by the foreign key, and `ambiguous = 1` rows are found by a second, small,
-- explicit index.
--
-- The alternative reading — "make cluster_id NOT NULL and always point at
-- something" — is the design that produced the bug this migration fixes, so it
-- is recorded here as rejected rather than left unstated.
--
-- Note: 0001 is not edited. §14.2 is explicit that an applied migration is
-- immutable in content and position, and this is a correction shipped as a new
-- migration for exactly that reason.

--:sqlite ../sqlite-forms/0002_appearance_nullable_cluster.sql
-- SQLite has no `ALTER COLUMN ... DROP NOT NULL`, so its mirror carries the
-- twelve-step table rebuild instead. The statement between the markers is what
-- Postgres runs; scripts/sync-migrations.py substitutes the named file for it
-- when generating the SQLite mirror.
ALTER TABLE appearance ALTER COLUMN cluster_id DROP NOT NULL;
--:end

-- The clustering engine's hot query is "every non-ambiguous member of this
-- cluster". A plain index on cluster_id would also carry every ambiguous row,
-- which is noise for this query and makes the index larger for no benefit.
CREATE INDEX appearance_cluster_idx ON appearance (cluster_id) WHERE ambiguous = 0;

-- The other hot query: "everything the user needs to review". Deliberately
-- partial on `ambiguous = 1`, so it stays small no matter how large the library
-- is, and ordered by creation so the review queue is in the order the doubts
-- arose.
CREATE TABLE pg_only_table (id TEXT PRIMARY KEY);
CREATE INDEX appearance_ambiguous_idx ON appearance (created_at, id) WHERE ambiguous = 1;
