# T-P6-007 — implementation plan: the public API boundary

**Spec:** `docs/spec/t-p6-007-public-api.md`. **Baseline:** `a16a3a0`
(= 1863 passed / 0 failed / 1 ignored, 104 suites — already measured, so
**there is no baseline worktree to run**; that is the first time this ticket
has started from a clean gate).

Read the spec's §1 table before starting. The one-line summary: **§11.5 names
five features; four are absent and the fifth exists undocumented. This ticket
builds the boundary — `/api/v1`, an OpenAPI document, a changelog, and a server
that actually consults the identity model it already depends on.**

---

## Step 0 — preconditions, and one command that must be true first

```sh
cd ~/code-local/rust/commons
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
git log --oneline -1        # must be a16a3a0 or a descendant
git status --short          # must be empty
```

**Then the dependency-truth check, before writing any code:**

```sh
grep -rnE '\basync_graphql::' --include=*.rs crates/ | wc -l    # expect 0
grep -rn 'async-graphql' crates/*/Cargo.toml                    # expect 1 file
```

If the first is still 0, the dependency is declared and unused — exactly
T-P6-006's `semver`/`uuid`, one size up. **Remove it in this ticket, do not
leave it as a promise.** Record the grep output in the commit; a claim
"it might be used" is not a finding.

## Step 1 — `docs/` first, because two of the acceptance criteria are docs

**1a. `CHANGELOG.md` at the repo root** (it does not exist — verified).

Shape:

