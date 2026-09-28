# T-P6-010 — wire `after:` into the GraphQL `objects` resolver

**Ticket:** T-P6-010. **Phase:** 6. **Origin:** T-P6-008 spec §4b shipped `after:`
as an explicit refusal because the capability did not exist; T-P6-009 built it.

**Predecessors:** T-P6-008 (`POST /graphql`), T-P6-009 (`Cursor::to_url` /
`from_url`, tag `phase-7-130-cursor-wire`).

---

## 1. The measurement, at `abc6e9c`

**Both halves already exist and neither is wired to the other.**

| Piece | State |
|---|---|
| `Cursor::to_url(sort)` / `from_url(s, &sort)` | T-P6-009, `commons-store` |
| `ObjectPage::next_cursor()` — `Option<Cursor>`, `None` iff `!has_more` | already public |
| Client `PageInput.after: string \| null` | already sent, every request |
| Client `pageInfo.endCursor` | already selected and consumed |
| `ui/src/lib/api/keyset.ts` — the UI's own cursor bookkeeping | already written |
| Server `objects` resolver | **refuses any non-null `after`** |

**And the handoff's own warning was right.** T-P6-009 spec §7 said to check
whether the UI needs changing before assuming it does. It does not: the client
has sent `after` since before the server could accept it, `endCursor` is in the
selection set, and `keyset.ts:208` already assigns `pageInfo.endCursor`. **This
ticket touches no UI file.** The plumbing was finished and waiting for the
server to catch up.

`grep '\$after' ui/` returns **nothing** — because `$after` is a GraphQL
*variable name* that the client never spells out; it passes `after` inside the
`$input` object. Worth recording, because "the query does not mention `after`"
reads as a missing feature and is not one.

## 2. What changes

In `crates/commons-server/src/graphql.rs`, the `objects` resolver:

1. **Delete the blanket refusal.** It is now false — the reason it gave
   ("this server has no way to encode a keyset cursor into a URL-safe value")
   stopped being true at T-P6-009.
2. **Decode** `input.after` against the *resolved* sort:
   `Cursor::from_url(after, &sort)`.
3. **Pass it** to `query_sorted` as the `after` argument, replacing the `None`
   currently hard-coded there.
4. **Return** `end_cursor` as `page.next_cursor().map(|c| c.to_url(&sort))`.

`start_cursor` stays `None` and `has_previous_page` stays `false` — both are
already correct, for a reason that does not change: this server has no backward
cursor and inventing one would give the UI a back arrow that cannot work.

## 3. Three decisions, each recorded because the obvious alternative is wrong

### 3a. The refusal becomes a *decode error*, not a removal

`after` is untrusted input, so it goes through `from_url`, which has seven
refusals. A malformed cursor must produce a **named GraphQL error naming the
mismatch** — not an empty page, and not the first page. T-P6-009 §4 built those
messages for exactly this caller.

**The refusal and the decode failure are different errors and must read
differently.** "Not available yet" told a client the feature was missing;
"this cursor was made for a different sort" tells it the cursor is stale
because *it* changed the sort between pages. A client that changed its sort mid
scroll needs the second message to know to restart rather than retry.

### 3b. The cursor is bound to the sort, so a client that changes sort mid-scroll
must restart — and the server says so

T-P6-009's fingerprint exists because `after_binds` checks length only. The
user-visible consequence lands here: **a cursor from one sort is refused by
another**, which is correct and will happen to a real user who changes the sort
column while scrolled down.

This is a *feature* (it is what prevents a silently wrong page), so it ships
with a message naming the cause. Not a workaround, and not a "just ignore the
cursor" path — ignoring it would return the first page under a
`hasNextPage: true`, which is the exact silent-wrong-page failure the
fingerprint was added to prevent.

### 3c. `total_count` stays `None` and `hasNextPage` is `page.has_more`

Not in this ticket's scope, and both are already right. `has_more` is the
store's own answer, computed with the same query that produced the page, so it
cannot disagree with the rows returned. `total_count` needs `count(*)` over a
consent-filtered set, which is a different query and a different ticket.

## 4. Acceptance

1. **A two-page walk terminates and covers the same rows as one large page.**
   This is the load-bearing test, and it is the one a round trip cannot prove:
   paging through `limit 3` over 7 objects must return all 7 exactly once, in
   sort order. Assert **no duplicates and no gaps**, not just a count.
2. **The cursor a page returns decodes as `endCursor` and is accepted as
   `after`.** A cursor that is emitted but not accepted is the failure mode of
   a wire format nobody round-trips.
3. **The last page returns `endCursor: null`** and `hasNextPage: false`, so a
   client stops rather than looping on a cursor that seeks past the end.
4. **A cursor from a different sort is refused with a message naming the
   mismatch** — using two *text* sorts, per T-P6-009 §8a, because a text/int
   pair would be refused by the type check instead and prove nothing.
5. **A malformed cursor is refused, and never silently returns the first page.**
   The test asserts the *error*, not an empty node list, because
   `nodes: []` is what both a refusal and a legitimately empty page look like
   to a client that ignores errors.
6. **Consent filtering still applies on page 2.** A cursor is a *position*, and
   a position must not widen visibility: the unverified object must stay
   invisible when resuming. This is the test that a paging feature can silently
   break, because page 1's filter looks right.
7. **The UI needs no change** — verified by running the existing
   `keyset.test.ts` and `graphql-live.test.ts` unedited, and by a live
   two-page walk through the real client.
8. **`T-P6-008`'s refusal test is deleted, not weakened.** It asserted the old
   behaviour, so keeping it green would mean the feature is not shipped. Its
   replacement is acceptance 4 and 5.

## 5. What this does NOT do

- **No backward paging.** `hasPreviousPage` stays `false` and `startCursor`
  stays `null`. Backward keyset needs a `before:` cursor and a reversed sort —
  a separate ticket, and the UI has no back arrow to satisfy.
- **No UI change.** §1 measured it; there is nothing to build.
- **No `totalCount`.** §3c.
- **No `after` on any other operation.** `BulkTags`, `BulkApplyTag` and
  `CreateAllMissing` are unfiltered writes with their own semantics; paging them
  is a different design question.
- **No cursor signing.** T-P6-009 §5: a cursor is a position, not a capability.
