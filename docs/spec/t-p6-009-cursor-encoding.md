# T-P6-009 — F8: a keyset cursor that can cross a process boundary

**Ticket:** T-P6-009. **Phase:** 6. **Origin:** T-P6-008 spec §5b F8, which
deferred it deliberately rather than doing it inside a transport ticket.

**Predecessor:** T-P6-008 shipped `POST /graphql` and left `PageInput.after`
as an explicit error. Spec `t-p6-008-graphql.md` §4b has the measurement that
led here; this ticket is the answer to it.

---

## 1. What exists, measured at `14e5a93`

`Sort` and `Cursor` are complete and tested. `Cursor` works **within one
process** and has never left it:

| Fact | Consequence |
|---|---|
| `Cursor::new` is `#[cfg(test)] pub(crate)` | no production caller can build one |
| `Cursor::from_row` is `pub(crate)` | only the store can build one |
| **nothing in the workspace serializes or deserializes a `Cursor`** | there is no wire form at all |
| `Filter` has `to_url` / `from_url` | the *filter* crosses boundaries; the cursor cannot |
| no route accepts a cursor | because none has ever needed one |

So `after:` paging is a **missing store capability**, not a missing GraphQL
field. The GraphQL side is already written and refuses the value; this ticket
supplies the value.

## 2. The design decision, and why it is not obvious

A cursor is `Vec<Value>` in `Sort::all_keys()` order. The encoding must
therefore answer one question the `Filter` encoding never had to: **which sort
is this cursor for?**

`Filter::from_url` decodes a self-describing value — the JSON says what it is.
A cursor cannot. `all_keys()` is `(o.date DESC, o.id ASC)` for one sort and
`(o.rating_sum DESC, o.id ASC)` for another, and **both have arity 2**. A
2-value cursor decoded from one and handed to the other binds cleanly, runs,
returns rows, and pages *wrongly* — no error anywhere.

That is the exact failure `Cursor`'s doc comment exists to prevent:

> Opaque on the outside by construction — there is no way to build one except
> from a page, so a cursor cannot be hand-assembled with the wrong arity, which
> is the failure that turns into a silently wrong page rather than an error.

**An encoding that omits the sort reintroduces at the wire the one thing the
type was built to rule out.** So the wire form carries a **sort fingerprint**, and
`from_url` is given the sort it is being decoded *for* and refuses a mismatch.

### 2a. The fingerprint is the sort's own `ORDER BY` text, hashed

Not a separate enum, not a version number, and not a hand-written label per
sort. `Sort::order_by()` is already the canonical description of the sort, it is
already what the query emits, and it changes **automatically** if a `SortKey` is
added or a direction is flipped. A hand-written label is a second thing to
forget — and forgetting it is silent, which is the whole hazard.

`fn fingerprint(&self) -> String` = first 8 hex chars of a 64-bit FNV-1a of
`order_by()`. FNV-1a rather than `DefaultHasher` for the reason `stable_usn`
already records: `DefaultHasher`'s output is unspecified across releases, and a
persistent on-disk identity must not change when the toolchain does.

**A truncated hash is a fingerprint, not a security boundary, and the code says
so.** 8 hex chars is 32 bits: a collision needs a deliberate search, not an
accident, and this value is not a capability. It is compared to catch *mismatch*,
which is a correctness concern. The doc comment says this in the type, because a
future reader will otherwise assume the hash is load-bearing for security and
either strengthen it needlessly or weaken it deliberately.

## 3. The wire shape

Canonical JSON, base64url — the same two steps `Filter::to_url` takes, and for
the same reason: §15.10 shareable URLs, and the alphabet is already implemented
and tested in `filter_ast.rs`.

```json
{"v":1,"f":"a1b2c3d4","k":[["s","2026-01-01"],["i",42],["s","o-7"]]}
```

- `v` — encoding version. Absent in a future version means *refuse*, not
  *assume 1*, for the same reason `from_url` refuses a malformed filter: a
  decoder that guesses is a decoder that will one day guess wrong.
- `f` — the sort fingerprint (§2a).
- `k` — the key list, **each value tagged with its variant**. `["s", str]`,
  `["i", int]`, `["f", float]`, `["b", bool]`, `["n"]` for null.