```markdown
# Changelog

All notable changes to the commons HTTP API.

## 1.0.0 — T-P6-007

The first versioned public API. See `docs/spec/t-p6-007-public-api.md` §6.

### Added
- `/api/v1/*` — the versioned public surface, additive within `v1`.
- `GET /api/v1/openapi.json` — generated from the handler types.

### Compatibility
Within `v1`: fields may be added, types widened to nullable, routes added.
Routes may not be removed, types not incompatibly retyped, and requiredness not
added. Those three changes require `/api/v2`.

### Not versioned
The DLNA routes (`/dlna/*`) and the media proxy routes are protocol surfaces
for third-party software that discovered them over SSDP. They keep their
existing paths, carry no version, and are deliberately absent from the OpenAPI
document.
```

**1b. Append §6 to the spec** if it is not already there — the compatibility
promise in prose. (The spec as written at `a16a3a0` carries it; if this plan
is being executed against a spec that does not, add it before the code.)

**Verify:** `test -f CHANGELOG.md && grep -c '^## 1.0.0' CHANGELOG.md` → `1`.

## Step 2 — `caller_from_request`: the bridge from a request to a `CallerId`

New file **`crates/commons-server/src/identity.rs`**. This is the heart of the
ticket, and the reason is in the spec's §3 table: `Role` appears **zero** times
in `commons-server/` today. Everything below the server is built; nothing above
it asks.

```rust
//! Resolving a request to an identity.
//!
//! # Why this file exists
//!
//! `Role`, `CallerId`, `filter_ast` and `ShareGrant` are all complete and all
//! used — 163 references to `Role` alone. What was missing is the one call
//! site: `media::local_caller()` returns a hardcoded constant, and every route
//! in `commons-server` calls *it*. So the authorization model was fully
//! specified and never consulted, and no amount of reading the store would have
//! revealed that; only `grep -c Role commons-server/` does.
//!
//! # The fallback is deliberate
//!
//! `local_caller()` remains the no-credentials answer so the 22 existing route
//! tests keep passing unchanged. It is NOT the design — it is the absence of
//! one, named so that it shows up in a diff when credentials arrive.
```

Expose exactly one public function:

```rust
pub async fn caller_from_request(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    uri: &Uri,
) -> CallerId
```

Resolution order, and the order **is** the policy:

1. **`Authorization: Bearer <token>`** → resolve as a share grant. Reuse
   `share::load_and_resolve`; it already does the whole policy in one call
   (signature, revocation, expiry, password) and logs every attempt. If it
   grants, the caller gets `Role::Public` with `account_id: Some(grant.id)` —
   **not** a higher role. A share link is a capability grant, not an account.
   Assigning a share a role would make a `View` link able to do whatever that
   role can do, which is the entire reason `Scope` has two variants.
2. **No credentials** → `local_caller()`, unchanged.

**The one decision to get right:** a share token must NOT produce a role with
permissions. It produces an identity whose *scope* is enforced by
`Scope::can_download()` — note the name is **not** `permits_serving` or any
guess: the real `Scope` API is `can_download()`, `as_str()` and
`from_str_opt()`, and the serving paths call it by that name. If a share token
ever grants `Role::Contributor`, something is wrong.

Then a `#[cfg(test)] mod tests` in the same file with **three** cases, which
is spec acceptance 4:

- `a_bearer_token_resolves_to_that_grant_and_no_higher`
- `no_credentials_falls_back_to_the_local_owner`
- `a_malformed_bearer_is_anonymous_rather_than_an_error`

The third matters: an invalid token must not 500, and must not be treated as
"no credentials but trusted". It is anonymous.

## Step 3 — the `/api/v1` prefix, mounted *alongside* not *instead of*

In `crates/commons-server/src/lib.rs`, where the router is built.

**Mount the same handler twice.** The prefix is a second path to the same
function, not a move. Rationale, and it is the load-bearing decision: the UI
calls the unversioned paths, the DLNA/proxy routes must keep theirs, and
"breaking the UI" is not a consequence this ticket is allowed to have.

Do **not** try to enumerate and re-declare all 22 routes. Build a sub-router
from the existing one where axum permits it, or mount the identical builder
function under both prefixes. Verify by grep afterwards that the route count
roughly doubles, and that the *handlers* are shared rather than reimplemented.

**Exclude by name, with a comment saying why** — the DLNA and proxy paths must
appear at their current paths only:

```rust
// The DLNA and proxy routes are discovered over SSDP by third-party
// software. They keep their current paths, carry no version, and are
// deliberately absent from /api/v1 — see spec §6.
```

**Verify** — this is the acceptance-1 test and it is two-sided on purpose,
because the failure mode is a 404 on the old path:

```
new test: an_unversioned_path_and_its_v1_path_serve_the_same_response
new test: a_dlna_route_is_not_reachable_under_the_v1_prefix
```

Run each new fixture **twice** (the standing rule — a dedicated instance per
fixture, never a shared row).

## Step 4 — the OpenAPI document, generated not hand-written

Add to the workspace `Cargo.toml` — **both crates, and the versions below are
resolved, not guessed:**

```toml
utoipa = { version = "5", features = ["axum_extras"] }
utoipa-axum = "0.1"
```

`axum_extras` on its own does **not** give router integration; it gives
`IntoParams`. The router half is the separate `utoipa-axum` crate, and a plan
that names only `utoipa` sends the implementer looking for a missing type.

**Proven combination** (built in a scratch crate, not inferred):
`utoipa 5.5.0` + `utoipa-axum 0.1.3` + `axum 0.7.9` (this workspace's axum),
compile clean. Note utoipa 5's *docs* target axum 0.8; the 0.1.3 axum adapter
works against 0.7.9, and the first attempt at this failed to compile only
because `OpenApiRouter` is **not** at the crate root in 0.1.3.

The real 0.1.3 incantation, which compiles:

```rust
use utoipa_axum::{router::OpenApiRouter, routes};   // note: `router::`

fn router() -> (axum::Router, utoipa::openapi::OpenApi) {
    OpenApiRouter::new()
        .routes(routes!(get_ping, post_ping))   // handlers, NOT method routers
        .split_for_parts()
}

#[utoipa::path(get, path = "/api/v1/ping", responses((status = 200, body = String)))]
async fn get_ping() -> &'static str { "ok" }
```

Two API facts that differ from the obvious guess, and both cost a compile to
learn: `OpenApiRouter` lives under `router::`, and `routes!` takes **handler
functions** rather than `get /path` method routers.

Derive `ToSchema` on the response types in `commons-server` and build the
document from the **router**, so the two cannot drift. Serve it at
`GET /api/v1/openapi.json`.

**Step 3 and this step interact, and the order matters.** `OpenApiRouter`
*replaces* the router rather than wrapping the existing one, so the `/api/v1`
sub-router from step 3 must be built as an `OpenApiRouter` from the start
rather than mounted after the fact. If step 3 has already mounted a plain
`axum::Router`, this step has to rebuild it — so **do step 4's router
construction first if you prefer, or convert in one commit rather than
rewiring twice.** The `#[utoipa::path]` attributes are additive and do not
change handler behaviour, so the 22 existing route tests are unaffected either
way.

**The test that makes this worth doing** is spec acceptance 2, and it is a
drift detector, not a smoke test:

```
new test: the_openapi_document_lists_every_v1_route_in_the_router
```

Extract the paths from the built router, extract them from the generated
document, and assert set equality. **A hand-written OpenAPI file passes every
test that checks it is valid JSON and fails the moment a route is added** —
that is the whole failure this test exists to prevent.

## Step 5 — wire `caller_from_request` into the media route

One call site, and it must be the only change to `media.rs`'s behaviour:

```rust
// was: let caller = local_caller();
let caller = identity::caller_from_request(&state, &headers, &uri).await;
```

The 22 existing route tests send no credentials, so they take the
`local_caller()` fallback and must pass **unchanged**. That is the safety
property of this design and the reason to build it this way round.

**Verify:** `cargo test -p commons-server` → the same count as before, no
test edited. **If any test needed editing, stop and find out why** — an
existing test that needs changing to accommodate a new auth path means the
fallback is not equivalent, and that is a real bug, not a chore.

## Step 6 — gate, mirror, tag

```sh
cargo build --workspace                 # 0 errors
cargo test --workspace --no-fail-fast -- --test-threads=1
cargo clippy --workspace --all-targets  # 0 warnings
cargo fmt --all -- --check              # clean
```

Then: update `docs/HANDOFF.md` with a T-P6-007 section, update this plan's
Progress in `docs/plans/implementation-plan.md`, commit, tag
`phase-7-090-public-api-boundary`, and mirror to `~/code/rust/commons` with
`git fetch origin --tags && git merge --ff-only origin/main`, then verify both
trees report the same commit and are clean.

## Definition of done

- [ ] Step 0's grep recorded in a commit; `async-graphql` used or removed.
- [ ] `CHANGELOG.md` exists with a `1.0.0` entry.
- [ ] `identity.rs` exists with three tests, and `grep -c Role
      crates/commons-server/src/` is **non-zero** afterwards.
- [ ] Every non-DLNA route reachable at both its old and `/api/v1` path, proven
      by a two-sided test.
- [ ] `/api/v1/openapi.json` serves, and the drift test passes.
- [ ] The 22 existing route tests pass **unedited**.
- [ ] Full workspace gate green; tag created; both trees clean and equal.

## Explicitly not in this ticket

GraphQL server (the UI's 11 files are proved against a mock — converting them
touches 11 test suites), Jellyfin (#2747, has an external contract), upload
endpoints (#4995/#1125/#13, need §14.1 attestation and a multipart limit
policy), the client SDK (a build artifact once the document exists). Each is
specced in `docs/spec/t-p6-007-public-api.md` §4 with its boundary named.
