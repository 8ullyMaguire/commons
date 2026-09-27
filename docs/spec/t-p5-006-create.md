# T-P5-006 item 10 — Creating objects (`create-from-subpage`, `create-all-missing`)

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 10
**Spec:** §10.10 Bulk editing, C86 (the two create actions); §5.14 (no fake
fidelity); §14.1 (consent enforced in the data layer); §7.1 (CSV import)
**Preceded by:** item 9 (`docs/spec/t-p5-006-vertical-feed.md`).
**Module:** `crates/commons-store/src/create.rs`;
**Tests:** `crates/commons-store/tests/create_db.rs`;
**Mutations:** `scripts/mutate-create.sh`, `scripts/mutations/create_*.py`

---

## 1. Why this is its own module

Item 3 built the bulk *write* surface. Two of §10.10's actions were left out,
and they are the only two with **no object to operate on**:

| action | issue | has a `Target`? |
|---|---|---|
| apply / remove tag | #5336 | yes |
| set / clear field | #5336 | yes |
| hide / unhide | #5336 | yes |
| **create-from-subpage** | #3694 | **no** |
| **create-all-missing** | #1017, #3122 | **no** |

Every write in this crate takes a set of existing ids. A create takes a *shape*
and mints ids, which means it must answer three questions the other writes never
had to ask. That is the whole reason for a separate module rather than two more
methods on `Store`.

## 2. The id is derived, not generated

`derive_id` hashes the draft's `identity()` — `kind`, `title`, `date`,
`producer_id`, `description` — with BLAKE3 through `commons_core::hash_bytes`,
prefixed `o-`.

Three properties, each one a place a plausible implementation goes wrong:

**It is deterministic.** A create that minted a UUID would make
`create-all-missing` impossible: re-running it would create a second copy of
everything, which is the exact bug the action exists to prevent. It also means
two devices creating "the same" clip converge rather than duplicate.

**It covers only what the object *is*.** `organized` is deliberately excluded,
and so are `rating_sum` / `rating_count`. These change over the object's life;
an id that covered them would rename the object on every review, and every tag,
relation, folder membership and undo entry pointing at it would dangle. The
comment on `identity()` says this so a future edit adding `organized` is a
deliberate act with its consequence visible.

**Field boundaries are unambiguous.** The five fields are joined with NUL. The
alternative — a comma — is not a stylistic difference; see §5.

## 3. A new object is visible to its creator, and to nobody else

This is the load-bearing part, and it is easy to get wrong in a way no test in
`bulk.rs` would catch.

`filter_ast::consent_clause` is a join to `consent_record` with a predicate on
`c.tier`. An object with **no** `consent_record` row matches nothing in every
tier list. So a create that writes only the `object` row produces an object that
is on disk, indexed, and **unfindable** — including by the person who just
created it. §14.1 is a constraint on who may *see* an item, and an item nobody
can see is not stored, it is lost.

So `insert_if_absent` writes an `unverified` consent row whenever it writes an
object row, and `unverified` is not `self_published`. Creating an object is not
an attestation about it, and a create that published itself would let a paste of
a CSV of scraped titles publish itself. `unverified` is visible to the owner
(`ConsentTiers::OWNER`) and to nobody else, which is exactly the state a fresh
scan is in.

## 4. `created` and `existing` are different numbers

`CreateOutcome` carries three counts. They cannot be one: a single "written: N"
cannot distinguish "created 5" from "created 5 and found 35 already there", and
the second is the case a user needs to be told about. `create_all_missing`
exists *because* "already there" is a common outcome rather than an error.

`refused` is always 0 in a local single-user build. It exists so a future shared
deployment does not have to change this struct's meaning under its callers.

`create_all_missing_ids` is the import path's one call: create, and report what
the library now holds, with the ids computed once. Duplicates in the input
collapse — `drafts.len() - ids.len()` is the redundant-row count, which is the
number worth telling a user about.

## 5. The separator, and a test that could not tell

The first version of the boundary test asserted that `title="ab", date="c"` and
`title="a", date="bc"` must derive different ids. They do — and the assertion
could not distinguish a correct implementation from a comma-joined one, because
`"scene,ab,c,,"` and `"scene,a,bc,,"` are different strings. The mutation
**survived**.

