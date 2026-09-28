# Changelog

All notable changes to the commons HTTP API.

The API is versioned by path prefix. `/api/v1/*` is the public surface for
scripts and external tools; `docs/spec/t-p6-007-public-api.md` §6 states the
compatibility promise in prose, and this file records what changed.

## Unreleased — T-P6-008

> **Not `1.1.0`,** although the plan for this ticket said so. The versioning here
> is by path prefix: `/api/v1/*` is the public surface and `1.0.0` is its
> version. `/graphql` is deliberately **not** under `/api/v1` (there is a test
> asserting `/api/v1/graphql` is a 404), so nothing about the versioned public
> API changed and bumping it would advertise a change that did not happen. A
> heading is a claim about the API, not about the commit.

`POST /graphql`, the UI's transport. Not a public surface: see "Deliberately NOT
under `/api/v1`" below.

### Added

- `POST /graphql` — the four operations the UI actually sends (`Objects`,
  `BulkTags`, `BulkApplyTag`, `CreateAllMissing`), resolved by hand.

  **No GraphQL library.** `async-graphql` 7.x declares axum 0.8 and this
  workspace is on 0.7, so the wire types and the operation-name dispatcher are
  hand-rolled. The cost is real and is stated in `commons-api/src/graphql.rs`:
  there is no selection-set enforcement, so a document requesting a field the
  server does not serve receives `null` rather than a validation error. The
  alternative was 149 compile errors.

- The consent gate is resolved **once**, in the handler, and passed into each
  resolver. `BulkTags` is the one operation that takes no caller, and the reason
  is written down at the call site rather than left to be inferred: `tag` has no
  `consent_record` join, because nothing about a tag is something a share link
  could grant or withhold.

### Changed

- Three client fields are **refused rather than ignored**, each with a message
  naming the field. `BulkTarget.excluded` and `PageInput.tiers` because honouring
  them is impossible without either writing a filter the store does not have or
  quietly widening what a caller sees, and `PageInput.after` because no cursor
  encoding exists anywhere in this workspace. A malformed filter or an unknown
  sort key is likewise an error: the default for a malformed filter is
  "everything", which is a privacy bug wearing a parse error's clothes.

### Deliberately NOT under `/api/v1`

- `/graphql` is the UI's transport, not a versioned script surface. Putting it
  inside `v1_routes()` would add a `POST` endpoint to the OpenAPI document that
  carries the compatibility promise. A test asserts `/api/v1/graphql` is a 404.

### Known gaps

- `after:` paging is not available. Keyset paging has never crossed a process
  boundary here — `Cursor`'s only constructor is `pub(crate)` and nothing
  serializes one — so a non-null `after` is an explicit error and the page
  cursors come back `null`. The first page, which is what the UI shows on load,
  works. Spec §4b.
- `create_all_missing` threads the caller through and **the store ignores it**
  (`let _ = caller;`). Honest for a local library, wrong for a shared one, and
  this is the first GraphQL write path.

## 1.0.0 — T-P6-007

The first versioned public API.

### Added

- `/api/v1/*` — the versioned public surface, additive within `v1`. Every
  route is reachable at both its existing path and its `/api/v1` path; the
  old paths are not removed and do not go away in `v1`.
- `GET /api/v1/openapi.json` — an OpenAPI **3.1.0** document, generated from
  the same `#[utoipa::path]` attributes the handlers carry rather than
  hand-written. Served at the versioned path only: the unversioned
  `/openapi.json` does not exist, because the versioned mount is the public one
  and an unversioned copy would invite consumers onto a path that carries no
  compatibility promise.

  A test asserts the document and the router list the same set of routes, and it
  compares against the router's **own source**, so adding a route does not mean
  editing a list of route names. It has already caught one real gap: a route that
  was served and undocumented.

  Generated with `utoipa` alone. `utoipa-axum` is not used, and cannot be: its
  `routes!` macro builds one method router for every handler and routes it onto
  every path, so any two same-method routes collide — and this API has eleven
  GETs. See `src/openapi.rs` for the reproduction.
- Request-scoped identity. A request carrying `Authorization: Bearer ***
  resolves to the share grant that token names; a request without credentials
  keeps the previous local-owner behaviour. A share token grants an
  **identity and a scope**, never a role.

### Deliberately NOT under `/api/v1`

`/healthz`, `/livez`, `/metrics`, `/dlna/description.xml`, `/dlna/control`,
`/media/:object_id/caps` and `/media/:object_id/proxy.m3u8` answer only at their
existing paths. This is stated here because a route missing from a public API
otherwise reads as an oversight, and a future maintainer would helpfully
"fix" it.

- **The DLNA routes are a protocol surface.** Third-party TV software discovers
  them over SSDP and holds the path. A version prefix on it breaks every device
  on the network and buys no consumer anything.
- **The proxy routes are a media-delivery mechanism** for players that were
  configured with a URL by hand.
- **The health and metrics routes are infrastructure**, not API surface; a
  scraper pointed at a versioned health check is a new thing to break when the
  prefix moves.

The public set is therefore a list that *omits* these rather than the full set
with a filter applied, so the guarantee is mechanical. A route added to the
unversioned router and forgotten in that list is **absent** from the public API
— a 404 a consumer hits at once, which is the safe direction. The inverse, a
public route nobody recorded, cannot be detected after the fact.

### Compatibility

Within `v1`:

| Change | Allowed |
|---|---|
| Add a field | yes |
| Widen a type to nullable or richer | yes |
| Add a route | yes |
| Remove a route | **no — requires `/api/v2`** |
| Retype a field incompatibly | **no — requires `/api/v2`** |
| Add requiredness to an existing field | **no — requires `/api/v2`** |

The last three are the changes that break a consumer, and a consumer that
ignores unknown fields will not break within `v1`. That property is what makes
an OpenAPI-generated client safe to upgrade.

### Not versioned, and deliberately absent from the OpenAPI document

The DLNA routes (`/dlna/description.xml`, `/dlna/control`) and the media proxy
routes (`/media/:object_id`, `/media/:object_id/proxy.m3u8`,
`/media/:object_id/caps`) are **protocol surfaces for third-party software**
that discovered this server over SSDP or was configured with its URL by hand.

They keep their existing paths, carry no version, and are not part of the `v1`
compatibility promise — versioning them would mean moving a path that living
room software has already discovered, and a version prefix there buys no
consumer anything. This is recorded explicitly because a route that is
deliberately missing from a public API needs to say so somewhere, or the next
reader treats the omission as an oversight.

### Removed

- The `async-graphql` and `async-graphql-axum` dependencies, which were
  declared and called from nowhere. §11.5's GraphQL half is **not** built;
  see the spec's §4.
