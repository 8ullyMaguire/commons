# T-P6-010 — implementation plan

Spec: `docs/spec/t-p6-010-after-paging.md`. Work in `~/code-local/rust/commons`.

```sh
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
```

**Step 0 below is an ANSWER, not a task** — it is the measurement that decides
the ticket's size, and it is the second time a grep has measured the wrong
string in this codebase.

---

## Step 0 — ANSWERED: the UI needs no change, and `grep '$after'` lies

**T-P6-009 spec §7 said to check whether the UI needs changing rather than
assuming. The check says it does not, and the ticket is server-side only.**

| Piece | State at `abc6e9c` |
|---|---|
| `PageInput.after: string \| null` | sent on every request |
| `pageInfo.endCursor` | in the selection set, consumed by `keyset.ts:208` |
| `ObjectPage::next_cursor()` | `pub`, `None` iff `!has_more` |
| `Cursor::to_url` / `from_url` | T-P6-009 |
| server `objects` | refuses non-null `after` |

**And `grep -rn '$after' ui/` returns NOTHING.** That is not a missing feature.
`$after` is a GraphQL *variable* name the client never spells — it passes
`after` inside the `$input` object, and the query document says
`query Objects($input: PageInput!)`. The same shape as T-P6-008's
`/api/bulk/tag`, which turned out to be two mentions inside comments. **A grep
that measures a variable name measures the query document's vocabulary, not the
wire.** The measurement that settles it is reading `client.ts:34-56` and
`keyset.ts:200-210`, not searching for a token.

**So the diff is one function, two tests rewritten, and one new test file's worth
of cases.** Not a UI change. Not a store change.

## Step 1 — the resolver, in `crates/commons-server/src/graphql.rs`

Replace the refusal block (lines 152-161) with nothing, and thread `after`
through. The order matters and is the reason for it:

```rust
    // `after` is decoded AFTER the sort, because a cursor is bound to the sort
    // it was made for: `from_url(after, &sort)` is the only call that can
    // check that, and it needs `sort` resolved. Decoding first would mean
    // decoding twice or guessing the sort.
    let filter = match decode_filter(input.filter.as_deref()) { /* as now */ };
    let sort = match decode_sort(input.sort.as_deref(), input.direction.as_deref()) { /* as now */ };

    let after = match input.after.as_deref() {
        None => None,
        Some(raw) => match Cursor::from_url(raw, &sort) {
            Ok(c) => Some(c),
            Err(e) => return GqlResponse::error(format!("bad `after` cursor: {e}")),
        },
    };
```

Then pass it, and emit the cursor:

```rust
    let page = match state.store.query_sorted(&filter, caller, sort.clone(), after, limit).await { ... };
```

**`sort` is cloned** because `to_url` and `from_url` both borrow it and the
`pageInfo` is built after the query. If `Sort` is not `Clone` yet, derive it —
it is a `Vec` of `(SortKey, SortOrder)`, both `Copy`, so `Clone` is a
no-brainer; if it already is, this is a note not a change.

```rust
        page_info: PageInfo {
            has_next_page: page.has_more,
            has_previous_page: false,   // unchanged, and the reason is unchanged
            start_cursor: None,         // unchanged: no backward cursor exists
            end_cursor: page.next_cursor().map(|c| c.to_url(&sort)),
        },
```

**`next_cursor()` is `None` whenever `has_more` is false** — including for a
short final page — so the last page returns `endCursor: null` for free, and a
client cannot seek past the end and receive an empty page that looks like a
filter that matched nothing. That is the store's existing contract; do not
re-derive it here.

### 1a. The module docs must change, not just the code

`graphql.rs`'s header has a whole section headed "`after:` is an explicit error,
not a silently ignored field" (lines 24-34), explaining why the refusal is
honest. **Every line of that is now false.** Rewrite it to say the opposite and
keep the reasoning: the refusal was right *then*, the capability arrived in
T-P6-009, and the same principle now applies one level down — a cursor that
cannot be decoded is an error, never a first page.

A stale comment that contradicts its own function is worse than no comment, and
this one is 11 lines of it.

## Step 2 — delete the two tests that assert the OLD behaviour

Both must go. Keeping either green would mean the feature is not shipped.

1. **`an_after_cursor_is_refused_rather_than_silently_ignored`** (line 399).
   It asserts the refusal. It is replaced by the malformed-cursor test (Step 3,
   case 5), which asserts the *stronger* property: a bad cursor is still an
   error, just a different one.
