# Handoff

Where Commons is, what is verified, and what is deliberately not done.

**As of:** 2026-09-26 · **Branch:** `main` · **Tags:** `phase-1-scan-core`,
`phase-1-content-types`

---

## Where it is

Phases 0 and 1 are complete. Every planned ticket in both phases is
implemented, tested, and committed. Nothing is a stub.

| | State |
|---|---|
| Rust workspace | 394 tests, 0 failures |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| UI unit tests | 43 pass (`node ./tests/run-tests.mjs`) |
| UI browser tests | 11 pass (`pnpm run test:e2e`) |
| UI type check | 0 errors (`svelte-check --threshold error`) |
| UI build | clean, no compiler warnings |

The one thing that is **not** working end to end: `crates/commons-api` is an
empty placeholder crate. There is no GraphQL server, so the UI is verified
against a stubbed network rather than a real backend. The client module and the
query shapes are written against the spec; the server that answers them is
Phase 2 work.

## How to verify

```sh
# Rust
cd crates
CARGO_TARGET_DIR=~/.cargo-target/commons cargo test --workspace
CARGO_TARGET_DIR=~/.cargo-target/commons cargo clippy --workspace --all-targets -- -D warnings

# UI — needs pnpm with the hoisted linker on this host, so the scripts call
# node directly rather than `pnpm build`
cd ui
pnpm install
node node_modules/vite/bin/vite.js build    # build
node ./tests/run-tests.mjs                  # unit
node_modules/.bin/svelte-check --threshold error
node_modules/.bin/playwright test           # browser, against the build
```

`pnpm run verify` in `ui/` runs all four.

## What Phase 1 shipped

| Ticket | Commit | Notes |
|---|---|---|
| T-P1-001 detection | `a501953` | magic bytes, not extensions; GIF vs video by frame count |
| T-P1-002 ffprobe | — | rotation, tags, stream selection, error paths |
| T-P1-003 segments | — | |
| T-P1-004 archives | — | listing and extraction |
| T-P1-005 hashing | — | BLAKE3 + xxh128; oshash deliberately absent (§stash-box #1115) |
| T-P1-006 thumbnails | `ca7cb70` | sprite sheets under a real memory budget |
| T-P1-007 content | `7d1152a`, `175d097`, `df186bf` | audio, comics, typed bodies, `PersonRef` on all seven kinds, funscript |
| T-P1-008 frontend | `94fade8` | one GraphQL client, keyset pagination, windowed grid |

## Three things to know before writing more code here

These are the ones that cost time. Each is written up in the code at the point
where it matters; this is the index.

### 1. A unit test cannot see any of the four UI bugs, and three of them only
### fire when the result is small

`started` was missing from `GridState`, so the success path rebuilt the state
object without it and the component's "have I asked yet" test went back to
false. The grid re-requested forever. It only did so when the whole result fit
on one screen, because with more rows than fit the scroll handler's own fetch
is what advanced the list and this loop ran harmlessly alongside it. **Every
test written against a 5,000-item library passed while the app was broken.**

The store's own tests passed too. They assert on `rows`, and a store that
returns the right rows while a flag its caller reads quietly reverts to its
initial value looks correct from every angle a unit test can see. The type
checker was the only thing that noticed, and only because `GridState` was an
interface and `initialState` was not assignable to it — which is a warning
about the type, not about the behaviour.

The lesson generalises past this bug: **a flag that only the caller reads needs
a test that reads it.** `ui/tests/keyset.test.ts` now has two.

Two more in the same family:

- Every row was two quarters too short. The row height multiplied the tile
  *width* by the width:height ratio instead of dividing. A fixed row height
  only has to be *consistent* for the window arithmetic to come out right, and
  the wrong value is perfectly consistent — so nothing that counted rows
  noticed.
- The scroll handler wrote `viewportHeight`, which `visibleCount` derives from,
  which re-renders the window, which re-lays out the spacer, which resizes the
  scroll area, which fires `scroll` again.

### 2. The build succeeded and shipped nothing

With `ssr` left on and no `prerender = true`, adapter-static prerenders zero
pages and writes **only** `200.html`. There is no `index.html`, so `/` is a
directory listing and the app never boots. The build reports `Wrote site to
build` and prints nothing wrong at all.

`ssr = false` has to be in `src/routes/+layout.ts`, not in the `kit` object in
`vite.config.ts` — SvelteKit reads it from the page options and ignores it
there. `prerender.entries` cannot be inferred by crawling a client-rendered
app either, because the nav is in the client bundle rather than the prerendered
HTML, so the entries are named.

### 3. The concurrency acceptance test in T-P1-006 passed with the check deleted

The plan's stated test — "set the ceiling to 1, run 4 concurrent generations,
assert max in-flight == 1" — does not fail when the ceiling check is removed.
An attempt to fix it with a `Barrier` inside the critical section deadlocked
correct code, because only one worker can be inside. The working version
detects the violation from inside the critical section, with a scoped worker
lifetime and an observer that never acquires the budget. All four attempts are
in `ca7cb70`. **Read it before writing another concurrency test in this repo.**

## Deliberately not done

**Paraglide message extraction (part of T-P1-008 point 1).** Every user-visible
string in the shell is inline. A message catalog with one value per key, kept in
sync with components by hand, is a layer that only starts paying for itself
when there is a second locale. This is the one item in a completed ticket that
is not implemented, and it is recorded in the plan as well as here.

## Next

Phase 2 — Library, scanning, performance. Exit condition: a 100k-item library
scans and browses within budget; C15–C20 closed; locator hashes computed.

The immediate blocker is `crates/commons-api`. The UI is built and tested, and
cannot be verified against a real backend until that crate exists.
