# Handoff

Where Commons is, what is verified, and what is deliberately not done.

**As of:** 2026-09-26 · **Branch:** `main` · **No git remote is configured**, so
nothing here has ever been pushed. That is the one part of the standing
instruction that cannot be satisfied without someone adding a remote.

---

## Where it is

Phase 0 and Phase 1 are complete. Phase 2 is 5 of 8 tickets. Nothing is a stub.

| | State |
|---|---|
| Rust workspace | 616 tests, 0 failures |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| UI unit tests | 43 pass (`node ./tests/run-tests.mjs`) |
| UI browser tests | 11 pass (`pnpm run test:e2e`) |
| UI type check | 0 errors (`svelte-check --threshold error`) |
| UI build | clean, no compiler warnings |

**Tags:** `phase-1-scan-core`, `phase-1-content-types`, `phase-1-complete`,
`phase-2-scan`, `phase-2-jobs`.

`crates/commons-api` is still an empty placeholder crate. There is no GraphQL
server, so the UI is verified against a stubbed network. That is Phase 4+ work
and is not a Phase 2 blocker.

## How to verify

```sh
# Rust
cd ~/code-local/rust/commons
CARGO_TARGET_DIR=~/.cargo-target/commons cargo test --workspace
CARGO_TARGET_DIR=~/.cargo-target/commons cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

# The hardware-acceleration acceptance tests drive the real ffmpeg against
# this machine's VA-API device. Without this they fall back to PATH and the
# hardware cases skip.
export COMMONS_FFMPEG=~/.hermes/tools/ffmpeg-9.0.1-linux-x64/bin/ffmpeg

# UI — pnpm with the hoisted linker on this host, so the scripts call node
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

## What Phase 2 has shipped

| Ticket | Commit | Notes |
|---|---|---|
| T-P2-001 walk | — | pending traversal stack, per-directory emitted counts |
| T-P2-001 progress | — | honest about unknown denominators |
| T-P2-001 watcher | — | debounced native notifications, polling fallback |
| T-P2-002 hashing | — | one physical read for BLAKE3 + XXH3 |
| T-P2-002 moves | — | a rename is not a delete plus an add |
| T-P2-003 state | — | `FileState` consolidated into `commons-core` |
| T-P2-003 volumes | `phase-2-scan` | a real loopback ext4 test with a stat-count criterion |
| T-P2-004 queue | — | fair FIFO, non-claimable delayed retries |
| T-P2-004 persistence | — | through the `job` table; a `running` row returns `queued` |
| T-P2-004 supervisor | `phase-2-jobs` | inhibitor held *during* a job, released when the last finishes |
| T-P2-005 acceleration | `6dcf001` | probe, plan, and the reason string |

## Six things to know before writing more code here

These are the ones that cost time. Each is written up in the code at the point
where it matters; this is the index.

### 1. A unit test cannot see any of the four UI bugs

`started` was missing from `GridState`, so the success path rebuilt the state
object without it and the component's "have I asked yet" test went back to
false. The grid re-requested forever. It only did so when the whole result fit
on one screen, because with more rows than fit the scroll handler's own fetch
advanced the list and this loop ran harmlessly alongside it. **Every test
written against a 5,000-item library passed while the app was broken.**

The lesson: **a flag that only the caller reads needs a test that reads it.**

Two more in the same family:

- Every row was two quarters too short. The row height multiplied the tile
  *width* by the width:height ratio instead of dividing. A fixed row height only
  has to be *consistent* for the window arithmetic to come out right, and the
  wrong value is perfectly consistent.
- The scroll handler wrote `viewportHeight`, which `visibleCount` derives from,
  which re-renders the window, which re-lays out the spacer, which resizes the
  scroll area, which fires `scroll` again.

### 2. The build succeeded and shipped nothing

With `ssr` left on and no `prerender = true`, adapter-static prerenders zero
pages and writes **only** `200.html`. There is no `index.html`, so `/` is a
directory listing and the app never boots. The build reports `Wrote site to
build` and prints nothing wrong at all.

`ssr = false` has to be in `src/routes/+layout.ts`, not in the `kit` object in
`vite.config.ts`. `prerender.entries` cannot be inferred by crawling a
client-rendered app, so the entries are named.

### 3. The concurrency acceptance test in T-P1-006 passed with the check deleted

The plan's stated test — "set the ceiling to 1, run 4 concurrent generations,
assert max in-flight == 1" — does not fail when the ceiling check is removed.
An attempt to fix it with a `Barrier` inside the critical section deadlocked
correct code. The working version detects the violation from inside the critical
section, with a scoped worker lifetime and an observer that never acquires the
budget. All four attempts are in `ca7cb70`.

### 4. ffmpeg cannot tell you whether you have a GPU

`ffmpeg -encoders` on a machine with no GPU lists `h264_nvenc`, `h264_vaapi` and
`h264_qsv`. They are compiled into the *binary*; the hardware is a different
machine, reached through a device node. An implementation that asks ffmpeg
reports acceleration available on a laptop with no GPU, and the first thumbnail
silently falls back after a multi-second timeout.

Each accelerator in `hwaccel.rs` is therefore probed against the device: does the
build have it, does the device exist, can this process open it. Each step that
fails says which one.

### 5. A hardware filter returns hardware frames, and `tile` is a software filter

`scale_vaapi` hands its output back as a device frame. Neither the software
encoder nor `tile` can read one, so `hwdownload` has to precede the **first**
software filter. Appending it works for a thumbnail and produces an **empty
sprite** — the worst outcome, because the path that looks fine is the one
nothing checks.

A second, quieter version: `-init_hw_device vaapi=va:0` fails on a machine whose
only DRM node is `card1` with `No VA display found for device 0`, while
`vaapi=va:/dev/dri/renderD128` works. The node is now carried from the probe
into the plan, so the status and the plan cannot disagree.

### 6. `cargo build` does not compile `#[cfg(test)]` code

