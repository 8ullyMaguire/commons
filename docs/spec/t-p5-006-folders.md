# T-P5-006 item 6 — Folders (smart collections)

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 6
**Spec:** §5.14 (smart collections, folder-like structure without folders),
§9.5 (nested folders, breadcrumbs, #1029, #1723)
**Preceded by:** item 5 (`docs/spec/t-p5-006-commands.md`).
**Module:** `crates/commons-store/src/folders.rs`; migration `0018_folders.sql`.

---

## 1. The design

A saved query with a name, a parent, and an order.

### 1.1 What a folder is, and what it is not

§5.14 is unusually firm about this: a smart collection is "the answer to
#1029 (folder-like structure) *without pretending folders are the right
model*". A folder here is a name, a filter, and a position in a tree. It holds
no objects of its own — membership is recomputed every time the folder is
opened, and two folders can match the same object.

The consequence that matters: **there is no object→folder column.** An object
is in a folder because it matches the folder's filter and nothing else. That
is what makes "re-evaluated on every open" true rather than aspirational, and
it is why an object tagged after a folder was created shows up in it with no
write of any kind.

The alternative — a `folder_members` join table, maintained on every write
path in the app — would be a second thing to keep correct forever, and it
would be wrong the moment a filter is edited: a join table records the
membership the filter *had*, not the one it has.

### 1.2 Why it extends the filter AST and does not replace it

`Filter::Saved { id }` already exists in `crates/commons-store/src/filter_ast.rs`
and compiles to `o.saved_filter_ids LIKE ?`. That column is not in any
migration. The query is not merely unimplemented; it is a runtime error, and
nothing caught it because the only test touching `Filter::Saved` asserts its
*serde shape* and never runs the SQL.

Two options, and the choice is forced by something outside this file:

  (a) Add the denormalized `o.saved_filter_ids` column. Fast, and it makes the
      existing SQL work. But it is a cache of the filter's answer stored on the
      row, which is precisely the join-table problem above, and every write
      path that changes an object has to invalidate it. A filter edit would
      leave stale membership until something happened to touch every member.

  (b) Expand the reference in SQL, as a subquery, and drop the column.
      Membership is always current by construction — there is no stored answer
      to go stale. The cost is a subquery per reference, and a self-reference
      needs a depth limit.

(b) is what this does. The correctness argument is not "faster" or "slower";
it is that a thing which is defined as "whatever matches this filter" cannot
be implemented by storing an answer and hoping it stays right.

### 1.3 The cycle question

A folder may contain folders (§9.5's tree, breadcrumbs in #1723). A filter
that references a folder which references back is a cycle, and a recursive
CTE over one runs until the database gives up. So expansion is depth-limited:
a reference beyond `MAX_FOLDER_DEPTH` resolves to "matches nothing" and says
so, rather than becoming a query the server cannot finish.

A cycle is reachable by editing a filter to reference an ancestor, so this is
a user action, not a corruption case. It gets a defined answer.

### 1.4 The URL

A folder is referenced by id, not by its filter, so a folder's URL is stable
when its contents change — which is the property that makes it linkable. The
filter is not in the URL because it is in the folder, and putting it there
would mean a shared link stops describing what the user shared once the
folder is edited.

## 2. What the implementation changed, and why

The design above was written before the code. Four things did not survive
contact with it, and the reasons are the useful part of this file.

### 2.1 Expansion is a rewrite of the AST, not a string of SQL

The draft had `expansion_sql` returning an `EXISTS (SELECT ... )` fragment
with an `{inner}` slot. That cannot work: the compiler in `filter_ast.rs` is a
pure function with no store access, so it cannot look up a folder, and
inlining SQL fragments into an AST that is meant to be a *value* (serialisable,
comparable, round-trippable through a URL) turns the type into a template
engine.

What ships instead is `folders::resolve_saved`, which takes a `Filter` and
returns a `Filter` with every `Saved` replaced by that folder's stored filter.
The compiler is untouched, and §5.14's "re-evaluated on every open" is
literally true: the resolution happens per query, against the current
definitions.

### 2.2 A cut cycle must be `Or([])`, not `And([])`

The empty-`And` answer compiles to `1 = 1`, because an empty conjunction is
vacuously true. So the first version of the depth guard returned the most
dangerous value available: a folder that referenced itself resolved to
"matches everything". `folders.rs`'s own test caught it, which is the argument
for writing the test to state the *answer* rather than to check that
something happened.

`Or([])` compiles to `1 = 0`. Empty result, and the error path is not
distinguishable from a real empty folder — which is the intended behaviour: a
folder inside itself is a folder with no members, not an error page.

### 2.3 The depth limit is not a substitute for cycle detection

`MAX_FOLDER_DEPTH` bounds the walk. A `seen` set ends it early and reports
which reference was cut. Both exist because they answer different questions:
the limit guarantees termination against a cycle the code cannot see, and the
`seen` set gives a good answer for a cycle it can.

`ResolutionReport` counts `expanded` and `cut` separately rather than one
number, because "how many references were in this filter" is not a number
anyone asks for. The first version had a single `count`, which meant that
reading `2` for a direct self-reference required knowing whether the cut
reference had been counted, and nobody did.

### 2.4 The compiler refuses an unresolved `Saved`, and that is a fix

`Filter::Saved` used to compile to `o.saved_filter_ids LIKE ?` — a column no
migration creates, so any filter naming a folder failed at the database with
"no such column". It now returns `FilterError::UnresolvedSavedReference`,
which names the missing step (`resolve_saved`) rather than a column that does
not exist. A caller who skipped resolution now gets a message that points at
the bug instead of one that points at the schema.

The existing test asserted the old behaviour — that a filter containing
`Saved` compiles to SQL with the id bound — and it passed, because it only
checked the string and never ran it. It now asserts the new contract, and a
second test asserts that resolution produces SQL with no trace of the folder.

## 3. What ships

`crates/commons-store/src/folders.rs`:

    pub const MAX_FOLDER_DEPTH: u8 = 8;

    pub trait FolderSource {
        fn filter_of(&self, id: &str) -> Result<Option<Filter>, FolderError>;
    }

    pub fn resolve_saved(
        filter: &Filter,
        source: &impl FolderSource,
        path: &mut HashSet<String>,
    ) -> Result<(Filter, ResolutionReport), FolderError>;

`ResolutionReport { expanded: u32, cut: u32 }`. `FolderError` is `NotFound`,
`Cycle`, `TooDeep`, or a wrapped `FilterError` — deliberately not merged with
`FilterError`, so a missing folder is never reported as a parse error.

## 4. The migration

`0018_folders.sql` holds definitions, not membership. `filter` is the AST's JSON
encoding, not the URL's base64url form: a folder's filter is long-lived and
user-editable, and the URL form is a transport encoding for transient state.

Names are unique among siblings and the roots get their own partial index —
`UNIQUE (parent_id, name)` does not cover them, because a unique index treats
NULL as distinct and roots are exactly the rows with `parent_id IS NULL`. The
workaround of a sentinel `parent_id` was rejected: `parent_id IS NULL` is the
root predicate, and a sentinel makes it match nothing.

The cycle guard is a trigger on both engines, as a `sqlite-forms` sidecar, since
`BEFORE INSERT OR UPDATE OF parent_id` and the plpgsql body are not portable.
The walk **descends from `NEW.id`** and asks whether `NEW.parent_id` is among its
descendants. Seeding a walk at the proposed parent and climbing cannot detect a
cycle, because the proposed parent is below the moved row, not above it — moving
`p` under its own child `c` seeds the climb at `c`, which reaches `p` and stops.
The write is allowed, and every later walk up the tree is an infinite loop. This
was reproduced against a live database before it was fixed: the reparent
succeeded and a recursive query over the result hung until it was killed.

`folders::ancestry`'s `seen` set carries the same rule in Rust, so a cycle that
arrives by a route that never touched a trigger — a restore, a fixture, a future
import — is a short breadcrumb rather than a hung query.
