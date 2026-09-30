# Handoff

Where Commons is, what is verified, and what is deliberately not done.

**As of:** 2026-09-28 · **Branch:** `main` · **Remote:** `origin` =
`https://github.com/8ullyMaguire/commons`; every commit is pushed, and every
milestone carries an annotated `phase-*` tag.

---

### Interviews: storage done, on both engines, with a correction that cannot overwrite

T-P6-004, in progress. Spec `docs/spec/t-p6-004-interviews.md` — **read §6a
first**; it is six portability bugs the two-engine test found, each of which
produced a wrong answer rather than an error, and each of which is a shape this
schema has now been bitten by before.

Done and tested: the ASR engine seam and window arithmetic
(`crates/commons-ml/src/asr/`), the store module and migration 0023
(`crates/commons-store/src/interview.rs`), and **both** of the ticket's accept
criteria. 24 store tests × 2 engines, 7 timing tests. 96 test binaries / 1684
tests green, workspace clippy clean, tag `phase-7-040-interview-store`.

**The correction criterion is the one the design turns on.** There is no
`update_word` in the module and nothing anywhere that writes `interview_word`
outside `replace_transcript`. A corrected word becomes a `FieldProposal`, and
the test that matters asserts the model's words are **unchanged** afterwards —
an implementation that wrote through would pass every other test in the file
while destroying the evidence. Once the model's output is gone nobody can tell
a misheard word from a typo, and a user correcting the same word twice has
nothing to compare against.

Two details that are constraints rather than choices: the field is
`transcript_word[<ordinal>]`, not a bare ordinal (ordinal 12 of a two-hour
interview and of a four-minute one are unrelated words), and a correction
**outlives a re-transcription** — a model update replaces the words, and
discarding the human decisions with them erases the reason the user re-ran it.

**Both engine adapters are done, and parakeet is a subprocess after all.**
whisper.cpp parses its own JSON (12 tests, no binary required). parakeet is a
Python sidecar over a pipe, and the spec's §3.1 was **revised in place** to say
so — the original text chose in-process "because the memory is bounded by chunk
size", which was reasoning about the model rather than about the repository.
The three reasons it changed, in order: an in-process ONNX path cannot be
tested here at all (HuggingFace 401s, no model fetchable, no runtime
installed), so it would ship with nothing but a clean compile as evidence; a
subprocess cannot wedge a scan, which the original argument applied to
whisper.cpp and then stopped; and `tract-onnx` pulls ~20 crates for one optional
feature. What survived is that both engines verify the digest before loading.

The 11 sidecar tests run the **real Python over a real pipe** against a fake
`onnxruntime`, so the protocol is verified on a machine with no model. Two
things worth knowing: the fake must be named `onnxruntime.py` (anything else is
importable by nobody and every run reports "onnxruntime is not installed",
which reads like a broken environment), and the blank index is a **parameter**,
not a constant — CTC puts it last, TDT first, and guessing emits a word per
frame of silence. The decode's run-tracking was wrong twice before it was
right: a run's start was set after the word was emitted, so every transcript's
first word was timed from frame 0.

Not done: chapters/quotes/topics as `Marker`s and weighted `Tag`s, speaker
attribution into `PersonCluster`, and the model-backed half of the timing test
(written, loudly skipped — not counted as passing).

**A test that could not fail, found by mutating the code it covers.**
`dropping_a_supervised_child_leaves_no_zombie` counted `ps -eo stat= | grep -c
'^Z'` — the whole machine's zombies — and failed at 13 against a baseline of 7
during a run with three cargo invocations in flight, having reaped all 20 of its
own children correctly. Rewriting it by identity exposed a second bug: in
`ps -eo pid=,ppid=,stat=` the pid comes **first**, so `$1 == me` matches the test
process's own row and never a child. That version passed with the reaper
neutered — an assertion that cannot fail. With `$2`, and `wait()` removed, it
names all 20 leaked pids. Worth knowing: Python's `Popen.__del__` reaps, so this
condition cannot be reproduced from a Python probe at all.

