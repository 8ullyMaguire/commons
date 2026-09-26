# Handoff

Where Commons is, what is verified, and what is deliberately not done.

**As of:** 2026-09-27 · **Branch:** `main` · **No git remote is configured**, so
nothing here has ever been pushed. That is the one part of the standing
instruction that cannot be satisfied without someone adding a remote.

---

## Where it is

Phases 0, 1, 2 and 3 are complete. Phase 3's eight tickets are T-P3-000 the
scan pipeline, T-P3-001 face detection, T-P3-002 clustering, T-P3-003 the §7.4
composite score, T-P3-004 merge/split/alias/disambiguate, T-P3-005 §7.5's
self-service performer claim, T-P3-006 the performer field model. All Phases 4
through 8 are not started. Nothing built is a stub.

`python3 scripts/plan-status.py` is the authority on that sentence, not this
file and not the plan. It counts 38 of 84 tickets closed and 42 genuinely
unstarted, and it exits non-zero if any ticket is *marked* done while the file
it names is absent -- the failure mode that reads as progress and builds as
nothing. `scripts/verify.sh` runs it, so the claim cannot rot.

| | State |
|---|---|
| Rust workspace | 1014 tests, 0 failures |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| UI unit tests | 43 pass (`node ./tests/run-tests.mjs`) |
| UI browser tests | 11 pass (`pnpm run test:e2e`) |
| UI type check | 0 errors (`svelte-check --threshold error`) |
| UI build | clean, no compiler warnings |

**Tags:** `phase-1-scan-core`, `phase-1-content-types`, `phase-1-complete`,
`phase-2-scan`, `phase-2-jobs`, `phase-2-complete`, `phase-4-candidates`,
`phase-4-history`, `phase-4-moderation`, `phase-4-points`.

`scripts/verify.sh` is the single command that does all of this, and it uses
`--no-fail-fast`. A target earlier in the list masks later ones: two failures
were sitting in `commons-scan` and `commons-store` while every run before them
reported green. See section 9 below.

`crates/commons-api` is still an empty placeholder crate. There is no GraphQL
server, so the UI is verified against a stubbed network. That is Phase 4+ work
and is not a Phase 2 blocker.

## Phase 4 so far

Four of the Phase 4 tickets are closed and each has a tag, a plan entry that
names the file it produced, and a mutation sweep over the rule the ticket is
about -- not over the code around it.

| Ticket | Spec | What the rule is | What the mutations killed |
|---|---|---|---|
| T-P4-001 | §8.1 | a field's value is recomputed from the accepted-vote set | — |
| T-P4-002 | §8.2 | a proposer is a rule about a *name*, and an exact phash match proposes | 9 proposers, 34 tests |
| T-P4-003 | §8.3 | a vote weighs what the voter is worth *on that field* | `MAX`→`SUM`, tie-break flips |
| T-P4-004 | §8.6 | a field's score is recomputed from the accepted-edit set, never a counter | 11 mutations, incl. `MAX`→`SUM` |
| T-P4-005 | §8.5 | authorization is per item type, from the type alone | 5, incl. `may_resolve`→`true` |
| T-P4-006 | §8.4 | a balance is a sum over a ledger, never a stored number | 8, incl. the withdraw bug |

The `MAX` → `SUM` mutation appearing twice is the point of the whole phase: a
score kept as a counter and a score recomputed from a set are the same code with
one character changed, and the tests are built so that character is load-bearing.

## How to verify

