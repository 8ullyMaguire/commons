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

## 1b. The field mapping is the ticket, and it is mostly `null`

The previous version of this section claimed the client's `ObjectRow` "mirrors
the store's structs field for field". **It does not, and the gap is the real
work.** Measured at `09176b8`:

| the client selects | the store can supply | how |
|---|---|---|
| `id`, `kind`, `title` | yes | `ObjectRow` already carries them |
| `date` | yes | `object.date`, **not selected today** |
| `organized` | yes | `object.organized`, not selected today |
| `rating` | **no** | `object.rating_sum` / `rating_count` are a sum and a count; there is no `rating` column, and `rating.stars` is per-account, so a single `rating` is an aggregate that does not exist yet |
| `coverPath` | **no** | no such column anywhere; `custom_field`/`custom_field_value` is the only place it could live, and nothing defines those field names |
| `width`, `height` | **no** | no such columns on `object`, `file` or `segment` |
| `durationMs` | **partial** | `segment.start_ms`/`end_ms` exist; the object-level duration is a sum over segments, which is a second query per row |
| `producer` | **wrong type** | `object.producer_id` is an **id**; the client wants a **name**, which needs a join to `producer.name` |
| `performers` | **no** | `appearance`/`performer` exist as tables; nothing aggregates them per object |
| `tags` | **yes, cheaply** | `object_tag` → `tag.name`; one extra join on the same page query |
| `folder` | **no** | there is no folder column; `file.path` holds it, one file to N objects |
| `totalCount` | **no, by design** | spec §3 |

**So `Objects` cannot be answered from `query_sorted` as it stands.** Five
fields have no source at all, and the two that do have a source need work
(`rating` is an aggregate that must be defined; `producer` is a join; `duration`
is a per-row sum). Returning `null` for a field the client asked for is
*correct GraphQL* and is also what the client's own types permit
(`string | null`, `readonly string[]`) — so the honest first implementation
serves the 3 real fields plus the 2 cheap joins, and returns `null` for the 6
that do not exist yet.

**That is a decision, and it is reversible, and it is the owner's to see.** The
alternative is to widen the ticket into "define the derived object projection"
— `rating`'s aggregate rule, where `coverPath` comes from, whether
`durationMs` is a segment sum — which is a §5.16/§8.1 design question about
metadata, not a transport question. The spec's position: **build the transport,
return `null` where the data layer has no answer, and write down each `null`
as a named follow-up** rather than inventing a definition to fill the shape.
Invention here would be worse than a visible `null`, because an invented rating
rule that disagrees with the UI's would be a data bug wearing a transport's
clothes.

The list of named follow-ups is §7.

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

## 4. What exists underneath, and what does not

The substrate is real, and the earlier version of this section was too
generous about it. Measured at `09176b8`:

| Need | Exists | Where |
|---|---|---|
| keyset paging | yes | `commons-store/src/query.rs` (`query_sorted`) |
| keyset cursors, total order | yes | `commons-store/src/sort.rs` (`Sort`, `Cursor`) |
| consent filtering | yes | `commons-store/src/filter_ast.rs` (`to_sql`, `consent_clause`) |
| request → identity | yes | `commons-server/src/identity.rs` (`caller_from_request`) |
| filter in / cursor out of the URL | yes | `filter_ast::to_url` / `from_url` |
| tag list, bulk tag write, create-missing | yes | `Store::all_tags`, `bulk_apply_tag`, `create_all_missing` |
| **the other 11 object fields the client selects** | **no** | §1b and §5b |

**No query engine is being built.** The GraphQL layer is a translation from
the client's documents to `query_sorted`, plus two cheap joins, plus the consent
gate. A plan that proposes a planner, an N+1 batcher or a dataloader is solving
a problem this repository does not have — and each would be a new place for a
consent bug to hide, which is the one category of bug this codebase cannot
afford another of.

**But `query_sorted` is also a narrower door than it looks.** Its own module
docs make it the *only* sanctioned object read, enforced by a test that scans
for `SELECT ... FROM object` elsewhere. So the two joins §1b wants
(`object_tag` → `tag.name`, and later `producer.name`) cannot be added as a
separate query the resolver runs — that would be a second object read, and the
invariant test would correctly fail. **They must extend the single SELECT
inside `query.rs`,** which makes them a change to the sanctioned read and
therefore a change to the thing §14.1 is about. That is the real integration
cost of this ticket, and it is why `tags` is in scope but `producer` is not:
one join, in the one place it is allowed, or none.

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

## 5b. The named follow-ups this ticket creates

Each is a real question about the data layer, deliberately **not** answered
here, and each is a `null` the client will render as absent data until someone
does:

| # | Field | The question that has to be answered first |
|---|---|---|
| F1 | `rating` | is it the mean of `rating.stars`, `rating_sum / rating_count`, or a settled `field_proposal`? The three disagree when ratings are one-sided, and the UI shows one number |
| F2 | `coverPath` | there is no column. `custom_field` is the only candidate and nothing names the field. Is it a path, an artifact id, or a `/media/:id/thumb` URL? |
| F3 | `width`, `height` | no column on `object`, `file` or `segment`. Is this from probe metadata (which means a rescan populates it) or from `custom_field`? |
| F4 | `durationMs` | `segment.start_ms`/`end_ms` exist. Is the object duration `max(end_ms)` over segments, or the sum? A clip with overlapping segments differs by more than rounding |
| F5 | `performers` | `appearance` → `performer` exists; nothing aggregates per object, and a naive join multiplies rows and breaks the keyset page |
| F6 | `folder` | no folder column. `file.path` holds it, and one file backs N objects via `segment`, so which file's path is the folder? |
| F7 | `producer` | needs a join to `producer.name`. Cheap, and the **one** of these that is genuinely just this ticket's work — except that it adds a join to the hot-path page query, which §5.16's own reasoning about the page query's shape should rule on first |

**F7 is deliberately not done in this ticket even though it is the easiest**,
because a join added to the page query is a change to the hot path that
`query.rs` documents at length, and "it was only one join" is how a page query
becomes slow. It belongs with F1–F6 in a single "object projection" decision
rather than arriving as a lone join with no rule behind it.

**None of these blocks the UI.** Every one is `string | null` or `number |
null` or `[]` in the client, and the grid renders absent data as absent. What
they block is *correctness of a number or a path*, which is why they are written
down rather than filled in.

## 6. Acceptance

1. All four operations resolve against a live server, and each is proved by a
   test that starts the real router — not a fake transport.
2. `Objects` is **consent-filtered through `caller_from_request`**, and a test
   proves three different callers get three different answers from the same
   query. This is the criterion that matters: a GraphQL resolver that reads the
   store without the caller is a data leak wearing a query's clothes, and the
   REST routes already gate every read.

   And every one of the client's 14 selected fields is **accounted for** —
   served from a real column, or explicitly `null` with the reason named in §1b.
   A field that is silently absent is a field the client renders as "no cover"
   forever. The test is `every_field_the_client_selects_is_served_or_null_on_purpose`.
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