---

### Subtitles: complete, and four more bugs that all failed silently

T-P6-002, done. Spec `docs/spec/t-p6-002-subtitles.md` — read §9 first, it is
the live done-when table. The parsers and the schema were steps 1–3; the rest of
this section is what came after, and every bug in it is one that cost a user a
subtitle track, a wrong caption, or a silent video **with nothing in the logs**.

**The four that are worth carrying to the next ticket:**

- **WebVTT has no hours field.** The grammar is `MM:SS.mmm`; ffmpeg writes that;
  and a parser that demanded three fields read *every* cue of a `mov_text` track
  as 0:00. The whole track fired on the first frame, and the document parsed
  cleanly, so nothing reported it. The fix is to treat the field *count* as the
  discriminator — and that has a consequence which is a constraint rather than an
  accident: in the short form minutes are unbounded, so `90:00.000` is ninety
  minutes and must parse.
- **A cue with an unreadable timestamp was being invented.** The parse path
  pushed `Cue::new(seq, 0, 0, text)`, commented as keeping "a missing caption
  from being invisible". A zeroed cue exists, is listed, and fires at 0:00 —
  strictly worse than dropping it. It is an error naming the line now.
- **`-map 0:<n>` wants ffprobe's ALL-STREAM index, not the ordinal among
  subtitles.** With a video at 0 and a subtitle at 1, the ordinal is 0, and
  `-map 0:0` selects the *video*. ffmpeg does not complain; it transcodes the
  picture as a caption. The acceptance test caught it by probing the OUTPUT — a
  test asserting exit status would have passed, and been wrong.
- **A `<track>` is a CHILD of the `<video>`.** A sibling is well-formed markup,
  renders nothing, is never fetched, and produces no console error: the video
  plays, the track list populates, and the user has no subtitles. Only the
  Playwright test that asserts parenthood can see this. The same shape of failure
  took out a first attempt at the component, which put a `<div>` inside the
  video element next to the tracks.

**Two decisions that look arbitrary and are not:**

- **A sidecar language is not validated.** `foo.forced.srt` has the language
  "forced" — wrong as a language, harmless as a label, and better than refusing a
  file that is sitting right there. Normalising a language is the store's job and
  happens once; doing it in the discovery layer too would be the second place.
- **The stem splits on the LAST dot.** `My.Movie.2024.mkv` has the stem
  `My.Movie.2024`; a first-dot rule gives `My`, which matches `My.srt` and not
  the real sidecar — so a dotted title finds nothing *and* a neighbouring file
  gets claimed by something else. Release titles are dotted constantly.

**The measurement.** The ticket's budget was 40 ms of timestamp drift through a
transcode round-trip. The worst case is **0 ms** for srt and mov_text and under
1 ms for ass, because milliseconds are integers end to end and nothing in the
path rounds. The 40 ms was not spent; the reason is worth more than the number.

