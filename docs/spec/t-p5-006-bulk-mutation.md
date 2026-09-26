# T-P5-006 item 3 — Bulk mutation, and the consent check it is missing

**Plan entry:** `docs/plans/implementation-plan.md` §T-P5-006
**Spec:** §10.7 (destructive actions reversible), §14.1 (rules enforced in the
data layer), §10.6 (selection)
**Scope file:** `docs/spec/t-p5-006-views-and-bulk.md` §3, item 3

---

## 1. The finding that shaped this

`Store::query` is documented as "the only way to read objects" and takes a
`CallerId` by reference with no default and no other constructor. The mechanism
is stated plainly: *a filter that can be omitted is a filter that will be
omitted, and the call site that omits it is the one nobody reviews.*

`Store::apply_tag` takes `(&self, object_id, tag_id, confidence, source)`. No
caller. So the same guarantee does not hold for writes, and it never has.

That is not this ticket's bug to introduce, and it is also not this ticket's
bug to leave: bulk editing is the first surface where a caller names a set of
objects and expects all of them to change. A bulk write without a consent check
does not leak one object, it rewrites 4,000. §14.1 says the rules live in the
data layer, not the UI, and the data layer's write path has no rule.

So this ticket does two things in one:

1. `bulk.rs` — the bulk mutation, consent-filtered, returning the server's
   affected count rather than the caller's expectation.
2. `apply_tag` gains a `CallerId`, and the one call site that had none is
   fixed.

Item 2 is not scope creep. It is the reason a bulk tag operation is safe to
ship at all, and shipping the bulk path without it would be shipping the
amplified version of a known hole.

---

## 2. What a bulk mutation must do

**Name the objects, or name the query.** Two shapes, because the selection has
two shapes (`selection.ts`): an explicit `Vec<ObjectId>`, or a `Filter` that
selects a set. The second is what makes "select all 4,000 and tag them"
possible without enumerating 4,000 ids from the client, and it is the shape
that is dangerous — so it gets the consent clause by construction rather than by
convention.

**Return the count the server computed.** Not the length of the list the client
sent, and not the client's own estimate. The three differ the moment a row
changed between the selection and the write, and the count a bulk modal reports
is the number a user will trust about what happened. A modal that says "12
tagged" when 12 were sent and 11 matched is a lie with a progress bar.

**Report consent skips separately.** A caller who cannot see a row must not be
told it was written. The return carries `applied` and `skipped_invisible`, and
the second is not an error: the object may be `unverified` for this caller and
perfectly visible to the owner. Making it an error would tell a user that
something is wrong with the object when the object is fine and the caller simply
may not see it.

**Be one transaction per batch, not per object.** 4,000 single-row statements in
4,000 transactions is a bulk edit that takes minutes and can half-finish. The
whole batch commits or none of it does.

---

## 3. The rules that need mutation testing

- The consent clause is present in the SQL. A mutation that drops it must be
  killed — and this is the one where "killed" means a test that fails *loudly*,
  because a missing clause does not error, it silently over-reports.
- `applied` is the server's count, not the request's.
- `skipped_invisible` is computed, and a mutation that returns 0 must fail.
- A batch that fails part-way rolls back entirely.
- The filter path and the id path both carry the clause. A mutation that
  applies the clause to one and not the other must fail — the id path is the one
  that will be forgotten, because it "obviously" needs no filter.

---

## 4. Not in this ticket

- **The modal itself.** It is the next step, and it is thin once this exists:
  it renders the affected count and the skips. Building it before this would
  mean rendering a number the server has not confirmed.
- **Undo.** §10.7 requires destructive actions be reversible, and a bulk tag is
  not destructive in the "cannot be recovered" sense — it is a reversible
  assignment. Genuinely destructive bulk operations (delete, purge) are
  deferred and must not ship without an undo record.
- **Delete.** There is no bulk delete, and adding one needs a trash concept
  that does not exist. A bulk delete that is a hard delete is exactly the
  §10.7 violation; a bulk delete that is a soft one needs a `deleted_at`
  migration.
