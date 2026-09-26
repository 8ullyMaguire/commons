# Commons

A consent-first media library and federated index, for the local library *and*
the public index in one codebase.

Commons combines the ideas of [stash](https://github.com/stashapp/stash) and
[stash-box](https://github.com/stashapp/stash-box) behind a single design where
**amateur material is private by default** and publication is an explicit,
revocable act. It is a library manager first and a federation participant
second — the local use case is complete and useful on its own.

> **Status: early development.** Phase 0 and Phase 1 are done, and Phase 2 is
> five of eight tickets in; see [Progress](#progress) below. Nothing here is a
> release yet, and the data model will still move.

## What makes it different

**Consent is a property of the data, not a setting.** Every object carries its
consent tier and share scope (§14.1). Private is the default; federating a
claim, publishing a file, or sharing with a peer are each distinct, separately
revocable grants. A private scene stays private when you tag it, when you
generate a cover for it, and when you link it to a public object.

**Amateur material is not indexed by default.** This is the design's centre of
gravity. Stash's community is built on scraped metadata; Commons treats
performer material as something that requires an explicit claim before it can
be discussed publicly, and a claim can be withdrawn.

**"Not a downloader" is enforced, not promised.** There is no code path in this
repository that fetches a file body over a peer-to-peer network. P2P locators
(ed2k, magnet, infohash) are stored and federated as ordinary claims, and the
client that resolves one is a sandboxed plugin with a single capability,
`loopback_http`. See `crates/commons-plugin` for the host that enforces this.

**Type comes from content.** A `.mp4` that is really a Matroska file is a
Matroska file. Detection is by magic bytes, with ffprobe only as a tiebreaker
(§5.2, §5.3). The extension is a hint, never the answer.

**Content identity is xxh128 + BLAKE3, and the distinction is load-bearing.**
The fast hash is the scan-time lookup key; the strong hash is the published
identity. `oshash` is deliberately not implemented — it hashes only the head
and tail of a file, so it reports false duplicates (stash-box #1115).

**P2P is a plugin, not a core feature.** Protocol-specific hashes live outside
the core, so the dependency graph does not acquire a networking stack because
someone wanted torrent support.

## Design principles

| Principle | Where it lives |
|---|---|
| The local library is the product; the index is an option | spec §1, §3 |
| Amateur material private by default, publication is explicit | §14.1 |
| Identities are pseudonymous unless a self-claim is verified | §7.5 |
| One binary, mode-selected: library or index | §3.1 |
| Federation-capable from day one, but with no ambient peer discovery | §6.5 |
| Idle footprint is a hard budget, not an aspiration | §4.3 |
| No ambient authority: every list URL is reproducible from its query string | §5.16 |
| Never `OFFSET` in a paginated query | §10.2 |

## The memory budget

**§4.3 sets 210 MB idle for the entire application.** This is a design
constraint that shapes the architecture, not a benchmark to run at the end. It
is why:

- media handling shells out to `ffmpeg` rather than linking `libavcodec` — the
  binaries are already on any machine that can play a video, and the alternative
  puts a large C dependency in the build graph;
- a local store is SQLite and a public index is Postgres, chosen by mode rather
  than by configuration;
- artifact generation is bounded by a token bucket sized from the same ceiling,
  and a job that cannot get a token returns `Deferred` instead of allocating.

The server binary currently idles at **15.1 MB RSS / 11.9 MB PSS**, 17 threads,
0.10 s cold start — measured, not estimated. `scripts/memory-budget.py` is the
harness, and it is wired into the test suite rather than left for someone to
remember.

## Architecture

Fourteen crates, layered so that the dependency edges are checked and not
merely intended. Six carry real code; eight are one-line placeholders for later
phases, so the workspace and the dependency table exist from the start.

```
commons-core     domain types, content hashing, audio, comics,
                 the filter language's AST
     ↑
commons-store    schema, migrations (SQLite + Postgres, proven equivalent)
commons-plugin   the capability host that confines plugins
commons-media    ffprobe wrapper, safe archives, thumbnails + sprite sheets
commons-scan     content detection, segmentation, typed object bodies, funscript
commons-server   the one binary, in two modes so far
```

Placeholders, with nothing importing them yet: `commons-api`, `commons-client`,
`commons-consent`, `commons-federation`, `commons-identity`, `commons-index`,
`commons-jobs`, `commons-ml`.

`crates/commons-store/tests/layering.rs` holds the dependency table. It is not
decoration: Cargo catches a true cycle, but it cannot catch a *legal* edge that
should not exist — `client → store`, for instance, which compiles fine and
couples a frontend to a database. That table is a backstop for exactly that
class of mistake, and it was verified by introducing a real violation and
watching it fail.

## Building

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Requires a recent Rust toolchain (`rust-toolchain.toml` pins it) and, for the
media tests, `ffmpeg`/`ffprobe` on `PATH`. Tests that need `ffprobe` skip with
a message when it is absent rather than silently passing.

The `CARGO_TARGET_DIR` on a network mount will be slow; point it at local disk:

```sh
export CARGO_TARGET_DIR=~/.cargo-target/commons
```

## Running

One binary. Two modes are implemented; `combined` is specified but not built
yet:

```sh
# Private library, bound to loopback only.
commons-server --mode library --library ~/Pictures/library

# Public index, bound to all interfaces.
commons-server --mode index --index-db postgres://...
```

Configuration is TOML with CLI-over-file-over-default precedence, and **unknown
keys are rejected rather than ignored** — a typo'd key that silently does
nothing is a broken config that looks fine.

Health endpoints (`/healthz`, `/livez`, `/metrics`) need no authentication, so a
supervisor holding no credential can still check the process. `/livez`
deliberately skips the database, so a database outage does not kill a process
that is still alive and would recover.

## Testing

616 tests across the workspace. The ones that matter most are the ones that
were verified by breaking the code on purpose:

- **Zip-Slip** (`commons-media`, `archive.rs`) — a comic archive is the most
  attacker-controlled file in the library. `validate_member` is a pure function
  with no I/O, tested against a fixture archive that was actually built to
  attack it. Removing the traversal check fails two tests; removing the symlink
  check fails three; making validation always return `Safe` fails the
  end-to-end test that plants a sentinel file outside the extraction root.
- **BLAKE3 against upstream's own vectors** — 24 lengths straddling every
  internal boundary. A stored library is not re-readable if a hash changes, and
  a self-consistency test would never notice.
- **Content detection** (`commons-scan`, 16 fixtures) — generated by
  `scripts/make-fixtures.sh`, which is checked in so the corpus is
  reproducible rather than a pile of binaries somebody uploaded once.
- **Layering** — verified by adding a real violation.
- **Comic page order** (`commons-core`, `comic.rs`) — the `ls -v` rule.
  Reducing `natural_cmp` to a plain string compare — which is what the first
  version silently did, because it stopped at the first non-digit in `img_2` —
  fails 7 tests, including a property test over every number width from 1 to 5.
- **Replay-gain clipping** (`commons-core`, `audio.rs`) — `would_clip` was
  written against a target *peak* rather than a *boost*, so every target above
  the file's own peak was trivially a clip and the check always fired.
  Inverting it fails 5 tests.
- **Funscript duration clamping** (`commons-scan`, `funscript.rs`) — a zero
  duration meant "unknown", not "empty timeline", so a comic or an unprobed
  video erased the whole script. There is a test that clamping to zero is a
  no-op.
- **The thumbnail memory budget** (`commons-media`, `thumbs.rs`) — the plan's
  own acceptance test for the token bucket did not fail when the ceiling check
  was deleted, because the critical section was too short to observe
  contention. It was rewritten to hold reservations across a barrier, and now
  removing the ceiling check fails it. A test that cannot fail is worse than no
  test, and this one was found by trying to break it.

Fixture content is deterministic. Re-running the generator produces
content-equivalent archives; zip entry timestamps are pinned precisely so a
regenerated corpus does not show up as a diff.

## Documentation

- [`docs/spec/commons-spec.md`](docs/spec/commons-spec.md) — the full design
  (16 sections, appendices, and a capability→issue matrix covering all 850
  upstream issues)
- [`docs/plans/implementation-plan.md`](docs/plans/implementation-plan.md) —
  the implementation plan, with per-ticket status and commit hashes

The spec is the authority. Where the code and the spec disagree, that is a bug
in the code, and the test names and comments here say which rule they are
enforcing so the intent is not lost.

## Progress

| Phase | Status | What it delivers |
|---|---|---|
| 0 — Foundations | done | schema, migrations, filter language, plugin host, server, memory harness |
| 1 — Content types | done | detection, probing, segments, archives, hashing, the five remaining content types, and the windowed grid |
| 2 — Library and scale | 6 of 8 | watcher, checkpoints, hashing, move detection, missing volumes, durable job queue, hardware acceleration, storage accounting and encoder configuration |
| 3–6 | planned | identity, federation, UI surfaces, review and automation |
| 11 — community ecosystem | planned, **last** | adapt the stashapp ecosystem rather than fork it: 729 YAML scrapers, 155 Python scrapers, 79 plugin directories, 12 theme directories. Nothing is vendored — every artifact is fetched at install time, pinned by commit. |

Closed so far, among others: #3530 (one file, many objects — 38 comments
upstream), #2276 (multi-part scenes), #2511 (virtual compilations), #1258
(audio), #1659 (comics), #1259 (text and links), #3031 (funscript discovery and
parsing), #6339 (multi-axis interactive), #5111 (GIF versus video), #7229
(non-zero start offsets), #1115 (oshash deprecation), #5683 (a drive that is
not plugged in), #7239 (an acceleration setting with no reason), #7007
(unprivileged containers).

### The community ecosystem (Phase 11)

`stashapp/CommunityScrapers` and `stashapp/CommunityScripts` are the largest
body of reusable work in this space, and Phase 11 makes them usable here rather
than forking them. It is deliberately the **last** phase: it is the only one
whose value is entirely borrowed, and every ticket before it is about the thing
it plugs into.

The two repositories are two different problems. 729 of the 982 scraper
definitions are **declarative YAML** — an entry-point table plus XPath/JSON
selectors and a `postProcess` chain — so adapting them means writing an
interpreter for that little language, not translating 729 programs. The other
155 are ordinary Python on a `py_common` runtime, and the 79 plugin directories
are Python or TypeScript. Those get a **compatibility layer**, because
reimplementing 155 working programs in Rust is a different project with a worse
success rate, and a hand-port that diverges from upstream is worse than no port
since the next upstream push fixes theirs and not ours.

**Nothing is vendored.** Every artifact is fetched at install time, pinned by
commit, and cached. Two reasons, and the second bites: AGPL-3.0 content in-tree
would make this workspace AGPL, and a vendored copy is stale the moment upstream
pushes. The compatibility report is generated from *execution*, so "works" means
something ran here.

The job queue is durable in the sense the ticket means: jobs are submitted,
claimed, retried and completed through the `job` table, and a row left `running`
by a crash comes back `queued`. Concurrency is bounded per (job kind, file), not
per kind, so one job per file does not mean one job in the library. The
suspend inhibitor is held *during* a job rather than between jobs, and released
when the last one finishes.

The frontend shell (`ui/`) is a SvelteKit 2 + TypeScript app with one GraphQL
client, a keyset-paginated store, and a fixed-row-height virtualized grid. Every
list view lives entirely in the query string, so a filter is a bookmark. A
5,000-item library renders 11 rows and 55 tiles; 43 unit tests and 11 browser
tests cover it, and 0 type errors.

## The scanner

`commons-scan` walks a library root once and can stop and resume without
redoing work. Three things there are less obvious than they look:

- **A checkpoint is not a directory name.** The walk is depth-first with an
  explicit stack, so "the walk was last in `d10`" says nothing about the
  siblings still waiting. The checkpoint carries the pending stack, and
  `files_in_dir`, because a batch flush lands mid-directory. Together they
  make an interrupted scan and its resume an exact partition: no gap, no
  duplicate file indexed twice.
- **A watcher is never trusted across a network boundary.** Events are lost
  across NFS, SMB, and rclone, and a lost `Create` is a file that silently
  never appears in the library. Remote volumes get a poll loop that emits the
  *difference*, not a rescan. A user can override in both directions.
- **A progress bar that cannot estimate says so.** There is no honest
  denominator for a filesystem walk, so `Progress` reports a fraction *and* a
  `Confidence`. A running scan never claims 100%, and a tree of empty
  directories reports no estimate at all rather than a confident wrong one.

`PersonRef` is on all seven object kinds, not just scenes. That is what makes
the owner-added interview type (§5.8) work: a person speaking in an interview
is an `Appearance` resolved to the same identity cluster as their other
appearances, so §7.1 clustering does not split them. A test walks all seven
kinds and fails if a future one is added without people.

See [`docs/HANDOFF.md`](docs/HANDOFF.md) for the current state, how to
verify it, and what is deliberately not done.

## Licence

**AGPL-3.0.** See [`LICENSE`](LICENSE).

The AGPL was chosen because the point of this project is that people can run
their own index and that improvements to it should come back. A network-facing
index is exactly the case the AGPL is designed to cover, and a permissive
licence would let a hosted fork keep those improvements to itself.
