# Handoff

Where Commons is, what is verified, and what is deliberately not done.

**As of:** 2026-09-27 · **Branch:** `main` · **Remote:** `origin` =
`https://github.com/8ullyMaguire/commons`; every commit is pushed, and every
milestone carries an annotated `phase-*` tag.

---

## Where it is

Phases 0, 1, 2, 3 and 4 are complete, and Phase 5 has opened with
T-P5-001 (search), T-P5-002 (fuzzy, phonetic, synonyms, aliases),
T-P5-003 (the tag system) and T-P5-004 (duplicate and similar detection).
Phase 3's
eight tickets are T-P3-000 the scan pipeline, T-P3-001 face detection,
T-P3-002 clustering, T-P3-003 the §7.4 composite score, T-P3-004
merge/split/alias/disambiguate, T-P3-005 §7.5's self-service performer claim,
T-P3-006 the performer field model. Everything from T-P5-005 onwards is not
started. Nothing built is a stub.

`python3 scripts/plan-status.py` is the authority on that sentence, not this
file and not the plan. It counts 43 of 84 tickets closed and 36 genuinely
unstarted, and it exits non-zero if any ticket is *marked* done while the file
it names is absent -- the failure mode that reads as progress and builds as
nothing. `scripts/verify.sh` runs it, so the claim cannot rot.

| | State |
|---|---|
| Rust workspace | 1290 tests, 0 failures; 277 UI unit + 68 Playwright, 0 failures |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| `scripts/scan-history-secrets.py` | 499 blobs, 0 findings |
| `scripts/scan-history-secrets-selftest.py` | 8/8 planted patterns caught |
| UI unit tests | 277 pass (`node ./tests/run-tests.mjs`) |
| UI browser tests | 68 pass (`pnpm run test:e2e`) |
| UI type check | 0 errors (`svelte-check --threshold error`) |
| UI build | clean, no compiler warnings |

**Remote.** `origin` is `https://github.com/8ullyMaguire/commons.git`,
**private**, `main` tracking it, all 19 tags pushed. `gh` is the credential --
there is no GitHub token in `~/.hermes/.env`, and none needs to be: the
environment file holds Postgres URLs and admin passwords for other projects,
and adding a GitHub token to it would be a downgrade.

Before the first push, `scripts/scan-history-secrets.py` read every blob ever
committed and found nothing. It exists because the repo's own history contains
a credential-shaped string -- `scripts/verify.sh`'s local `DATABASE_URL` -- and
"we checked" should be reproducible rather than a memory. Its first version
scanned 0 blobs and reported "0 findings" because an `awk` inside a shell
string lost its quoting, and it now exits non-zero when it looks at nothing.
`scan-history-secrets-selftest.py` plants one blob per pattern in a scratch
repo and fails unless all eight fire, so a clean result is evidence rather than
a regex that quietly stopped matching.

**Tags:** `phase-1-scan-core`, `phase-1-content-types`, `phase-1-complete`,
`phase-2-scan`, `phase-2-jobs`, `phase-2-complete`, `phase-4-candidates`,
`phase-4-history`, `phase-4-moderation`, `phase-4-points`,
`phase-4-consent-filter`, `phase-4-takedown`, `phase-5-search`,
`phase-5-fuzzy`, `phase-5-dedup`.

`scripts/verify.sh` is the single command that does all of this, and it uses
`--no-fail-fast`. A target earlier in the list masks later ones: two failures
were sitting in `commons-scan` and `commons-store` while every run before them
reported green. See section 9 below.

`crates/commons-api` is still an empty placeholder crate. There is no GraphQL
server, so the UI is verified against a stubbed network. That is Phase 4+ work
and is not a Phase 2 blocker.

## Phase 5 so far

Two tickets, both about one behaviour: §9.2 and §9.3 must mean the same thing
on SQLite and on Postgres, and the tests run against a real local Postgres to
prove it rather than asserting that they would.

| Ticket | Spec | What the rule is | Mutations killed |
|---|---|---|---|
| T-P5-001 | §9.2, §9.3, §3.5 | one tokenizer, one synonym table, one ranking | 6 |
| T-P5-002 | §9.3 | typo tolerance, phonetics, synonyms, aliases | 11 |
| T-P5-003 | §5.15, §9.4 | namespaces, typed attributes, groups, confidence | 11 |
| T-P5-004 | §9.7, §5.18 | `Identical`/`ReEncode`/`Similar`/`Distinct`, relations, opt-in auto-merge | 12 |
| T-P5-005 | §10.2, §9.6 | the lightbox: pan, flick, wheel, zoom, back on a dirty modal | 11 |
| T-P5-006 (1-3 of 17) | §10.4, §10.6, §10.7, §14.1 | selection as a value; the list table; the consent-checked bulk write and its modal | 17 |

**T-P5-005 is the first UI ticket in this phase, and it is the first one whose
tests could not all be written against the pure function.** The ticket's own
accept criterion — a wheel event dispatched mid-pan must not change the image
index — failed against a first implementation that was correct by every reading
of the gesture rules. A drag on a *fitted* image has nowhere to pan, so
`classifyWheel` checking "is there anything to pan?" first still navigated under
the user's finger. The fix is a third argument, `dragging`, checked *before* the
others: which gesture owns the input is a different question from what the input
would mean. That ordering is the whole ticket, and it is why the four
recognizer functions take their arguments in the order they do.

Three things cost real time here and are worth not learning again:

- **The e2e gestures are in absolute CSS pixels, not fractions of the stage.**
  They were fractions first. The stage grew from 212px to 640px when the
  fixture's preview was fixed, and the same 4% went from 8px (a flick) to 25px
  (a pan) — a green test turned red with no change to the code under test. The
  thresholds are in pixels; a fraction of a box is not a pixel.
