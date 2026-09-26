# Commons

A consent-first media library and federated index, for the local library *and*
the public index in one codebase.

Commons combines the ideas of [stash](https://github.com/stashapp/stash) and
[stash-box](https://github.com/stashapp/stash-box) behind a single design where
**amateur material is private by default** and publication is an explicit,
revocable act. It is a library manager first and a federation participant
second — the local use case is complete and useful on its own.

> **Status: early development.** Phases 0 and 1 are largely built; see
> [Progress](#progress) below. Nothing here is a release yet, and the data
> model will still move.

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

Six crates, layered so that the dependency edges are checked and not merely
intended:

Six crates are built; eight more exist as empty placeholders for later phases.

```
commons-core     domain types, content hashing, the filter language's AST
     ↑
commons-store    schema, migrations (SQLite + Postgres, proven equivalent)
commons-plugin   the capability host that confines plugins
commons-media    ffprobe wrapper, safe archive listing and extraction
commons-scan     content detection, segmentation: path on disk → typed objects
commons-server   the one binary, in two modes so far
```

Declared but not yet written: `commons-api`, `commons-client`,
`commons-consent`, `commons-federation`, `commons-identity`, `commons-index`,
`commons-jobs`, `commons-ml`. They are one-line placeholders so the workspace
and the dependency table exist from the start; nothing imports them yet.

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

225 tests across the workspace. The ones that matter most are the ones that
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
| 1 — Content types | 5 of 8 | detection, probing, segments, archives, hashing |
| 2 — Library and scale | next | watcher, checkpoints, 100k-item performance |
| 3–6 | planned | identity, federation, UI surfaces, review and automation |

Closed so far, among others: #3530 (one file, many objects — 38 comments
upstream), #2276 (multi-part scenes), #2511 (virtual compilations), #1258
(audio), #1659 (comics), #1259 (text and links), #5111 (GIF versus video),
#7229 (non-zero start offsets), #1115 (oshash deprecation).

## Licence

**AGPL-3.0.** See [`LICENSE`](LICENSE).

The AGPL was chosen because the point of this project is that people can run
their own index and that improvements to it should come back. A network-facing
index is exactly the case the AGPL is designed to cover, and a permissive
licence would let a hosted fork keep those improvements to itself.