```sh
# One command: fmt, clippy, and the full test suite with --no-fail-fast.
./scripts/verify.sh

# Or by hand:
cd ~/code-local/rust/commons

# Set this FIRST. The hardware-acceleration and encoder acceptance tests drive
# the real ffmpeg against this machine's VA-API device. Unset, they fall back
# to whatever `ffmpeg` is on PATH and the hardware cases have nothing to run
# against -- the count is the same either way, but the hardware assertions are vacuous.
export COMMONS_FFMPEG=~/.hermes/tools/ffmpeg-9.0.1-linux-x64/bin/ffmpeg

CARGO_TARGET_DIR=~/.cargo-target/commons cargo test --workspace --no-fail-fast   # 751
CARGO_TARGET_DIR=~/.cargo-target/commons cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

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
| T-P2-006 storage | `e45924b` | three bases, each with the `du` that computes it |
| T-P2-006 encoders | `c8b44a2` | format, quality, threads — all previously literals |
| T-P2-007 locators | `78a0ac0` | the write path that was a stub |
| T-P2-008 throughput | `9eb7800` | the budget, as a number that fails |

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

### 7. `du` counts an inode once, even under `--apparent-size`

So `du -sb` is apparent size **per inode** — a fourth quantity that matches
none of the three bases T-P2-006 computes, and therefore cannot be the
cross-check for any of them. The plan said "equals `du -sb` within 1 %"; that
criterion is wrong and has been corrected in place. Each basis has its own
reference invocation, and the test asserts *exact* equality, because a tolerance
hides the only failure that matters here: having summed the wrong field.

### 8. A mistake that only exists on a filesystem this host lacks passes here forever

`f_frsize` versus `f_bsize` in `statvfs` decides whether reported disk space is
right or 8× wrong, and on ext4, xfs, tmpfs and btrfs the two are **equal**. So
swapping them is a no-op here — three attempts at a test passed while the code
was wrong. The fix was to stop testing the syscall and test the composition
(`DiskSpace::from_statvfs` takes the raw counts, so a test can supply a
filesystem where they differ). This recurs; when a test "cannot see" a bug,
the first question is whether the host can express the case at all.

### 9. A green run above a failing run is not a green run

`cargo test --workspace` stops at the first failing target. Two tests were
sitting broken behind one that had just started failing: an inotify test that
asserted settledness after a fixed 900 ms (so it failed on CPU contention
rather than on a debounce bug), and a consent test whose table contradicted
§14.1. Both were reported green for as long as something above them passed.
**Always verify with `--no-fail-fast`**, and read the count of failures, not
just the exit code.

### 10. A test that asserts a count cannot see a sequence bug

The keyframe clamp divided a *count* by a count and used the result as a
*distance* in milliseconds: a three-hour file got a 22 ms stride and sampled its
first second in 50 samples. Right count, strictly increasing, starting at zero —
every property a reasonable test asserts, and completely wrong. The same shape
appears in the Phase 2 pipeline bugs and in the SHA-256 chunking bug, where
every published test vector was too small to reach the broken path.

So: assert where the *last* item lands, not how many there are; assert *which*
error came back, not merely that one did; and when a bug survives a mutation
check, ask whether the test only ever exercised the failing path.

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

**T-P3-002 — clustering.** Faces exist and have vectors; nothing has ever put
two of them in the same cluster. This is the first ticket where §7.1's
*proposal* framing has to be carried, not just its mechanics: a link between
two appearances is a claim about a person, and the spec requires that a person
can decline it.

**T-P3-001's honest limitation, which T-P3-002 inherits.** No ONNX runtime is
linked, so no test asserts that a real face is found in a real frame, and none
claims to. `Detector` takes its recogniser as a parameter
(`with_recognising`), which is how the 36 tests drive it; linking a runtime is
a change behind that seam, not through it. Until then, the *detection rate* is
untested and the *decision logic around it* is tested hard. Anyone reading
T-P3-002 should treat the thresholds as uncalibrated — the clustering distance
threshold is a number with no measurement behind it yet.

**What a reader should know before starting:**

- **The sidecar is a file, deliberately.** `sidecar.rs` is not a TODO. It
  writes vectors and provenance to `faces.usearch` with a digest trailer, and
  the reason is rule 2: `pgvector` is an extension, and depending on one would
  make the index need Postgres. Losing the file costs a re-detect; the
  clusters in SQL are the expensive part and they are unaffected.
- **The search is an exact scan, deliberately.** Not an approximate index, and
  not yet a `usearch` one. At 200k faces an exact cosine scan is a few
  milliseconds of SIMD. The ANN index is where T-P3-003 earns its keep —
  introduced when a measurement says the exact scan is too slow, because an
  approximate index that silently drops a true match is much harder to debug
  than a slow one.
- **The two masked failures are fixed and the test command changed.** See
  "How to verify": `--no-fail-fast` is now part of it, and the two bugs it
  exposed (an inotify test that asserted settledness after a fixed wait, and a
  consent test that contradicted §14.1) are in `b7a8870`.

**Still open from T-P3-000, unchanged:** artifact jobs are not submitted by
the pipeline, volume backoff is not consulted, Postgres is unproven on the scan
path, and `ScanInput` supplied by a caller is ignored (the pipeline builds one
from the walk; `reconcile::plan` is the reusable entry point).


## Phase 11 — the community ecosystem (added 2026-09-26)

The owner asked for the whole `stashapp` ecosystem, specifically
CommunityScripts and CommunityScrapers, adapted to this implementation, and
**only after everything else is done**. Added to the plan as Phase 11 with
seven tickets. Nothing implemented.

The measurements that shaped it, all taken from the GitHub API rather than
assumed — re-take them before trusting any of these numbers, since upstream
pushes daily:

| | |
|---|---|
| `CommunityScrapers` (`master`) | 982 files under `scrapers/`: **729 YAML**, 155 Python, 9 `py_common` |
| `CommunityScripts` (`main`) | 462 plugin files across **79 plugin directories**; 12 theme directories, 54 CSS |
| Both | AGPL-3.0, actively pushed |
| `stash` itself | Go, 13k stars — the reference implementation, not a target |
| Archived | `StashServer`, `StashFrontend`, `StashOSX` (all pre-2019) |

Three decisions, and the reasoning matters more than the decisions:

1. **The YAML scrapers get an interpreter, not a translator.** 729 declarative
   definitions, each a program in a small language (entry-point table, XPath and
   JSON selectors, a `postProcess` chain of `replace`/`parseDate`/`truncate`/
   `map`). A per-file converter has to track every upstream construct forever.
2. **The Python scrapers and plugins get a compatibility layer, not a
   rewrite.** `py_common` is a real library; reimplementing 155 working programs
   in Rust is a different project with a worse success rate. A scraper needing
   a capability Commons lacks is reported as incompatible *at load time*, not
   halfway through a scrape.
3. **Nothing is vendored — ever.** Fetched at install time, pinned by commit.
   AGPL-3.0 in-tree would make the whole workspace AGPL, and a vendored copy is
   stale the moment upstream pushes. This is now §0.1 rule 6 of the plan, and
   the compatibility report is generated from *execution* so that "works" means
   something actually ran.

The phase's one hard dependency is `commons-api`, which does not exist yet, so
T-P11-007 is genuinely last even within the phase.

## A pattern worth naming

Four defects in T-P3-000 were invisible to every component's own tests, and
all four were *properties of the composition* — the reconciler could not
insert, a partial scan deleted the library, `mark_absent` wrote a state the
enum does not have, `insert_object` was not idempotent. Each component was
correctly tested and the sequence was catastrophically wrong.

T-P2-002's reconciler had 13 passing tests and could not create a single row,
because all 13 started from a library that already had rows. Coverage of a
component is not coverage of its use.

The general form: when a ticket says "compose these", the tests that matter
are the ones where the components meet, and they have to include the
*degenerate* case for each boundary — a scan that saw nothing, a scan that
saw half, content that already exists, an object that already exists. Every
one of the four bugs above was a degenerate case.

## Lessons from T-P3-004

### A test asserting the ERROR cannot see a broken STATE

The T-P3-002 lesson (a test asserting a COUNT cannot see a sequence bug) came
back as its dual. `split` refused an empty split with the right `EmptySplit`
error and left an empty cluster behind, because the check doing the work ran
*after* the insert. The test asserting the error passed the whole time. The
test that found it counts clusters *after* the refusal:

    assert!(ops::split(&store, &id, &[]).await.is_err());
    assert_eq!(clusters_after, clusters_before);   // <- this line found it

Same shape twice more in this ticket: `set_body_centroid` was dead code and the
"written once" test passed *because* of it, and `blocked_pair`'s symmetric SQL
could never have been reached because both rows were always written -- so no
test could ever have caught it. When a test passes, ask whether it *could* have
failed.

The mutation pass is how this surfaces. It is less a way to find bugs than a way
to find the assertions that cannot fail: of twelve mutations here, five
survived the first pass, and every survivor was a gap in what the tests asked
rather than a gap in the code.

### `sqlx::migrate!` embeds at compile time, and cargo missed a new file

A freshly added migration was "no such table" at runtime against a green build.
`touch crates/commons-store/src/lib.rs` forces the re-embed. If a migration is
missing from a test database, suspect the build before the SQL.

---

## Two lessons from T-P3-005, both about fixtures

**A fixture with one of everything cannot test a filter.** The first pass of
T-P3-005's mutation sweep had two survivors, and both were the same mistake: every
fixture had exactly one verified account and exactly one cluster. With one
account, "return this account's appearances" and "return every appearance in the
database" are the same result set, and with one cluster, "the verified account" and
"the verified account *of this cluster*" are the same string. A mutation that
replaced `WHERE v.account = ?` with `WHERE 1 = 1` passed.

The fix is not a better assertion, it is a second account. A test of a *filter*
needs at least two things to filter between, and the assertion that matters is
the one on the row that must **not** appear. Every filter in this repo should be
read with that question: what is the second thing?

**One performer per cluster is a decision, not a primary key.** `performer_verification`
is keyed by `cluster_id`, so one cluster is one person. I hit this as a test that
would not set up -- three accounts, one cluster, `UNIQUE constraint failed` -- and
nearly "fixed" it by loosening the schema. That would have been the wrong fix: two
accounts verified against one cluster means a takedown request goes to both, and
each learns that the other is verified against the same content. The constraint is
load-bearing. It is now written down in the migration and asserted directly.

**When a test will not set up, check whether the schema is telling you something.**
A unique-violation error is the database disagreeing with the fixture, and the
disagreement is usually about the domain, not about the test.

---

## Phase 3 is closed, and what it left behind

The identity engine is the part of Commons with the most expensive mistakes, so
the shape of it is worth stating for whoever picks up Phase 4.

**`commons-core` owns the decisions, the crates above it own the work.** §7.12's
"does this appearance count" is `AppearanceType::counts_as_appearance` in
`commons-core`. The appear-with graph in `commons-identity` reads it. During
T-P3-006 the graph first had its own list of credited type strings, which is the
same decision in two places and the copy is what goes stale. If a rule is a rule
about the domain, it belongs in `commons-core` and everything else reads it.

**The schema is the authority wherever a query could depend on it.** Which
attribute types exist, which are multi-valued, which statuses are legal: all
three are tables or constraints, and `AttrType::from_schema` refuses to proceed
if the Rust enum disagrees. It fired during T-P3-006 (`date` was seeded
single-valued, the enum said multi) and it fires in the right place — at the
first read of a field, not three screens later.

**Two schema bugs predating Phase 3, both found by tests written for a feature
that needed them.** 0002 made `appearance` unique per (object, type), which
means a scene with two performers cannot be recorded at all — and §7.12's graph
is built by joining appearances through `object_id`, so the feature was
unbuildable against the constraint. 0001 made `custom_field_value` unique per
(subject, date), which means a `multi` field cannot hold two values written on
the same day. Both were fixed in 0006 by rebuilding the constraint properly
rather than by working around it in the query layer. **If a feature is
unimplementable, the schema is the thing to read first** — the workaround is
always available and always worse.

**§7.11's career span is a query, not a column.** No stored span, because a
stored span drifts, cannot explain itself, and turns "editable but derived" into
two sources of truth with a rule about which wins. The index on
`(cluster_id, object_id)` makes it as cheap as it needs to be; the cost of a
stored column is paid on every write forever and this one is paid on a read that
already touches the row.

---

## Phase 4 has started, and the first ticket is where the design was hardest

`commons-index` is no longer an empty crate. T-P4-001 is `resolve`, and three
things in it are worth carrying forward because they are the kind of mistake
that looks correct.

**A cache key must include everything that changes the answer.** The obvious key
is `(subject, field, fingerprint)` — the evidence. It silently ignores
configuration: two different `half_life_days` produce identical keys, so the
second is served the first's result. Two decay tests were passing for the wrong
reason, agreeing with the no-decay answer. Any cache whose key omits a knob has
this bug, and the tests will not find it unless two settings of the knob are
compared against each other in the same test.

**Clamp a contribution, never a total.** A floor on a *proposal's* weight gives
every unvoted proposal the same positive weight, so one beats a heavily-backed
value. A ceiling on a total makes reputation 5 and 6 indistinguishable, so
"better evidence moves the value" stops holding at the top of the scale. Both
were written, both were caught, and both look like reasonable bounds.

**Distinguish inference from extraction.** A tagger that looked at a frame and a
parser that moved a string out of a filename are both `is_automatic`, and both
carry a `confidence`. They are not the same claim. Only the first one's
confidence is support; the second is a candidate. Without the split, a freshly
scanned library settles every field on whatever the first parser guessed.

T-P4-003 is now done too, and it resolved that question: **the log is
authoritative, the columns are its cache, and a reputation change applies from
the next ballot.** Two things there are worth more than the ticket.

**A feature that computes a number and a feature that reads it can be
separately complete and still not connected.** `cast_vote` read
`account.field_reputation`; `reputation.rs` wrote `reputation_event`. Both were
finished, both were tested, and reputation could not affect a single vote —
because nothing wrote the column the vote read. The wiring test now asserts the
cache equals the log to a float epsilon. When a ticket adds a producer and a
consumer, check they touch.

**A rule with no off switch cannot be tested in isolation.** The three
coordination-detection rules were each individually deletable with the suite
green, because every realistic input trips two or three at once. The config now
carries a switch per rule, and six tests arm one at a time. The off switch is
production code, not test scaffolding: a component that can only be tested
through the shape of its input is telling you something about its design.

**A decay must be asserted round by round, not at the end.** The compounding
forgiveness factor made the third rejection *raise* an account's standing. "Is
the final weight lower" passes; "is the weight lower than last round" does not.

**T-P4-002 is next**, and the fixture work above is relevant to it: three
`resolve` tests were silently asserting nothing for a whole ticket, because a
weight they set on an account was recomputed away by `cast_vote`. Any new test
that sets up state by writing a cache rather than through the code that owns it
is worth a second look.