2. **`the_page_cursors_are_null_rather_than_a_string_that_would_be_refused`**
   (line 428). It asserts `endCursor` is null. That assertion is now **false
   when there is a next page** — which is the feature. Rewrite it rather than
   delete: keep the "present, not omitted" assertions (those are still right
   and still matter — a client that reads `endCursor` must not get `undefined`),
   keep `hasPreviousPage == false` and `startCursor` null, and change only the
   `endCursor` claim to "null **on a page with no successor**, a real cursor
   otherwise".

**Do not weaken it to `assert!(endCursor.is_null() || endCursor.is_string())`** —
that is a tautology over JSON and asserts nothing.

## Step 3 — the new tests

A **new file**, `crates/commons-server/tests/after_paging.rs`, not additions to
`graphql_route.rs`. Reasons: `graphql_route.rs` is 863 lines and about the
*surface* (does each operation resolve, is each input field handled), while this
file is about one field's *behaviour across pages*; and the harness is already
`#![allow(dead_code)]` shared, so a second file costs nothing.

### 3a. The load-bearing test: a two-page walk, asserting NO GAPS and NO DUPLICATES

```rust
/// Seed `n` objects and walk them all with `limit` per page.
async fn walk(app: &TestApp, n: usize, limit: usize, sort: &str) -> Vec<String> { ... }
```

Seed with `media_fixture_at_tier` at the same tier for each — the harness's
`seed` inserts the object, the file row and the consent row, so a same-tier seed
is what makes them all visible to the owner.

**The assertion is the point, and a count is not enough:**

```rust
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let res = graphql(&app, objects_document(), json!({
            "input": { "first": limit, "after": cursor, "sort": sort }
        })).await;
        let v = body_of(&res);
        assert!(v.get("errors").is_none(), "page error: {v}");
        let nodes = &v["data"]["objects"]["nodes"];
        seen.extend(node_ids(&res));
        if v["data"]["objects"]["pageInfo"]["hasNextPage"] == false { break; }
        let next = v["data"]["objects"]["pageInfo"]["endCursor"].as_str();
        assert!(next.is_some(), "hasNextPage is true but endCursor is null: {v}");
        cursor = next.map(|s| s.to_string());
    }
    assert_eq!(seen.len(), n, "paging lost or repeated rows");
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), n, "a row came back twice: {seen:?}");
```

**Why both assertions.** A keyset bug that repeats the boundary row inflates
`seen` and the count catches it; a bug that *skips* it deflates the count the
same way. Neither shows up in a "the cursor round-trips" test. The duplicate
check is separate because a bug can repeat one row and drop another and leave
the count unchanged.

7 objects at `limit 3` gives 3+3+1 — three pages, and the last page is *short*,
which is the case where `has_more` must go false. Do not use a divisor of n
(6 at 3) or the short final page never happens.

### 3b. Consent must still filter page 2 — the test a paging feature silently breaks

```rust
#[tokio::test]
async fn resuming_does_not_widen_visibility() { ... }
```

Seed 3 public + 2 unverified, walk with `limit 2` as the owner, and assert the
unverified ids **never appear on any page** — not just page 1. A cursor is a
position, and a position must not become a capability: the fingerprint proves a
cursor matches its *sort*, and nothing about it carries a *caller*. The consent
clause is re-evaluated per query because the caller is passed per query; this
test is what proves the cursor did not replace it.

**This is the test most likely to be the only one that catches a real bug**, and
it is worth saying why: every other test in the file uses a single-tier fixture
or reads only page 1, and a cursor that cached the first page's filter would
satisfy all of them.

### 3c. The four refusals, each asserting the ERROR not an empty page

| Case | Must be refused because |
|---|---|
| a cursor from `title ASC` used with `date DESC` | fingerprint mismatch (spec §3b) |
| `"eyJ2IjoxfQ"` (no fingerprint) | shape/version |
| a base64url string that is not JSON | not a cursor |
| a cursor with the right fingerprint but 1 key instead of 2 | arity |

**Use two TEXT sorts for the mismatch case.** T-P6-009 §8a: `Date` vs
`RatingSum` is refused by the *type* check, so it would pass whether or not the
fingerprint existed. `date` and `title` are both `Value::Str` and both arity 2.

Each test asserts `errors` is present **and** that the message names the cause
(`different sort`, `version`, `base64`, `number of keys`). Asserting only "there
is an error" would pass if the resolver failed for an unrelated reason — a
500-shaped refusal is still a refusal.

