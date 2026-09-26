-- Migration: 0018 folders (saved_filters) - SQLite cycle guard
--
-- Substituted into `migrations/sqlite/0018_folders.sql` by
-- `scripts/sync-migrations.py`, via the `--:sqlite ../sqlite-forms/...` block in
-- the Postgres file. 0017 is the precedent: SQLite has no
-- `ALTER TABLE ... DROP CONSTRAINT`, so the table is rebuilt, and that rebuild
-- is a sidecar rather than generated.
--
-- # Why a sidecar rather than generated
--
-- `sync-migrations.py` translates portable DDL -- table bodies, indexes, column
-- types. It does not translate procedural SQL, and should not: the Postgres form
-- of this trigger is plpgsql with a recursive CTE and a RAISE, and mechanically
-- translating that into SQLite's trigger syntax would look right and be wrong in
-- a way nobody would catch until a user dragged a folder into its own
-- descendant.
--
-- # The two forms are not textually equal, and should not be
--
-- Postgres takes one trigger for both events (`BEFORE INSERT OR UPDATE OF
-- parent_id`). SQLite's `RAISE` body cannot be shared the same way, so this file
-- declares two. Same rule, same walk, same answers -- and what has to agree is
-- the *behaviour*, which `tests/folders_db.rs` checks on both engines. A parity
-- test on trigger *text* would fail forever and tell nobody anything.
--
-- # The walk descends from NEW.id
--
-- The non-obvious part, and the first version of this file got it wrong in the
-- same way the Postgres form did -- twice, in fact. Seeding at `NEW.parent_id`
-- and climbing cannot detect a cycle, because the proposed parent is *below* the
-- moved row: moving `p` under its own child `c` seeds the climb at `c`, whose
-- chain runs to `p` and stops, so every check passes and the cycle is written.
-- Descending from `NEW.id` and asking "is `NEW.parent_id` one of my
-- descendants?" is the same question the write is actually asking.
--
-- A further consequence worth stating: on a BEFORE INSERT the row does not exist
-- yet, so its descendant walk is empty and only the existence check can fail.
-- That is correct rather than a gap -- a folder cannot be inserted beneath its
-- own future child, because the child cannot exist first.
--
-- # How a check is written here
--
-- SQLite evaluates `RAISE` once per row the enclosing SELECT returns, so a
-- SELECT matching nothing raises nothing. That makes the `WHERE` the whole test:
-- the query computes a number and the WHERE says when that number is bad.
--
-- `WHEN NEW.parent_id IS NOT NULL` on the trigger itself rather than a check
-- inside it, so a root insert costs no walk at all.
CREATE TRIGGER saved_filters_no_cycle_trigger
BEFORE INSERT ON saved_filters
WHEN NEW.parent_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'folder cannot be its own parent')
    WHERE NEW.parent_id = NEW.id;

    -- Cycle, then depth, then existence -- cheapest first.
    --
    -- The walk descends from NEW.id and asks whether NEW.parent_id is among the
    -- descendants. `NEW.id` is level 0 and always present, so the count is only
    -- non-zero when the proposed parent is genuinely *below* the moved row.
    --
    -- `UNION ALL` with a `level < 64` bound rather than `UNION`: the seed is
    -- level 0 and every later row is distinct, so a dedup pass buys nothing and
    -- costs a sort. The bound is also what stops a tree that arrived in a cycle
    -- by a route that never touched a trigger.
    SELECT RAISE(ABORT, 'folder cannot be moved under its own descendant')
    WHERE (
        WITH RECURSIVE descendants(id, level) AS (
            SELECT id, 0 FROM saved_filters WHERE id = NEW.id
          UNION ALL
            SELECT f.id, d.level + 1
            FROM saved_filters f JOIN descendants d ON f.parent_id = d.id
             WHERE d.level < 64
        )
        SELECT COUNT(*) FROM descendants WHERE id = NEW.parent_id
    ) > 0;

    SELECT RAISE(ABORT, 'folder tree deeper than 64 levels')
    WHERE (
        WITH RECURSIVE descendants(id, level) AS (
            SELECT id, 0 FROM saved_filters WHERE id = NEW.id
          UNION ALL
            SELECT f.id, d.level + 1
            FROM saved_filters f JOIN descendants d ON f.parent_id = d.id
             WHERE d.level < 64
        )
        SELECT MAX(level) FROM descendants
    ) > 64;

    -- Existence last. A bare FOREIGN KEY failure says "FOREIGN KEY constraint
    -- failed", which for a sidebar drag is not an actionable sentence.
    SELECT RAISE(ABORT, 'folder parent does not exist')
    WHERE NOT EXISTS (SELECT 1 FROM saved_filters WHERE id = NEW.parent_id);
END;

-- Updates get the same three checks. `BEFORE UPDATE OF parent_id` rather than
-- `BEFORE UPDATE`, so a rename does not pay for a tree walk it cannot fail.
CREATE TRIGGER saved_filters_no_cycle_update
BEFORE UPDATE OF parent_id ON saved_filters
WHEN NEW.parent_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'folder cannot be its own parent')
    WHERE NEW.parent_id = NEW.id;

    SELECT RAISE(ABORT, 'folder cannot be moved under its own descendant')
    WHERE (
        WITH RECURSIVE descendants(id, level) AS (
            SELECT id, 0 FROM saved_filters WHERE id = NEW.id
          UNION ALL
            SELECT f.id, d.level + 1
            FROM saved_filters f JOIN descendants d ON f.parent_id = d.id
             WHERE d.level < 64
        )
        SELECT COUNT(*) FROM descendants WHERE id = NEW.parent_id
    ) > 0;

    SELECT RAISE(ABORT, 'folder tree deeper than 64 levels')
    WHERE (
        WITH RECURSIVE descendants(id, level) AS (
            SELECT id, 0 FROM saved_filters WHERE id = NEW.id
          UNION ALL
            SELECT f.id, d.level + 1
            FROM saved_filters f JOIN descendants d ON f.parent_id = d.id
             WHERE d.level < 64
        )
        SELECT MAX(level) FROM descendants
    ) > 64;

    SELECT RAISE(ABORT, 'folder parent does not exist')
    WHERE NOT EXISTS (SELECT 1 FROM saved_filters WHERE id = NEW.parent_id);
END;
