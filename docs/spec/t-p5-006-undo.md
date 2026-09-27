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

**The undo record is written by the write, in the same transaction.**

The alternative — reconstruct from `object_tag` after the fact — looks cheaper
and is wrong in three ways, each of which is a real bug rather than a nicety:

1. *Which objects?* The undo must restore exactly the objects the write touched.
   Re-running the target selection later can return a different set, because the
   filter's result is not stable: an object may have been added, removed, or
   edited by another action between the write and the undo.
2. *What was there before?* "Remove this tag" applied to an object that already
   had it was a no-op; the inverse is *add it back*. Without the prior state the
   undo cannot tell a no-op from a change, so it would delete a tag the user set
   deliberately.
3. *Is undo still allowed?* A write that was superseded must not be undoable into
   a state nobody was ever in. That is a comparison against a recorded prior
   state, so it needs the record.

The cost is one table and one insert inside a transaction that already exists.

## 4. Shape, as shipped

    pub struct Write {              // what the caller hands in
        pub id: String,
        pub caller: String,
        pub action: String,          // "bulk.tag.add" | "bulk.tag.remove"
        pub tag_id: String,          // ON THE RECORD, not a parameter
        pub requested: i32,
        pub matched: i32,
        pub entries: Vec<UndoEntry>, // the inverse, one per CHANGED object
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

**The write is not atomic with its record.** `bulk_apply_tag` is a single
`INSERT ... SELECT`, so making the write and `record_undo` one transaction needs
that statement to run on a connection `record_undo` also holds. Not done. The
failure left is a write with no undo — the user is told the write happened,
which is the safe direction, but it is a gap and it is written down here rather
than left to be discovered. The fix is a `Store::bulk_apply_tag_undoable` that
takes the connection; it wants the bulk module's error type, so it is bulk's
to write rather than undo's.

**An entry has no `after.row_existed`.** It is reconstructed as `true` on read,
because an entry is only written for an object the write *changed* and a write
that changed the row had a row afterwards. This is the one place the
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

## 7. Status

Done. `crates/commons-store/src/undo.rs`, migration `0019_undo.sql` (both
engines), `tests/undo_db.rs` (11 tests, both engines), 7 unit tests,
`ui/src/lib/api/undo.ts` (11 tests). 1217 Rust, 237 UI. Clippy clean.

Not done, and named: the atomicity gap in section 6; the `.svelte` component
that renders the toast (the model is done and tested, the DOM is not); and the
GraphQL operation — `undo` has no route, so nothing can call it from the client
yet. The client model is written to be usable the moment the operation exists.
