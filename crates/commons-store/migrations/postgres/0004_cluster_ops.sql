-- §7.2's operations leave records, and one of them needs a table of its own.
--
-- # Why a merge record rather than an audit log
--
-- Re-pointing every appearance from one cluster to another is not reversible
-- from the data that survives it: once the loser is gone, nothing says which
-- appearances used to be in it. An operator who merges the wrong two clusters
-- has no way back, and the aggregate corpus has no way to tell a merge from a
-- cluster that simply never existed. So the record names both sides and the
-- actor.
--
-- The record is written for the *human* case and is not an audit trail: the
-- federation's own events are §14's business. Keeping it here is a deliberate
-- boundary, so the local operator's mistakes are recoverable without coupling
-- §7.2 to the event log.
--
-- `winner`/`loser` rather than a from/to pair, because the operation is not
-- symmetric in effect: the winner survives with its identity, and the loser is
-- retired. Reading a merge backwards from "to_id" would have to know which side
-- that was, and the record is more useful if it says so.

CREATE TABLE IF NOT EXISTS cluster_merge (
    id              TEXT PRIMARY KEY,
    winner_id       TEXT NOT NULL,
    loser_id        TEXT NOT NULL,
    moved_appearances BIGINT NOT NULL DEFAULT 0,
    actor           TEXT,
    created_at      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS cluster_merge_loser ON cluster_merge (loser_id);

-- A split is recorded for the same reason and by the same argument, with one
-- difference that matters: the appearances that moved are recoverable from
-- `appearance.cluster_id` alone, because the loser survives. What is *not*
-- recoverable is that they were ever in the same cluster, and that the
-- original's centroid was recomputed as a result. A user who reviews the split
-- months later is looking at a normal-looking pair of clusters.
CREATE TABLE IF NOT EXISTS cluster_split (
    id              TEXT PRIMARY KEY,
    original_id     TEXT NOT NULL,
    new_id          TEXT NOT NULL,
    moved_appearances BIGINT NOT NULL DEFAULT 0,
    actor           TEXT,
    created_at      TEXT NOT NULL
);

-- §7.2's `same_as` / `not_same_as` assertions.
--
-- An assertion that automation can overrule silently is not an assertion, so
-- the engine reads this table when ranking candidates and the merge operation
-- refuses a blocked pair unless an operator overrides it explicitly. That is
-- the reason this is a table and not a note on the cluster: a note would have
-- to be interpreted by each reader, and the one reader that matters -- the
-- merge path -- is exactly the one that would forget.
--
-- Both directions are stored, not one plus a flag, so "is this pair blocked"
-- is one indexed lookup instead of a scan with a lexicographic comparison. The
-- pair is written twice, once per direction, on insert; `blocked_pair` checks
-- both and the cost of the duplication is one row.
--
-- `note` is nullable and is not a substitute for the assertion. §7.2 asks for
-- free-text disambiguation *and* an assertion, and they are different things: a
-- note explains, an assertion prevents.
CREATE TABLE IF NOT EXISTS cluster_assertion (
    id          TEXT PRIMARY KEY,
    left_id     TEXT NOT NULL,
    right_id    TEXT NOT NULL,
    same        INTEGER NOT NULL,
    note        TEXT,
    actor       TEXT,
    created_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS cluster_assertion_pair ON cluster_assertion (left_id, right_id);

-- §7.2 asks for alias scoping by studio/era (#422, stash-box#818) and a
-- selectable primary name (#610).
--
-- `producer_id` already existed and carries the studio case, so the only
-- missing piece is era. A name that is right for one era and wrong for another
-- -- a performer who changed stage name, an early name retired with the act --
-- collides in exactly the way `UNIQUE (performer_id, name, producer_id)`
-- cannot express, because the same (performer, name) is legitimately two
-- different things.
--
-- `era` is a free-text field rather than a foreign key to a table that does not
-- exist yet. The spec names the concept, not the shape of the thing, and a
-- nullable TEXT column that can be indexed when a real era table arrives is
-- honest about that; inventing a table here would be guessing at a schema the
-- rest of the spec has not settled.
--
-- `is_primary` already existed and defaults to 0, which is what makes "exactly
-- one primary" an invariant the code has to establish rather than a default it
-- inherits. `set_primary` clears the others in the same transaction; a
-- half-applied demotion would leave two primaries and no way to tell which the
-- UI was showing.
ALTER TABLE performer_alias ADD COLUMN era TEXT;