**And the negative control**: assert that the same request **without** `after`
succeeds on the same fixture. Without it, a test that passes because the
resolver is broken for every input looks identical to one that passes because
the refusal works.

### 3d. The last page returns `endCursor: null`

Walk to the end (Step 3a's loop already collects this) and assert the final
`pageInfo` is `{hasNextPage: false, endCursor: null}`. A server that keeps
emitting a cursor past the end gives a client an infinite loop that looks like a
very slow query.

## Step 4 — the UI, verified rather than assumed

The spec claims the UI needs no change. **Prove it, do not assert it.**

```sh
cd ui && node ./tests/run-tests.mjs keyset graphql-live
```

`keyset.test.ts` mocks the server with `endCursor: String(end)` — a *counter*,
not a real cursor. **That is the thing to notice:** the UI's own test suite has
never seen a real cursor, and it passes with a fake one. So the live test is
the only evidence that the two halves agree. `graphql-live.test.ts` already
runs the real client against a real server; extend it with a two-page walk
through `fetchObjects` and assert the same no-gaps/no-duplicates property.

**If `keyset.test.ts` passes unchanged, that is evidence the UI is
cursor-shape-agnostic** — which is the claim. Record it as evidence, not as a
coincidence.

## Step 5 — the gate, three times

```sh
for n in 1 2 3; do
  cargo test --workspace > $S/t610-$n.log 2>&1
  # A run that produced NO results must not read as a pass.
  suites=$(grep -cE "^test result" $S/t610-$n.log)
  [ "$suites" -eq 0 ] && { echo "RUN $n GATE FAIL: nothing ran"; continue; }
  python3 - $n <<'PY'
import re,sys
n=sys.argv[1]
p=f=i=s=0
for line in open(f"/home/alvaro/.hermes/profiles/coding/cache/scratch/t610-{n}.log",errors="replace"):
    m=re.match(r'test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored',line)
    if m: p+=int(m.group(1)); f+=int(m.group(2)); i+=int(m.group(3)); s+=1
print(f"RUN {n} {'PASS' if f==0 and s>0 else 'FAIL'}: passed={p} failed={f} ignored={i} suites={s}")
PY
  cargo clippy --workspace --all-targets 2>&1 | grep -cE '^(warning|error): [a-z]'
done
```

**Count by regex, never by `awk -F'[ ;]'`.** The last loop found this the hard
way: `;` is itself a delimiter, so `$3` is an *empty* field and `$5` is the
literal word `"failed"`, and a runner written `f+=$3; i+=$5` reports
`failed=0 ignored=0` on a line that says `7 failed; 1 ignored`. Verified by
feeding it exactly that line. **Reconcile the delta** — the baseline is
1938/0/1/110, and this ticket's new tests must account for the whole difference,
or a test was lost in a rename.

## Step 6 — docs, tag, mirror

1. `CHANGELOG.md` — `after:` paging now served. **No version bump**: `/api/v1`
   is the public surface, `/graphql` is not under it, and no path changed.
2. `docs/spec/t-p6-008-graphql.md` — §4b and its follow-up row: the refusal is
   historical, and F8's second half (this ticket) is done.
3. `docs/spec/t-p6-009-cursor-encoding.md` §7 — struck, pointing at T-P6-010.
4. `docs/HANDOFF.md` — `hasPreviousPage: false` and `startCursor: null` are now
   *load-bearing* (a cursor exists forward, not back), so that paragraph needs
   the reason restated rather than the fact.
5. Tag `phase-7-140-after-paging`, `git pushall --tags`, mirror to
   `~/code/rust/commons` by `git merge --ff-only`, and **verify by comparing
   `HEAD^{tree}` on both** — never by reading the merge output. `pushall.sh`
   verified against `ls-remote` during T-P6-009 and caught a real 524, which is
   why its "Not reporting success" line can be trusted.

## Step 7 — a mutation, because a paging test is easy to write vacuously

The one mutation that matters: **make the resolver ignore the decoded cursor**
(pass `None` to `query_sorted` while still emitting `endCursor`). Every
acceptance test should fail — the walk returns page 1 forever, so
`seen.len() == n` fails and the duplicate check fires.

If the walk test passes under that mutation, it is measuring nothing. A second
mutation: **swap `sort.clone()` for the wrong sort** in the fingerprint check,
which should fail the mismatch test and nothing else — that difference is what
tells you the refusal and the walk are independently covered.


---

## Addendum — three fixture defects, and a plan prediction that was wrong

The plan's steps were right about the *shape* (one function, two tests rewritten,
one new file) and wrong about three things, all found by tests failing.