A green build can hide a fully broken test suite. Always run
`cargo test --workspace` before believing a change is done.

## Deliberately not done

**Paraglide message extraction (part of T-P1-008 point 1).** Every user-visible
string in the shell is inline. A message catalog with one value per key, kept in
sync with components by hand, only starts paying for itself when there is a
second locale. This is the one item in a completed ticket that is not
implemented.

**Hardware WebP encoding.** There is none in ffmpeg, so the encoder never
changes. `AccelPlan::encoder()` takes no argument precisely so no caller can
pass a hardware encoder and get a JPEG named `.webp`.

## Known gaps

Real, and not yet fixed.

- **The `job` table is SQLite-only.** `Store`'s Postgres arm cannot be bound
  through without a second path, and `store.pool()` panics there rather than
  returning a plausible wrong answer.
- **`run_one` is synchronous**, so `WorkerPool` is a serial loop with a bound,
  not parallel workers. The bound is enforced and the per-(kind, target)
  conflict rule is real; nothing runs concurrently yet.
- **`ui/static/favicon.png` is a temporary transparent placeholder.** Replace
  before the project is complete.
- **The walk → hash → reconcile pipeline is not wired together.** Each stage is
  implemented and tested; no orchestrator joins them. That is T-P2-008's real
  work.
- **XXH3-128 whole-file values come from a custom fold** of one-shot
  per-buffer digests, because `xxhash-rust` 0.8.18 has no streaming XXH3-128.
  The 23 official vectors validate the primitive; the fold is validated for
  determinism and boundary-safety, not against a reference whole-file value.
  A spec-compliance audit is outstanding.
- **No volume table**, so durable mount tracking and per-volume opt-out are
  in-memory. A matching SQLite/Postgres migration is needed before T-P2-003 is
  fully closed.

## Testing conventions worth keeping

- Every behavioural claim is checked by **mutation**: break the code on
  purpose, confirm the test fails, restore. A test that cannot fail is worse
  than no test, and several here were found that way.
- Tests needing real hardware **say so when they skip**, via an `eprintln!`
  beginning `SKIPPED`. No silent skips.
- Timers and locks get deterministic controls (`PollOverride`, explicit
  timestamps, atomic gates) rather than sleeps.
- Prefer asserting on the *machine* — a file, a database row, a PID — over
  inferring behaviour from a clock. A timeout test that checks elapsed time
  cannot tell a supervisor that killed the child from one that forgot to.

## Environment

- `CARGO_TARGET_DIR=~/.cargo-target/commons`
- ffmpeg 9.0.1 at `~/.hermes/tools/ffmpeg-9.0.1-linux-x64/bin`
- Passwordless `sudo`, `mount`, `losetup`, `mkfs.ext4` available. `/home` is
  btrfs and cannot host a mount point; `/tmp` is used for loopback tests.
- This machine has a Radeon with a working VA-API render node at
  `/dev/dri/renderD128`, so the acceleration acceptance tests really do run.

## Next

T-P2-006 — storage accounting (§6.6, §5.2): real on-disk size per file, a
per-library rollup, free/available disk space on the statistics page
(#7194), and a configurable temp root distinct from `generated/` (#5646).

Phase 2's exit condition is a 100k-item library scanning and browsing within
budget, C15–C20 closed, locator hashes computed.
