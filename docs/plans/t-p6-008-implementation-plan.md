# T-P6-008 implementation plan — GraphQL for the four operations the UI sends

**Spec:** `docs/spec/t-p6-008-graphql.md`. **Baseline:** `a238cca` (T-P6-007
closed, 1887/0/1/107). **Repo:** `~/code-local/rust/commons`, mirror to
`~/code/rust/commons` at the end.

Written to be implementable from this file alone. Every signature quoted below
was read out of the tree at `a238cca`, and every verification command states
its expected output so a later reader can tell a real green from a plausible
one.

---

## Step 0 — ANSWERED: `async-graphql` cannot be used. This is the third fallback branch.

**Step 0 was run. `async-graphql` is unusable against this workspace, and the
branch it selects is the third one: a hand-rolled resolver over `serde_json`.**
Do not re-run the probe; the evidence is below and it is not ambiguous.

### What was measured

The question was never "does it compile" — it was "does it work against **axum
0.7.9**", because that is what the workspace pins and what T-P6-007 learned the
hard way about `utoipa-axum`.

| `async-graphql-axum` | axum its `Cargo.toml` asks for | outcome |
|---|---|---|
| 7.2.1 | **0.8.1** | `GraphQLRequest` implements axum **0.8**'s `FromRequest`; axum **0.7**'s `Handler` bound is unsatisfied (`E0277`, pointing at `axum-0.8.9/src/handler/mod.rs:148`) |
| 7.2.0 | 0.8 | same |
| 7.1.0 | 0.8 | same |
| 7.0.17 | 0.8 | same |
| 7.0.0 | **0.7** | the only one that wants 0.7 — and **`async-graphql` 7.0.0 itself fails to compile on this toolchain: 149 errors**, all `E0195 … lifetimes do not match method in trait`, inside its own `model/directive.rs` |

So the 7.x line is exhausted from both ends: everything that builds against axum
0.7 does not compile at all, and everything that compiles needs axum 0.8.

### The two false results, and why they are recorded

Both of these looked like success and were not. They are the reason the table
above has a "builds" column and a "works" column.

1. **A bare scratch crate with `axum@0.7` AND `async-graphql-axum@7` builds with
   zero errors at every 7.x version.** It resolves *two* axum copies (0.7.9 and
   0.8.9) and nothing uses the 0.8 one, so nothing conflicts. A probe that
   declares both versions proves nothing about either. The scratch crate is
   necessary for the version survey and **insufficient as an acceptance test** —
   a later run of Step 0 must include the route, in the workspace, not the probe.
2. **Adding the deps to the workspace "succeeded"**: `commons-api` built with 0
   errors and `Cargo.lock` showed a single axum 0.7.9. That was 7.2.1 resolving
   to axum 0.8 *transitively*, and the failure only appeared when real code used
   the extractor. **`cargo build` on a crate that does not use the integration is
   not a test of the integration.**

### The third branch: hand-rolled, and what it costs

`async-graphql` is removed again. What replaces it is a resolver of about 150
lines, in `commons-api`, over `serde_json`:

- `POST /graphql` takes `{ query, variables }` and returns
  `{ data, errors }` — **the shape the client's `query()` already parses**, so
  the client does not change by a line.
- Dispatch is on the **operation name** (`Objects`, `BulkTags`, `BulkApplyTag`,
  `CreateAllMissing`), which is present in every document the UI sends and is
  available without a parser. A full GraphQL parser is out of scope and is not
  needed for four known documents.
- A document that does not match a known operation returns a GraphQL-shaped
  `errors` array, not a 500.
- **Depth and complexity limits are then ours to implement**, so they are
  explicit and testable rather than a library default nobody chose.

**What this gives up, stated plainly:** no schema introspection, so no generated
client and no GraphiQL; no input validation from a type system, so
`BulkTarget`'s "neither `ids` nor `query`" case is a runtime check; and aliases
(`alias: field`) will not work unless explicitly handled. **The client sends
none of these**, which is why the ticket is viable at this size — and why a
fifth operation, or a consumer that wants introspection, means revisiting this
decision rather than extending the hand-rolled resolver.

