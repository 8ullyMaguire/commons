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
- `GET /api/v1/openapi.json` — generated from the handler types rather than
  hand-written, and covered by a test that asserts the document and the
  router list the same set of paths.
- Request-scoped identity. A request carrying `Authorization: Bearer <token>`
  resolves to the share grant that token names; a request without credentials
  keeps the previous local-owner behaviour. A share token grants an
  **identity and a scope**, never a role.

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