**The variant tag is load-bearing, not decoration.** Without it a
`Value::Int(42)` and a `Value::Str("42")` are the same JSON, and `Value::Null`
and `Value::Str("")` are both `null` or `""`. A cursor that decodes an int where
the column is text binds a number to a string column, and the comparison
silently does the wrong thing. `Value` has six variants and a cursor carries one
per key, so a sort over `[date, rating_sum]` has a tag list that is
`[s, i, s]` — and the arity check below is what catches a cursor whose tags do
not match the sort it claims.

## 4. What `from_url` refuses, and why each is an error

Every path returns `Err`. None defaults, and none "repairs":

1. **Not base64url** → `Err`. The filter's own note: "a malformed link cannot
   widen a query into 'everything'."
2. **Not JSON / wrong shape** → `Err`.
3. **Unknown `v`** → `Err`. A future version must not be read as v1.
4. **`f` does not match the sort it was decoded for** → `Err`, naming both.
   **This is the one the whole design exists for.**
5. **Arity ≠ `sort.all_keys().len()`** → `Err`, naming both counts. The existing
   `after_binds` assert catches this too, but as a **panic** — and a panic in a
   request handler is a 500 for a client error.
6. **A tag that the sort's key cannot have** → `Err`. `SortKey::RatingSum` is
   `Value::Int` and `SortKey::Title` is `Value::Str`; a cursor claiming
   `[i, s]` for `(rating_sum, id)` is refused rather than bound.
7. **A value that is not a `Cursor` key at all** (`List`, say) → `Err`. A key is
   scalar by construction; a list in slot 0 is a malformed cursor.

## 5. What this ticket does NOT do

- **It does not make `Cursor::new` public.** The constructor stays
  `#[cfg(test)]`; `from_url` is the *only* production way in, and it validates.
  A public `new` would undo the type's premise.
- **It does not wire `after:` into GraphQL.** T-P6-008's resolver already
  handles the refusal; turning it on is a one-line change to that resolver plus
  a UI test, and it belongs with the UI's cursor handling so the two land
  together. It is §7, not §3.
- **It does not version the *filter* encoding** even though the same argument
  applies. Out of scope, and named here so its absence is a decision.
- **It does not sign or MAC the cursor.** A cursor is a position, not a
  capability — §14.1 gates *objects*, and the query the cursor feeds is
  already consent-filtered. Signing would be theatre. Worth stating because
  "opaque token in a URL" invites the assumption that it is one.

## 6. Acceptance

1. `to_url` → `from_url` round-trips for **every** sort shape, including
   multi-key, and including `Value::Null` in a nullable column (`o.date`).
2. A cursor from sort A is **refused** by `from_url` for sort B, naming both —
   and the test uses two sorts of **the same arity**, because that is the case
   that does not error on its own.
3. The arity check and the tag check each have their own test, and each names
   the mismatch.
4. Every one of §4's seven refusals has a test.
5. The fingerprint **changes** when a direction flips and when a key is added,
   and is **stable** across runs. A fingerprint that does not change when the
   sort does is the silent failure, so this is asserted rather than assumed.
6. `sort_db.rs`'s page-boundary tests still pass **unedited** on both engines —
   the plan's falsifiable claim for why this is additive.
7. `Cursor::new` is still `#[cfg(test)]`, asserted by a test that greps the
   source, because "I did not make it public" is a claim about absence.

## 7. The one-line follow-up this leaves — **DONE in T-P6-010**

`POST /graphql`'s `objects` resolver: replace the `after` refusal with
`Cursor::from_url(input.after, &sort)`, and return `pageInfo.endCursor` as
`last_row.to_url()`. Then the UI's existing `endCursor` plumbing works with no
UI change at all — which is worth checking before assuming it needs one.

**Shipped in T-P6-010, and the check came out the way this section hoped:** the
UI needed **no change at all**. `PageInput.after` was already sent on every
request, `endCursor` was already in the selection set, and `keyset.ts` already
consumed it. The plumbing had been finished and waiting.

