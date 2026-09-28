# Changelog

All notable changes to the commons HTTP API.

The API is versioned by path prefix. `/api/v1/*` is the public surface for
scripts and external tools; `docs/spec/t-p6-007-public-api.md` §6 states the
compatibility promise in prose, and this file records what changed.

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