The real collision needs a separator character *inside* a field:
`title="a", date="b,c"` and `title="a,b", date="c"` both join to
`scene,a,b,c,,`. Two different objects, one id, and the second create is a
silent no-op that nothing can detect afterwards. The test now uses that pair, in
three positions, and the mutation is killed.

The general rule: a test that names a boundary must be built from the
boundary's actual arithmetic, not from a pair that merely *looks* adjacent.

## 6. `ON CONFLICT (id) DO NOTHING`, not `OR IGNORE`

`OR IGNORE` also swallows a NOT NULL or CHECK violation and reports success. A
create whose `kind` is empty would then return `true` for a row that was never
written, and `CreateOutcome::created` would be a lie. `ON CONFLICT (id)` names
the one constraint it is willing to ignore.

## 7. `organized` is resolved in Rust, not in SQL

The first implementation bound `organized` as `COALESCE(?, organized)`, to let
`None` take the schema's default. That does not work: inside an INSERT's VALUES
list there is no `organized` row in scope, so **SQLite fails the statement** with
`no such column: organized`, while Postgres treats it as a column reference and
never substitutes anything. Two engines, two behaviours, and the SQLite one only
appears on a test run.

The default now lives in `DEFAULT_ORGANIZED` in Rust. That duplicates
`0001_core.sql`'s `DEFAULT 'unreviewed'` — deliberately, but *watched*:
`the_default_organized_matches_the_schema` reads the default out of the live
schema on both engines (`pragma_table_info` for SQLite, `information_schema` for
Postgres) and asserts it equals the constant. A change to the migration alone
would otherwise silently create every new object as `organized`, a tier nothing
looks at, so nothing would fail.

## 8. Test results

`crates/commons-store/tests/create_db.rs` — 20 tests, each run against **both**
engines via the `on_each_store!` macro, on a fresh `open_memory()` SQLite and a
scoped Postgres schema.

| claim | test |
|---|---|
| the id is deterministic | `the_id_is_derived_so_the_same_fields_always_give_the_same_id` |
| the id is prefixed | `the_id_is_prefixed_so_it_is_distinguishable_from_a_bare_hash` |
| different fields differ | `different_fields_give_different_ids` |
| **boundaries are unambiguous** | `field_boundaries_are_unambiguous` |
| **the id ignores what changes** | `the_id_ignores_fields_that_change_after_creation` |
| NULL ≠ empty string | `a_missing_field_and_an_empty_one_are_different_objects` |
| dedupe counts collisions | `dedupe_counts_how_many_drafts_share_an_id` |
| dedupe of nothing | `dedupe_of_nothing_is_empty_and_zero` |
| fields land | `a_created_object_exists_with_its_fields` |
| create is idempotent | `creating_the_same_draft_twice_creates_one_object` |
| **creator can see it** | `a_created_object_is_visible_to_its_creator` |
| a visitor cannot | `a_visitor_cannot_see_a_freshly_created_object` |
| counts are separate | `create_all_missing_reports_created_and_existing_separately` |
| idempotent as an action | `create_all_missing_twice_creates_nothing_the_second_time` |
| nothing in, nothing out | `create_all_missing_handles_nothing` |
| ids once, sorted | `create_all_missing_ids_returns_each_id_once` |
| `None` takes the default | `an_unset_organized_takes_the_schema_default` |
| `Some` is a claim | `an_explicit_organized_is_stated_not_defaulted` |
| a steward is not an attestor | `a_steward_may_still_create_and_the_object_is_still_unverified` |
| **constant vs migration** | `the_default_organized_matches_the_schema` |

**Every fixture seeds its own rows.** `create_all_missing` asks a *global*
question — "does this id exist?" — about the whole `object` table, and the
Postgres side of the harness is one database shared by every test in the file.
A fixed id would be an ambient row any other test could create, and the counts
would be another test's to decide.

## 9. Mutations

`scripts/mutate-create.sh` — 5 mutations, **5 killed**.