**One thing this section got wrong, worth recording since it is the second time
this codebase has measured the wrong string.** It said "worth checking before
assuming", and the check a reader would naturally run is
`grep -rn '$after' ui/` — which returns **nothing**. Not because the feature is
absent, but because `$after` is a GraphQL *variable* name the client never
spells: the document is `query Objects($input: PageInput!)` and the client
passes `after` inside the `$input` object. A grep for a variable name measures
the query document's vocabulary, not the wire, and its empty result reads
exactly like a missing feature. The measurement that settles it is reading
`client.ts:34-56` and `keyset.ts:200-210`.

**And the second half of §2's hazard finally has a user.** The sort fingerprint
means a cursor from one sort is *refused* by another — which will happen to a
real user who changes the sort column while scrolled down. That is the feature
working, and T-P6-010 made the refusal say so: the message names the sort
mismatch, because the remedy is "restart the walk", not "retry".

---

## 8. Addendum — what the implementation changed, at `0e57b77`

Three things this spec got wrong or did not foresee, recorded here because the
spec is the document a future implementer reads before writing code.

### 8a. The hazard is cross-SORT, and the spec said only "cross-sort" without saying which

§2 said a wrong-sort cursor "binds cleanly, runs, returns rows, and pages
wrongly". That is **half** true, and the half that is false matters:

- **Cross-type** (`date` cursor seeked in `rating_sum`): binds a `Value::Str` to
  an INTEGER column. The comparison is never true, so the result is an **empty
  page** — visible, if unhelpful.
- **Cross-sort among text keys**: `Date`, `Title`, `Kind` and `AddedAt` are
  **all** `Value::Str` and all arity 2. `o.title < '2026-01-01'` is a valid text
  comparison that **returns rows**, and the page is computed with the wrong
  column. Nothing errors.

So the second is the dangerous one, and the two are distinguished only by the
value types — which is why §6.2 requires the mismatch test to use two *text*
sorts. A test using `Date` vs `RatingSum` would pass with no fingerprint at all.

### 8b. `serde` cannot derive this format. Both directions are hand-written.

The two-element array `["s", "2026-01-01"]` is not a derivable shape for a
newtype enum. `#[serde(tag = "t", content = "v")]` — the closest available —
produces an object, and was tried before being rejected: `from_url` refused the
array form, which is how the mismatch surfaced.

Separately, **serde's derived struct `Deserialize` consumes fields in
DECLARATION order**, so the envelope's `v, f, k` order is load-bearing on the
*read* side too — and `serde_json::json!` sorts keys alphabetically, so the
natural way to write a test fixture emits `f, k, v` and fails with an error
pointing at a column *inside the fingerprint*. `Wire::parse` therefore reads the
three fields by name. The format on the wire is unchanged; only the reader's
strictness about it is gone.

### 8c. §6.7's absence tests were vacuous as first written, and passing is not evidence

§6.7 asked for two tests about things that are *not* there. Both were written as
source greps and **both passed against the exact regressions they exist to
catch**:

- the `#[cfg(test)]` check matched the **doc comment above the attribute**,
  which contains that literal text;
- the sweep treated any `#[cfg(test)]` earlier in a file as proof that a later
  line was in a test.

Fixed to read attribute lines only, and comment lines only. Each now fails its
mutation. The general rule, which is the third time this codebase has tripped it:
**a grep that can match a comment is not a search for the thing, and a test that
has never failed has not been tested.**

§6.1's round-trip is also weaker than it looks — both halves of the wire format
are in the same crate, so a round trip proves they agree, not that either is
right. That is why §6's golden test is now **literal bytes** rather than
interpolating `sort.fingerprint()`: the interpolated version was a round trip in
disguise and would have caught nothing. Both claims are verified by mutation —
tag/value swap and a zero-length NULL tuple each fail it.

### 8d. The spec's own 8-hex fingerprint is not what `format!("{h:08x}")` gives

`08` is a *minimum* width, so the 64-bit FNV-1a printed all 16 digits. The spec
says 8, so `h as u32` makes the truncation explicit. Found by the golden test,
which is precisely the test that exists to notice when the format changes.