**The honest alternative, if the owner prefers it: bump axum to 0.8.** That is
a workspace-wide change — 22 routes, every handler's extractors, `tower-http`,
`tower` — and it is *not* this ticket's decision to make unilaterally. It is
recorded here as the option this ticket chose not to take, and why: one
GraphQL surface is not worth a framework migration across the whole server.

## Step 1 — the baseline, and why a worktree is the wrong tool here

**This step was written to use a git worktree with its own `CARGO_TARGET_DIR`
under `/tmp`, and running it filled `/tmp` and produced a green that was not a
test result at all.** Record what happened, because the plan's own advice was
the bug:

- `/tmp` on this host is a **16G tmpfs**, and a full second `target/` for this
  workspace is **5.7G** on its own. One baseline worktree took the filesystem
  to 100%.
- The run then failed with `No space left on device (os error 28)` during
  linking, `exit=101`, and **zero `test result` lines** — so the counting
  pipeline printed `passed= failed= ignored= suites=` and a `grep` for `FAILED`
  returned **0**. Both halves of the gate said "fine" on a run that executed no
  tests at all.

**A gate that reports success on a run that produced no results is the
dangerous shape**, and it is the same one as the `1884/0/1/106`-with-2-FAILED
run: the number is what a careful session checks, and an empty aggregate looks
like a suspiciously clean zero rather than like a failure.

### Do this instead

**`/home/alvaro/.cargo-target/commons` is already built for this workspace, and
it is 86G on a real disk with 565G free.** A second target directory is a
second full build of ~40 crates for no information the shared one does not give.

So: **measure the baseline by diff, and reuse the shared target.**

```sh
cd ~/code-local/rust/commons
# What did the last commit actually touch? If no .rs, the test counts are
# unchanged BY CONSTRUCTION and no baseline run is needed at all.
git diff --name-only <previous-tag-commit> HEAD | grep -c '\.rs$'
```

At `285f7f0` that is how the numbers were established:

| | |
|---|---|
| `a238cca` (T-P6-007 closed, tagged) | **1887 passed / 0 failed / 1 ignored, 107 suites** |
| `09176b8` vs `a238cca` | 2 files, both `.md`, **0 `.rs`** → counts identical by construction |
| `285f7f0` (this commit) | **1892 / 0 / 1 / 108** — +5 tests, +1 suite, exactly `object_read_invariant.rs` |

**If a commit touches no `.rs`, do not spend 20 minutes and 5.7G re-measuring
what cannot have changed.** A diff of file paths is a stronger instrument than a
re-run when the question is "did the test count move", because it answers the
actual question rather than a proxy for it.

**When a baseline run genuinely is needed** (the commit touches `.rs`), use the
shared target dir and the existing worktree, and clean up the worktree in the
same breath:

```sh
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
git worktree add /tmp/commons-baseline <commit>
# ... run, count, grep FAILED ...
cd ~/code-local/rust/commons && git worktree remove /tmp/commons-baseline --force
```

**Never point `CARGO_TARGET_DIR` at `/tmp` on this host.** Check `df -h /tmp`
first if in doubt; it is tmpfs, so a full build is a full *RAM* cost, and the
failure mode is a linker error that reads nothing like a disk problem.

## Step 2 — `commons-api`: the schema types and the dispatcher

`commons-api` currently holds `claim.rs` and a lib.rs that says it holds "HTTP
and GraphQL surfaces" and holds neither. It becomes the GraphQL crate. That is
the right home: the schema is the API's description, and the server depends on
the crate, not the reverse.

**There are no derive macros here** — Step 0 removed `async-graphql`, so
`SimpleObject`, `Object` and `InputObject` do not exist. Every "type" below is a
plain `serde` struct plus a hand-written field-name mapping, because the wire
names are camelCase (`coverPath`, `hasNextPage`, `skippedInvisible`) and serde
does not guess them.

### 2a. `Cargo.toml` — no GraphQL library

```toml
serde = { workspace = true }
serde_json = { workspace = true }
commons-core = { workspace = true }
commons-store = { workspace = true }
```

### 2b. `src/schema.rs` — the input types, as plain serde

