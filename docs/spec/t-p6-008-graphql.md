# T-P6-008 — §11.5 GraphQL: the four operations the UI actually sends

**Ticket:** T-P6-008. **Phase:** 6. **Spec:** platform spec §11.5 (C68), read
with §3.3 (the shell is a browser), §5.16 (filters in the URL) and §14.1
(consent tiers).

**Predecessor:** T-P6-007 closed the REST boundary and named this as its
deferred GraphQL half. This ticket is that half.

---

## 1. What the ticket says, and what is actually there

§11.5 asks for "GraphQL for UI parity". T-P6-007's spec recorded the cost as
"the UI's 11 files are currently proved against a mock and converting them to a
live server is a change to *every* UI test that touches those endpoints."

**That cost estimate does not survive measurement, and the difference decides
how this ticket is built.** Measured at `a238cca`, from the git index rather
than a filesystem walk (the `counting-a-corpus` lesson — a raw walk over `ui/`
also matches `.svelte-kit/output/`, which is build output and accounts for a
quarter of the apparent hits):

| Question | Measurement |
|---|---|
| tracked `ui/` `.ts`/`.svelte`/`.js` files | **130** |
| of those, naming `gql` or `graphql` | 29 — **21 are `ui/e2e/*`, 12 are `ui/tests/*`** |
| files containing a GraphQL **document** | **1**: `ui/src/lib/api/client.ts` |
| GraphQL **operations** that file declares | **4** |
| direct `fetch(` outside `client.ts` | **0** (asserted by `no-direct-fetch.test.ts`) |

The four operations, which is the entire server-side contract:

| Operation | Kind | Backing store call |
|---|---|---|
| `Objects` | query | `Store::query_sorted` — exists |
| `BulkTags` | query | tag list — need to confirm |
| `BulkApplyTag` | mutation | bulk tag write — need to confirm |
| `CreateAllMissing` | mutation | creates absent sidecars — need to confirm |

**And the "mock" is not a mock.** `client.ts` has a `Transport` type and a
`setTransport` injector, but the *default* transport is a real `fetch` to
`POST /graphql`. Tests inject a fake; the application code has never been
proved against anything but the real request shape. So the work is **not**
"convert 11 mock-proved files" — it is **"make 4 documents resolve, and prove the
existing client against a live server"**, which is a much smaller change with a
much better test story.

**The `e2e/` hits are not GraphQL either.** They contain the string `gql` as
part of Playwright fixture names and route stubs. They must not be counted
toward this ticket's surface, and a plan that says "11 files" will send the next
person looking for 11 files to change.

## 2. The three paths the UI calls over REST, and why GraphQL is not replacing them

`client.ts` also calls `fetch()` directly for these, and they are **not**
candidates for GraphQL — this is a decision the spec must state, because
"GraphQL for UI parity" reads like an instruction to move them:

| UI call | Server route | Verdict |
|---|---|---|
| `/media/:id/caps` | exists | **stays REST** — a streaming manifest, not a query |
| `/media/:id/subtitles` | exists | **stays REST** — ditto |
| `/media/:id/funscripts` | exists | **stays REST** — ditto |
| `/media/:id/playback` | exists | **stays REST** — `PUT` is a state write with `keepalive` |
| `/api/share` | exists | **stays REST** — §7.5 status-code translation, not a query |
| `/api/bulk/tag` | **MISSING** | **stays REST, and the route must be built** |
| `/api/thumbs` | **MISSING** | **stays REST, and the route must be built** |

**Media bytes never go through GraphQL.** T-P6-007's `CHANGELOG.md` and §11.5's
own framing put `/api/v1` as the script surface; a GraphQL layer in front of
byte delivery would mean a second, differently-authenticated path to the same
files, and the DLNA and proxy surfaces already exist for players.

**The two MISSING routes are the real gap this ticket exposes**, and they are
not GraphQL's job either — they are REST routes the UI calls today and the
server does not serve. Building them is two small handlers plus tests, and it is
listed here because discovering it while measuring is cheaper than a UI bug
report later.

## 3. The `totalCount` conflict, and the decision

`OBJECTS_QUERY` selects `totalCount`. `Connection<T>` in the client declares it
`number | null` and documents that as **"Total count when the server can answer
it cheaply. `null` when it cannot."**

The store disagrees and says why in `query.rs`:

> **What is NOT here** — No `totalCount`. A `COUNT(*)` over the same filter is a
> second scan of the same inner join, on the hot path, to produce a number the
> UI can show approximately. §5.16 does not require an exact count, and a keyset
> page cannot produce one honestly anyway — the honest number is "at least this
> many", which is what `has_more` already says.

**Decision: `totalCount` resolves to `null`.** The client's type already allows
it, the store's reasoning is sound, and the alternative — a `COUNT(*)` per page
on the hot path — trades a measured hot path for a number the UI shows
approximately.

This is written down because it looks like a bug from either side: a client
reading it sees a field that is always null, and a server author reading
`OBJECTS_QUERY` sees a field asked for and not returned. `totalCount_is_null_rather_than_a_count_star` is the test that pins the decision, and the GraphQL schema comment repeats it.

**Do not add a `totalCount` argument to "fix" this.** If a count is genuinely
wanted, §5.16's answer is a filter-scoped count endpoint the UI calls
separately and caches, not a field on every page.

## 4. Everything underneath already exists

The ticket is small because Phase 9 and T-P6-007 built its substrate:

| Need | Exists | Where |
|---|---|---|
| keyset paging | yes | `commons-store/src/query.rs` (`query_sorted`) |
| keyset cursors, total order | yes | `commons-store/src/sort.rs` (`Sort`, `Cursor`) |
| consent filtering | yes | `commons-store/src/filter_ast.rs` (`to_sql`, `consent_clause`) |
| request → identity | yes | `commons-server/src/identity.rs` (`caller_from_request`) |
| filter in / cursor out of the URL | yes | `filter_ast::to_url` / `from_url` |

**Nothing here is a new query engine.** The GraphQL layer is a translation
between the client's documents and `query_sorted`, plus the consent gate. A
plan that proposes a query planner, an N+1 batcher, or a dataloader is solving a
problem this repository does not have, and each of those is a new place for a
consent bug to hide — which is the one category of bug this codebase cannot
afford another of.

## 5. Dependency decision — MEASURED: `async-graphql` is not usable, and the answer is a hand-rolled resolver

`async-graphql` and `async-graphql-axum` 7.0 were removed from this workspace
two tickets ago as declared and unused. T-P6-008 is the ticket that would have
used them, so this is where they came back — and where the plan's Step 0 probe
killed them.

**The version pairing was not safe to assume, and utoipa-axum is the precedent
for why**: in T-P6-007, `utoipa-axum` 0.1.3 had a routing defect and 0.3.0
required axum 0.8.4 against this workspace's 0.7.9, so no version worked. The
same question here has the same answer, from both ends:

- Every `async-graphql-axum` from 7.0.17 up declares **axum 0.8**, so its
  `GraphQLRequest` implements axum **0.8**'s `FromRequest` and cannot satisfy
  axum **0.7**'s `Handler` bound. Two axum versions end up in the graph.
- The one 7.x that declares **axum 0.7** — 7.0.0 — does not compile on this
  toolchain at all: **149 errors**, all `E0195` lifetime mismatches inside the
  crate's own `model/directive.rs`.

**Decision: no GraphQL library.** The resolver is hand-rolled over `serde_json`
in `commons-api`, roughly 150 lines, dispatching on the operation name. The
client's `query()` already parses `{ data, errors }`, so it does not change by a
line.

**What this costs, stated rather than glossed:** no introspection, so no
generated client and no GraphiQL; no type-system validation, so `BulkTarget`'s
"neither `ids` nor `query`" case becomes a runtime check; and aliases will not
work. The client sends none of these, which is what makes the ticket viable at
four operations — and what makes a fifth operation, or any consumer wanting
introspection, a reason to revisit this decision rather than extend the
hand-rolled resolver.

**The alternative was not taken:** bumping the workspace to axum 0.8. That is a
framework migration across 22 routes, every extractor, and `tower-http`, and one
GraphQL surface is not worth it. It stays available as a deliberate
owner-level decision, recorded here so the next person does not read this
ticket's outcome as "axum 0.8 was tried and rejected on the merits."

**Two probe results that looked like success and were not**, recorded because
they are the trap this ticket's predecessor fell into:

1. A scratch crate declaring `axum@0.7` *and* `async-graphql-axum@7` builds
   clean at every 7.x version, because two axum copies resolve and nothing uses
   the 0.8 one. **A probe that depends on both versions proves nothing about
   either.**
2. Adding the deps to the workspace built with 0 errors and a single axum
   0.7.9 in the lock — 7.2.1 pulling axum 0.8 transitively. The failure appeared
   only when real code used the extractor. **`cargo build` on a crate that does
   not use the integration is not a test of the integration.**

## 6. Acceptance

1. All four operations resolve against a live server, and each is proved by a
   test that starts the real router — not a fake transport.
2. `Objects` is **consent-filtered through `caller_from_request`**, and a test
   proves three different callers get three different `totalCount`-shaped
   answers from the same query. This is the criterion that matters: a GraphQL
   resolver that reads the store without the caller is a data leak wearing a
   query's clothes, and the REST routes already gate every read.
3. `totalCount` resolves to `null`, with `totalCount_is_null_rather_than_a_count_star`
   asserting it and a comment in the schema saying why.
4. Paging is keyset end to end: no `OFFSET` anywhere in the resolver, asserted
   by a test that reads the generated SQL, and the client's `PageInput` has no
   `offset` field so a skip-to-page-400 is inexpressible.
5. A **depth limit** and a **query-size limit** are enforced, each with a test.
   An unbounded GraphQL endpoint on a server that serves a 100k-object library
   is a denial-of-service surface that did not exist before this ticket.
6. **`async-graphql` stays removed**, and the `declared_dependencies.rs` test
   from T-P6-007 is left asserting that — unchanged, not deleted. Step 0 proved
   the library cannot be used here, so the correct outcome of this ticket is the
   dependency staying out. That test is what will catch a future attempt to
   reintroduce it, and it is the reason this criterion is cheap; deleting it to
   make a grep pass is exactly what it exists to prevent.

   The probe's evidence is recorded in the plan's Step 0 rather than here,
   because a spec that carries a build transcript is a spec that will be
   re-argued instead of re-measured.
7. The two missing REST routes (`/api/bulk/tag`, `/api/thumbs`) exist with tests,
   or this ticket is explicitly split and the omission is written down.
8. `CHANGELOG.md` and `docs/HANDOFF.md` record the new surface, and
   `docs/spec/t-p6-007-public-api.md` §4's GraphQL bullet is struck with a
   pointer here, so the deferral list does not keep naming work that is done.
