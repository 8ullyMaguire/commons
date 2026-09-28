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

## Step 1 — baseline, in a worktree, before the first commit

Three tickets in a row shipped a wrong delta because the baseline was measured
after the work started.

```sh
cd ~/code-local/rust/commons
git worktree add /tmp/commons-base-a238cca a238cca
export CARGO_TARGET_DIR=/tmp/commons-base-target
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
cd /tmp/commons-base-a238cca && cargo test --workspace 2>&1 \
  | grep -E "^test result" | awk '{p+=$4;f+=$6;i+=$8;s++} END {print "passed="p" failed="f" ignored="i" suites="s}'
```

**Expected: `passed=1887 failed=0 ignored=1 suites=107`.** Record the number in
the vault before proceeding. Then remove the worktree and its target dir —
together, or the disk fills:

```sh
cd ~/code-local/rust/commons
git worktree remove /tmp/commons-base-a238cca --force
rm -rf /tmp/commons-base-target
```

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

## Step 3 — `commons-server`: the resolvers, the gate, and the limits

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

## Step 4 — the tests, and what each one is for

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

## Step 5 — the REST routes the UI already calls and the server does not serve

Measured in spec §2: `/api/bulk/tag` and `/api/thumbs` are called by
`client.ts` and are absent from the router's 22 routes.

Either build them here with tests, or **split them into their own ticket and
write that down in this file and the vault.** Do not leave them unmentioned —
they are a live bug in the UI today, independent of GraphQL, and finding them
was the point of measuring.

Note that `bulkApplyTag` arriving via GraphQL does **not** fix the REST gap: the
UI calls the REST path from a different code path, and "we have it in GraphQL
now" is not a reason to leave a 404 in place.

## Step 6 — the two missing routes' tests must not be a mock's job

`client.ts` has a `setTransport` injector, and its default transport is a real
`fetch`. **After this ticket, at least one test must exercise the real client
against the real server** — that is the difference between "the schema compiles"
and "the UI works". If the UI test harness cannot reach a live server cheaply,
that is a finding to record, not a reason to fall back to the mock.

## Step 7 — gate, docs, tag, mirror

```sh
cd ~/code-local/rust/commons
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
cargo fmt --all
cargo build --workspace                                   # 0 errors
cargo test --workspace 2>&1 | grep -E "^test .* FAILED|panicked at"   # must print nothing
cargo test --workspace 2>&1 | grep -E "^test result" \
  | awk '{p+=$4;f+=$6;i+=$8;s++} END {print "passed="p" failed="f" ignored="i" suites="s}'
cargo test --workspace 2>&1 | grep -E "^test .* FAILED|panicked at"   # AGAIN: a flake hides here
cargo clippy --workspace --all-targets 2>&1 | grep -cE "^warning: [a-z]"   # 0
cargo fmt --all -- --check                                            # clean
```

**Run the suite TWICE and grep for `FAILED` both times, not just the totals.**
One run of this workspace printed a perfect `1884/0/1/106` *and* 2 FAILED lines
— the count reconciled and two tests were broken, which is the most dangerous
shape a gate has, because the count is what a careful session checks first.

Expected: `passed=1887+14=1901 failed=0 ignored=1 suites=108` if every Step 4
test is new. **If the number differs, do not adjust the expectation to match** —
count the files and say which test moved.

Then:

1. `CHANGELOG.md` — a `1.1.0` section for the GraphQL surface, stating that
   `/graphql` is the UI transport and is **not** part of the `/api/v1`
   compatibility promise.
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
