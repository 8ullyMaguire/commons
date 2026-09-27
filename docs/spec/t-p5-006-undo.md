# T-P5-006 item 7 — Undo for destructive actions

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006 item 7
**Spec:** §10.7 (undo for destructive actions, #3221, #1052, #647), §10.10
(right-click paste, bulk editing), §8.6 (edit history, revert, blame)
**Preceded by:** item 5 (the command registry that a Cmd-Z binding needs) and
item 3 (the bulk write that is the thing being undone).
**Module:** `crates/commons-store/src/undo.rs`, migration `0019`.

---

## 1. What this is

A destructive action is reversible from the same screen, for long enough to
notice the mistake. §10.7 pairs undo with confirmation rather than instead of
it: confirm catches the mistake you see coming, undo catches the one you do not.

The whole design follows from one question: **what is the inverse of the write
that just happened?** Every answer that is not "the exact set of rows, with
their prior values" is a guess, and a guess is wrong in the case that matters —
the one where the user's mental model of the prior state was wrong too.

## 2. What exists today, measured

- `crates/commons-index/src/history.rs` — the accepted-edit set. `accept`,
  `retract`, `field_history`, `blame`, `recompute`, `field_score`. Built for
  T-P4-001, and §8.6's rule (scores recomputed, never counted) is already
  implemented.
- `crates/commons-store/src/bulk.rs` — `bulk_apply_tag` returns `BulkOutcome`
  with counts (`applied`, `skipped`, `already_present`, …). **It does not say
  which objects it changed.**
- `ui/src/lib/api/selection.ts` — the selection model.
- `ui/src/lib/api/commands.ts` — the command registry from item 5.

So the gap is not storage and not UI. It is that the bulk write's *result*
discards the information an undo needs, and no amount of querying afterwards
recovers it: by the time the user presses Cmd-Z, the selection may have changed,
a filter may return a different set, and an object edited elsewhere in the
meantime is indistinguishable from one that was not.

## 3. The decision that shapes everything

**The undo record is written by the write.** (Not in the same transaction —
the draft said so and it is false; see section 6.)

The alternative — reconstruct from `object_tag` after the fact — looks cheaper
and is wrong in three ways, each of which is a real bug rather than a nicety:

1. *Which objects?* The undo must restore exactly the objects the write touched.
   Re-running the target selection later can return a different set, because the
   filter's result is not stable: an object may have been added, removed, or
   edited by another action between the write and the undo.
2. *What was there before?* `bulk_apply_tag` is `ON CONFLICT DO UPDATE`, so an
   object that already carried the tag has its `confidence` and `source`
   *replaced*. The inverse is "put back what was there", and what was there is
   not recoverable from the row afterwards. Without the prior state the undo
   would restore the write's own values and call it a restoration.
3. *Is undo still allowed?* A write that was superseded must not be undoable into
   a state nobody was ever in. That is a comparison against a recorded prior
   state, so it needs the record.

The cost is one table and one insert.

## 4. Shape, as shipped

    pub struct Write {              // what the caller hands in
        pub id: String,
        pub caller: String,
        pub action: String,          // "bulk.tag.add" | "bulk.tag.remove"
        pub tag_id: String,          // ON THE RECORD, not a parameter
        pub requested: i32,
        pub matched: i32,
        pub entries: Vec<UndoEntry>, // the inverse, one per object touched
    }

    pub struct UndoRecord {         // what comes back out
        pub id: String,
        pub caller: String,
        pub action: String,
        pub tag_id: String,
        pub requested: i32,
        pub matched: i32,
        pub created_at: String,
        pub expires_at: String,
        pub undone_at: Option<String>,
        pub entries: Vec<UndoEntry>,
    }

    pub struct UndoEntry {
        pub object_id: String,
        pub before: TagState,
        pub after: TagState,
    }

    pub struct TagState {           // NOT the draft's FieldState
        pub row_existed: bool,       // <- the column the draft lacked
        pub confidence: Option<f64>,
        pub source: Option<String>,
        pub created_at: Option<String>,
    }

`FieldState` was the draft's answer and it was too thin. It is a three-state
enum (`Unset`/`Set`/`Null`), which sounds right, and `history.rs` does define
one — but a tag row is four columns, three of them nullable, and the state of
the row is not a state of any one of them. `TagState` keeps the columns, because
the inverse has to put back *which* values were there, and an enum cannot say
"confidence 0.3, source manual".

The `row_existed` field is the one that has no draft equivalent and cannot be
derived. All three value columns are nullable, so three NULLs cannot be told
apart from no row at all — and a row of three NULLs is the state every
`object_tag` row written before migration 0016 is in. Restoring that as
"absent" deletes a row that existed.

`tag_id` sits on the record rather than being a parameter to `undo()` because a
caller that supplies it can supply the wrong one, and the staleness check would
still pass: it reads `after` from the record and compares against the tag the
record names. A record naming a different tag than the one being checked is a
silent corruption that satisfies every assertion in the function.

`requested` and `matched` are `i32`, not `i64`. `INTEGER` is INT4 on Postgres
and INT8 on SQLite, so a decode asking for `i64` succeeds on SQLite and fails
on Postgres. `relations.rs` states the same rule on its row type; the parity
test checks column *names* and cannot see it.

## 5. Acceptance

- `bulk_apply_tag` returns the identity of every object it changed, not only a
  count, and writes an `undo_record` in the same transaction.
- Undo is refused for a record whose `before` no longer matches the object's
  current state, with an error that says which object and which field.
- Undo of an already-undone record is refused rather than applied twice.
- Expiry is enforced by the read path, not by a sweeper, so a stale record is
  invisible whether or not anything has run recently.
- Both engines, and the tests state the *answer* rather than asserting a write
  happened.

## 6. What the implementation found

Four things the design above did not know when it was written, each of which
changed the code.

**The write and its record cannot be one statement, so the ORDER carries the
safety instead.** The obvious fix is a data-modifying CTE — `WITH rec AS (INSERT
…) INSERT … SELECT … FROM rec` — and measured against both engines it is not
available: Postgres runs it, and SQLite refuses it with `near "INSERT": syntax
error`, because an `INSERT` as a CTE body is not SQLite syntax at all. A
transaction is the only spelling that works on both, and `search.rs` already
records why this codebase does not take one: the `Transaction` executor is as
engine-specific as the pool it came from, so supporting both engines means one
copy of the writer per engine, and the two drift.

So [`Store::prepare_undo`] writes the record **before** the write it reverses,
which is the opposite of the obvious order, and the asymmetry is not a
preference:

- Record first, write second: a failure between them leaves a record describing
  a write that never happened. Pressing undo on it restores `before` onto a row
  the write never touched — and the staleness check refuses it, because the row
  is not in the `after` state the record claims. The record is inert and expires
  on its own.
- Write first, record second: a failure leaves a write the user cannot reverse,
  which is the failure a user actually experiences.

This holds only because the staleness check is a statement about the *row* rather
than about the record, so a test builds the residue directly — a record whose
write never ran — and requires the press to be refused. It also means the
staleness check is load-bearing for a second, unrelated reason, and removing it
kills eight tests.

A write that reached nothing records nothing, and says so with
`PreparedUndo::Nothing` rather than an empty entry list: a record with zero
entries is offerable to the user and undoes nothing, which is a button whose
only effect is to disappear.

**An entry has no `after.row_existed`.** It is reconstructed as `true` on read,
because an entry is only written for an object the write *touched* and a write
that touched the row had a row afterwards. "Touched" is every object the write
reached, not a subset: `ON CONFLICT DO UPDATE` replaces a row that already
carried the tag instead of skipping it, so "reached" and "changed" are the same
set here. That was not obvious and the spec first claimed the opposite; a probe
against both engines settled it (`applied = 1`, `0.3 / manual` in,
`0.9 / bulk` out). This is the one place the
reconstruction is not literal, and it is the only place it could be otherwise:
a `false` after would mean the write removed the row, which is an action that
does not exist yet and would need its own entry kind. A test round-trips the
whole record to catch the reconstruction being wrong.

**The UI half needed a rule the server already had.** `bulk.ts` classifies
rather than counts, and `undo.ts` follows it: the count arrives on the `ok`
outcome and is never recomputed, because a client count is a guess and a guess
that disagrees with the server is a count the user finds wrong by pressing the
button. The refusals are the substance — five of them, five different sentences,
and `failed` is the only one with no button.

**Expiry needs no sweeper.** `expires_at` is checked on read by both
`undoable()` and `undo()`, and by nothing else. A sweeper is a second thing to
run, schedule, and notice has stopped. Rows are never deleted: an expired
record is invisible and inert, not gone, because the history of what was done
and undone is itself history.

**The check is two passes, and the second one repeats it.** The first version
wrote as it checked, so a two-object record whose second object had been
superseded left the first object restored — and its doc claimed all-or-nothing
unconditionally. A test asserting a two-object stale record touches *neither*
object caught it. The shipped version reads every object's state first (refusing
before any write, which is the refusal that actually happens), then writes with
the expected state in each statement's own `WHERE` and treats the row count as
the check. The repeat is not redundant: it is the only part that survives a
commit landing between the two passes.