```rust
//! The GraphQL input types, which are the client's, not the store's.

use serde::Deserialize;

/// A bulk-edit target.
///
/// `kind` is required and named rather than inferred from which field is set,
/// because `ids: []` and "no ids given" are different things. `ids: []` is the
/// dangerous one: it is a request to tag nothing, and a server that treated it
/// as an absent filter would tag the whole library. The client already models
/// it this way (`ui/src/lib/api/client.ts`), and the test
/// `an_empty_id_list_tags_nothing_rather_than_everything` is why.
#[derive(Deserialize, Clone, Debug)]
pub struct BulkTarget {
    /// The wire value is `IDS` or `QUERY`, spelled exactly as the client spells
    /// it. `rename_all = "SCREAMING_SNAKE_CASE"` is the one guessed convention
    /// allowed here, and Step 4's round-trip test is what makes it true rather
    /// than probable.
    pub kind: BulkTargetKind,
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub excluded: Option<Vec<String>>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BulkTargetKind { Ids, Query }
```

**The round-trip test is no longer optional bookkeeping.** With no derive macro
generating this mapping, the mapping is code somebody wrote by hand, and
`a_bulk_target_round_trips_the_client_exact_shape` is the only thing standing
between a silent rename and a client that cannot tag anything. A library would
have generated it; a hand-rolled resolver means a person has to, and the test
has to prove they did.

### 2c. `src/schema.rs` — the output types

These mirror the store's structs field for field; the mapping is in 3b. **Every
field name below is written out in camelCase on the wire**, because serde will
not derive it from a snake_case field and a wrong name here is a `null` the UI
renders as "no cover" rather than an error.

```rust
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ObjectNode {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub date: Option<String>,
    pub rating: Option<f64>,
    pub organized: Option<String>,
    pub cover_path: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_ms: Option<i64>,
    pub producer: Option<String>,
    pub performers: Vec<String>,
    pub tags: Vec<String>,
    pub folder: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>,
    pub end_cursor: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ObjectConnection {
    pub nodes: Vec<ObjectNode>,
    pub page_info: PageInfo,
    /// Always `null`. See the spec §3 and the doc comment on the resolver.
    /// `Option<i64>` with no `skip_serializing_if`, because the client asks for
    /// the field and expects it present-but-null; omitting the key entirely is
    /// a different document and this is the mistake a null-as-absent shortcut
    /// invites.
    pub total_count: Option<i64>,
}
```

`rename_all = "camelCase"` on all three is not a style choice: it is the single
place the four wire names (`coverPath`, `hasNextPage`, `pageInfo`,
`totalCount`) are decided, and it must be visible in one diff rather than
scattered across per-field attributes.

### 2d. `src/schema.rs` — the remaining output types

```rust
#[derive(Serialize, Clone, Debug)]
pub struct TagRef { pub id: String, pub name: String }

#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BulkApplyTagResult {
    pub applied: usize,
    pub skipped_invisible: usize,
    pub requested: usize,
}

#[derive(Serialize, Clone, Copy, Debug)]
pub struct CreateAllMissingResult {
    pub created: usize,
    pub existing: usize,
    pub refused: usize,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PageInput {
    pub first: usize,
    /// Opaque. Never parsed by the client, never constructed by hand.
    #[serde(default)]
    pub after: Option<String>,
    /// §5.16: the serialized filter, as a JSON string.
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    /// §14.1. Absent means "whatever the caller may see".
    #[serde(default)]
    pub tiers: Option<Vec<String>>,
}
```

**There is deliberately no `offset` field.** The client's `PageInput` has none
either, and the pair of those is a type-level guarantee that "skip to page 400"
is inexpressible. Adding one "for symmetry" would restore exactly the problem
`commons-store/src/query.rs` refuses to solve.

### 2e. `src/dispatch.rs` — the request and response envelope

The client's `query()` already parses `{ data, errors }`, so this is the only
wire type that is not a field mapping:

```rust
#[derive(Deserialize)]
pub struct GqlRequest {
    pub query: String,
    #[serde(default)]
    pub variables: serde_json::Value,
    #[serde(default)]
    pub operation_name: Option<String>,
}

#[derive(Serialize)]
pub struct GqlResponse {
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<GqlError>>,
}

#[derive(Serialize)]
pub struct GqlError {
    pub message: String,
}
```

**The operation name is the dispatch key, and reading it is the whole parser
trick.** Every document the UI sends names its operation
(`query Objects(...)`, `mutation BulkApplyTag(...)`), so extracting the name is
a regular expression over the query string — no GraphQL parser, no document AST,
and no way for a field to be silently dropped for want of a selection-set
implementation.

**A failure is an `errors` array with HTTP 200, not a 500** wherever the client
could plausibly show it. This is what the client expects: `query()` reads
`errors` and surfaces the message, and a 500 would lose the message behind a
transport-level failure.

## Step 3 — DONE (`14ccfa5`): `commons-server`: the resolvers, the gate, and the limits

### 3a. Identity, and why there is no `caller()` helper to fall back on

The resolvers need the `CallerId` **from the request**, not from a field. The
mechanism already exists: `identity::caller_from_request(&state, &headers)`, and
it is `async`. So each resolver takes the state and the header map from the
`Request` axum already handed the handler, and resolves the caller exactly the
way the REST routes do:

```rust
async fn caller(state: &AppState, headers: &HeaderMap) -> identity::CallerId {
    identity::caller_from_request(state, headers).await
}
```

**There is deliberately no `local_caller()` fallback anywhere in this file, and
no helper that takes an `Option<HeaderMap>`.** T-P6-007's whole substance was
that a route which cannot identify its caller must fail rather than assume the
owner; a GraphQL resolver that falls back to the owner is the same bug wearing
a query's clothes, and it would be invisible in review because there is no
`#[route]` attribute to look at. `a_graphql_resolver_without_state_fails_rather_than_defaulting_to_the_owner` is the test that keeps this honest.

### 3b. `objects` — the one resolver that matters

```rust
async fn objects(
    state: &AppState,
    headers: &HeaderMap,
    input: PageInput,
) -> GqlResult<ObjectConnection> {
    let caller = caller(state, headers).await;
    let filter = decode_filter(input.filter.as_deref())?;
    let sort = decode_sort(input.sort.as_deref(), input.direction.as_deref())?;
    let cursor = decode_cursor(input.after.as_deref(), &sort)?;
    let page = state
        .store()
        .query_sorted(&filter, &caller, sort, cursor, input.first)
        .await?;
    // total_count is None, and always None. See spec §3.
    Ok(ObjectConnection { nodes: page.nodes.into_iter().map(Into::into).collect(), /* ... */ })
}
```

`decode_*` are the three places where a **client-supplied string becomes a
trusted value**, and each has a test:

| Function | Rejects | Test |
|---|---|---|
| `decode_filter` | malformed JSON, a filter naming a column it may not | `a_malformed_filter_is_an_error_not_an_empty_page` |
| `decode_sort` | a sort key that is not in `Sort`'s enum | `an_unknown_sort_key_is_an_error_not_a_silent_default` |
| `decode_cursor` | a cursor of the wrong arity, or unparseable | `a_cursor_from_a_different_sort_is_rejected` |

**Each returns `Err` rather than a default.** A resolver that falls back to
"date descending, first page" on a bad cursor is a resolver that answers a
different question than was asked, and the UI shows the user's library rather
than an error.

### 3c. `tags`, `bulkApplyTag`, `createAllMissing`

Straightforward mappings onto `Store::all_tags`, `Store::bulk_apply_tag` and
`create_all_missing` — all three exist, all three take a `&CallerId`, and all
three return structs whose fields match the client's expectations exactly.

**`bulk_apply_tag` must pass the resolved `caller` through**, and the
`skippedInvisible` counter it returns is the reason that matters: a caller who
names 40 objects and sees `applied: 3, skippedInvisible: 37` is being told the
truth, where an error would be a lie about permissions and a success count
alone would be a lie about the request.

`create_all_missing` is a **mutation that creates rows**, so it is also the
first GraphQL write path. It goes through the same consent gate as the REST
equivalent, and the test is `create_all_missing_cannot_write_past_the_caller_s_tiers`.