- **A collapsed gesture surface reads as a flick.** With a 1x1 preview image the
  stage was 1px wide, every coordinate was the same point, a zero-travel
  release is a flick, and the lightbox advanced under a user who only tried to
  drag. The stage now carries `min-width`/`min-height`. This looked like a
  recognizer bug and was a fixture bug, and the way to tell them apart was to
  log the stage's rect and the measured travel rather than to reason about it.
- **`data-lightbox-index` on a tile is `rowIndex * cols + i`,** the absolute
  index. The inner `each` index alone is window-relative, which opens the wrong
  image for every tile below the first row — and the first row is the only one
  that would look right in a test.

Left for T-P5-006: paging past the loaded prefix. The lightbox clamps to the
rows it was given, so the worst case is that "next" stops, never that it shows
the wrong image. Deep-linking to an image is also not done: the index is
component state, not URL state, because the pan offset and zoom are not
serialisable and a URL carrying only the index produces a back button that
reopens the lightbox somewhere the user did not leave it. `dirty` is a prop
with a tested pure function behind it, but no route sets it yet — there is no
edit form in the app to set it from — so the e2e covers the clean path and says
so in the file.

**The decision these tickets rest on.** With native FTS, "the same tokenizer in
both engines" is unimplementable: SQLite's `unicode61` and Postgres's
`to_tsvector('english',…)` disagree about stemming, stop words and
hyphenation, and two native implementations are two tokenizers. So `tokenize`
produces the terms in Rust and `search_term` / `search_fuzzy` are ordinary
tables neither engine has an opinion about. Cost: a scan per term. A hosted
index can be added *alongside* these rows later — faster, never different.

**§9.3's "ass finds both meanings" did not work, and the fix was structural.**
Synonym expansion was flat, so a query for `arse` demanded an object carrying
`arse` *and* `ass` *and* `hole`. Expansion is now **OR within a synonym group,
AND between groups** — the shape every engine with synonym expansion uses —
and `expand` returns groups rather than a flat list precisely so that shape is
expressible. A flat `Vec<String>` loses the information needed to build the
query, which is how the bug got in.

**`CASE` has no spelling that means the same thing in both engines.** The
`HAVING` that counts matched *groups* needs `CASE term -> group index`.
`CASE WHEN ? THEN 1` is rejected at `PREPARE` on Postgres ("argument of
CASE/WHEN must be type boolean"), and `CASE WHEN 'ass' THEN 1` then fails with
"invalid input syntax for type boolean". Only the subject form —
`CASE s.term WHEN 'ass' THEN 1 ... ELSE 0 END` — means the same in both. The
terms are interpolated rather than bound; that is forced by the planner, and is
safe because `tokenize` yields only alphanumeric runs.

**Two bugs were the same bug.** `levenshtein` returned `None` for a string
against itself: the two-row rotation advanced `prev2` to the row about to be
discarded, *and* the early exit rejected the initial row `[0,1,2,…]` for any
string three characters long, so every comparison failed. And
`index_object` never wrote the fuzzy keys, so the two indexes could drift — an
alias was findable exactly and not findable with a typo. `index_object` now
writes both, so the drift is not expressible rather than merely untested.

**A test that cannot see the cascade is not a test.** `remove_alias` re-indexes
from the aliases that remain, because blanking the field made removing one of
three aliases make the other two unfindable — and a single-alias test cannot
tell "removed the alias" from "removed every alias", so the test now has three.

## The Postgres migration tree has never been applied

`_sqlx_migrations` in the local Postgres is **empty**. T-P5-001's parity test is
what found this, and two defects stop the tree running end to end:

1. `0001` declares a foreign key to `producer` five statements before creating
   it. SQLite does not check a foreign key until a write; Postgres does.
2. `0002` creates two indexes under names `0001` already used. SQLite's `0002`
   rebuilds the table instead, so there is no collision there.

Both are worked around **in the test harness, not in the migrations** — the
files are applied and immutable, and a fix is a new migration rather than a
reorder. With the two worked around the tree applies cleanly: 75 tables.

**Owed:** nothing in the suite would notice those two defects being
reintroduced, which is the part that matters. The parity test proves the tree
applies; it does not prove it applies *for the right reasons*.

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
| T-P4-007 | §14.1 | one sanctioned read; the guard keys on the statement, not the file | 10, 2 by the behaviour layer |
| T-P4-008 | §14.1 | the output is a content-hash blocklist, not a tier | 8, incl. removing the signature check |
| T-P5-001 | §9.3 | the tokenizer is ours; a native FTS cannot be given one | 6, incl. the AND becoming an OR |

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

### T-P5-006 item 9 — the feed, and two ways a test proves nothing

Two failures in this item, both of them a test that passed and proved nothing.
Both are recorded here because the shape recurs, not the instance.

**A mutation survived because the fixture had one of everything.** The first
preload test used the feed's own fixture — three clips and a still, focus 0 —
and asserted that exactly items 0, 1 and 2 preload. With `preload="auto"`
hardcoded on every video the test still passed. The reason: with focus 0 and
`PREFETCH_AHEAD = 2`, *every* video in that fixture is inside the prefetch
radius, so the real function and the mutation return the same list. A fixture
whose every element is inside the filter cannot test the filter. Six clips
killed it. `media-view.test.ts` had already made the same argument about a
fixture with one of everything.

**A test asserted an attribute the framework never emits.** The muted-video
test checked `toHaveAttribute('muted', '')`. Svelte compiles a bare `muted` on
an element to a *property* assignment, so the markup carries no such
attribute and the assertion was checking something the component does not
produce — it would have passed on a feed whose videos never played anything.
The property is also the only one that matters: it is what the autoplay gate
reads. The test now asserts the IDL property.

**The third way, which is not a test bug at all.** In `media.rs` the inner
join cannot be mutated into an observable difference, and that is a fact about
the SQL rather than a gap in coverage: `consent_clause` emits
`c.tier IN (...)`, so under a `LEFT JOIN` a missing consent row produces
`c.tier = NULL`, the predicate is false, and the mutant is a no-op that returns
exactly the unmutated result. The script records it as a known survivor with
that reason rather than pretending a test was written for it.

**And the infrastructure betrays you here too.** The mutation script's first
verdict logic grepped for `0 failed`, which Playwright never prints, and read
`tail -1`, which only ever sees the pass line — so it reported the inverse of
the truth for every mutant and looked like four survivors. A harness that
reports the wrong answer is worse than no harness, because it sends you looking
for holes that are not there.

### T-P5-006 item 13 — per-field ignore lists, and §10.10 is complete

#2318 (ignore fields when using the tagger) and #2399 (exclude fields from the
search query) look like two features and are one mechanism at two different
points. An APPLIED list decides what a scrape writes, so "Accept all" cannot
overwrite a field the user curates by hand. A SEARCH list decides what a query
looks at; nothing is written. Conflating them gives the worst of both: a user
who excludes a field from the search silently stops getting its value, with
nothing on screen saying why. Two lists, one shared vocabulary, never merged —
and the e2e asserts independence by ticking one and checking the other.

