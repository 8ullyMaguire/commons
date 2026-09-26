# T-P5-006 — View modes and bulk editing

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006
**Spec:** §10.4, §10.6, §10.7, §10.9, §10.10

**The plan's accept:** "Playwright spec per surface. The unsaved-protection test
must attempt navigation with an unsaved edit and assert a confirm appears. **Done
when: that test exists.**"

---

## 1. What this ticket actually is

The plan lists seventeen things: group-by and auto-scroll, rich list tables,
folder view, unified media view, a vertical feed, a keyboard map, a command
palette, undo, a bulk-edit modal, right-click paste, unsaved protection, CSV
import, per-field ignore lists, create-from-subpage, and create-all-missing.

That is not one ticket. It is a phase, and delivering it as one commit would
mean fifteen of the seventeen are stubs — which is the failure mode this
repository's whole test gate exists to prevent. So this document does what the
plan's own "done when" says and no more: it scopes the seventeen, orders them by
what unblocks what, and defines the acceptance for each.

**A test that exists is not a test that passes.** The plan's "done when" is a
floor, not a ceiling. Each surface below is marked done only when its test
passes and the mutation count for its rules is non-zero.

---

## 2. What actually exists today

Measured, not assumed:

| Thing | State |
|---|---|
| `VirtualGrid`, `ImageGrid`, `Lightbox` | built (T-P5-005) |
| `view.ts` — URL → view state | built, sort/filter/density/direction |
| `keyset.ts` — cursor paging | built |
| Selection state | **does not exist** |
| A "select N things" API | **does not exist** |
| Any bulk mutation endpoint | **does not exist** |
| Command palette, keyboard map, undo | **does not exist** |
| CSV import, ignore lists, create-* | **does not exist** |

So this is not a UI ticket with a large tail. Four of the seventeen need a
server mutation that has to be designed, migrated, and tested in Rust first.

---

## 3. The order, and why

Ordered by dependency, not by ticket number. Most of the list is blocked on
the first item, and one item is blocked on the server.

**Spine — selection.** A selection model that survives paging, filtering, and
re-sorting, and that names things by identity rather than by index. Everything
below is a view over a selection, and a selection keyed on row index is wrong
the moment a page lands under it.

**Then, in order:**

| # | Surface | Needs | Why here |
|---|---|---|---|
| 1 | Selection model | — | every other row is a view over it |
| 2 | Rich list table | selection | the first surface that shows it |
| 3 | Bulk-edit modal | selection + a mutation | the first thing that writes |
| 4 | Unsaved protection | the modal | its test is the plan's floor |
| 5 | Command palette + keyboard map | selection | reads the same model |
| 6 | Group-by + auto-scroll | list table | needs rows to group |
| 7 | Folder view | a tree query | needs a server query that does not exist |
| 8..17 | the rest | various | see §5 |

**Paging past the loaded prefix**, which T-P5-005 explicitly left to this
ticket, belongs to item 2. The lightbox currently clamps to the rows it was
given, so "next" stops at the end of the loaded list. That is correct and
incomplete, and it is a one-line change once the store can be asked for the
next page on demand.

---

## 4. The selection model, in detail

Because it is the spine, its design is fixed here rather than left to
whoever writes it.

**Identity, not position.** A selection is a set of object ids. Row indices are
a *view* over a selection and are derived per render. A selection held as
indices breaks in the ordinary cases: the user selects three rows, pages, and
the third selected row is now row 0 of the new page.

**Select-all is a mode, not an operation.** "Select all 4,000 items" and
"select the 40 loaded" are different intents and the user must be able to tell
which they did. So: `selected: Set<id>` plus an optional `selectAll: query`
that names a filter rather than a set. The count shown is the estimate from the
server when `selectAll` is set, and the exact size of the set otherwise. This
is the same shape as a database cursor and it is the only one that makes
"select all 4,000, then bulk-tag" both correct and honest about what it will
touch.

**What a filter change does.** A filter change drops the `selectAll` mode and
keeps only the ids still present in the new result — then reports how many were
dropped. Silently keeping a selection the user can no longer see is how a bulk
edit hits the wrong rows, and §10.7's rule is that a destructive action is
reversible, which requires the user to know what it covers.

---

## 5. What is deferred, and to where

| Surface | Deferred to | Why |
|---|---|---|
| Folder view, unified media view | needs a tree/parent query | no server support; designing a query is its own ticket |
| CSV import, per-field ignore lists | T-P5-009 (data in) | import is a data concern, not a view one |
| create-from-subpage, create-all-missing | T-P5-009 | same |
| right-click paste | after the palette | needs the same command registry |
| Undo | after the first mutation | undo of a mutation that does not exist is undo of nothing |

---

## 6. Acceptance, per surface

Not "a test exists" — a test that fails when the behaviour is wrong.

1. **Selection survives paging.** Select rows on page 1, page to page 2,
   assert the selected ids are unchanged and still selected.
2. **Select-all is distinguishable.** `selectAll` set: the count is the
   server's estimate, not the loaded size, and the UI says so.
3. **A filter change reports what it dropped.** Not silently.
4. **Bulk edit writes only the selection.** Seed a 50-item library, select 3 by
   id, bulk-tag, assert 3 changed and 47 did not.
5. **Unsaved protection.** Per the plan: attempt navigation with an unsaved
   edit, assert a confirm appears. This is the ticket's floor.
6. **The palette runs a command by name** and the keyboard map reaches the same
   command by key, and both go through one registry — a second dispatch path is
   a second thing to keep in sync.

---

## 7. Rules that will need mutation testing

Predicted now, measured when the code exists:

- `selectAll` must not be treated as a set of ids (a selection that silently
  narrows to the loaded page).
- The filter-change prune must not drop ids the user can still see.
- The bulk write's "affected count" must not be the selection's size — it is
  the server's count, and they differ the moment a row changed under it.
- The unsaved guard must key on *dirty*, not on *any edit* — an edit that was
  saved is not unsaved, and a guard that cannot tell blocks a user who saved.

These are the rules most likely to be wrong, and the mutation script will say
which are actually covered once the code is there.