### 3d. Route and limits

```rust
// in router(), alongside the other routes:
.route("/graphql", post(graphql_handler))
```

**`/graphql` is deliberately NOT under `/api/v1`.** It is not a versioned
public script surface — it is the UI's transport, and the OpenAPI document
describes `/api/v1`. Putting it in `v1_routes` would put a POST endpoint into
the document that carries the compatibility promise. `the_graphql_endpoint_is_not_under_api_v1` asserts it.

**The depth and complexity limits are now ours, and that is a real
responsibility rather than a library default.** With no `async-graphql`, nothing
enforces them unless this file does. The minimum that has to exist:

```rust
// A document deeper than this, or wider than this, is refused before any
// resolver runs. Both are counted over the raw query text.
const MAX_DEPTH: usize = 12;
const MAX_BYTES: usize = 64 * 1024;

fn check_limits(query: &str) -> GqlResult<()> {
    if query.len() > MAX_BYTES {
        return Err(...);
    }
    if brace_depth(query) > MAX_DEPTH { return Err(...); }
    Ok(())
}
```

`brace_depth` counts `{`/`}` nesting in the query string. It is a crude measure
of selection depth and that is acceptable **because the client sends four known
documents** — the limit is a bound on a closed set, not an attempt to score
arbitrary queries. `a_deeply_nested_query_is_rejected` and
`an_oversized_query_is_rejected` prove each. The numbers are starting points; if
a test shows the UI's own `OBJECTS_QUERY` tripping one, raise the limit rather
than narrowing the document, and say so in the commit.

## Step 4 — DONE (`2235d39`): the tests, and what each one is for