**The decision a Set cannot make.** With a `Set<string>`, "ignore nothing" and
"ignore everything" are both an empty set, and #2399's real use case is an
allow-list wearing an ignore-list's clothes. So the scope is `{ ignoreAll,
fields }` and the inversion happens in exactly one place.

**The vocabulary is pinned to the schema, in both directions.** `FieldProposal.
field` is a String on purpose — §8.1 wants a proposal to be able to name a field
this build has never heard of — so a UI has no checkbox list to offer, and
`tagger-fields.ts` is that list. A test reads `crates/commons-core/src/domain.rs`
and checks both directions: every offered name is a real column, and every
user-facing column is either offered or on the not-offerable list. The second
direction is the one that rots, and it names the struct and field in its failure.

That test failed on its first run: `Object.kind` was neither offered nor
excluded. It is now excluded, with the reason in the module.

**A test that forced a design change.** The exclusion list was flat, and a test
asserting no name is in both lists failed correctly. `Object.kind` is the §5.1
discriminator and must not be offerable; `Producer.kind` (studio, circle,
individual, collective) is metadata a tagger should propose. A flat list has one
entry for `kind` and the two subjects need opposite verdicts, so it must pick
one and be wrong. It is now per-subject, with a test pinning the pair.

**Two bugs only the browser found.** The tick handler used `isIgnored`, so under
an inverted scope every box unticked itself the moment the inversion went on,
leaving an allow-list in which nothing was allowed — the label says "only the
ticked fields are used", so a tick must mean membership. And the scope was
written with a bare `history.replaceState`, which SvelteKit does not observe:
the address bar changed and `page.url` did not, so the link the user copied was
not the state they were looking at. Now `goto(..., { replaceState: true })`.

**Two more tests that named a boundary without straddling it** — the fourth and
fifth. `ignoredFields` gains a `.sort()`: the test's input was already
alphabetical AND asserted a single ignored field, and one field sorts to itself.
Two ignored fields in non-alphabetical order is the smallest case that differs.
And `isOfferable` inverted, because that function — the one a settings UI calls
— had no test of its own; the others read the lists directly. A function with no
caller under test and no test of its own is the easiest bug to ship.

28 mutations: 28 killed, 0 survived, 0 stale. 66 UI tests, 15 e2e.
UI now 490, e2e 112, Rust 1310.

§10.10 is now complete: the bulk-edit modal (#5336), right-click paste (#7139),
unsaved-entry protection (#6466, #3253), CSV and paste-parse import (#1296, #431),
the per-field ignore lists (#2318, #2399), and create-from-subpage /
create-all-missing (#3694, #1017, #3122).

### T-P5-006 item 12 — CSV import, and a correction to item 11's spec

**First: item 11's spec was wrong.** It closed with a "what is deliberately
not here" list saying "No CSV import. That is #1296, its own item." §10.10
reads "CSV **and** paste-parse import (#1296, #431)" — one issue, two formats.
The CSV half was never separate work, it was the rest of the same issue, and it
is item 12. A "not here" list is a claim about the future, and an unexamined
claim about the future is how work falls through the gap between two items.

`ui/src/lib/api/csv.ts` is a character-by-character state machine rather than
`paste.ts` with a different splitter, because RFC 4180 allows a newline INSIDE a
quoted field and a Postgres export puts a paragraph in one cell. A
line-splitting parser turns one value into three and blames three different rows.
It reuses `ParsedValue` and `preview` from `paste.ts` so the file path and the
paste path share one preview, one Apply and one Esc — asserted directly in the
e2e by checking the same element carries `data-source="file"` then
`data-source="paste"`.

**A bug the unit tests could not see.** The first `decodeCsvBytes` asked
whether the bytes were valid UTF-8. NUL is a *valid* UTF-8 character, so a
UTF-16 file decoded as UTF-8 comes out as `a\0b\0c\0` with no replacement
character, the check passes, the UTF-16 heuristic below it is unreachable, and
the file imports as mojibake. "Is this valid UTF-8" cannot separate the two
encodings for ASCII; "are there NULs where text should be" can.

**Three more tests that named a boundary without straddling it** — the sixth,
seventh and eighth in this project, each found by a surviving mutation. The
sharpest: the sniffer test used a comma inside quotes, where a naive count and
the quote-aware count give the same modal value, so the test passed with the
quotes ignored. Two further tests were not enough either. The case that
separates them puts the wrong delimiter BOTH inside a quoted field and as a
real separator, so the tie-break is magnitude and the naive count wins.

**Four exempt mutations, each proved rather than assumed.** `TextDecoder` strips
a BOM itself (`ignoreBOM: false` means *do not ignore*), so the `subarray` is
redundant — kept anyway, with a comment saying the reason is explicitness rather
than coverage. The sniffer's escaped-quote branch is not load-bearing because
that function only counts and never emits; an exhaustive sweep over all 1,093
strings of length <= 6 in {a, ", ;} found zero differences. The UTF-16LE BOM
check widened to accept a UTF-8 BOM is unreachable *because* the UTF-8 check
returns first, so a test asserts the ordering directly instead.

**A bug only the browser could find.** Esc did not dismiss a file import. The
keydown handler was on the field wrapper, and a file import leaves focus on the
hidden file input, outside it — so Esc did nothing. That is the only way to open
a preview without touching the field, which makes it the only way a user who
opens a file, reads the warning and changes their mind has no way out. Now
listened for on the document while a preview is open, in the capture phase so it
beats an enclosing modal.

Also: a truncated file gets NO Apply button, not a disabled one, because a
greyed-out button beside "Add 3" is a control the user cannot use and cannot
understand. Blank lines are reported as gaps on their own count, separate from
empty rows, because "we discarded 40 lines" reported as 0 is a lie.

34 mutations: 30 killed, 4 exempt, 0 stale. 76 UI tests, 14 e2e; UI 425, e2e 97.

### T-P5-006 item 11 — right-click paste, and a survivor that was a real bug

`ui/src/lib/api/paste.ts` parses a paste into N values; the design decision is
that **a separator only counts outside quotes**. Splitting on newlines alone
misses a spreadsheet column; splitting on commas turns "Smith, John" into two
tags. The chosen rule trusts a quote, because the quoted form is unambiguous
and the unquoted form is not — and a user who pastes `Smith, John` unquoted
can see how to fix it (add quotes), where a parser that silently merged two of
their tags leaves them nothing to do.

A paste does not land until it is confirmed. One sentence, an Add button,
Cancel, and Esc. A paste of 400 tags into a field holding 380 is not
reversible by hand, and §10.10's rule — an action states its scope before you
commit to it — is the bulk modal's scope line applied to the smallest unit it
has.

**A surviving mutation that turned out to be a real bug.** Removing `unquote`'s
`endsWith` check survived, and chasing it found the defect: the splitter kept
quote characters in the value buffer and stripped them at the end, so `"A", B`
— balanced quotes, a separator *outside* them — read as one value with stray
quotes in it. A quote only protects a separator next to it. The fix strips
quotes during the scan and tracks whether a part opened with one, so a quoted
blank survives as data. One of the three new tests guards a case that fix broke:
trimming before filtering reduced `" "` to the empty string and discarded it.

**One mutation is exempt, and proved rather than assumed.** "A doubled quote is
read as a close, then an open" emits `""` instead of `"` and consumes one
character instead of two. Both paths consume exactly two quote characters and
leave the state unchanged, and the doubled form is undone by `unquote`. An
exhaustive check over every string of length ≤ 8 in `{a, "}` found zero
differences. Equivalent mutant, recorded next to the list with its reason rather
than deleted to improve the number.