`IS NOT DISTINCT FROM`, not `=`, in that `WHERE`. Every value column is nullable
and `=` never matches NULL, so a row of three NULLs would fail its own equality
test and be refused as superseded — a false refusal on the most common legacy
state. Six of the thirteen tests fail when the comparison is mutated to `=`.

**Still not atomic across objects.** Pass two is a loop of separate statements,
so a failure on the third leaves the first two applied. That needs a
transaction, which this store's error type has no variant for, and it is the
gap section 6 names rather than a property to claim.

## 7. Status

Done. `crates/commons-store/src/undo.rs`, migration `0019_undo.sql` (both
engines), `tests/undo_db.rs` (13 tests, both engines), 7 unit tests,
`ui/src/lib/api/undo.ts` (11 tests), the offer in `BulkEditModal.svelte`
with 3 e2e cases in `ui/e2e/bulk.spec.ts`. 1221 Rust, 237 UI, 56 e2e. Clippy
clean.

**Where the offer renders, and why it is not a toast.** Inside the bulk modal,
beside the result line. A floating toast was written first and it cannot work:
that dialog is `showModal()`, which makes the rest of the page inert, so a toast
raised while the modal is open cannot be clicked at all. Playwright reported it
as `bulk-modal intercepts pointer events` — the browser saying the interaction is
gone, which a user reads as a frozen page. Item 3's own code already states the
principle one line above where the toast would have gone: a toast that outlives
a window the user has closed reports a failure against a dialog they are no
longer in. The component and its three browser tests were deleted and the offer
moved into `BulkEditModal.svelte`.