New file `crates/commons-server/tests/graphql_route.rs`, using the existing
`TestApp` harness (it lives in `tests/support/mod.rs` and a `src/` test cannot
reach it — same split as T-P6-007's `v1_prefix.rs`).

| Test | Proves |
|---|---|
| `all_four_operations_resolve_against_a_live_server` | the whole surface, one test, so a regression names the operation |
| `objects_is_consent_filtered_for_three_different_callers` | **the criterion that matters** — the same document, three callers, three answers |
| `totalCount_is_null_rather_than_a_count_star` | spec §3's decision, pinned |
| `no_resolver_emits_offset` | reads the generated SQL; keyset only |
| `a_malformed_filter_is_an_error_not_an_empty_page` | `decode_filter` fails loudly |
| `an_unknown_sort_key_is_an_error_not_a_silent_default` | `decode_sort` fails loudly |
| `a_cursor_from_a_different_sort_is_rejected` | `decode_cursor` fails loudly |
| `a_deeply_nested_query_is_rejected` | the depth limit |
| `an_expensive_query_is_rejected` | the complexity limit |
| `a_bulk_target_round_trips_the_client_exact_shape` | `IDS`/`QUERY` serialise as the client sends them |
| `an_empty_id_list_tags_nothing_rather_than_everything` | the `ids: []` hazard from the client's own comment |
| `a_graphql_resolver_without_state_fails_rather_than_defaulting_to_the_owner` | no `local_caller()` fallback |
| `the_graphql_endpoint_is_not_under_api_v1` | §3d's scope decision |
| `create_all_missing_cannot_write_past_the_callers_tiers` | the first GraphQL write is gated |

**The three-caller test is the one to write first and to read most carefully.**
It is the GraphQL analogue of T-P6-007's identity test, and its failure mode is
the same: a resolver that ignores the caller returns the owner's library to
everyone, and a test that only checks "a query returns something" would pass.

Use `ViewDownload` × `unverified` for the positive case — the same combination
T-P6-007's handoff records as the one that works, because a fixture of only
publicly-visible tiers cannot test a permission.

### Step 3/4 amendments — two things the plan assumed that were not true

**1. `after:` was assumed decodable. It is not, and cannot be without a store
change.** The plan's step 3 lists "`decode_cursor` — a cursor of the wrong arity,
or unparseable" as though decoding were a small function. Reading the store
instead of planning found: `Cursor`'s only constructor is `#[cfg(test)]
pub(crate)`, `Cursor::from_row` is `pub(crate)`, and **nothing in the workspace
serializes a `Cursor` at all** — the filter has `to_url`/`from_url`, the cursor
has neither, because no surface has ever needed one. Keyset paging has never
crossed a process boundary. So a non-null `after` is an **explicit error** and
`startCursor`/`endCursor` come back `null`; the encoding is its own ticket,
because a cursor's arity is a function of the sort (`Sort::new` appends `id` as
the trailing tiebreak), so an encoding that does not bind the cursor to its sort
reintroduces at the wire the "wrong arity, silently wrong page" failure the type
was built to prevent. Spec §4b has the full reasoning. The first page — what the
UI shows on load — works.

**2. Two client fields are refused rather than ignored, and the plan did not
name either.** `BulkTarget.excluded` (the store's `Target` has no exclusion
list, and ignoring a field that looks like a filter is how a bulk edit touches
something the user protected) and `PageInput.tiers` (a narrowing hint the consent
clause already implies; applying a client-supplied tier list on top would be a
*widening* dressed as a filter). Both are errors with messages naming the
field. `decode_filter`/`decode_sort` also **error rather than default**, because
the default for a malformed filter is "everything" and that is a privacy bug
wearing a parse error's clothes.

**3. The consent test was itself wrong on its first run, and that is the most
useful thing this step produced.** It used a headerless request as the
"anonymous" caller. A headerless request **is** the local owner
(`identity.rs`: the fallback is "the absence of a design, named so that it
shows up in a diff when credentials arrive"), so the owner and the "anonymous"
caller were the same caller and the assertion was vacuous — the leak it was
meant to catch would have passed. Anonymous means a well-formed bearer token
naming no grant. The fix puts `assert_ne!` on the two id lists *before* the
substantive assertions, so a caller that stops being consulted is reported as
"the identity layer is not being consulted at all" rather than blamed on the
consent clause. And the test is **mutation-proven**: replacing the
passed-through caller with `crate::media::local_caller()` — the exact defect the
module docs warn about — kills it at that `assert_ne!`. (The first mutation
attempt did not compile; a mutation that fails to build proves nothing.)

## Step 5 — ANSWERED: there is no REST gap. Spec §2's measurement was wrong.

Spec §2 recorded that "`/api/bulk/tag` and `/api/thumbs` are called by
`client.ts` and are absent from the router's 22 routes", and step 5 was written
to build them. **Both are false, and the way they are false is the finding.**

Re-measured at `2235d39`:

- `/api/bulk/tag` appears in `client.ts` **exactly once, inside a comment** —
  the block explaining why the bulk-tag mutation is GraphQL rather than REST
  ("a route that grew its own `fetch('/api/bulk/tag')` passed every functional
  test and failed that one — correctly"). Zero code references.
- `/api/thumbs` appears **exactly once, inside a comment** — the share-links
  block explaining why those verbs live in `client.ts` at all, naming
  `fetch('/api/thumbs')` as the hypothetical two-line change the invariant
  `tests/invariants.test.ts` exists to prevent. Zero code references.
- Neither path appears anywhere in `crates/commons-server/src/` at all.

**So there is nothing to build, and the reason is that the removal already
happened deliberately.** `/api/bulk/tag` was superseded by the GraphQL mutation
this ticket just implemented; `/api/thumbs` was never added, by design, because
a second HTTP path in the UI is a second place for a base URL, a header, an auth
token and an error shape to disagree.

**How the original measurement went wrong, and it is worth writing down.**
Grepping a file for a path finds the path in its prose. Both strings survive in
`client.ts` as the *reasons* the code is the way it is, and a grep cannot tell
a call site from a citation. A measurement that says "the UI calls a 404" is a
claim that should be cheap to disprove, and the cheapest disproof is the one
that was skipped: count the mentions that are not inside a comment. A route
built on the strength of that measurement would have been a **second** HTTP
surface for a feature that deliberately has one, and it would have looked
correct.

## Step 6 — DONE (`2eb3fff`): the two missing routes' tests must not be a mock's job

`client.ts` has a `setTransport` injector, and its default transport is a real
`fetch`. **After this ticket, at least one test must exercise the real client
against the real server** — that is the difference between "the schema compiles"
and "the UI works". If the UI test harness cannot reach a live server cheaply,
that is a finding to record, not a reason to fall back to the mock.

## Step 6 amendment — what the live test found, and what it had to be careful about

`ui/tests/graphql-live.test.ts` spawns the real `commons-server` binary on a
real port and points the real `query()` at it. 7 tests, all passing.

**The mutation is what makes it a test rather than a demonstration.** Removing
`#[serde(rename_all = "camelCase")]` from `ObjectConnection` — one attribute —
kills exactly the three tests that read the connection and leaves the four that
do not touch it green. That discrimination is the evidence: the suite is
measuring the wire, not re-asserting the client's own TypeScript types back at
itself.

Three things it had to get right, each of which would have made the test pass
for the wrong reason or fail for a confusing one:

1. **It imports `OBJECTS_QUERY`, `fetchObjects` and `fetchTags`** rather than
   retyping them. A copy is a second statement of the query: someone widens the
   selection set and the copy keeps passing against the old shape — which is
   the exact drift the step exists to catch.
2. **It spawns with `--mode library --data-dir <tempdir>`.** Without `--mode`
   the server picks its own default, which on a developer machine is the *real*
   library directory. A test that reads someone's actual library is a test that
   must never be able to fail silently.
3. **It polls `/healthz` rather than sleeping**, because a fixed sleep is either
   too short (a flake on a busy machine) or too long (a slow suite), and the
   failure mode of the first is a test that fails for no reason and gets
   deleted.

**Two of my own assertions were wrong before they were right**, both the same
mistake: `fetchObjects` returns `{ objects: Connection }` and `fetchTags`
returns `{ tags: [...] }` — the GraphQL data shape with the wrapper included.
Asserting `res.nodes` instead of `res.objects.nodes` would have been a test that
passes by throwing `undefined is not an array`: a failure that reads as a server
bug and is actually a misread of the client's return type.

**It skips loudly when the binary is absent**, printing the path it looked for.
The Postgres parity tests refuse to skip and are right to, because the ticket
*is* the equality; the difference is that this one needs a build artifact, and a
build is not the ticket. A silent skip is the one option both harnesses reject.

**`run-tests.mjs` ignores its filter argument** — `run-tests.mjs graphql-live`
runs all 925 UI tests. Pre-existing, recorded rather than fixed; changing the
runner is not this ticket's business.

## Step 7 — gate, docs, tag, mirror

```sh
cd ~/code-local/rust/commons
# NEVER under /tmp: it is a 16G tmpfs and this workspace's target/ is 5.7G.
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
cargo fmt --all
cargo build --workspace                                    # 0 errors
cargo test --workspace > /tmp/ws1.log 2>&1
```

**Then the gate, and it is a script rather than a pipeline because a pipeline
cannot fail on "nothing ran":**

```sh
#!/bin/sh
# $1 = a log file from `cargo test`
log=$1
suites=$(grep -cE "^test result" "$log")
if [ "$suites" -eq 0 ]; then
  echo "GATE FAIL: no test results in $log -- the run produced nothing."
  echo "The usual cause is a build that died before running (disk, linker)."
  grep -E "^error|No space left|could not compile" "$log" | head -5
  exit 1
fi
grep -E "^test .* FAILED|panicked at" "$log" && { echo "GATE FAIL: FAILED lines"; exit 1; }
grep -E "^test result" "$log" \
  | awk '{p+=$4;f+=$6;i+=$8;s++} END {print "passed="p" failed="f" ignored="i" suites="s}'
echo "GATE PASS: $suites suites, no FAILED lines"
```

**Run it twice** — `for n in 1 2; do cargo test --workspace > /tmp/ws$n.log 2>&1; ./gate.sh /tmp/ws$n.log; done` —
because a flake at 2% per spawn shows up in roughly one run in three and a
second run is the only thing separating "fixed" from "not observed yet".

**The `suites -eq 0` check is the point, and it is here because it was needed
here.** Step 1's baseline run died with `No space left on device` during
linking, printed `exit=101` and **zero** `test result` lines — so the counting
pipeline printed `passed= failed= ignored= suites=` and the `FAILED` grep
returned **0**. Both halves of a two-part gate reported success on a run that
executed no tests. An empty aggregate is not a clean zero; it is a missing
measurement, and a gate that cannot tell them apart will happily certify one.

**Run the suite TWICE and grep for `FAILED` both times, not just the totals.**
One run of this workspace printed a perfect `1884/0/1/106` *and* 2 FAILED lines
— the count reconciled and two tests were broken, which is the most dangerous
shape a gate has, because the count is what a careful session checks first.

Expected: `passed=1887+14=1901 failed=0 ignored=1 suites=108` if every Step 4
test is new. **If the number differs, do not adjust the expectation to match** —
count the files and say which test moved.

Then:

1. `CHANGELOG.md` — a section for the GraphQL surface, stating that
   `/graphql` is the UI transport and is **not** part of the `/api/v1`
   compatibility promise.

   **Done as `Unreleased`, not `1.1.0` as this step specified.** The versioning
   is by path prefix and `1.0.0` is the version of `/api/v1/*`. `/graphql` is
   deliberately not under that prefix — a test asserts `/api/v1/graphql` is a
   404 — so the versioned public API did not change and `1.1.0` would advertise
   a change that did not happen. A version heading is a claim about the API,
   not about the commit, and the commit has a tag.
2. `docs/spec/t-p6-007-public-api.md` §4 — strike the GraphQL bullet with a
   pointer here, so the deferral list stops naming finished work.
3. `docs/HANDOFF.md` and the vault.
4. Tag `phase-7-120-graphql-operations`.
5. Mirror:

```sh
cd ~/code/rust/commons
git fetch origin && git merge --ff-only origin/main
# then verify, do not trust the merge output:
git rev-parse HEAD^{tree}          # must equal the local repo's
git rev-list --count HEAD..origin/main   # 0
```

`git -c diff.external= diff` is required on this host — `git diff` is
configured with `diff.external = difft` and a plain `git diff | grep -c '^+'`
returns **0** for a large diff.

---

## Step 7 result

**Gate, run twice as this step requires** (a flake at 2% per spawn shows up in
roughly one run in three, and a second run is the only thing separating "fixed"
from "not observed yet"):

```
RUN 1 GATE PASS: passed=1919 failed=0 ignored=1 suites=109
RUN 2 GATE PASS: passed=1919 failed=0 ignored=1 suites=109
```

`ignored=1` is a `commons_media` **doc-test** (`subtitles::to_store_cues`), not
a lost test. Worth recording that an earlier count reported `ignored=0`: the
awk sums fields after splitting on spaces and semicolons, which is off by one
on a `0 passed` line, and the error is silent. The log is the authority — a gate
that reports a number its own input contradicts is worse than no gate.

**Two suites only run with `DATABASE_URL` set**, and the harness refuses to skip:

> DATABASE_URL must be set and reachable. §3.5 makes two engines a property of
> the product, so a parity test that skips when the database is absent is a
> parity test that never runs — and the whole ticket is the equality.

That is correct and worth keeping, but it means a bare `cargo test --workspace`
in a shell without the env var **aborts the run partway through** rather than
finishing with 7 failures — which is how the count came to be 1550 once instead
of 1919. The abort is the harness working; the confusing part is that it looks
like a crash.

**Docs updated in this step:** `docs/spec/t-p6-008-graphql.md` (status CLOSED,
§2 corrected, §4b added, F8 added to the follow-up table), `CHANGELOG.md` (an
`Unreleased — T-P6-008` section stating the no-library cost and the three
refused fields), `README.md` (the endpoint, why it is not under `/api/v1`, and
the live test — whose existence is the only reason the UI's wire format is
tested at all).

**F8 is the follow-up this ticket hands on**, and it is the only one that needs
its own ticket rather than a row in a table: a cursor encoding is a new public
constructor on a type built so it cannot be hand-assembled, and it must bind the
cursor to its sort because §5.16 makes the sort attacker-controlled. Getting
that wrong does not produce an error; it produces the wrong page, silently.