14 mutations: 13 killed, 1 exempt. Four needed retargeting after the fix, which
is the script's `COULD NOT APPLY` line doing its job — a stale mutation is a
script that has stopped testing anything.

The right-click test dispatches the event and reads `defaultPrevented` rather
than clicking, because a menu that appears *beside* the browser's is the
failure. An earlier version asserted on `navigator.userAgent`, which was both
meaningless and false — HeadlessChrome contains an `x`.

39 UI tests, 8 e2e, e2e now 83.

### T-P5-006 item 10, part 2 — the create UI, and a bug only a browser could see

`ui/src/lib/api/create.ts` (pure) turns `CreateOutcome`'s three counts into the
one sentence a user reads. The button says `Create 5`, not `Create 40`, when 35
of 40 are already in the library — the count the click actually causes. The rows
live in the URL as repeated `?rows=` rather than a joined string, so a title
containing a `&` or a `,` survives; a test pastes `'Two Words'`, `'A & B'` and
`'quote " and \\ backslash'` and asserts they arrive intact.

A hardcoded state literal at the route's call site: `result = { state: 'done' }`
instead of `classify(outcome)`. Every unit test passed, because the tests
exercise the module and the module is correct — a run with `refused > 0`
rendered `data-state="done"` with no warning styling, which is exactly what
`classify` exists to prevent. **The e2e suite caught it and nothing else
could**: the bug is in a caller ignoring a function, and no test of a function
sees a caller that ignores it. This is the strongest argument yet for running
the browser suite rather than trusting the unit count.

29 UI tests, 7 mutations, 7 killed. E2E 75 (was 68).

It is a route and not two rows in the bulk modal because the modal is
*selection*-driven — its scope line is a promise about exactly which ids will be
touched — and neither create action has a selection by construction.

### T-P5-006 item 10, part 1 — the create path, and a test that could not tell