### 1. The same inversion as T-P6-008, one layer down

`resuming_does_not_widen_visibility` walked every page with a **headerless
request and called it "anonymous"**. A headerless request **is the local owner**
— `caller_from_request` falls back to `media::local_caller()` — so the test
asserted the owner does not see the owner's own `unverified` objects, and
failed. **T-P6-008 was bitten by exactly this, in this repo, for this reason**,
and its fix was a `bearer()` token naming no grant. This time it was worth
noting the pattern rather than just the fix: *a test that reasons about a
caller must construct that caller explicitly, because the default is the most
privileged one.*

The token is now sent on **every page**, not just the first. A cursor carries no
caller, so the identity layer must be consulted per request, and a test that
authenticated once then paged anonymously asserts the opposite of what it says.

### 2. The fixture removed the thing under test

`a_cursor_with_the_wrong_number_of_keys_is_refused` seeded **2 objects and asked
for 2**, so `has_more` was false, `endCursor` was null, and the `.expect("a
cursor")` fired. A page with no successor is exactly the case that emits no
cursor — so the fixture had made the arity check **unreachable**. Now 4 at a
page of 2, plus an assertion that the fixture really has a next page, so the
premise is checked rather than assumed.

This is the shape `agent-claim-review` records as "a test that pins a hazard
must first prove the hazard is still reachable", and it is worth adding the
converse: **a fixture can remove a hazard by accident, and the test then fails
for a reason that looks nothing like the one it is about.**

### 3. A hand-rolled codec in the test

The arity test needed a tampered cursor, so it carried its own base64url
encode/decode pair — and its decoder **failed on the server's real cursor**. The
test was therefore measuring my codec's compatibility with the server rather
than the server's arity check. Now uses `commons_store`'s own codec.

**A helper that reimplements the thing under test is a second source of truth,
and here it was simply wrong.** The general form: when a test must fabricate a
value in the system's own encoding, use the system's encoder.

A fourth, smaller one: `o.date` is nullable, so slot 0 of a cursor can be a
`NULL`, and the tamper took `doc["k"][0]` — producing a bare string, so the
envelope failed as `Shape` rather than reaching the arity check at all. The
refusal was real; it named the wrong cause, which is exactly what the assertion
was written to prevent. It takes the last key now.

### The UI's own test had a fixture-dependent assertion

`runs the client's OWN fetchObjects against the server` asserted
`objects.nodes.length === 0, 'a fresh temp library has no objects'`. Its comment
*already said* the emptiness was not the point — "it is that the call RETURNED
rather than throwing" — but the assertion stayed, which made the test depend on
running **first in the file**. The new paging test seeds seven objects, node does
not promise order, and the two tests contradicted each other.

**An assertion about the FIXTURE is not an assertion about the CODE.** "The
library is empty" is a property of the temp directory, and every test that
writes there invalidates it. The wire-shape claims — array, boolean, `null`
count — all survive a populated library, which is the stronger claim anyway.

### The plan's Step 7 prediction was wrong, and the finding is better

The plan said a second mutation (fingerprint against the wrong sort) "should
fail the mismatch test **and nothing else**". It failed **9 of 10**.

That is the better result and the plan's prediction was too narrow: the
fingerprint is on **every** decode path, so corrupting it makes every cursor
refused, not just the mismatch case. Recorded here because the plan's own text
is the artefact a future reader checks the result against, and leaving the
wrong prediction in it would teach the wrong lesson.

### Mutations, and what they prove

```
MUT1  decode the cursor, then pass None       4 of 10 fail
      -> the_cursor_a_page_returns_is_accepted_as_the_next_after
         a_walk_covers_every_row_exactly_once
         a_walk_in_the_other_direction_covers_every_row_too
         resuming_does_not_widen_visibility
      and the four refusal tests stay GREEN.

MUT2  fingerprint against an empty sort       9 of 10 fail
```

**MUT1 is the one that matters.** It fails exactly the four walk tests and
nothing else, which is the independence the plan wanted: the walk tests and the
refusal tests cannot both be satisfied by one wrong behaviour. A single
tautological assertion would have gone green under both.
