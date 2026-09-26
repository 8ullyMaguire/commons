-- 0018_folders.sql - SQLite mirror (Postgres: 0018_folders.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0018_folders.sql and re-run that script.
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

CREATE TABLE saved_filters (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    -- The AST as JSON. Not the URL's base64url form, and not a query string:
    -- see the header.
    filter      TEXT NOT NULL,
    -- NULL at the root. `ON DELETE CASCADE` rather than RESTRICT: deleting a
    -- folder must take its contents with it, and a folder whose parent is gone
    -- is a tree with a hole in it rather than a root.
    parent_id   TEXT REFERENCES saved_filters(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL DEFAULT 0,
    icon        TEXT,
    -- §5.14's "notify when new items match". Off by default: a subscription
    -- that defaulted on would mean a background job per folder for every user
    -- who ever made one.
    notify      INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- The sibling listing's WHERE clause, as an index. `parent_id IS NULL` for the
-- roots cannot use a plain index on `parent_id` usefully -- half the queries in
-- a fresh install are the roots query and an index on a column that is NULL for
-- all of them stores nothing but the NULLs. So the order columns are indexed
-- together, and the root case is left to the planner: it is a full scan of a
-- table that is small by construction, because a folder tree is a navigation
-- aid and not a content table.
CREATE INDEX saved_filters_sibling_order
    ON saved_filters(parent_id, position, id);

-- A depth guard at the schema, not only in the resolver. The resolver bounds
-- *reference* depth; this bounds *tree* depth, which is a different thing and is
-- reachable by dragging a folder into its own descendant. A `parent_id` cycle
-- is a row that is its own ancestor, and every walk up the tree would then be
-- infinite. SQLite has no recursive CHECK, so the trigger below carries it on
-- SQLite and the resolver's `seen` set carries it everywhere else; this index
-- and the trigger are belt and braces rather than the only defence.
CREATE INDEX saved_filters_parent ON saved_filters(parent_id);

-- Folder names are unique among siblings, not globally. Two people -- or two
-- unrelated parts of one library -- may each have a "Favourites", and forcing
-- global uniqueness would make the second one a rename of the first.
--
-- Two indexes rather than one, because a plain UNIQUE(parent_id, name) does not
-- cover the roots: a unique index treats NULL as distinct, and the roots are
-- precisely the rows with `parent_id IS NULL`. Without the partial index, two
-- folders called "Favourites" can both sit at the top level and the sidebar
-- shows two identical entries with no way to tell them apart.
--
-- The obvious workaround -- a sentinel value in `parent_id` for roots -- is
-- worse than the gap. `folders::parent_predicate_sql` is `parent_id IS NULL`,
-- and a sentinel makes that predicate miss every root, so the roots listing
-- returns nothing rather than the roots. A partial index fixes the uniqueness
-- without touching the predicate.
CREATE UNIQUE INDEX saved_filters_sibling_name
    ON saved_filters(parent_id, name);

-- The root case, where the plain index above is inert.
CREATE UNIQUE INDEX saved_filters_root_name
    ON saved_filters(name)
    WHERE parent_id IS NULL;

--
-- Not expressible as a CHECK on either engine: a CHECK takes a row and returns
-- a boolean, and whether a folder is its own ancestor is a question about
-- *other* rows. So it is a trigger, and because a trigger's body is procedural
-- -- a recursive walk and a conditional raise -- the body is a sidecar rather
-- than something `sync-migrations.py` can translate.
--
-- The rule is enforced in three places, and the three are not redundant:
--
--   1. The trigger here, at write time, with an error naming the folder. This
--      is the one a user meets.
--   2. `folders::ancestry`'s `seen` set, because a cycle can arrive by a route
--      that never touches this trigger: a database restore, a fixture, a future
--      import. It turns a cycle that got in anyway into a short breadcrumb
--      rather than an infinite walk.
--   3. The `depth < 64` bound inside the walk, because `UNION` terminates on a
--      cycle only if the cycle is among the rows it walks.
--
-- `RAISE EXCEPTION` aborts the statement and leaves the transaction usable, so
-- a caller inserting several folders gets an error on the bad one and can
-- correct it without losing the good ones.
-- SQLite form of the statement above (../sqlite-forms/0018_folders_triggers.sql).
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