| mutation | claim | result |
|---|---|---|
| `create_separator_is_comma.py` | field boundaries are unambiguous | killed |
| `create_id_covers_organized.py` | the id ignores fields that change | killed |
| `create_skips_consent.py` | a created object is visible to its creator | killed |
| `create_publishes_itself.py` | a create does not publish itself | killed |
| `create_lies_about_already_there.py` | created and existing counted separately | killed |

`create_separator_is_comma.py` survived the first pass, for the reason in §5.

## 10. A repo-wide invariant caught the new tests

`consent_filter.rs::no_object_query_bypasses_the_consent_clause` scans **every**
test file for a read of `object` without a tier predicate, and it failed on
`create_db.rs` — correctly. The test enforces §14.1 ("rules, all enforced in the
data layer") on the tests themselves, on the reasoning that a clause nobody is
obliged to call is a convention rather than a guarantee.

The reads now join `consent_record` and state the tiers. This is the right fix
rather than an allowlist entry: these tests assert the consent tier of the
object they made, so the read *should* be under a stated tier.

## 11. What is deliberately not here

- **No file handling.** `create` makes an `object` row; the scanner's
  `new_file` path attaches bytes and a `file` row. Coupling them would make a
  create depend on a path existing, and §5.18 is explicit that the operator's
  disk layout is not an API.
- **No HTTP route for create.** The bulk mutations go through GraphQL and so do
  these, but the server-side resolver is not built: the UI half below is proved
  against a mocked endpoint, and a real resolver is the remaining work. Stated
  plainly because "the client can call it" is not "the server answers it".
- **No CSV parsing.** §7.1's importer is a separate ticket; `dedupe` and
  `redundant_count` are the shape it will need, and nothing here parses a
  delimiter or guesses a column order.
- **No undo entry.** `Store::undo` records bulk writes; a create is not
  reversible in the same way, and §10.9's undo scope does not cover creation.
  Adding one would mean deciding what undoing a create does to the consent row,
  which is a spec question this item does not answer.

---

## 12. The UI half

`ui/src/lib/api/create.ts` — pure, no DOM, and the only place the three counts
become a sentence. `ui/src/lib/components/CreateFromSubpage.svelte` — layout,
focus, keyboard. `ui/src/routes/create-from-subpage/+page.svelte` — the route,
which owns the request. `ui/e2e/create-from-subpage.spec.ts` — 7 browser tests.

### Why a route and not two rows in the bulk modal

The bulk modal is *selection*-driven: its scope line is a promise about exactly
which ids a write will touch. The two create actions have no selection by
construction — one starts from a single item's subpages, the other from a list
that is not in the library yet. Forcing them in would mean inventing a fake
selection or teaching the modal a second notion of scope, and the modal exists
because its scope line is trustworthy.

The route follows the same rule the feed follows: the rows are in the URL, as
repeated `?rows=` rather than one joined string. A title containing a `&` or a
`,` is a title, and a separator a user can type into their own data is a
separator that eventually splits a row in the wrong place. There is a test
that pastes `'Two Words'`, `'A & B'` and `'quote " and \\ backslash'` and
asserts they arrive intact.

### The button names what will be created

`Create 5`, not `Create 40`, when 35 of 40 are already in the library. The count
is the one the click causes, and a button reading the pasted number is a small
overstatement that teaches users to stop believing a UI's numbers.

### A hardcoded state literal, caught only by the browser

The route built its result as `{ state: 'done', outcome }` — a literal, rather
than `classify(outcome)`. The unit tests passed, because they exercise the
module and the module is correct. A run with `refused > 0` therefore rendered
`data-state="done"` and no warning styling, which is precisely what
`classify` exists to prevent.

This is the e2e suite earning its place: the bug is at a *call site*, and no
test of the module can see a caller that ignores it. The test now asserts
`data-state="partial"` on a refused row.

### UI results

`ui/tests/create.test.ts` — 29 tests. 7 mutations in
`scripts/mutate-create-ui.mjs`, **7 killed**: dropping the already-there count,
rendering an all-duplicates run as a success count, collapsing `blocked` into
`done`, dropping the refused count from a partial, leaving the button live while
running, naming the pasted count on the button, and rendering a negative
duplicate count from a stale distinct count.