**Not claimed, on purpose:** caption search (#4985), multi-language entries
(#5514), DLNA exposure (#5420), external-player injection (#2770). They belong
with T-P6-004, T-P6-007 and T-P6-005. The plan says so, and the plan's
`plan-status.py` now fails the build on a claim whose files are absent, so the
distinction is now enforced rather than merely written down.

**`commons-media/src/subtitles.rs`** — `parse` for srt, vtt, ass/ssa, `to_webvtt`,
and a pure `Cue`. 35 tests, no file, no process. The separation is the point: a
parser tested only by round-tripping through ffmpeg cannot tell a parser bug from
an ffmpeg bug, so the parsers take bytes and the process never enters it.

Six parser bugs, found by tests written from the format specs rather than from
the code. Two are worth carrying:

- **A karaoke tag applies to the syllable *after* it.** `{\k20}ka` attaches
  `{\k20}` to nothing and `ka` to the timing. Writing it the other way round
  produced `"kadoke"` for `{\k20}ka{\k30}doke`, which is a real word, which is
  why it survived my first two passes. Every hard line break in every ASS file
  also survived as a literal `\N`, because `\N` is in the *text* and not inside
  an override block where the stripper was looking.
- **A seconds field with two separators is neither half.** `"01.000,500"` — the
  fractional part is after the **last** separator, and the part before it is not
  a valid integer. The obvious split on the first separator leaves `"01.0"`, which
  does not parse, and dropping *all* separators glues the digits into `"01000"`.
  The answer is to take the last separator as the fraction's and keep everything
  before it verbatim.

**`commons-store/src/subtitles.rs`** and **`0021_subtitles.sql`** — both engines,
7 tests each. Three bugs, all of which are silent in production:

- **A document is identified by its track, not its digest.** The unique index was
  `(object_id, sha256)`, which is the obvious key and it *deletes a user's
  subtitle track without an error*: a film with an English and a forced-signs
  track commonly has identical text in both files, so the second write reads as
  "unchanged" and is skipped. Now `UNIQUE (object_id, origin,
  COALESCE(stream_index, -1), COALESCE(language, ''), format)` — where a track
  *is*, not what it contains — and the store's skip uses the same tuple as a
  `TrackKey`. The `COALESCE`s are load-bearing: NULL never equals NULL in a
  unique index on either engine, so two sidecars of one language collided.
- **`migration_parity` cannot see a type.** It compares column *names*, and
  `INTEGER` is a valid name on both engines while meaning INT4 on one of them.
  `stream_index` and `byte_size` were `INTEGER` and decoded as `Option<i64>`:
  they applied cleanly, wrote cleanly, read cleanly on SQLite, and failed **at
  read time on Postgres and nowhere else**. Only the store test found it, and only
  because it round-trips a row through both engines. Every `i64` column is now
  `BIGINT` on Postgres, with the reason in the migration, because the mirror looks
  wrong side by side and will be "corrected" otherwise.
- **`placeholders(n)` is not `placeholder(n)`.** One character apart, and they
  mean opposite things: the first is the *list* 1..=n, the second the nth. A
  five-slot query built with the first produced eleven placeholders for five bound
  values, and Postgres reported it as `syntax error at or near ","` at whatever
  character offset the extra comma landed on. `placeholder` now exists beside it
  with that in its doc comment.

The general lesson is the one the first of those three is named for: **a test
written after the schema encodes the schema; a test written before it encodes
what the schema is for.** `two_languages_of_one_object_are_both_kept` existed
before the index did, which is the only reason it was there to catch it.

---

### Two remotes, and a check that they agree

The repository is mirrored to two private remotes and `verify.sh` now ends by
comparing them:

| Remote | Where |
|---|---|
| `origin` | `github.com/8ullyMaguire/commons` |
| `forgejo` | `git.polarisocial.xyz/hirrolot19/commons` |

Both private. `git pushall` (an alias for `scripts/pushall.sh`) moves both; a
bare `git push` still goes to `origin` alone, deliberately unchanged, so nothing
about the existing workflow moved.

The alias was first written as an inline shell function using `git push -q`
chained with `&&`, and it **exited 0 having pushed nothing**: `-q` swallowed the
output and the trailing `&&` made the function's status the second push's. A
commit followed by a `&&`-chained `pushall` therefore reported success while the
remotes stayed a commit behind, and only a `git ls-remote` comparison noticed.
It is a script now, no `-q`, and it prints the remote before each push.

The check compares `HEAD` and the tag set per remote against local, over the
wire, and is the half-state guard: a push to one remote and not the other leaves
local looking complete and the mirror quietly behind. Proven by pushing a
throwaway tag to `origin` only and watching the check fail, then removing it.

One trap in writing it: `git ls-remote --tags` emits **two lines per annotated
tag** (the tag and its peeled `^{}` ref), so counting lines reports 91 for 48
tags, and counting only peeled refs reports 43 and reads as five missing on both
remotes. It has to count **distinct tag names**. Both remotes do hold all 48.

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

`crates/commons-api` is no longer an empty placeholder — that was written at
Phase 2 and is now false. T-P6-008 (at `2eb3fff`) put a `POST /graphql` server
in `commons-server` and the wire types in `commons-api`, and one UI test now
runs the real client against the real binary. The rest still stub the network,
which is correct for testing the client and wrong for testing the wire.

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

## Twelve things to know before writing more code here

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

### T-P5-006 item 17 -- the unified media view, and the last item

#1030 asked for a Media tab combining scenes and images. The model turned out to
be ready: spec 5.3 says "Image is an Object, so it votes like one", `object.kind`
is an indexed TEXT column on the ONE table, and `CmpOp::In` already compiles a
value list. So the whole server side is one facet and there is no join.

    ui/src/lib/api/media.ts                    membership, ordering, the facet
    crates/commons-store/tests/filter_wire_shape.rs   the wire shape, from Rust
    ui/src/routes/media/+page.svelte           the tab
    ui/tests/media.test.ts                     65 tests
    ui/e2e/media.spec.ts                       10 tests
    scripts/mutate-media.mjs                   29 mutants, all killed
    docs/spec/t-p5-006-media-view.md           the spec

**All 17 items of T-P5-006 are now implemented.**

The load-bearing finding: `object.kind` is unconstrained TEXT in the database and
an enum in commons-core, and NOTHING keeps them in sync. So an unknown kind is an
ordinary event, not corruption -- and a grid that drops those rows looks correct
and silently loses content. So the tab audits every page and names what it could
not place, and KINDS is asserted equal to `ObjectKind::as_str` BY READING THE
RUST FILE. A kind added in Rust and not here is a test failure, not a tile that
renders as a photograph.

The filter shape is checked rather than assumed. Three details of serde's
external tagging are invisible if you write the obvious thing -- `field` is
`{"builtin":"kind"}` not `"kind"`, a value is `{"str":"scene"}` not `"scene"`,
`op` is `"in"` not `"In"` -- and getting any wrong means the server rejects the
filter and the tab looks like an empty library. The shape is now emitted as data
from a Rust test and compared on BOTH sides, so a change to serde's attributes
fails a test rather than a tab.

Two decisions worth knowing about:

- **`media.ts` is not `media-view.ts`.** Item 8 owns tile SHAPE; item 17 owns
  MEMBERSHIP and ORDER. A test asserts item 17 exports no ratio function of its
  own, because two copies of ASPECT_LIMITS disagree within a release and the
  disagreement is a wall whose images and scenes do not line up. The two
  `isPlayable` functions disagree about a zero-duration scene ON PURPOSE -- a
  tile has only the duration to go on, membership has the kind -- and a test
  names that disagreement so it does not read as a bug.
- **The tab REPLACES `?q=`, and does not honour it.** The filter is opaque by
  design ("it is the thing the server parses"), so a kind restriction cannot be
  ANDed onto a user filter without decoding it. This is a real gap, documented in
  the route header and the spec, and the first thing to fix when the filter codec
  moves client-side. It is the honest answer rather than a guess that looks right.

Also: verify.sh now parse-checks every `scripts/mutate-*.mjs`. Three of them
shipped with a syntax error -- an apostrophe in a single-quoted note, a multi-line
pattern in a single-quoted string -- and the symptom reads as "the mutation script
found nothing" rather than "the script never ran".

### T-P5-006 item 16 -- the folder view

#1586 was filed under C50 and 9.5, which turned out to mean the VIEW, not the
model: `crates/commons-store/src/folders.rs` has been done since item 6. What
was missing is the thing a person touches.

    ui/src/lib/api/folder-tree.ts          the pure tree
    ui/src/lib/components/FolderPane.svelte  search, trail, tree, reports
    ui/src/routes/folders/+page.svelte     pane + wall, all state in the URL
    ui/tests/folder-tree.test.ts           46 tests
    ui/e2e/folders.spec.ts                 19 tests
    scripts/mutate-folder-tree.mjs         25 mutants, all killed
    docs/spec/t-p5-006-folder-view.md      the spec

The design fact that drives the view is item 6's: a folder holds no objects, so
opening one REPLACES the filter rather than narrowing it. The pane shows the
running filter in a visible slot for exactly that reason -- a user who arrives
with `?q=cat`, opens "Untagged" and gets a column of tagged items would file a
bug.

Two real bugs, both found by mutation and both producing a PLAUSIBLE navigator
rather than a crash, which is why reading the code was not enough:

- the root loop placed children as well as parents, so every folder rendered
  twice -- once nested, once loose;
- `reachable` was written to treat "returning to the start" as reachable. In
  `a -> b -> a` no folder is a root, so the loop skipped both and BOTH SILENTLY
  VANISHED. A path that returns to its start is a cycle, and a cycle has no root.

A folder is a URL, and a folder that is not linkable is not a place. Opening one
expands its ancestors, so the pane shows where you are rather than collapsing to
one row. Broken data is reported rather than swallowed: the route fixture
deliberately contains an orphan and a parent cycle, because a folder that
silently never appears is the worst outcome a navigator has.

Counts are deliberately a dash, not a zero -- membership is recomputed on open,
so counting a folder is a query per folder, and a column of zeroes before the
counts land looks like an empty library. Follow-up once the server can count
cheaply.

### T-P5-006 item 15 -- windowing the wall, and closing 4.2

Item 14 recorded its own gap: the wall virtualized per PAGE, not per pixel, so a
group holding 50,000 rows rendered 50,000 tiles. That is not cosmetic. Section
4.2 says virtualization throughout, and the DOM is what stops the browser.

**Why it was left open, which is a real tension rather than an oversight.**
VirtualGrid gets 4.2 free from one decision: fixed row height, so the first
visible row is floor(scrollTop / rowHeight) - OVERSCAN and the window is O(1). A
grouped wall has variable row heights, so there is no single rowHeight to divide
by. The resolution is to keep the O(1) property per GROUP rather than for the
wall as a whole, recovering each group's row height from the group's own reserved
height -- an exact recovery rather than an estimate, because groupHeight
assembled the height from the same arithmetic groupLayout takes apart.

**The bug the structure invites.** A group's row window must use ITS OWN scroll
offset, scrollTop - b.top. The naive version divides the WALL's scrollTop, which
is correct for the first group -- which is exactly what makes it survive a
casual look. A group starting 5,000px down has a local offset of zero, so the
naive window lands tens of thousands of rows past the end and that group renders
its header and no tiles. Three unit tests pin it and the e2e asserts the
rendered consequence: every group overlapping the viewport has tiles in it.

**A second bug: overscan past the end of a group.** For a group ENTIRELY above
the viewport -- which overscan deliberately includes -- localTop exceeds the
group's height, rawFirst runs past the end, and a Math.max(first, last) guard
then inflated last ABOVE loadedRows. The wall asked for rows 0..3 of a 2-row
group. Harmless in the DOM because the slice clamps, and wrong in the
arithmetic -- the kind of wrong that becomes visible the moment the slice stops
clamping. The invariant is now asserted at five scroll positions, not one.

**A test that named the wrong thing.** "The first RENDERED group is the one at
the top of the viewport" failed, and the failure was correct: the first rendered
group is the OVERSCAN one, which starts above the viewport. It now asserts the
claim worth making -- every group that overlaps the viewport is rendered. A test
that pins a wrong invariant is worse than a missing test, because it survives the
fix and fails after it.

**Two component bugs, both with the same signature: a green build and an empty
page.** A {@@const} inside a nested {@each} is a runtime error, not a compile
one -- it must be an immediate child of a block -- and a loop renamed from
`as g` to `as gi` whose body still said `g.indices` leaves the surface blank
with HTTP 200. Neither points anywhere; both are fixed by a console probe rather
than a re-read, and both are now in the harness skill. The first attempt also
looked the row window up with indexOf inside the template, which is O(n^2) over
the rendered set and a re-derivation of an answer wallWindow already returned in
order; it is positional now.

41 mutations, 41 killed, 0 survived, 0 stale. 21 UI tests, 4 new e2e.
UI now 549, e2e 127, Rust 1310.

The test that matters most is "every group is reachable by scrolling to it": a
window that renders only the top groups passes every count assertion. Walking the
whole wall and checking each group renders when the viewport is on it is what
rules that out.

### T-P5-006 item 14 -- the wall, with group-by and auto-scroll

#6544 (group-by) and #6955 (auto-scroll) are one surface, and the interesting
part is a consequence of combining them that neither issue mentions.

**Grouping breaks VirtualGrid's fixed row height.** The fixed height is what
lets the grid compute a 100,000 item library's scroll height from the count
alone, so the scrollbar is right on the first frame. A grouped wall has a
VARIABLE row height -- a header, then however many rows the group needs -- so
the wall has its own scrolling and `wall.ts` owns the geometry. The honest
choice was made over the convenient one: not "measure the groups" (which needs
the whole list before the first paint) but "reserve a placeholder and SAY the
scrollbar is approximate". `groupExtents` returns an `exact` flag for exactly
that reason; a scrollbar that is approximately right must not be presented as
exact. The placeholder is the MEDIAN group height, not the mean, so one group of
5,000 items cannot make every other section reserve 5,000 items of blank space.

**Three bugs the pure tests found before any browser ran.**
`String(null)` is the four characters "null", so every unfiled row landed in a
section literally titled "null" (organized and rating both). `yearOf('2024')`
returned null because the regex demanded a dash, and a year-only date is what a
badly-named file produces -- "group by year" on such a library produced no
sections, silently, because an empty result is a valid result. And auto-scroll
re-pinned on "close enough": the unpin path checked `atBottom`, which includes
the 48px slack, so a user who scrolled up by 40 pixels got dragged straight back
down. The exact behaviour the feature exists to prevent, re-entering through
the unpin path. Only the true bottom re-pins now.

**A test that names a direction, not a magnitude.** A pending group reserves at
least `max(placeholder, known + pending)`, and the assertion is
`before >= after` -- "it is tall enough" passes on a wall that teleports; only
"it never gets shorter" catches the shrink.

**Two component bugs only the browser could find**, both the shape the harness
skill already records. `KeysetStore` is a class with a `get state()`, and a
getter over `#state` is not a tracked dependency -- so `$derived(store.rows)`
computed once and the wall rendered empty while looking finished. It mirrors the
store now, and reassigns after EVERY load, because a `.then` on the initial load
catches one page and none of the rest. And the e2e learned the
`toBeVisible()`-does-not-wait lesson from the other direction: section elements
exist the instant the query starts and hold nothing until the page lands, so a
`toHaveCount` on a section passed against an unloaded wall. It waits for a tile.

**Four test files broke at once when the row grew four grouping fields**, each
with its own `function row(...)`. Now one total factory,
`ui/tests/helpers/row.ts`, that names every field of `ObjectRow` -- a new field
breaks it and nothing else. Making it run cost a runner fix worth recording:
`run-tests.mjs` globs `tests/*.test.ts`, which is NOT recursive, so a helper
under `tests/helpers/` is never compiled into the temp dir, the importing file
fails to resolve it, and `node --test` reports one failure with no assertion
while the file's tests silently vanish from the count. The suite went 490 -> 459
with two failures and the cause was a glob.

**A mutation script that reported 22 stale out of 28 and looked fine.** "0
survivors" is the number a lazy script reports when it is not finding the source
at all: `substitute` compiled string patterns as regexes, and half of them are
source lines full of metacharacters. A high stale count is a matcher bug, not
drifted source. After the literal-match fix, 28/28 killed on the first run, and
most of the 26 that had looked fine were never actually exercised.

**Known gap, stated rather than hidden:** the wall is virtualized per PAGE, not
per pixel, so a group holding 50,000 rows renders 50,000 tiles and violates
4.2. That is the next item, not a claim this one makes. Grouping is a
client-side switch over the loaded page; real group-by is a query parameter and
belongs with T-P6-007.

28 mutations, 28 killed. 38 UI tests, 11 e2e. UI now 528, e2e 123, Rust 1310.

A pre-existing flake, recorded: `commons-jobs`' zombie test failed once in a
full-suite run ("left 3 zombies, up from 2") and passes 3/3 in isolation. It
counts SYSTEM-WIDE zombies, so a busy host with parallel test threads makes it
non-deterministic. Not from this item, and not fixed by it -- but a test reading
a machine-global counter will fail in CI too, and is worth its own fix.

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
### 11. A grep that matches a comment is not a search for the thing

T-P6-009 added two tests for claims about something that is **not** present: that
`Cursor::new` is still `#[cfg(test)]`, and that no production code builds a
`Cursor` directly. Both were written as source greps. **Both passed against the
exact regressions they exist to catch:**

- the `#[cfg(test)]` check searched the 40 lines above `fn new` for that text —
  and the doc comment immediately above the attribute *contains that literal
  string*, because the comment explains that the constructor is `#[cfg(test)]`;
- the sweep treated any `#[cfg(test)]` earlier in a file as proof that a later
  line was inside a test, which is true of nearly every file in a crate with one
  test module at the bottom.

Both now read attribute lines only, and comment lines only, and each fails its
mutation. The same trap bit twice in this codebase before: §4b's "two missing
REST routes the UI calls" were two mentions inside **comments**, and the
`object_read_invariant` family greps for column names that also appear in
prose.

The rule: **a test that has never failed has not been tested.** For a property
about absence, a mutation is the only proof the test is real — a passing test
cannot distinguish "the invariant holds" from "my check matches nothing".

Two related, from the same ticket:

- A **round trip proves two halves agree, not that either is right.** The
  cursor's wire-format test initially interpolated `sort.fingerprint()` into its
  expected string, which made it a round trip in disguise. It is now literal
  bytes, so a change to the format fails it.
- **A test cannot call what a `#[cfg(test)]` gate forbids**, so a production
  `Cursor::new` caller is a *compile* error. The grep is kept anyway — it
  reports which file, across all crates — but its mutation has to remove the
  gate *and* add the caller, since that is the only way that state can exist.

### 12. A headerless request is the most privileged caller, and the test that reasons about a caller must construct one

T-P6-010's `resuming_does_not_widen_visibility` walked every page with a
**headerless request and called it "anonymous."** It is the local **owner** —
`caller_from_request` falls back to `media::local_caller()`, which
`identity.rs` calls "the absence of a design" — so the test asserted the owner
does not see the owner's own `unverified` objects, and failed.

**T-P6-008 was bitten by exactly this, in this repo, for the same reason**, and
fixed it with a `bearer()` token naming no grant. Twice is enough to state the
rule rather than the case: *a test that reasons about a caller must construct
that caller explicitly, because the default is the most privileged one.* The
default being the *owner* rather than the anonymous caller is the dangerous
direction — a test that "verifies anonymous is restricted" and silently gets the
owner verifies nothing at all, and the assertion that fails is the one
asserting the restriction EXISTS, which is exactly the direction that gets
skipped as unimportant.

The second-order point: the fix is not only the token, it is sending it on
**every page**. A cursor carries no caller, so the identity layer has to be
consulted per request, and a test that authenticated once and then paged
anonymously asserts the opposite of what it says.