**The atomicity gap in section 6 is closed.** The restore is one statement per
shape over a `VALUES` block, and a statement is atomic, so a record restores
entirely or not at all — no transaction needed, which is what makes it possible
in a store whose error type has no transaction variant. Two tests pin it: one
where a stale object mid-record leaves every other object untouched, one where a
four-object record mixing both shapes restores all four with each updated object
getting its *own* prior values. Restoring the loop kills both, plus six others.

Three things about that statement were measured rather than assumed, and all
three failed differently:

- `UPDATE … FROM (VALUES …)` is a **syntax error on SQLite** (`near "("`): there
  is no `VALUES` table form in a `FROM` clause. A CTE works on both engines, and
  the `FROM e` is required — SQLite says `no such column: e.b` without it, Postgres
  says `missing FROM-clause entry`.
- The flag columns must be bound as **booleans**. A `VALUES` list takes one type
  per column across all rows, so a bound integer becomes a column the engine may
  type as anything, and `= 1` then fails *quietly*: SQLite returns fewer rows than
  the data contains, and Postgres refuses with `VALUES types bigint and text
  cannot be matched`. Casting does not rescue it — Postgres rejects
  `CAST(? AS BOOLEAN)` on a bigint, and `IS TRUE` on an integer column. Binding
  the right type is the only spelling that survives.
- The `?` bind order follows the **textual** order of the placeholders, so the
  `VALUES` block owns the low-numbered ones. Getting it backwards shifts every
  entry by one and the join matches nothing: an `UPDATE … FROM` that affects
  zero rows with no error to notice.

**On the missing route — not this item's gap.** `ui/src/lib/api/client.ts` is
the only module permitted to name `fetch`, and the whole client is *queries*:
there is no mutation document anywhere in `ui/src/lib/api/`, and no server-side
schema in the repository. So "undo has no GraphQL operation" is true of every
write in the project, `bulk_apply_tag` included, and listing it as this item's
unfinished business would misattribute a phase-wide gap to a ticket that did not
create it. The client model is written to be usable the moment an operation
layer exists.
