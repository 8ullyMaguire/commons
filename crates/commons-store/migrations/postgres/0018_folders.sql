-- Migration: 0018 folders (saved queries)
--
-- §5.14 and §9.5, and the one thing `filter_ast`'s `Filter::Saved` needed and
-- did not have.
--
-- `Filter::Saved { id }` existed in the AST and compiled to
-- `o.saved_filter_ids LIKE ?` -- a column that no migration creates. Any filter
-- containing a saved reference failed at the database with "no such column",
-- and nothing caught it because the only test touching that variant asserted
-- its serde shape and never ran the SQL.
--
-- The fix is not this table. The fix is `folders::resolve_saved`, which
-- expands the reference before compilation, so no denormalized membership
-- column is needed anywhere: an object is in a folder because it matches that
-- folder's filter and for no other reason, which is what makes §5.14's
-- "re-evaluated on every open" true rather than aspirational.
--
-- So this table holds *definitions*, not membership. That is the whole design
-- and it is worth stating here, at the schema, because the obvious next change
-- someone will want to make is a `folder_members` join table -- and that would
-- reintroduce exactly the staleness this avoids.
--
-- The filter is stored as the AST's own JSON encoding, not as the base64url
-- string a URL carries. A folder's filter is not a URL fragment: it is a
-- long-lived thing users will edit, diff and review, and the JSON form is
-- readable and versionable. The URL form is a transport encoding for a
-- transient state, and storing the transport encoding would mean every folder
-- becomes opaque the moment it is saved.

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
--:sqlite ../sqlite-forms/0018_folders_triggers.sql
CREATE OR REPLACE FUNCTION saved_filters_no_cycle() RETURNS TRIGGER AS $$
DECLARE
    -- `depth` is the deepest descendant reached from NEW.id; `reached` is how
    -- many of those rows are the proposed parent. Two variables because they
    -- answer two different questions, and the first version tried to answer both
    -- with `depth` alone and could answer neither.
    depth   INTEGER;
    reached INTEGER;
BEGIN
    -- A root has no parent and therefore no cycle. First, so nothing below has
    -- to reason about NULL.
    IF NEW.parent_id IS NULL THEN
        RETURN NEW;
    END IF;
    -- Cheapest case, and the one a user hits by accident.
    IF NEW.parent_id = NEW.id THEN
        RAISE EXCEPTION 'folder % cannot be its own parent', NEW.id;
    END IF;
    -- Walk *down* from NEW.id and ask whether the proposed parent is among
    -- NEW.id's descendants.
    --
    -- The direction is the whole thing, and the first version had it backwards.
    -- Seeding a walk at `NEW.parent_id` and climbing to the root cannot detect a
    -- cycle, because the proposed parent is *below* the moved row, not above it:
    -- moving `p` under its own child `c` seeds the climb at `c`, whose chain runs
    -- to `p` and stops, so every check passes and the write is allowed. The tree
    -- then holds a cycle and every later climb is an infinite loop. Reproduced
    -- against a live database: the reparent succeeded, and a recursive query over
    -- the result hung until it was killed.
    --
    -- Descending from `NEW.id` asks the question that matters and answers it from
    -- the rows the table actually holds: is `NEW.parent_id` a descendant of me?
    -- If so, attaching it makes me my own ancestor.
    --
    -- `UNION ALL`, not `UNION`: the seed is `level = 0` and every later row is a
    -- distinct row, so a dedup pass buys nothing and costs a sort. Termination
    -- comes from `level < 64` instead, which also bounds a tree that arrived in a
    -- cycle by some route that never touched a trigger.
    --
    -- The CTE column is `level`, not `depth`: plpgsql resolves a bare name in
    -- `MAX(depth)` against the variable *and* the column and reports "column
    -- reference depth is ambiguous". Naming them alike made the trigger fail on
    -- every insert that had a parent -- every insert but a root, so the table
    -- looked fine and nesting was simply impossible.
    WITH RECURSIVE descendants(id, level) AS (
        SELECT id, 0 FROM saved_filters WHERE id = NEW.id
      UNION ALL
        SELECT f.id, d.level + 1
        FROM saved_filters f JOIN descendants d ON f.parent_id = d.id
         WHERE d.level < 64
    )
    SELECT MAX(level), COUNT(*) FILTER (WHERE id = NEW.parent_id)
      INTO depth, reached
      FROM descendants;

    -- NEW.id is level 0 and always in the result, so the proposed parent
    -- appearing at any level is the only thing that means "this closes a loop".
    IF reached > 0 THEN
        RAISE EXCEPTION 'folder % cannot be moved under %: % is its own ancestor',
            NEW.id, NEW.parent_id, NEW.parent_id;
    END IF;
    IF depth > 64 THEN
        RAISE EXCEPTION 'folder tree deeper than 64 levels at %', NEW.id;
    END IF;
    -- A parent that is not there. Caught here rather than left to the foreign key
    -- so the message names the relationship the user was editing; a bare FK
    -- violation says "FOREIGN KEY constraint failed", which for a sidebar drag is
    -- not an actionable sentence.
    IF NOT EXISTS (SELECT 1 FROM saved_filters WHERE id = NEW.parent_id) THEN
        RAISE EXCEPTION 'folder % has parent %, which does not exist',
            NEW.id, NEW.parent_id;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER saved_filters_no_cycle_trigger
    BEFORE INSERT OR UPDATE OF parent_id ON saved_filters
    FOR EACH ROW EXECUTE FUNCTION saved_filters_no_cycle();
--:end