`create-from-subpage` (#3694) and `create-all-missing` (#1017, #3122) are the
only two actions in §10.10 with **no object to operate on**, which is why they
are a module (`create.rs`) rather than two more methods on `Store`. The id is
derived, not generated, so re-running an import is a no-op; it covers only what
the object *is*, because an id covering `organized` would rename the object on
every review and dangle every tag and relation pointing at it. A new object
gets an `unverified` consent row in the same batch: without one it matches no
tier list anywhere and is created and then **unfindable**, including by the
person who made it. 20 tests on both engines, 5 mutations killed.

Three findings worth keeping.

**A test that named a boundary but did not straddle it.** The first
boundary test asserted `("ab","c")` and `("a","bc")` must hash differently. They
do — and it could not tell a correct implementation from a comma-joined one,
because `"scene,ab,c,,"` and `"scene,a,bc,,"` are different strings. The
mutation survived. The real collision needs a separator *inside* a field:
`title="a", date="b,c"` and `title="a,b", date="c"` both join to
`scene,a,b,c,,`. A test that names a boundary must be built from the boundary's
actual arithmetic, not from a pair that merely looks adjacent. This is the
fourth time this shape has appeared in this repo, and the rule is now in the
`codebase-invariant-testing` skill.

**Two engines, two behaviours, and only one of them loud.** Binding
`organized` as `COALESCE(?, organized)` to let `None` take the schema default
works in neither engine: inside an INSERT's VALUES list there is no `organized`
row in scope, so SQLite fails the statement outright and Postgres treats it as a
column reference and never substitutes anything. The default now lives in Rust,
and because that duplicates the migration, a test reads the default out of the
live schema on both engines and asserts the two agree.

**A repo-wide invariant caught the new tests, and was right to.** The
`consent_filter` scan fails on any test file that reads `object` without a tier
predicate — including mine. The fix was to make the reads carry the predicate,
not to add an allowlist entry: these tests assert the consent tier of the object
they made, so the read should be under a stated tier.

### T-P5-006 item 9, part 2 — the media route, and three survivors in a row

The `Range` parser, the `/media/:object_id` route, and the same lesson from a
third direction. Worth its own heading because every step of it went wrong in a
way the previous two did not.

**A half-open range and an inclusive header are the same bug twice.** The wire
form is `bytes=0-99` inclusive; the internal form is `start..end`. The first
version of `ByteRange` stored the wire value and documented itself as half-open,
so `len()` was `end - start` and every length was one short. The tests caught it
-- but only because they asserted `len` and the `Content-Range` string
separately, which is the shape a range test needs and is not the obvious one to
write. The conversion now lives in exactly two places (`resolve` and
`content_range`) and `scripts/mutate-range.sh` kills both directions.

**A mutation survived because every fixture agreed with itself.** Setting the
route's `account_id` to `None` -- making it ask as a visitor rather than as the
library owner -- passed all fifteen route tests. Every fixture was seeded at
`self_published`, which is in `ConsentTiers::PUBLIC`, so "allowed because the
caller is the owner" and "allowed because everyone is" were indistinguishable.
The `unverified` fixture kills it, and `unverified` is not exotic: it is what a
freshly scanned file is, so it is the state every new user's library is in.

The same thing again on the length: resolving a `Range` against the index's
*recorded* size instead of the on-disk size also survived, because every fixture
wrote a file whose length matched its row. A file replaced since the last scan is
the case that matters, and there is now a fixture that is deliberately
inconsistent.

That is the third time in this repo — after `media-view.test.ts` and the feed's
preload test — that a fixture whose every element falls inside the filter cannot
test the filter. It is worth a rule rather than three anecdotes: **when a test
is about a boundary, the fixture must straddle it.**

**A compiler warning was the only thing that caught a dead security branch.**
The route resolved the `Range` twice: once against the recorded size, once
against the file on disk. The first result was assigned to a variable rustc
reported as unused, so the entire first `match` -- including the 416 arm -- could
not execute. Every test was green, because the surviving second `resolve`
returns the same answer for every range a test would send against a
correctly-sized file. A test cannot find this; the compiler can, and did, in a
warning that was one line above four other warnings.

**A harness that reports the wrong answer is worse than no harness.** Three
separate scripts in this item reported mutants incorrectly, each differently:

- `mutate-feed.sh` grepped for `0 failed`, which Playwright never prints, and
  read `tail -1`, which only ever sees the pass line -- so it reported the
  inverse of the truth for all five mutants and looked like five survivors.
- `mutate-range.sh` checked for `error:` before the test result, and cargo
  prints `error: test failed` at the end of every *failed* run -- so every
  killed mutant read as "did not compile".
- `mutate-media-route.sh` printed a bare `no result` for a mutant that neither
  killed nor compiled, which cost a full manual investigation to trace to a
  `s.replace` that matched a *prefix* of a `cargo fmt`-reflowed line and spliced
  half an expression into the file.

The route's mutations are now one file each under `scripts/mutations/`, each
ending in an `assert` naming the text it could not find. The reason is not
tidiness: an unquoted shell heredoc eats the backslashes in a regex, so a
mutation written inline as `\s` arrives at python as `s`, and the failure looks
like a coverage problem rather than a shell problem.

**And one that is a fact, not a gap.** `scripts/mutate-media.sh` records
`JOIN consent` -> `LEFT JOIN consent` as a known survivor, with the reason: the
clause is `c.tier IN (...)`, so under a `LEFT JOIN` a missing consent row yields
`c.tier = NULL`, the predicate is false, and the mutant returns exactly the
unmutated answer. There is no test to write, because there is no observable
difference to observe.

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

### T-P5-006 item 3 — what this cost, and why the shapes are what they are

Five bugs, four of which no unit test could have found, and two of which were
caught by a *different* test failing for an unrelated reason.

**1. `Store::apply_tag` had no consent check, and now the bulk path does.**
`apply_tag` still takes no `CallerId` -- it is unchanged, with ~15 test callers.
That was survivable when a write named one object. It stops being survivable
when a write names four thousand, so `bulk_apply_tag` takes a `CallerId` by
reference with no default and no other constructor, exactly as `Store::query`
does. A missing clause does not error there, it *silently over-reports*, so the
tests that matter assert the negative: an unverified object is not written and
`skipped_invisible` says so.

**2. `set_consent_tier` and `insert_object` were SQLite-only.**
Both called `store.pool()`, which panics on Postgres. Both are inputs to every
visibility decision in the store. Invisible for as long as every caller was the
SQLite scanner; the first Postgres test to set a consent tier found it. Now both
`match` on the engine.

**3. `redistribution_permitted` is `INTEGER`, not boolean.** On both engines. My
first Postgres version bound `false` and Postgres refused to coerce it into the
column. The schema is the authority; the fixture had guessed.

**4. There is no `public` consent tier.** The public tiers are
`self_published`, `performer_claimed`, `third_party_permitted`. My first fixture
used `"public"`, which made every object invisible to every caller -- and the
tests passed *vacuously* with `applied: 0`, because the assertion was `== 1` on a
path that had already failed earlier. The fixture now names
`ConsentTiers::PUBLIC[0]`, so a tier rename cannot silently re-break it.

**5. `tests/invariants.test.ts` caught a REST API I invented.** The first
version of the route used `fetch('/api/bulk/tag')`. Every functional test passed;
the invariant failed, correctly, because no module outside the transport may
name `fetch`. The write is now a GraphQL mutation (`bulkApplyTag`) on the same
path as every other request, and the e2e stubs one route with a per-operation
switch.

**Svelte-specific, and both cost real time:**

- A destructured prop is a plain local. `let { open } = $props()` then reading
  `open` inside `$effect` registers no dependency, so the effect ran once at
  mount and the dialog never opened -- no error, no warning, a modal wired
  correctly to a prop that could not change. The component now keeps the props
  *object* and reads `p.open` where a tracked read is needed.
- `canApply` gated on the outcome, and the outcome is what pressing the button
  produces. The gate is now the *scope*, which is known before any write, and the
  outcome only gates a re-press.

**A prop that was passed but never declared.** `error={bulkError}` reached the
component and was silently dropped, because `Props` had no `error`. The first
version had a local `let failure` instead -- write-only, so the branch rendered,
the markup was tested, and no code could ever reach it. That is the shape a stub
takes, and it is why the error is a prop: the request belongs to the parent.

**Mutation results, both new scripts.** `mutate-bulk.py` 8/8, `mutate-bulk-ui.py`
9/9. The two Rust survivors on the first run were both real gaps in the tests,
not in the code: a provenance test that only ever *inserted* (so the
`ON CONFLICT DO UPDATE` half was unproven) and a missing-tag test that asserted
only `is_err` (which a foreign-key violation also satisfies). Both now take the
same path twice.

### T-P5-006 item 4 — the guard that destroyed what it was guarding

The plan's floor for this item is one test: navigate with an unsaved edit,
assert a confirm appears. It exists and passes. It is also not the interesting
part, because the obvious implementation of a navigation guard passes its own
test and is still wrong.

Watch `page.url`. See the navigation. Call `goto()` back to where the user was.
SvelteKit destroys the outgoing page component when a navigation commits, so
that does not put the user back on their page — it mounts a fresh one. Every
`$state` in it resets. An in-progress edit is destroyed by the very navigation
the guard performed to protect it, and the confirm then renders over a
now-clean page reporting nothing to save.

The dialog appeared. The URL did not move. The test passed.

The trace that gave it away, once I stopped guessing and started logging: the
surface's effect logged `dirty=true`, the guard fired, and then the *same*
surface logged `dirty=false draft=""` and deregistered itself. The guard had
unmounted the thing it was guarding.

So: intercept, do not undo. A capture-phase `click` listener on the document
runs before the browser follows the link, so `preventDefault()` there stops the
unmount outright — there is nothing to undo afterwards. The decision is held as
a thunk and run only once the user has answered.

Three things that cost the afternoon, all of them silent:

- An `$effect` on `page.url` that also *wrote* `lastUrl`. Svelte kills that
  with `effect_update_depth_exceeded` after a thousand rounds, and the symptom
  is an empty dialog over a genuinely unsaved edit, because the loop starves
  the render. The bookkeeping is a plain `let` now: it is never rendered.
- A boolean set in a click handler and read inside an `$effect`. Not a
  synchronisation bug — the effect is scheduled by `page.url`, so its body can
  run before the handler's write lands. Recording the URL the handler saw and
  comparing it puts both reads at a moment the watcher controls.
- Deregistration on unmount was missing entirely, so a registration outlived
  its component: the library page opened with "Unsaved changes" in the toolbar
  forever, with no editor on it. Adding `onDestroy` then broke the back button,
  because the unmount clears the registry one step before the guard reads it.
  Hence `guard.last`, written by `add` and by nothing else — a surface
  unmounting is a *consequence* of the navigation and must not erase the
  evidence. `consume` clears it, because that is the one caller meaning "yes,
  lose it".

**Known limit, documented rather than papered over.** For a history navigation
— back button, form submit, a `goto()` from code — there is no click to
intercept, and by the time the URL watcher runs the page has already unmounted.
`beforeunload` does not fire for same-document navigations. So "stay" means *go
back to the page you were on*, and the unsaved edit does not survive the round
trip; the confirm still names what was at risk, which is what §10.7 and §10.10
ask for. A tab close is worse still — only `beforeunload` sees it and only the
browser may render a prompt — so that case gets `beforeunload` plus a visible
indicator and nothing more. Full reasoning in
`docs/spec/t-p5-006-unsaved-guard.md` §3.

What the mutation script found: both survivors the first run were gaps in the
*tests*, and one was in the *code*. Replacing `if (!isDirty(reg))` with a size
check in `summary` changed nothing observable, which is the script's way of
saying that guard is dead. It was. Deleted, and the test that should have caught
it now exists — every earlier `summary` test asked about a registry with
something in it, and the empty case renders "You have ." into the dialog.

| T-P5-006 | 4 | unsaved-entry guard: `guard.ts` + `+layout.svelte` | done | `ui/src/lib/api/guard.ts`, `guard-store.svelte.ts`, `ui/src/routes/+layout.svelte`, `ui/src/lib/components/EditSurface.svelte`, `ui/e2e/guard.spec.ts` (9), `ui/tests/guard.test.ts` (24), `scripts/mutate-guard-ui.py` (10/10), `docs/spec/t-p5-006-unsaved-guard.md` |

### T-P5-006 item 5 — command palette and shortcut map

`docs/spec/t-p5-006-commands.md`. Commit on the `phase-5-006-commands` tag.

One `keydown` listener on `window` in the shell, resolving through a command
registry (`ui/src/lib/api/commands.ts`) with a scope stack. Before this there
were three independent listeners — the lightbox, the bulk modal, the list table
— so one `Escape` was interpreted by components that could not know about each
other. #2833 is not a feature that can be added to that arrangement; it is what
having one resolver makes possible.

- `keys.ts` — `Chord`, `normalizeKey`, `chordsEqual`, `inTextField`, `formatChord`
- `commands.ts` — `CommandRegistry`: register/drop/resolve, the scope stack, the
  palette filter, `reorder` for the #6218/#5587 selection shortcuts
- `commands-ui.ts` — `handleKey`, `paletteRows`, `moveHighlight`
- `command-bindings.ts` — the app's eight commands and their keys
- `CommandPalette.svelte` — the dialog, mounted in the shell

Three decisions worth knowing about, because each replaced something that looked
right:

**The palette chord is a registered command, not a special case.** The first
version recognised `Ctrl+P` in the keydown handler and returned a magic
`'opened-palette'` outcome; the layout discarded the return value, so the chord
was correctly recognised, correctly `preventDefault`ed, and did nothing at all.
Every layer reported success and no palette appeared. It is now a command with
`global: true`, resolved before the scope stack, so a modal cannot swallow it
and the palette lists it like everything else.

**A command may decline.** `run` returning `false` leaves the key to the
browser. The bulk modal's `Escape` depends on it: its frame wins the key so the
app scope's `select.clear` does not run behind an open modal, and then declines
so the browser's native dialog dismissal still happens. Winning-and-preventing
would have been a modal that will not close.

**Disabled is a state, not a filter.** A command needing a selection is shown
greyed out rather than hidden — hiding it makes the palette an incomplete map,
and "why is bulk tag not in here" is a worse answer than "it is, and it needs a
selection".

Tests: 226 UI unit (`tests/commands.test.ts`, `tests/commands-ui.test.ts`,
`tests/commands-rank.test.ts`), 53 e2e, and `scripts/mutate-commands-ui.py` at
43/52 with **zero real survivors** — the other nine are documented as
known-equivalent in the script, each with the reason it is unobservable.

The mutation pass earned its keep twice. It found that every mutant in
`commands-ui.ts` survived, which is true: no unit test had ever called
`handleKey`, because the e2e only asserts what a user can *see*, and a
`preventDefault` has no visible consequence. And it found that my own ranking
tests all used title pairs differing on two axes at once, so removing any single
ranking rule left the right answer on top anyway. Both are now covered by
single-axis cases.

§10.7 is met except for one thing, recorded in the spec as §3.4: the three
selection-and-bulk bindings are registered and listed but their *handlers* live
on the list route, which owns the selection, so the shell delegates them as a
`CustomEvent` and cannot confirm they ran. `view.search` is likewise registered
without a focusable target. Both are visible in the palette rather than hidden,
which is the honest state, but they are not complete and the spec says so.

### T-P5-006 item 6 — folders, and a latent failure that had been there since T-P5-001

`crates/commons-store/src/folders.rs`, migration `0018_folders.sql`, spec
`docs/spec/t-p5-006-folders.md`. 32 tests, both engines. Rust total 1198.

The ticket reads as a sidebar — a tree, a name, a drag handle. What had to be
built was the query half, and the reason is worth more than the ticket.

**`Filter::Saved { id }` has been in the AST since T-P5-001, and it compiled to
`o.saved_filter_ids LIKE ?` — a column no migration creates.** Any filter naming
a folder failed at the database with "no such column". Nothing caught it because
the only test touching that variant asserted its serde shape and never ran the
SQL, and because the whole path was unreachable: there was no table, no
resolver, and no UI to reach it with. It now returns
`FilterError::UnresolvedSavedReference`, which names the missing step instead of
a column that does not exist.

Three bugs, all found by tests that run the schema rather than the resolver:

- **A cut filter-cycle must be `Or([])`, not `And([])`.** `And([])` compiles to
  `1 = 1`, so the first version's cycle guard made a self-referencing folder
  match the *whole library*. It looks right — a folder with a lot in it is not
  obviously wrong — and nobody finds out until a filter on `tagged = "x"` returns
  things not tagged x. `Or([])` compiles to `1 = 0`.
- **The tree cycle walk has to descend, not climb.** Seeding a recursive walk at
  `NEW.parent_id` and asking how deep it goes cannot detect a cycle: the
  proposed parent is *below* the moved row, so moving `p` under its own child `c`
  seeds the climb at `c`, which reaches `p` and stops. The write is allowed and
  every later walk up the tree is an infinite loop. This one was verified against
  a live database before it was fixed — the reparent succeeded and a recursive
  query over the result hung until it was killed. The walk now descends from
  `NEW.id` and asks whether `NEW.parent_id` is among its descendants.
- **A plpgsql variable and a CTE column both named `depth`.** `MAX(depth)` is
  then ambiguous, and the trigger fails on every insert that has a parent — every
  insert but a root. The table looked completely fine and nesting was simply
  impossible.

**A folder holds no objects.** There is no `folder_members` table and no object
column; membership is recomputed per query, which is what makes §5.14's
"re-evaluated on every open" true rather than aspirational. The obvious
alternative — a denormalized `saved_filter_ids` that the existing broken SQL
already expected — is a cache of the filter's answer stored on the row, and a
filter edit would leave it stale until each member was touched.

`tests/folders_db.rs` exists because `tests/folders.rs` could not have found any
of the three. That file tests the resolver over a literal map; it cannot tell
whether the table exists, whether the partial index covers the roots, or whether
a trigger fires. The harness's own header says it: "It proves the tree applies. A
migration that applies for the wrong reason still applies."

### T-P5-006 item 7 — undo for destructive actions

`crates/commons-store/src/undo.rs`, migration `0019_undo.sql` on both engines,
`tests/undo_db.rs`, `ui/src/lib/api/undo.ts`. 1217 Rust, 237 UI, clippy clean.

An undo is a **recorded inverse**, not a re-derivation, and the reason is
`bulk_apply_tag`'s `ON CONFLICT DO UPDATE`: an object that already carried the
tag has its `confidence` and `source` *replaced* rather than gaining a row, so
what was there is not recoverable from the row afterwards. The record stores the
before and after state of every object the write **changed** — not every object
it reached. `bulk_apply_tag` is `ON CONFLICT DO UPDATE`, so a row that already
had the tag has its `source` and `confidence` *replaced* — every object the
write reaches is a row it changes, and the inverse is a real restoration, not
nothing. (Measured, not assumed: a probe with a pre-existing row at
`confidence 0.3 / source manual` came back `applied=1, confidence 0.9, source
bulk`.)

Four things worth knowing before touching this:

1. **`row_existed` is stored because it cannot be derived.** All three value
   columns are nullable (0016 added them to a table that lacked them), so three
   NULLs are indistinguishable from no row — and a row of three NULLs is what
   every `object_tag` row written before 0016 is. Restoring that as "absent"
   deletes a row that existed.
2. **A superseded write is refused, not applied.** Restoring `before` onto an
   object that has since changed does not put it back; it moves the object into
   a state nobody was ever in and discards the edit that superseded the one being
   undone. `undo()` is **two passes**: read every object's state and refuse
   before writing anything, then write with the expected state in each
   statement's own `WHERE` and treat the row count as the check. The repeat is
   what survives a commit landing between the passes.
   `IS NOT DISTINCT FROM`, not `=`, or a row of NULLs refuses itself.
3. **`INTEGER` decodes as `i32`.** INT4 on Postgres, INT8 on SQLite. A decode
   asking for `i64` passes on SQLite and fails on Postgres; the parity test
   checks column *names* and cannot see this. Same rule as `relations.rs`.
4. **`tag_id` is on the record, not a parameter.** A caller that supplies it can
   supply the wrong one, and the staleness check still passes, because it reads
   `after` from the record and compares against the tag the record names.

**Known gaps, both named rather than left to be found:**

- **Two atomicity gaps, both needing a transaction the store cannot express.**
  (a) The write and its record are not atomic: `bulk_apply_tag` is a single
  `INSERT ... SELECT`, so making them one transaction needs that statement on a
  connection `record_undo` also holds. The failure left is a write with no undo
  — the safe direction, since the user is told the write happened. (b) `undo`'s
  write loop is not atomic *across objects*: a failure on the third leaves the
  first two applied. Both need a transaction, and `StoreError` has no
  transaction variant. (a) is fixed in `bulk.rs`, which owns the error type; (b)
  is fixed in `undo.rs` by taking one.
  This is the one claim the first version got wrong in the dangerous direction —
  the doc said all-or-nothing unconditionally, and a two-object stale-record test
  caught it restoring the first object before refusing the second.
- **No route — and that is not this item's gap.** The offer renders in
  `BulkEditModal.svelte` beside the result line, not in a floating toast: the
  modal is `showModal()`, so a toast raised while it is open is unclickable, and
  item 3's own code already says why ("a toast that outlives a window the user
  has closed reports a failure against a dialog they are no longer in"). There
  is also no GraphQL operation, and that is true of
  every write in the project: `client.ts` is the only module allowed to name
  `fetch`, the whole client is queries, there is no mutation document in
  `ui/src/lib/api/` at all, and there is no server-side schema in the repo.
  `bulk_apply_tag` has no route either. Worth knowing before someone reads the
  missing undo route as a hole in item 7 rather than a phase-wide gap.

Expiry is enforced **on read** (`undoable()` and `undo()`), never by a sweeper —
a sweeper is a second thing to run, schedule, and notice has stopped. Rows are
never deleted: an expired record is invisible and inert, not gone.

**The record is written BEFORE the write it reverses**, which is deliberate and
the opposite of the obvious order. A data-modifying CTE would make them one
statement and SQLite refuses it outright (`near "INSERT": syntax error` — an
`INSERT` as a CTE body is not SQLite syntax), and a transaction is refused
project-wide for a reason `search.rs` already records. So the ordering carries
the safety: a crash between the two leaves a record describing a write that
never happened, and undoing that is refused by the staleness check and expires
on its own. The other order leaves a write the user cannot reverse.

**The restore is atomic across objects**, and this took three measurements to
get right. It is one statement per shape over a `VALUES` CTE, and a statement is
atomic — so no transaction, which matters because `StoreError` has no
transaction variant and adding one is a change to shared infrastructure. Three
things had to be measured rather than assumed, and each fails *quietly* rather
than loudly:

- `UPDATE … FROM (VALUES …)` is a syntax error on SQLite. Only the CTE form
  works on both, and the `FROM e` is mandatory.
- The two flag columns are bound as **booleans**, never integers. A `VALUES`
  list fixes one type per column across all rows, so an integer flag becomes a
  column the engine may type as anything — and `= 1` then matches *too few*
  rows on SQLite and errors on Postgres. Casting does not fix it; binding the
  right type is the only spelling that survives.
- The `?` bind order follows the placeholders' **textual** order, so the
  `VALUES` block owns the low-numbered ones. Reversed, every entry shifts by
  one and the statement reports zero rows affected with no error.

This is the kind of code where a passing test suite is not evidence. The bug
that started this was a loop that wrote the first object before discovering the
third was stale; the tests all passed until one was written that put *different
rows* through the *same statement*.
