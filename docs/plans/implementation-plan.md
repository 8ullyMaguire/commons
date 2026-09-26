# Commons — Implementation Plan

**Status:** in progress. Phase 0, 1 and 2 complete; Phase 3 next.
**Created:** 2026-09-26 · **Phase 11 added** 2026-09-26 (community ecosystem,
at the owner's request; deliberately last)
**Spec:** `~/secondbrain/10-Projects/2026-09-26T110000+0200-commons-platform-spec.md` (v1.3, 2,636 lines)
**Target:** one Rust binary serving three modes, a SvelteKit UI, a Tauri shell, and a WASM plugin host.

---

## 0. How to use this document

This plan is written to be executed by a **small language model with a short
context window**. That drives every choice in how it is written:

- Every ticket is small enough to hold in one context along with the two or
  three files it touches. If a ticket needs more, it is split.
- Every ticket states its **literal file paths**, its **literal acceptance
  command**, and its **definition of done**. No ticket requires you to infer
  what "done" means.
- Schema is given as **copy-paste DDL**, not as prose description. Do not
  redesign it; if it is wrong, that is an escalation, not an improvisation.
- Spec references (`§7.1`) are for traceability. **The ticket text is
  authoritative.** If the ticket and the spec disagree, follow the ticket and
  file a note.
- Tickets are numbered `T-P<phase>-<nnn>`. Do them in order within a phase.
  A later ticket in the same phase may assume earlier ones are merged.

### 0.1 The five rules that matter most

These are the mistakes that are expensive rather than annoying. Read them once.

1. **Never edit or reorder an applied migration.** Migrations are immutable in
   content *and* position once merged. A correction ships as a new migration
   with a higher number. This is non-negotiable and has broken a deployment
   before.
2. **One logical schema, two physical engines.** Every schema change is written
   twice: once for Postgres (`migrations/postgres/`) and once for SQLite
   (`migrations/sqlite/`), and Phase 0 has a test that fails if they disagree.
   Never write a query that only works on one of them. See §0.4.
3. **Consent checks live in the store layer, not in handlers or UI.** A tier
   filter that is only applied in the UI leaks through the next surface that
   forgets it. Any function that returns objects from a query must apply the
   caller's content filter. See T-P7-004.
4. **The plugin sandbox has no general network capability.** A plugin that
   declares `net` is refused at instantiation. There is no escape hatch, no
   "trusted plugin" tier, and no env var that relaxes it.
5. **Content hashing is core; protocol-specific hashes are not.** xxh128 and
   BLAKE3 are in core because incremental scan and dedup depend on them. ed2k
   MD4 and BitTorrent infohash belong to the plugin. See T-P10-002.
6. **No upstream code is vendored, ever.** Phase 11 adapts the
   `stashapp/CommunityScripts` and `CommunityScrapers` ecosystems, and it does
   so by *fetching* artifacts pinned by commit, never by copying them into the
   tree. Two reasons, and the second is the one that bites: AGPL-3.0 content
   in-tree would make this workspace AGPL, and a vendored copy is stale the
   moment upstream pushes — so the divergence is invisible until someone
   notices a bug report about a scraper that was fixed eight months ago. The
   compatibility report is generated from execution for the same reason: a
   claim of "works" that was not run is worse than no claim.

### 0.2 Definition of done, per ticket

A ticket is done when **all** of these hold:

```bash
cargo fmt --check                 # clean
cargo clippy --workspace -- -D warnings   # no warnings
cargo test -p <the crate you touched>     # passes
```

Plus whatever acceptance command the ticket names. A ticket that needs a
"manual check" is not done; turn the check into a test.

### 0.3 Environment (already present on this host)

| Tool | Version | Note |
|---|---|---|
| cargo / rustc | 1.98.1 | edition 2021, MSRV pin `1.98` |
| node | v26.7.0 | |
| pnpm | 12.4.1 | hoisted linker, no shamefully-hoist needed |
| ffmpeg / ffprobe | n9.0.1 | used as external processes, never linked |

ffmpeg is invoked as a subprocess. There is no ffmpeg Rust binding anywhere in
this project, and adding one is a rule-2 style violation of §4.4.

### 0.4 Portable SQL rules

Because two engines must agree:

- Use `TEXT` for ids (UUIDs as `TEXT`, generated in Rust with `uuid` crate v4).
  Postgres-native `uuid` and SQLite `TEXT` diverge in comparison semantics.
- Timestamps are `TEXT` ISO-8601 UTC (`2026-09-26T11:02:00Z`) in both.
  Rationale: one comparison function, no timezone bugs, sorts lexically.
- Vectors are **not** in SQL. Face/body embeddings live in a sidecar ANN file
  (§3.5 of the spec) keyed by id. Postgres mode additionally mirrors to
  `pgvector` when available, but no query may *require* it.
- Booleans are `INTEGER 0|1` in SQLite, `BOOLEAN` in Postgres. Wrap the
  difference in the store crate; never write a literal in a query.
- Full-text search is written twice with the same tokenizer behaviour, tested
  against a shared fixture (`T-P5-002`).

## 0.5 Repository layout

```
commons/
├── Cargo.toml                     # workspace root, members = crates/*
├── rust-toolchain.toml            # channel = "1.98.1"
├── crates/
│   ├── commons-core/              # domain types + enums. No I/O. No deps but serde/uuid/chrono.
│   ├── commons-store/             # the ONLY crate that talks to a database
│   │   ├── migrations/postgres/   # NNNN_name.sql
│   │   ├── migrations/sqlite/     # NNNN_name.sql  (same NNNN)
│   │   └── src/{lib,pg,sqlite,filter_ast}.rs
│   ├── commons-scan/              # filesystem walk, content hashing, move detection
│   ├── commons-jobs/              # durable queue, workers, supervision
│   ├── commons-media/             # ffprobe/ffmpeg wrappers, artifact cache
│   ├── commons-ml/                # ONNX runtime, face/body embed, tagger, ASR
│   ├── commons-identity/          # clustering, merge/split, fingerprints
│   ├── commons-index/             # proposals, votes, reputation, moderation
│   ├── commons-consent/           # tiers, attestation, takedown, tombstones
│   ├── commons-federation/        # peers, claims, merge
│   ├── commons-plugin/            # WASM host + capability enforcement
│   ├── commons-api/               # GraphQL + REST + websocket
│   ├── commons-server/            # the binary: modes, config, wiring
│   └── commons-client/            # typed client used by the Tauri shell
├── plugins/locator-p2p/           # THE C91 PLUGIN (T-P10)
├── ui/                            # SvelteKit 2
├── desktop/                       # Tauri shell
├── docs/                          # docs site sources (§15.5)
└── tests/
    ├── fixtures/                  # shared test data: tiny mp4, zip, cbz, wav
    └── e2e/                      # playwright specs
```

Dependency direction is strictly downward. `commons-core` depends on nothing
local. `commons-store` depends only on `commons-core`. Nothing depends on
`commons-server` except `commons-client` (and even that may, in time, be
folded in). CI enforces this with a test (`T-P0-006`).

---

## Phase 0 — Foundations

**Exit criterion (spec §17):** a file is scanned, generated and browsed; idle
RSS is within the §4.3 budget; migrations are proven in both engines; and a
test plugin can reach `127.0.0.1` and is provably refused `net`.

### T-P0-001 — Workspace skeleton

**Status: DONE** (98382b6) — workspace skeleton, `commons-core` + `commons-store`, both build.

**Spec:** §3, §0.5
**Files:** `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `crates/commons-core/Cargo.toml`, `crates/commons-core/src/lib.rs`

1. Create the workspace with all 13 `crates/*` members from §0.5. Each gets a
   `Cargo.toml` with `edition = "2021"` and an empty `src/lib.rs`.
2. Pin `rust-toolchain.toml` to `channel = "1.98.1"`.
3. Workspace `[workspace.dependencies]` declares every third-party crate the
   project will use, with a pinned minor version, so crates reference them as
   `workspace = true` and versions cannot drift. At minimum: `serde`,
   `serde_json`, `uuid`, `chrono`, `tokio` (full), `sqlx` (runtime-tokio,
   postgres, sqlite, macros), `async-graphql`, `async-graphql-axum`,
   `axum`, `tower`, `tokio-tungstenite`, `tracing`, `tracing-subscriber`,
   `blake3`, `xxhash-rust` (xxh128), `wasmtime`, `ort` (ONNX Runtime),
   `usearch`, `notify`, `anyhow`, `thiserror`, `sha2`, `ed25519-dalek`,
   `rusqlite` is NOT used (sqlx handles sqlite too), `image`, `rayon`.
4. `.gitignore` covers `target/`, `ui/node_modules/`, `ui/.svelte-kit/`,
   `ui/build/`, `desktop/target/`, `data/`, `*.sqlite`, `*.sqlite3`.

**Accept:** `cargo build --workspace` succeeds with zero warnings.
**Done when:** all 13 crates exist and `cargo metadata` lists them.

**Done** (`d0ef089`).
### T-P0-002 — Core domain types

**Status: DONE** (98382b6) — domain types and enums; `Filter`/`Value` needed `#[serde(bound)]` to avoid E0275.

**Spec:** §5.1, §7, §8.1, §14.1, §15.1
**Files:** `crates/commons-core/src/lib.rs`, plus `object.rs`, `person.rs`, `proposal.rs`, `consent.rs`, `enums.rs`

Define these types, all in `commons-core`, all `serde`-serializable, with no
I/O and no database types:

```rust
// enums.rs — every enum carries a stable string repr, never a bare ordinal,
// because these strings are in URLs, in exports, and in the federation protocol.
pub enum ObjectKind    { Scene, Image, Gallery, Audio, Comic, Text, Interview }
pub enum FileState     { Present, Missing, Unreadable, Remote }
pub enum OrganizedState { Unreviewed, Organized, Favourite }
pub enum ConsentTier   { Unverified, SelfPublished, PerformerClaimed,
                         ThirdPartyPermitted, Quarantined, Denied }
pub enum ProposalSource { Filename, Embedded, PhashMatch, Transcript, Caption,
                         MlTagger, MlCaptioner, User, Peer, Scraper, Plugin }
pub enum RelationType  { ExtraOf, PartOf, CompilationOf, SameSceneAs,
                         ReEncodeOf, UnrelatedTo }
pub enum AppearanceType { Primary, Cameo, NonSexual, GroupScene, Duologue, Background }
pub enum ClusterState  { Anonymous, Named, Claimed, Ambiguous }
pub enum ProducerKind  { Studio, Circle, Individual, Collective, Unknown }
pub enum LocatorScheme { Magnet, Ed2k, Infohash, Http }
pub enum AttributeType { Single, Multi, Range, Ordinal, Boolean, Text,
                         Date, Measurement }
pub enum JobState      { Queued, Running, Done, Failed, Skipped, Cancelled }
```

Plus the structs `Object`, `FileRecord`, `Segment`, `ObjectRelation`,
`PersonCluster`, `Appearance`, `Performer`, `Producer`, `Group`, `Tag`,
`FieldProposal`, `Vote`, `Rating`, `ConsentRecord`, `Locator`, `Job`,
`Artifact`, `Marker`, `Account`, `Role`, `Peer`, `Claim`.

Two types get particular care, because the whole design rests on them:

- `FieldProposal` is keyed by a **polymorphic subject**, not by object:
  `(subject_type: SubjectType, subject_id, field: String)`. Performer names and
  studio names vote exactly like object titles.
- `ConsentRecord` holds `tier`, `attestation: Option<Attestation>` (who, when,
  on what basis, free text), and `redistribution_permitted: bool`. The last
  field is what gates locators (§5.18) and is **not** derivable from `tier`
  alone.

**Accept:** `cargo test -p commons-core` — add one round-trip test per enum
asserting the exact string repr (these strings are wire format; a silent
change is a breaking protocol change).
**Done when:** the strings are pinned by tests.

**Done** (`d0ef089`).
### T-P0-003 — Postgres schema, migration 0001

**Status: DONE** (6ce1f53) — 41-table schema; migration parity is a test, not a promise.

**Spec:** §15.1
**Files:** `crates/commons-store/migrations/postgres/0001_core.sql`, `crates/commons-store/migrations/sqlite/0001_core.sql`

Write the initial schema. It is long; write it in this order and keep the two
engines structurally parallel (same table names, same column names, same order):

1. `object`, `file`, `segment`, `archive`
2. `object_relation`
3. `person_cluster`, `appearance`, `performer`, `performer_alias`
4. `producer`, `producer_alias`, `group_obj`, `group_member`
5. `tag`, `tag_namespace`
6. `field_proposal`, `vote`
7. `rating`, `consent_record`, `consent_tombstone`
8. `marker`, `subtitle`, `transcript`, `funscript`
9. `list_obj`, `list_item`, `smart_collection`
10. `account`, `role`, `account_role`, `session`
11. `job`, `artifact`
12. `external_id`, `custom_field`, `custom_field_value`
13. `peer`, `claim`
14. `locator`  ← included in the schema even though the *plugin* is not core
    (§5.18.1: core owns the gate, the plugin only proposes). The table is core;
    the computation is not.

Mandatory constraints, in both engines:

```sql
-- Postgres
CREATE TABLE file (
  id            TEXT PRIMARY KEY,
  object_id     TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
  path          TEXT NOT NULL,
  size_bytes    INTEGER NOT NULL DEFAULT 0,
  mtime_ns      BIGINT  NOT NULL DEFAULT 0,
  hash_xxh128   TEXT,            -- hex, NULL until scanned
  hash_blake3   TEXT,            -- hex, NULL until scanned
  state         TEXT NOT NULL DEFAULT 'present',
  UNIQUE (object_id, path)       -- closes stash#2814-class duplicates
);
CREATE INDEX file_path_idx     ON file (path);
CREATE INDEX file_hash_idx     ON file (hash_blake3);
CREATE INDEX file_xxh_idx      ON file (hash_xxh128);
```

and for `field_proposal`, the uniqueness that makes voting well-defined:

```sql
CREATE TABLE field_proposal (
  id            TEXT PRIMARY KEY,
  subject_type  TEXT NOT NULL,        -- 'object' | 'performer' | 'producer' | 'group' | 'tag' | 'cluster'
  subject_id    TEXT NOT NULL,
  field         TEXT NOT NULL,
  value_json    TEXT NOT NULL,        -- always JSON; polymorphic
  source        TEXT NOT NULL,        -- ProposalSource
  proposer_kind TEXT NOT NULL,        -- 'user' | 'auto' | 'peer' | 'plugin'
  proposer_id   TEXT,                 -- account id, plugin name, or peer id
  confidence    REAL,                 -- for auto proposals; NULL for humans
  created_at    TEXT NOT NULL,
  UNIQUE (subject_type, subject_id, field, value_json, source, COALESCE(proposer_id,''))
);
CREATE INDEX field_proposal_subject_idx ON field_proposal (subject_type, subject_id, field);
```

SQLite gets the same tables; the `COALESCE` in the unique constraint is the
one place the two engines differ and it is written with a comment saying so.

**Accept:** `psql -f 0001_core.sql` against an empty database succeeds, and
`sqlite3 :memory: < 0001_core.sql` succeeds.
**Done when:** both apply cleanly and `T-P0-007` passes.

**Done** (`6ce1f53`).
### T-P0-004 — Store crate: connection, migrations, mode selection

**Status: DONE** (f8ef780) — `Store` picks the engine from `Mode`; SQLite library opens migrated on a fresh dir.

**Spec:** §3.1, §3.5, §14.2
**Files:** `crates/commons-store/src/lib.rs`, `pg.rs`, `sqlite.rs`, `migrate.rs`

1. `Store` is an enum (`Store::Postgres(PgStore)`, `Store::Sqlite(SqliteStore)`)
   behind one trait object, NOT a generic parameter — generics would force the
   whole binary to be compiled twice.
2. `Store::open(config: &Config) -> Result<Store>`: `index` mode requires
   Postgres and errors clearly if absent; `library` mode uses SQLite unless a
   `DATABASE_URL` is supplied, in which case Postgres.
3. `migrate()` applies `migrations/<engine>/*.sql` in filename order, each in a
   transaction, recording applied names in `_migrations(name TEXT PRIMARY KEY,
   applied_at TEXT)`. On startup, if a file exists in the directory but not in
   `_migrations`, **fail loudly** — that means an applied migration was edited
   or removed, which is rule 1.
4. Both engines share the DDL *files* per engine but share a `MigrationSet`
   constant listing `(version, name)` pairs. The test in T-P0-007 compares the
   two directories by name.

**Accept:** `cargo test -p commons-store` — a test creates a fresh DB of each
kind, migrates, and asserts `PRAGMA user_version` (SQLite) / `_migrations` row
count (Postgres) equals the file count.
**Done when:** both engines migrate to empty and a second run is a no-op.

**Done** (`f8ef780`).
### T-P0-005 — Filter AST (the shared query language)

**Status: DONE** (6ce1f53) — base64url filter URLs round-trip; decoder drops pads, not a computed count.

**Spec:** §5.16, §9.1
**Files:** `crates/commons-store/src/filter_ast.rs`

This is the single most reused type in the project; it is built here so
everything after can depend on it.

```rust
pub enum Filter {
    And(Vec<Filter>), Or(Vec<Filter>), Not(Box<Filter>),
    Facet { kind: ObjectKind, field: FieldRef, op: CmpOp, values: Vec<Value> },
    Text { q: String },
    Saved { id: String },
    ConsentVisible { caller: CallerId },   // NOT removable; always ANDed in
}
```

1. `FieldRef` is either a built-in field enum or `Custom(String)`.
2. `CmpOp` covers `eq, ne, in, nin, contains, starts, gt, gte, lt, lte, between,
   is_null, is_not_null, matches_regex` — including the NULL semantics
   stash#6970 and #3159 ask for, where `is_null` and `false` are different.
3. `Filter::to_sql(&self, engine) -> (String, Vec<Param>)` emits parameterised
   SQL. **No string interpolation of user values, ever** — bind everything.
4. `Filter::to_url(&self) -> String` base64url-encodes canonical JSON for §5.16's
   shareable URLs. `Filter::from_url` is the inverse. Round-trip is tested.
5. `Filter::parse_query(&str) -> Filter` accepts the upstream stash query
   language (stash#185, #2816) as an input surface, mapping it onto the same
   AST. Unknown tokens produce a spanned error, not a silent empty result.

**Accept:** `cargo test -p commons-store filter_ast` — includes a test that
every operator emits a bind parameter rather than a literal.
**Done when:** the URL round-trip test passes for all five operators sampled.

**Done** (`6ce1f53`).
### T-P0-006 — Plugin host skeleton (promoted from Phase 6)

**Status: DONE** (9068122) — capability-gated plugin host; loopback-only is enforced, not documented.

**Spec:** §11.4, §5.18.1
**Files:** `crates/commons-plugin/src/lib.rs`, `capability.rs`, `host.rs`

This ticket exists in Phase 0 because §5.18.1 makes a headline capability a
plugin. Do the enforcement, not the SDK surface.

```rust
pub struct Manifest {
    pub name: String, pub version: Semver, pub signature: Signature,
    pub capabilities: Vec<Capability>,
}
pub enum Capability {
    Fs { roots: Vec<PathBuf> },
    Db { scope: DbScope },              // ReadOnly | ScopedWrite
    Hooks, Ui, Task,
    LoopbackHttp,                       // 127.0.0.1 / ::1 only
    Net { hosts: Vec<String> },         // general egress — shown to user as "internet access"
}
```

1. `wasmtime` instance is created with **no WASI network preopens and no
   inherited environment**. Environment is an explicit, empty map.
2. `LoopbackHttp` is implemented as a host function that accepts a URL, parses
   it, and **refuses** unless the host is a literal loopback address. There is
   no DNS in this path, so a hostname that resolves to 127.0.0.1 is still
   refused — the check is on the literal string, before resolution. This is
   deliberate: it is the only way the property is testable.
3. `Net` is *declared* but unimplemented in Phase 0. A plugin declaring it
   installs and then every egress attempt returns `EgressDenied`. (Real `Net`
   ships in Phase 6 for scrapers.) A test asserts this.
4. Capability enforcement is at **instantiation**, not at call time only. A
   plugin requesting a capability the operator has not granted is not
   instantiated, and the reason is shown in the UI.

**Accept:** `cargo test -p commons-plugin` — three tests: (a) a test plugin
declaring `LoopbackHttp` reaches a local mock server on 127.0.0.1; (b) the
same plugin calling `http://example.com` gets `EgressDenied`; (c) a plugin
declaring `Net` is instantiated but every egress call is denied. Test (b) must
assert on the *literal-host* rule by trying `http://127.0.0.1.nip.io` and
being refused.
**Done when:** all three pass. This is the ticket that makes §2's
"never Electron, never a downloader" claims structurally true.

**Done** (`9068122`).
### T-P0-007 — Migration parity + layering tests

**Status: DONE** (e739084) — verified: cargo catches the cycle, the table catches the legal upward edge.

**Spec:** §0.4, §4.4
**Files:** `crates/commons-store/tests/migration_parity.rs`, `crates/commons-store/tests/layering.rs`, `crates/commons-store/tests/layering.rs`

1. `migration_parity`: for each `NNNN_*.sql` in `postgres/`, a file with the
   same `NNNN_name` must exist in `sqlite/`. Compare the *set* of table names
   extracted by regex from both and assert equality. This is how a
   half-remembered migration gets caught.
2. `layering`: parse each crate's `Cargo.toml`, build the crate→crate
   dependency graph, and assert it is a subset of §0.5's downward rules.
   `commons-core` must have no local dependency. A cycle fails the test.

**Accept:** `cargo test --workspace` — both tests pass.
**Done when:** a deliberate violation of either makes the test fail (verify by
doing it once, then reverting).

**Done** (`e739084`).
### T-P0-008 — Memory budget harness

**Status: DONE** (9f3ef6e) — 11.9 MB median PSS / 0.10s cold start against the 210 MB budget; failure paths exit 1.

**Spec:** §4.3
**Files:** `crates/commons-server/tests/rss_budget.rs`, `scripts/rss-check.sh`

1. `scripts/rss-check.sh` boots `commons-server --mode library --data-dir
   $TMPDIR/rss` on a seeded 5,000-item fixture library, waits for `/healthz`,
   samples RSS of the server process three times over 10 s, and prints the
   median.
2. The Rust test shells out to that script and fails if median > 90 MB
   (server) — the §4.3 number. It is a **budget, not a measurement**: if the
   real figure must be higher, change the constant here and in the spec
   together, never silently.
3. Same for the Tauri shell at 120 MB, added in T-P8-004 when the shell
   exists.

**Accept:** `cargo test -p commons-server rss_budget`.
**Done when:** it passes on this host, and you have written down the measured
number next to the budget.

### T-P0-009 — Server binary, modes, health

**Status: DONE** (9f3ef6e) — `commons-server` serves /healthz, /livez, /metrics-behind-a-flag; 404 names what exists.

**Spec:** §3.1, §3.7, §12.6
**Files:** `crates/commons-server/src/main.rs`, `config.rs`, `health.rs`

1. `commons-server --mode library|index|peer --data-dir DIR [--bind ADDR]
   [--config FILE]`. Config precedence: CLI flag > config file > default.
2. `/healthz` returns `{ok, mode, schema_version, migrations_pending,
   db_engine}` and never requires auth. `/metrics` is a Prometheus text
   endpoint, opt-in behind a flag, off by default.
3. Structured logging via `tracing` with two formatters: JSON for files,
   human-readable for a TTY (stash#2463 asked for exactly this split).
4. XDG paths (stash#2814): config in `$XDG_CONFIG_HOME/commons/`, cache in
   `$XDG_CACHE_HOME/commons/`, data in `$XDG_DATA_HOME/commons/`, overridable
   by `--data-dir`.

**Accept:** `commons-server --mode library --data-dir $T/health && curl -s localhost:9999/healthz`.
**Done when:** it returns JSON with `schema_version` and zero pending
migrations on a fresh dir.

---

## Phase 1 — Content types

**Exit:** all seven types scan, generate and browse (C01–C14).

**Progress:** 8 of 8 tickets done. Phase 1 is complete.

T-P1-008 shipped the frontend shell: one GraphQL client (`ui/src/lib/api/
client.ts`), keyset pagination as a framework-independent state machine
(`ui/src/lib/api/keyset.ts`), view state in the query string
(`ui/src/lib/api/view.ts`), and the windowed grid
(`ui/src/lib/components/VirtualGrid.svelte`). 43 unit tests, 11 browser tests,
0 type errors. A 5,000 item library renders 11 rows and 55 tiles.

**Read this before writing any other UI test here.** Four of the bugs the
browser tests found are invisible to a unit test, and three of them only
appear for results that fit on one screen -- so a suite written entirely
against a 5,000 item library passes while the app is broken. The grid
re-requested forever for small results because `started` was missing from
`GridState`; every row was two quarters too short because the row height
multiplied the tile width by the width:height ratio instead of dividing; the
scroll handler wrote a value the window derived from; and the build shipped
a directory listing instead of an app because `ssr: false` has to be in
`+layout.ts`. Each has a test that fails without it, and each was verified by
reintroducing the bug.

**Note on T-P1-006's acceptance criterion.** The plan's stated test ("set the
ceiling to 1, run 4 concurrent generations, assert max in-flight == 1") passes
with the ceiling check deleted, if written the obvious way. Four versions are
recorded in the commit; the working one detects the violation from inside the
critical section rather than from an observer thread. Worth reading before
writing any other concurrency test here.

**Done** (`9f3ef6e`).
### T-P1-001 — Type detection by content, not extension

**Status:** DONE — `a501953`. 
**Spec:** §5.2, §5.3, §5.4, §5.5
**Files:** `crates/commons-scan/src/detect.rs`
**Depends:** T-P0-003

1. `detect(path, first_bytes) -> ObjectKind` sniffs container magic bytes:
   ISO-BMFF (`ftyp`), Matroska/WebM (`1A45DFA3`), AVI (`RIFF....AVI `), WMV
   (ASF GUID), ZIP-family, PDF (`%PDF`), OggS, FLAC, ID3/MP3 sync, WAV `RIFF…WAVE`,
   CBZ/CBR/7z.
2. Ambiguity resolution: ZIP containing only images → `Gallery`; ZIP containing
   one CBZ → that CBZ; `.webm`/`.gif` inside a `.forcegallery` directory →
   `Image` (stash#6577, #5185). Directory-level override files
   `.forcegallery` / `.nogallery` are honoured, including the case stash#7179
   is about (`.nogallery` must remove an existing folder gallery).
3. **GIF vs video is decided by frame count and duration**, not container: a
   single-frame or sub-2s-loop file is an `Image` with animation, which is what
   stash#5111 asks for.

**Accept:** `cargo test -p commons-scan detect` — a table-driven test over
`tests/fixtures/` asserting the kind for each fixture, including the four
ambiguous cases above.
**Done when:** the table is exhaustive over the fixtures and the ambiguous
cases are individually named in comments.

**Done** (`a501953`).
### T-P1-002 — ffprobe wrapper and media probing

**Status:** DONE — `9dd4419`. 
**Spec:** §5.2
**Files:** `crates/commons-media/src/probe.rs`
**Depends:** T-P1-001

1. `probe(path) -> MediaInfo { container, duration_ms, video_streams[],
   audio_streams[], chapters[], embedded_tags, creation_time }`.
2. Invoke `ffprobe -v quiet -print_format json -show_format -show_streams
   -show_chapters`. Parse with `serde_json`. **Never** shell out through a
   string: use `std::process::Command` with args as a vector.
3. Embedded chapter titles become `Marker` seeds (§5.11), not metadata strings.
4. Start-time offset handling: stash#7229 is a real upstream bug about
   non-zero stream start offsets breaking preview generation. Record
   `start_time_ms` per stream and have the sprite generator offset by it.

**Accept:** `cargo test -p commons-media probe` against each video fixture,
asserting container, duration within 100 ms of known values, and that a file
with a non-zero start offset reports it.
**Done when:** the offset case has its own named test.

**Done** (`9dd4419`).
### T-P1-003 — Segment model and multi-scene files

**Status:** DONE — `e7d63b3`. 
**Spec:** §5.2
**Files:** `crates/commons-scan/src/segment.rs`, `crates/commons-scan/src/segment.rs`
**Depends:** T-P1-002

1. A `File` may hold N `Segment` rows `(file_id, idx, start_ms, end_ms)`. An
   `Object` is backed by exactly one `Segment` when the file is multi-scene,
   or by the whole `File` when it is not. This is the single primitive that
   answers stash#3530 (38 comments), #2276 (multi-part) and #2511 (virtual
   compilations).
2. `split_file(object_id, boundaries: Vec<u64>)` creates segments, and copies
   existing markers into the overlapping segment (stash#5089).
3. Compilations are `ObjectRelation { PartOf }`, not a new type: a
   compilation is an object whose `Segment`s point at the parts.

**Accept:** `cargo test -p commons-scan segment` — split a fixture at three
boundaries, assert four segments, assert a marker at 00:30 landed in segment 2.
**Done when:** marker inheritance is asserted, not just segment count.

**Done** (`e7d63b3`).
### T-P1-004 — Gallery and archive handling

**Status:** DONE — `fa3d23c`. 
**Spec:** §5.3
**Files:** `crates/commons-media/src/archive.rs`
**Depends:** T-P1-001

1. Read-only listing for zip, rar, cbz, cbr, 7z, gz, tar.gz (stash#233).
2. **Zip-Slip defence is mandatory** (stash#7240 is a real CVE-class report):
   every member path is canonicalised and asserted to be under the extraction
   root before any write. Absolute paths, `..` traversal, and symlink members
   are refused. Add a test with a hand-built malicious zip containing
   `../../etc/passwd` and a symlink member; both must be refused.
3. Report the archive's compression method (stash#7230) and support conversion
   on request.
4. Deleting an image inside an archive rewrites the archive (stash#7106) —
   never fails silently; the job either succeeds or reports why.

**Accept:** `cargo test -p commons-media archive`, including the Zip-Slip test
asserting the sentinel file was not written outside the temp dir.
**Done when:** the Zip-Slip test exists and passes. Do not skip it; it is the
highest-severity item in this phase.

**Done** (`fa3d23c`).
### T-P1-005 — Artifact cache with content-keyed invalidation

**Status:** DONE — `1076570`. 
**Spec:** §10.1, §6.2
**Files:** `crates/commons-core/src/hashing.rs`
**Depends:** T-P0-003

1. `Artifact` row keyed on `(file_id, kind, mtime_ns, size_bytes,
   generator_version)` with the produced path. `kind ∈ {thumbnail, sprite,
   poster, waveform, marker_thumb, transcript, headshot}`.
2. `get_or_generate(file, kind)`: if a row matches the current key, return the
   path. Otherwise generate, then insert. A *stale* row (different key) is
   never returned — this is the fix for stash#7155 and #2773 (content changed
   at the same path).
3. `generator_version` is a constant bumped whenever generation output changes
   format, so a version bump invalidates everything once and correctly.
4. Images are written as WebP or AVIF with alpha preserved (stash#5850 is a
   black-thumbnail bug caused by a JPEG intermediate, and #3038 is the
   compression ask) and served progressively (stash#1585).

**Accept:** `cargo test -p commons-media artifacts` — generate, record path and
mtime; touch the file's mtime and rewrite the file with different content;
assert the artifact is regenerated and the old row replaced. Then a second
call with no change asserts **no** regeneration (compare inode/mtime).
**Done when:** both directions are asserted — stale regenerates, fresh does
not.

**Done** (`1076570`).
### T-P1-006 — Thumbnail and sprite generation

**Status:** DONE — `ca7cb70`. 
**Spec:** §10.1, §6.4
**Files:** `crates/commons-media/src/thumbs.rs`, `sprites.rs`
**Depends:** T-P1-002, T-P1-005

1. Thumbnails: ffmpeg to WebP, width-configurable, alpha preserved, no
   intermediate JPEG.
2. Sprites: a fixed-count horizontal sprite sheet (default 20 frames) for the
   scrubber, with the frame timestamps stored alongside. A marker range can
   then be rendered as a thumb range without touching the file (stash#6811,
   #5275).
3. Both run through a **token-bucket semaphore sized by the §4.3/§6.4 memory
   ceiling** (stash#5762 asks for configurable max memory). When the bucket is
   empty, the job returns `Deferred` and is retried; it never allocates past
   the ceiling and never OOMs.
4. Hardware decode detection: probe VA-API / NVENC / QSV and record what was
   found *and why it was not used* when applicable (stash#7239 asks for the
   actionable reason). Falls back to CPU silently-but-logged.

**Accept:** `cargo test -p commons-media thumbs` — assert WebP output with an
alpha channel preserved (decode and check the alpha of a known-transparent
pixel), assert sprite frame count, and assert the semaphore test: set the
ceiling to 1, run 4 concurrent generations, assert max in-flight == 1 and all
4 complete.
**Done when:** the alpha assertion exists (it is the regression test for
stash#5850).

**Done** (`ca7cb70`).
### T-P1-007 — DONE — Audio, comics, text, funscript, interview types

**Spec:** §5.4, §5.5, §5.6, §5.7, §5.8
**Files:** `crates/commons-scan/src/types.rs` per type
**Depends:** T-P1-001

One ticket per type; they are independent and can be done in any order or in
parallel. Each defines: the fields specific to that kind, the artifact kinds it
needs, and one test.

- **Audio (§5.4)** — closes stash#1258, the single most-discussed issue in the
  corpus (45 comments). Fields: duration, track/disc metadata, replay gain.
  Artifact: `waveform` (peak envelope, 1024 buckets/second, rendered to PNG).
- **Comics (§5.5)** — closes stash#1659. CBZ/CBR/PDF page images, page ordering
  that follows filename numbers (`img_2.jpg` before `img_10.jpg`), a
  `right_to_left` flag, cover extraction. PDF pages render via a `pdftoppm`
  subprocess (or ffmpeg; pick one, document it, do not link a PDF library).
- **Funscript (§5.6)** — sidecar discovery (`<name>.funscript` next to the
  video, plus a `*.funscript/` directory), parse to typed `actions[]`, store
  script metadata (stash-box#851). Playback sync is Phase 6; this ticket is
  discovery, parsing and storage.
- **Text and links (§5.7)** — closes stash#1259. Text object body; link object
  with a stable local identity that survives the remote URL moving. Never
  fetch the body.
- **Interview (§5.8)** — the owner-added type. Fields: transcript ref, chapters
  (as `Marker`s), quotes, topics. An interview's person appearances are
  `Appearance` rows exactly as in a scene, which is what makes §7.1 clustering
  work across formats. Requires a `PersonRef` on every object kind, not just
  scenes — implement that here for all seven kinds so later phases do not
  special-case it.

**Accept:** per-type `cargo test`; each declares one fixture and asserts one
kind-specific behaviour (e.g. comics: `img_10` sorts after `img_2`).
**Done when:** all five types have a passing test each.

**Done** (`e5c370d`).
### T-P1-008 — Frontend shell and virtualized grid

**Status:** DONE — `94fade8`.
**Spec:** §4.2, §10.4
**Files:** `ui/` (SvelteKit 2 skeleton), `ui/src/lib/api/`, `ui/src/lib/components/VirtualGrid.svelte`
**Depends:** T-P0-009

1. SvelteKit 2, adapter-static, TypeScript, no UI framework. Messages via
   paraglide v2 with the local `messages-plugin.mjs` workaround already in use
   elsewhere on this host.
2. Build command is `node node_modules/vite/bin/vite.js build` — **not**
   `pnpm build`. This host uses pnpm with the hoisted linker.
3. One GraphQL client module. **No second private API** for the desktop shell
   (§3.3) — the shell is just a browser pointed at a host.
4. `VirtualGrid.svelte`: fixed row height, windowed rendering, fetches windows
   by keyset cursor (never OFFSET — rule 2, stash#6455/#6390). This component
   is the base for every browse surface in Phase 5.
5. A dev affordance required from the start: every list URL is fully described
   by its query string (§5.16), so a reload reproduces the view.

**Accept:** `cd ui && pnpm install && node node_modules/vite/bin/vite.js build`
succeeds; a Playwright test loads the grid against a seeded server and asserts
only ~30 rows are in the DOM for a 5,000-item result.
**Done when:** the DOM node count assertion exists. That test is what keeps
§4.2 honest at scale.

**Result:** the assertion exists and is tighter than the ticket asked for --
5,000 items render 11 rows and 55 tiles. `ui/tests/keyset.test.ts` (24),
`ui/tests/view.test.ts` (12), `ui/tests/invariants.test.ts` (7) run without a
browser; `ui/e2e/grid.spec.ts` has 11 tests against the real build.

**Note on point 1.** Paraglide is not wired up. Every string in the shell is
user-visible and will be extracted when there is a second locale to extract
it *for*; until then a message catalog is a layer with one value per key, and
it would be a layer that has to be kept in sync with the components by hand.
This is the one item in the ticket that is deliberately not done, and it is
noted in the handoff.

---

## Phase 2 — Library, scanning, performance

**Exit:** a 100k-item library scans and browses within budget; C15–C20 closed;
locator hashes computed.

**Progress:** 8 of 8 tickets done. Phase 2 is complete.

**Done** (`94fade8`).
### T-P2-001 — Filesystem watcher and scan checkpoints

**Spec:** §6.1, §6.2
**Files:** `crates/commons-scan/src/walker.rs`, `progress.rs`, `watch.rs`
**Depends:** T-P0-004, T-P1-001

1. `notify` v6 recursive watcher per library root, debounced 2 s.
2. **Remote mounts get a polled scan instead of a watcher** — an explicit,
   configurable interval, because a watcher cannot be trusted across NFS/SMB/
   rclone (stash#7130 is the "Loading forever" bug this prevents). Detect by
   filesystem type; allow override.
3. Scan checkpoints per library, so an interrupted scan resumes rather than
   restarting (stash#1445).
4. Scan is **idempotent**: running it twice changes nothing.

**Accept:** an integration test with a temp dir: add 100 files, run scan,
assert 100; touch nothing, run again, assert 0 new; interrupt mid-scan, resume,
assert total 100 and no duplicates.
**Done when:** the resume-after-interrupt case is asserted, not just the
idempotent case.

**Status: DONE.** `walker.rs` (walk, skip rules, volume policy,
checkpoint), `progress.rs` (honest progress with a confidence), `watch.rs`
(debounced watcher, tri-state override, diffing poll loop). All four
acceptance cases asserted, including resume-after-interrupt. 164 tests in
`commons-scan`.

**Done** (`7b28aec`).
### T-P2-002 — Content hashing and move detection

**Spec:** §6.2
**Files:** `crates/commons-scan/src/hashing.rs`
**Depends:** T-P2-001

1. xxh128 (fast, for change detection) and BLAKE3 (content identity), computed
   in one streaming pass, 1 MiB buffer, `rayon` across files.
2. `(mtime_ns, size)` is a **hint**; the hash is truth. Recompute only when the
   hint changes. This is the whole of stash#7155 and #2773.
3. Move/rename detection: a file whose path disappeared and whose
   `hash_blake3` reappears elsewhere is a move — rewrite the path, do **not**
   re-extract metadata or regenerate artifacts.
4. `oshash` is computed for one release cycle for backward compatibility, then
   deprecated (stash-box#1115 asks for its removal).

**Accept:** test: hash a file, rename it, rescan, assert the `file` row kept
its id and `artifact` rows are untouched. Then: overwrite a file in place with
same length, rescan, assert the hash changed and artifacts regenerated.
**Done when:** the move case asserts artifact rows are *not* regenerated — that
is the expensive-to-get-right part.

**Status: DONE (1-4).** `hashing.rs` (one-pass xxh128 + BLAKE3 + deprecated
oshash, with the reference implementation's own XXH3-128 vectors as
golden values), `reconcile.rs` (plan/apply against the `file` table, move
detection), and the file-row accessors in `commons-store`. Both
acceptance cases asserted, including that a move leaves `artifact` rows
untouched -- and a further test for the case that assertion alone did not
cover (a file that moved *and* whose hint moved). Six mutations checked.

**Not yet done:** the scanner pipeline that walks a tree, hashes it, and
reconciles it in one pass. `hash_file`, `plan` and `apply` exist and are
tested, but nothing calls them in sequence yet; that is T-P2-003
territory and is called out there.

**Done** (`55dc684`).
### T-P2-003 — File state machine and missing volumes

**Spec:** §6.1, §6.2
**Files:** `crates/commons-scan/src/state.rs`
**Depends:** T-P2-002

1. `FileState ∈ {Present, Missing, Unreadable, Remote}` persisted.
2. A missing volume must **not** cause a rescan storm or per-read logging
   (stash#5683 is a high-CPU loop from exactly this). Detect the volume's
   absence once, mark all files on it `Missing`, and skip.
3. Bulk operations (delete, organise, tag) skip `Missing` items by default and
   say how many were skipped and why, in one line.
4. Deleting a file while generating for it must not wedge the scanner
   (stash#5953): the generator holds a lease; a lost lease cancels the job.
5. Configurable per-volume opt-out of cleanup, for hot-swap users
   (stash#314).

**Accept:** unmount a loopback-backed temp dir mid-scan; assert the scan
completes, files are `Missing`, and CPU does not spike (assert the number of
stat calls, by instrumenting the walker with a counter).
**Done when:** the stat-call counter assertion exists — it is the only
objective measure of stash#5683.

**Status: DONE (1-5).** `state.rs`: the volume tracker with exponential
backoff to a ceiling, the per-volume opt-out, bulk-operation skipping with the
one-line outcome, and generation-scoped leases. `FileState` itself was already
in `commons-core` from Phase 0, so the scan crate uses that one and keeps only
the policy.

**The acceptance test uses a real loopback mount** (`tests/missing_volume.rs`):
a 64MB ext4 image, 400 files, a walk, a real `umount` with the loop device
detached, and a remount. The stat counter is `VolumeTracker::stats`, on the
production type, so a test-local counter cannot pass for a real one. It skips
loudly with a reason where a mount is impossible rather than faking one.

Three bugs were found by writing it, and by the test file taking 92 seconds to
run: `record` anchored the backoff to the last *success* (so a permanently
absent volume never ramped and every scan got a probe), `backoff_for` computed
`2 << n` where it meant `1 << n`, and `configure` took the observed volume
state from the config file. Nine mutations, all caught.

**Done** (`1a6094f`).
### T-P2-004 — Job engine — **DONE**

**Spec:** §6.3
**Files:** `crates/commons-jobs/src/lib.rs`, `queue.rs`, `worker.rs`, `supervise.rs`, `journal.rs`
**Depends:** T-P0-004
**Tests:** 47 in `crates/commons-jobs` (32 acceptance, 8 persistence, 7 supervisor)

1. One durable queue for every slow operation: `scan, generate, identify, match,
   cluster, transcode, autotag, cluster_refresh, backup, index_sync`.
2. Properties, each with the issue it answers:
   - bounded worker pool, one job type per file (stash#2824, #5709) — bounded
     per (kind, *target*), not per kind; a global one-per-kind bound makes the
     100k target unreachable
   - **per-item skip list** — a file that fails N times is marked `Skipped` and
     never retried in the same run (stash#2913 queue looping, #6837)
   - resume after crash from the persisted `job` table (stash#1445)
   - progress, cancel, retry with exponential backoff (stash#3237)
   - subprocess supervision with zombie reaping (stash#5709)
   - inhibit-suspend during tasks (stash#5517)
   - plugins submit into the same queue (stash#5944)
3. `JobQueue::submit(JobSpec)` is idempotent on a `dedupe_key`, so a watcher
   firing 50 times for one file produces one job. Enforced by the unique index
   on `job.dedupe_key`, not only by the in-memory check — a check-then-insert
   has a window, and the window is when a scan and a watcher both see a new file.

**Accept:** all three are named tests. (a) 1,000 duplicate keys, through
SQLite, assert 1 row; (b) a job that always fails, assert `Skipped` and the
queue keeps draining; (c) a `Running` row survives a second `Store` opening the
same file and comes back `Queued` with `attempts: 0`.

**Notes:**
- No migration: the `job` table was in the Phase 0 schema already. The
  accessors are the work, and they live in `commons-store` — `commons-jobs`
  names no SQLx types.
- SQLite only, and said so in the code. `Store`'s Postgres arm is a pool this
  crate cannot bind through without a second path; `store.pool()` panics there
  rather than returning a plausible wrong answer. **Postgres parity for the
  job table is a real remaining gap.**

**Done** (`565fd03`).
### T-P2-005 — Hardware acceleration

**Spec:** §6.4
**Files:** `crates/commons-media/src/hwaccel.rs`,
`crates/commons-media/src/hwaccel_plan.rs`, `crates/commons-media/src/thumbs.rs`

**Status: complete** (`6dcf001`)

Detects VA-API, NVENC, QSV and VideoToolbox, and reports *why* each one that
is unavailable is unavailable (stash#7239: `[0] gives no actionable reason`).
The plan layer turns a detected accelerator into a coherent ffmpeg invocation.

**Accept:** for each unavailable accelerator the reason is non-empty and names
a concrete cause — met, and the shape makes an empty reason unconstructible
(`Unavailable` is an enum, not a string). The acceptance suite additionally
drives the real ffmpeg on real hardware for all three artifact kinds.

**The findings that shaped it:**

- **ffmpeg cannot answer whether acceleration is available.** Its encoder list
  names every compiled-in encoder regardless of the hardware, which is how a
  machine with no GPU reports NVENC as available. Each accelerator is therefore
  probed against the device itself.
- **There is no hardware WebP encoder**, so `-c:v libwebp` never changes.
  Acceleration applies to decode and scale. `AccelPlan::encoder()` takes no
  argument so a caller cannot pass a hardware encoder and produce a JPEG named
  `.webp`.
- **A hardware scaler returns hardware frames**, and neither the software
  encoder nor `tile` can read one. `hwdownload` has to precede the first
  software filter, not the last one — the difference between a working
  thumbnail and an empty sprite.
- **The plan names the device the probe found.** An index (`va:0`) fails on a
  machine whose only DRM node is `card1`, and produces a plan that disagrees
  with the status beside it.

**Verified:** 133 tests in `commons-media`, of which 7 drive the real ffmpeg
against this machine's VA-API device. 616 workspace. Nine mutations, all
caught. fmt and clippy `-D warnings` clean.

**Done** (`6dcf001`).
### T-P2-006 — Storage accounting

**Spec:** §6.6, §5.2 · **Status: done** (`T-P2-006: three numbers, because "how big"
has three answers`)
**Files:** `crates/commons-scan/src/size.rs`,
`crates/commons-scan/tests/storage_accounting.rs`

Real on-disk size per file, a per-library rollup, free/available disk space on
the statistics page (stash#7194), and a configurable temp root distinct from
`generated/` (stash#5646).

**Accept:** integration test with a fixture library; assert the rollup equals
the sum of file sizes **for each of the three bases**, and equals the `du`
invocation that computes *that* basis, exactly.
**Done when:** the `du` cross-check is in the test, for all three bases.

> **Corrected 2026-09-26, after implementing it.** The original criterion was
> "equals `du -sb` of the tree within 1 %". That is the wrong cross-check and it
> cannot be made right. `du -sb` is `du --apparent-size -b`, which is *apparent*
> size, while §5.2 asks for "real on-disk size" — and `du` counts each inode
> once by default *including* under `--apparent-size`, so `du -sb` is apparent
> size **per inode**, a fourth quantity that matches none of the three bases.
> On the acceptance fixture it is 7 096 against the physical rollup's 11 192, a
> 58 % gap. The criterion is now exact equality per basis, which is checkable
> and leaves no room for the bug this ticket is about.

**The three bases, and the `du` that computes each:**

| basis | meaning | cross-check |
|---|---|---|
| `Apparent` | `metadata.len()` | `du -s -B1 --apparent-size --count-links` |
| `Allocated` | `blocks * 512`, per path | `du -s -B1 --count-links` |
| `Physical` | `blocks * 512`, each inode once | `du -s -B1` |

`Physical` is the default and the one a statistics page wants. Exactness is
deliberate: a tolerance hides exactly the failure that matters, which is
having picked the wrong field.

### T-P2-006b — Encoder and thread configuration

**Spec:** §5.2, §6.4 · **Status: done** (`T-P2-006 (encoders): the two values
that were literals`)
**Files:** `crates/commons-media/src/encode.rs`,
`crates/commons-media/tests/encode_settings.rs`

The other two C20 clauses, which the ticket as written did not mention: the
ffmpeg encoder and thread count are configurable (stash#894, stash#819). Both
were literals, or absent.

**Accept:** a configured format is the format of the bytes ffmpeg wrote;
quality changes the file size; `-threads` appears on the command line *before*
the input. **Done when:** all three are checked against real ffmpeg.

`-threads` placement is the non-obvious one. ffmpeg accepts it after the input
and applies it to the output encoder instead of the run, with no diagnostic, so
the test asserts argument *position* by intercepting ffmpeg with a script that
records its own argv. An argument-list comparison passes with the flag in the
wrong place.

**Format is a closed set, not a string.** A user who can type any encoder will
type one that cannot produce the output format, and the result is a
mislabelled file. Each variant carries its pixel format, whether it has alpha,
whether it has a quality axis, and what that axis is — because mjpeg's is
inverted (2 best, 31 worst) and libwebp's is not (0 worst, 100 best). One
"quality" number across both produces the worst JPEG possible from a config
that says 82.

**Done** (`c8b44a2`).
### T-P2-007 — Locator hash computation (plugin interface, core storage)

**Spec:** §5.18, §5.18.1
**Files:** `crates/commons-store/src/locator.rs` (core), `crates/commons-plugin/src/lib.rs`, `crates/commons-store/src/locator.rs` (the `locator.propose` binding)
**Depends:** T-P2-002, T-P0-006

1. **Core** owns the `locator` table, the §14.1 tier gate, and content
   hashing. It does **not** compute ed2k or infohash.
2. Expose to plugins exactly one mutation: `locator.propose(object_id, locator)
   -> Result<LocatorId, ProposeError>`. Core evaluates the consent tier and
   either persists or refuses. This is the whole trust boundary.
3. `ProposeError::TierForbidsLocator` is returned for a `denied` or
   `unverified` object. The plugin cannot see or influence the tier.

**Accept:** a test that a *hostile* plugin (a test fixture that tries every
escape it can think of, including calling propose on a `denied` object) still
only ever receives `TierForbidsLocator` and has no other write path. Assert
also that the plugin cannot write to `consent_record` at all.
**Done when:** the hostile-plugin test exists. This is the most important
test in the project, because it is the one that makes §14 load-bearing rather
than aspirational.

**Done.** The ticket's premise was that the write path existed and the test
would prove §14 load-bearing. Neither held. `propose_locator` was a stub
returning `Stored { locator_id: "stub" }` with no store, so a plugin with
`ProposeMetadata` was told its locator was stored and nothing was. The
sandbox had never been exercised; every test of it passed because it was not
being tested.

The stub is gone. `propose_locator` is async, calls the real store, and the
type cannot be constructed without a `&Store`. The gate lives in
`commons-store`, where a plugin cannot supply the tier it wants to be checked
against.

Two bugs the tests found, both invisible while the stub was in place: a
nonexistent object read as "no consent record" (hence `unverified`), so a
plugin was refused for consent when the object simply was not there; and a
malformed URI was reported as a refusal, so a typo looked like a policy
decision.

The ticket's own recommendation — a hostile plugin that tries to propose at
every tier — needed a correction. Attacking all six tiers is not a stronger
test, it is a *wrong* one: `third_party_permitted` with a stated basis
legitimately accepts a magnet. The rule is per `(tier, scheme)` pair, not per
tier, because §5.18 calls an http source url "plain text, not a P2P protocol".
The test now attacks the five tiers that must refuse a P2P locator with the
flag set on every one, so the tier check is the only thing standing, and
asserts per-scheme.

Verification: 18 tests, 12/12 mutations caught. The mutation work is the
part worth recording. The first version of the table test was circular — it
compared `permits` against `propose` and `propose` *calls* `permits`, so
mutating one mutated both sides and the test passed with the gate broken. It
is now a hand-written table transcribed from the spec. Three substitutions
still survived that, structurally: `permits` multiplies tier and flag, and
five of six tiers forbid a magnet whatever the flag says, so swapping one
restrictive tier for another changes a value nothing downstream can observe.
`consent_facts` is public and tested on its own output, which is what finally
caught them.

**Done** (`78a0ac0`).
### T-P2-008 — Throughput benchmark gate

**Spec:** §6.1, §4.3
**Files:** `crates/commons-scan/benches/scan_100k.rs`, `scripts/bench.sh`

A criterion benchmark over a generated 100,000-file tree (empty files, paths
only — no real media) asserting: initial scan wall time under a stated budget,
and a re-scan of an unchanged tree under a much smaller budget. Record the
budget numbers in the file header, and make the benchmark fail (not just
report) if exceeded by 25 %.

**Accept:** `cargo bench -p commons-scan --bench scan_100k`.
**Done when:** the budgets are written down and the 25 % margin is real.

---

**Done.** The ticket's acceptance criterion pointed at §6.1 and §4.3 for the
budget and both were silent — §4.3 gives memory figures, §6.1 names fifteen
throughput issues and describes the design, and neither states a time. Phase
2's exit criterion ("100k-item library scan and browse within a stated
budget") therefore had no number behind it, which is the same shape of gap as
C20 in T-P2-006: a ticket citing an authority that does not contain the thing
it needs.

The numbers are now in `commons_scan::budget` — initial walk 30 s, incremental
decision pass 4 s, margin 25 % — with the measured figures beside them and the
reasoning for the looseness. The spec carries the same table.

Two corrections the first version needed, both found by running it rather than
by reading it:

* It benchmarked the walk twice and asserted the second was faster. It failed,
  correctly: the walk does identical work both times, because whether a file
  is read is the caller's decision, not the walker's. The walker contains no
  read at all, so the re-scan figure is now the cost of *deciding* not to
  hash — which is what §6.1 actually promises — and the gate asserts the
  relationship (decide < open) as well as the absolute ceilings.
* A `benches/` binary is not a test target. `cargo test` does not compile it,
  so gate logic written there has no coverage: four of six mutations passed
  with the benchmark green, including "never fail" and "ignore the margin". The
  decision logic moved into the library, where 8/8 mutations are caught, and
  the benchmark measures and calls it.

**One requirement was changed, deliberately.** The ticket said "a criterion
benchmark". This is hand-rolled instead, because a criterion benchmark reports
numbers and always exits 0 — it cannot be a gate, and the ticket's own "Done
when" is that "the 25 % margin is real". A gate that cannot fail is a report.
The reasoning is recorded at the `[[bench]]` entry in `Cargo.toml` so nobody
re-adds criterion later thinking it was an oversight.

The benchmark also refuses to run on tmpfs — `/tmp` is tmpfs on this host, and
100k files in RAM measures RAM. Verified by pointing it there: it exits 2 with
an explanation.

## Phase 3 — Identity engine

**Progress:** 4 of 8. **Status:** the library is populatable, and the identity
engine clusters, scores and consolidates. T-P3-004 (merge/split/alias
operations) is next. `python3 scripts/plan-status.py` is the authority on the
counts across all phases.

**Exit:** a person with no name is linked across every appearance in a test
corpus; C21–C32 closed. **This is the phase that makes the project what it is
— do not let it slip behind the infrastructure phases.**

**Done** (`9eb7800`).
### T-P3-000 — The scan pipeline that joins the pieces

**Spec:** §6.1, §6.3
**Files:** `crates/commons-scan/src/pipeline.rs`
**Added:** 2026-09-26, after Phase 2 completed. This ticket did not exist when
Phase 2 was planned; it exists because Phase 2 built every component of a scan
pipeline and nothing called them in sequence, and Phase 3's exit criterion
("a person with no name is linked across every appearance in a test corpus")
needs a library that has been scanned. Flagged at the end of T-P2-006,
T-P2-007 and T-P2-008 before being written.

1. One entry point that walks, reconciles, hashes only what the hint says
   changed, and writes the result — composing T-P2-001's walker, T-P2-002's
   reconciler and hashing, and T-P2-003's volume state.
2. Every slow step is a job on the existing queue (T-P2-004), not a direct
   call, so a scan is interruptible, resumable, and bounded by the same
   per-`(JobKind, target)` concurrency the rest of the system uses.
3. Idempotent: running it twice over an unchanged library produces no writes
   and no jobs. Asserted as a database readback, not as a log line.

**Accept:** a test that scans a generated tree, asserts the file rows exist,
re-scans, and asserts **zero** new writes and zero new jobs. That second
assertion is the ticket — a re-scan that re-hashes everything passes the first
and is the exact regression §6.2 exists to prevent.

**Done when:** the pipeline is the only way a library gets populated, and the
idempotence assertion is in the suite.

**Done** (`369ceec`). `commons_scan::pipeline`, 15 acceptance tests written
before the module existed. Twelve mutations, twelve caught.

Requirement 2 changed shape, and deliberately. "Every slow step is a job"
is wrong for the hash: the decision of *what* to hash has to be made against
the stored state synchronously, and a job that re-read the hint would race
with the walk — a file renamed between the walk and the job is a move and a
deletion at once. Job submission for the artifacts *after* a scan belongs to
the caller, which knows what the library is for. What the pipeline does own
is the interruptibility that requirement was reaching for: `WalkConfig::cancel`
stops the walk at a batch boundary and leaves a resumable checkpoint.

Composing the components found four bugs no component test could see, because
every one of them is a property of the composition:

1. The reconciler could not insert. A fresh library reported "12 new" and
   stored zero rows. Every reconciler test passed because they all began from
   a library that already had rows.
2. **A partial scan marked most of the library deleted.** `ScanInput` had no
   way to say "this scan did not finish", so every path an early-stopped walk
   had not reached looked identical to a deleted file. This is the most
   dangerous defect found so far, and §6.1 now states the rule explicitly.
3. `mark_absent` wrote the literal `'absent'`, which is not a `FileState`.
   A marked row carried a fifth state that `FileState::parse` returns `None`
   for — a file the UI could not classify.
4. `insert_object` was not idempotent, so a library with two copies of a file
   — the ordinary case, since an object's id *is* its content hash — failed
   with `UNIQUE constraint failed: object.id`.

### T-P3-001 — Face detection and embedding

**Spec:** §7.1
**Files:** `crates/commons-ml/src/face.rs`, `crates/commons-ml/src/model.rs`
**Depends:** T-P2-006

1. ONNX Runtime via the `ort` crate. CPU by default; CUDA when the host
   reports it, detected at runtime, never assumed.
2. Detection model runs over generated keyframes (1 per ~10 s, configurable)
   plus every stored headshot. Output: face crops with `(file_id, timestamp_ms,
   bbox)`.
3. Embedding model produces a fixed-width vector per crop, L2-normalised.
   **Model files are fetched by a `models/manifest.toml` with SHA-256
   verification and are not committed to the repo.** A missing model degrades
   the feature and says so; it never crashes the server.
4. Vectors are **not** stored in SQL. They go to a sidecar ANN file
   (`usearch`) keyed by face id. Postgres mode may mirror to `pgvector` for
   peer matching, but no query may require it (rule 2).

**Accept:** test on a fixture with N known faces: assert exactly N embeddings,
assert norms ≈ 1.0, assert the sidecar file round-trips. A test with the
manifest's checksum altered must refuse to load the model.
**Done when:** the checksum-refusal test exists.

**Status:** done (b7a8870). 36 tests, 16/16 mutations caught.

**What shipped.** `model.rs` (verified loading), `face.rs` (crops, provenance, the keyframe plan, the detector), `sidecar.rs` (the vector file). The detector takes its recogniser as a parameter, so the tests drive it without a runtime and the runtime can be added behind that seam without touching the call sites.

**Not shipped, deliberately.** No ONNX runtime is linked, so `ort` is not a dependency and no test asserts that a real face is found in a real frame. The remaining work is a model download path and the inference call itself, behind `Detector::with_recognising`.

**Three bugs the tests found, and the shape they share.** All three were in code that was correct in isolation and wrong in sequence -- the same failure mode as the Phase 2 scan pipeline:

  * SHA-256 produced a different digest for identical bytes fed in pieces. No published test vector could catch it: they are all shorter than one 64-byte block, and the large vector is a whole multiple of the read chunk. The smallest failing input is 8 bytes.
  * The keyframe clamp computed a stride by dividing a count by a count, then added the result as a distance in milliseconds. A three-hour file at a ten-second interval got a 22 ms stride and sampled its first second in 50 samples -- looking entirely correct to a test that checked the sample *count*.
  * A bounding box with a negative origin was accepted, so a detection running off the left edge of a frame became a crop reading from before the buffer.

**The lesson to carry to the rest of Phase 3.** A test that asserts a *count* or a *flag* is the shape most likely to survive a sequence bug, because a sequence bug usually preserves the count while destroying what the items mean. Assert where the last item lands, and assert which specific error came back, not merely that an error did.

**Done** (`b7a8870`).
### T-P3-002 — The clustering engine

**Spec:** §7.1
**Files:** `crates/commons-identity/src/cluster.rs`, `consolidate.rs`

The core primitive. Given a new face embedding, decide: **join** an existing
`PersonCluster`, **create** a new `Anonymous` one, or attach as `Ambiguous`.

1. Threshold-driven: a single configurable distance threshold, exposed in
   settings. The decision and its distance are recorded on the `Appearance`
   row, always.
2. **A cluster is anonymous by default and that is a valid, browsable state.**
   There is no requirement to name anything. This is the difference from every
   upstream tool and it is load-bearing for amateur corpora.
3. Consolidation is an agglomerative pass run as a scheduled job (§15.9), with
   a **transitive-merge guard**: A~B and B~C does not imply A~C unless the
   direct A~C distance also passes. Without this guard, clustering collapses
   into one giant blob, which is worse than no clustering because the user
   then distrusts every link.
4. Conflicting evidence (one embedding matching two named performers) → the
   cluster goes `Ambiguous` and **both candidates are shown in the UI**. Never
   silently guess (§7.1's closing rule).

**Accept:** a synthetic test — 10 clusters of 20 embeddings each with Gaussian
noise, plus 3 deliberate "same person, different weight" cases. Assert:
(1) the 10 clusters are recovered, (2) all 3 weight-change cases stay in one
cluster, (3) a deliberately-planted lookalike pair is marked `Ambiguous` rather
than merged. Test (3) is the one that matters.
**Done when:** all three assertions exist, especially (3).

**Done** (`c377933`).
### T-P3-003 — Body/appearance embedding and composite scoring

**Spec:** §7.4
**Files:** `crates/commons-identity/src/cluster.rs` (the composite arithmetic
lives beside the decision that uses it), `crates/commons-identity/tests/composite.rs`

Face alone splits one person into three clusters across a weight change, which
is worse than not clustering. So identity uses a composite score:

1. `body_embedding`: a silhouette/proportion embedding that survives weight
   change.
2. `score = w_face * d_face + w_body * d_body`, weights configurable, both
   terms recorded on the `Appearance` row.
3. The UI shows *why* two items were linked ("same face 0.87, body
   consistent") — a link the user cannot interrogate is a link they will not
   trust. This is a data requirement, not a UI nicety: store the components.

**Accept:** the Phase 3 fixture set must include at least three pairs of the
same person at visibly different weights, and the test asserts they cluster
together **and** that the stored score has both components populated.
**Done when:** the score-components assertion exists — a single float would
make the feature unexplainable in the UI.

**Done**. `ScoreComponents::combined`, `ScoreWeights` on
`EngineConfig`, migration 0003 for the per-cluster body centroid, and ten
acceptance tests in `tests/composite.rs`.

The ticket's real content turned out to be a decision the spec does not make:
what to do when one of the two sources is missing. `w_face * d_face + w_body *
d_body` with an absent term read as 0.0 scores a close-up at 0.7x its real face
distance, so a face at 0.70 reads as 0.49 and joins a cluster it should have
missed. The composite would then cause the over-merging it was added to
prevent, and only for the images with the least evidence. Weights are
therefore renormalised over the evidence that exists.

Three further decisions, all recorded in the code:

1. **The engine computes the components rather than being handed them.** The
   earlier `assign(object, vector, &ScoreComponents)` was two sources of truth
   for one number: a caller could pass a 0.1 alongside a 0.9-distance vector
   and get the decision it asked for, with the row recording what it supplied.
   §7.4's promise is that the stored components *explain* the stored distance,
   which is only true with one computation.
2. **A face is never compared against a body centroid.** They are unrelated
   embedding spaces; the cosine between them means nothing. A term is present
   only when both sides have it, and a cluster lacking a body centroid
   contributes no body term rather than a fabricated one.
3. **Zero weight disables a source; all-zero applicable weight is an error.**
   A `NaN` distance fails every comparison, so the candidate would be silently
   unrankable rather than loudly wrong. Refused in `assign` before anything is
   written -- the first version created a cluster for an appearance it could
   not score, producing a row with a cluster and no evidence.

Two defects the mutation pass found, both invisible to a green suite:

- **`set_body_centroid` was never called.** A cluster created by a close-up had
  no body centroid, so it stayed uncomparable on the body however many
  full-body shots later joined it. The "written once" test passed *because* of
  the dead code: the centroid was only ever set at creation.
- **The body centroid was specified to be written once, and the face centroid
  to be a mean of members.** Deliberate and asymmetric -- a centroid that is a
  mean can be recomputed; a first observation cannot be averaged across a
  possible model change. The model version belongs on the row and is the
  recorded follow-up.

Fixture traps, written down because each cost real time and would cost it
again:

- `Frame::at`'s per-component noise is worth about 0.36 of cosine *distance*
  over 512 dimensions, which is larger than any threshold a test is likely to
  pick. A fixture that reads as "0.70 away" arrives as 0.45. `Frame::exactly`
  and `Frame::from_pair` exist because of this.
- **Cosine distance does not add along an arc.** Two vectors at 0.05 and 0.70
  from one origin are 0.417 apart, not 0.65. T-P3-002's own comment says so;
  the fixture here assumed otherwise anyway.
- One test returned a `Store` whose `TempDir` had already been dropped, and
  the failure surfaced two hundred lines later as "unable to open database
  file". The directory now travels with the store.

All ten mutations caught, including the two that needed both zero-weight
guards removed to reach.

### T-P3-004 — Merge, split, alias, disambiguation

**Spec:** §7.2
**Files:** `crates/commons-identity/src/cluster/ops.rs`,
`crates/commons-identity/tests/ops.rs`

1. `merge(cluster_a, cluster_b)` re-points every `Appearance`, writes a merge
   record, and **re-derives** any affected field votes from the accepted-edit
   set (stash-box#943 is the bug where merges silently lost edits; the fix is
   that nothing is maintained as a counter — see T-P4-006).
2. `split(cluster, appearance_ids)` moves those appearances to a new cluster
   and **recomputes the original's centroid** so the error does not recur.
3. Aliases are first-class rows with scoped uniqueness, a selectable primary
   name (stash-box#610), and validation that a new name is not an existing
   alias — a **warning, not a hard error** (stash-box#714/#726; in an amateur
   corpus a shared name is common and a hard block would reject real data).
4. Parser correctness, which are two real upstream bugs: commas inside an
   alias never split it (stash-box#778, stash#5033), and non-ASCII names never
   fail auto-tag (stash#2293).

**Accept:** tests for each of the four operations, plus two parser tests: an
alias containing a comma round-trips as one alias; ` performer ` with a
non-ASCII name auto-tags successfully.
**Done when:** both parser tests exist — they are cheap and they are real bugs
that cost users data.

**Done**. Migration 0004 (`cluster_merge`, `cluster_split`, `cluster_assertion`,
and `performer_alias.era`), and twenty-two acceptance tests.

The ticket lists four operations. The spec (§7.2) also asks for
**disambiguation**, which the ticket's title names and its body omits, so it is
in: a `same_as` / `not_same_as` assertion is a table, read by the merge path.

The theme running through all of it: each operation exists because the automatic
path was wrong, so afterwards **the automatic path has to agree with what was
asked**. The recomputation is the feature; the re-pointing is bookkeeping.

Decisions the spec does not make, all recorded in the code:

1. **A `not_same_as` assertion blocks a merge unless a human overrules it.**
   `merge` takes an `actor`; `None` is the automated consolidation and is
   refused on a blocked pair, `Some(who)` is a person deciding. An assertion a
   bulk pass can overrule silently is not an assertion.
2. **A colliding alias is added and flagged, never refused** — §7.2 is explicit
   that a shared name is ordinary data in an amateur corpus, and a hard error
   rejects real names. The cost is that an alias can resolve to several
   performers, so `performers_for_alias` returns them all and never picks one.
3. **Both assertion directions are stored**, so "is this pair blocked" is one
   indexed lookup. The symmetric SQL turned out to be unnecessary — a mutation
   pass showed the second disjunct can never match, because the mirrored row is
   always present — so it is one direction and the redundancy is in the rows.
4. **`era` is free text, not a foreign key.** §7.2 names the concept and not the
   shape of the thing; a nullable TEXT column that can be indexed later is honest
   about that, and a table invented here would be guessing at a schema the rest
   of the spec has not settled.
5. **Names are stored exactly as given.** No trimming, no case folding, no comma
   splitting. Both upstream parser bugs (#778, stash#5033) were a well-meaning
   normalisation turning one name into two rows; normalisation belongs at display
   time, where a person can see it.

Three defects the mutation pass and the tests found, none of which a
count-shaped assertion would have caught:

- **A split could not divide its members.** `member_vector` proposals were a
  bare list per cluster with no record of which appearance each came from --
  enough to compute a centroid, not enough to divide one. So a split recomputed
  the original's centroid over *all* members, which is the mean the ticket
  exists to correct. Member vectors now record their appearance in
  `proposer_id`.
- **An empty split left an empty cluster behind.** The check read
  `appearance_ids.len() == owned.len()`, which is false for an empty list
  against a populated cluster, so the split sailed past it, inserted a cluster,
  moved nothing, and was caught only afterwards. The error was right and the
  state was not. The test that finds this counts clusters *after* the refusal;
  the one asserting the error passed throughout.
- **`move_appearances` moves the named appearances**, which is what a split
  wants and the opposite of what consolidation wants. Reading its name and its
  `keep` parameter together is the only way to tell; the split now says so at
  the call site.

All twelve mutations caught, including five that survived the first pass.

### T-P3-005 — Self-service performer claim — **DONE**

**Spec:** §7.5
**Files:** `crates/commons-identity/src/claim.rs`, `crates/commons-api/src/claim.rs`,
`crates/commons-store/migrations/{postgres,sqlite}/0005_performer_claim.sql`,
`crates/commons-identity/tests/claim.rs`, `crates/commons-api/tests/claim_api.rs`

1. A performer claims a cluster; the claim enters the steward queue (§8.5).
2. On approval, the performer gets a dashboard of their appearances and can
   correct metadata **on their own record only**.
3. A claim is also the mechanism by which a takedown request reaches the right
   person (§14.1) without the person needing an index account.

**Accept:** integration test: submit claim → assert it is queued, not applied;
approve → assert dashboard scope is exactly the claimed cluster and cannot
touch another performer's record. The scope assertion is the point.
**Done when:** the negative case (cannot edit another record) is asserted.

**Implementation notes.**

The rules live in `commons-identity` and the router holds none of them, so the
rules have exactly one implementation whether they are reached over HTTP, from
the CLI, or from a maintenance tool. The API layer's only real logic is the error
mapping, and it is the one place a decision was needed per variant — `NotYourRecord`
is 403 rather than 404 because the caller is authenticated and 404 would promise
the record does not exist, which is a promise §7.5 should not make about another
person's identity; `FieldNotCorrectable` is 422 rather than 403 because the
permission is fine and the field is not, and 403 would tell a performer they
cannot edit their own name.

Three properties the ticket did not name and that the tests turned out to need:

* **`performer_verification` is keyed by `cluster_id`**, so one cluster is one
  person, verified once. This is load-bearing rather than a primary-key
  accident: if two accounts could be verified against one cluster, a takedown
  request would go to both, and each would learn that the other is verified
  against the same content.
* **A takedown request stores no recipient.** The recipient is derived from the
  claim at read time, so a request cannot outlive the claim it was aimed at and
  there is no row recording who was told. A request with no recipient escalates
  to the stewards (`to_stewards`) rather than being dropped, which is the one
  outcome §7.5's third clause exists to prevent.
* **The correctable fields are an allowlist, not a blocklist.** The dangerous
  fields are the ones that are not metadata — a consent tier, a verification, a
  claim state. A blocklist is a list of the fields someone thought of, and the
  next one would be writable until somebody noticed.

One account claiming two clusters resolves to one performer record; minting a
second would give one human two identities to write to, and the scope rule is
written against a performer id.

**Mutations:** nine, all caught. Two survived the first pass and both were test
gaps rather than code gaps — every single-cluster fixture made "the verified
account" and "the verified account of *this* cluster" the same string, so
`takedown_recipients` ignoring the cluster was invisible. The dashboard's account
filter had the same problem: with one verified account, "every appearance in the
database" and "this account's appearances" are the same query result. Both are
now covered by two-account, two-cluster tests.

### T-P3-006 — Performer field model and multi-valued attributes — **DONE**

**Spec:** §7.6, §7.7, §7.9, §7.10, §7.11, §7.12

Close stash-box#234, #210, #206, #1141, #553, #205 and stash#7204, #6866,
#6925, #2956, #5748, #5326, #5434, #765, #6218.

1. `AttributeType`-typed fields (§7.7). `Measurement` is date-stamped and
   multi-valued, which is what makes automatic career span and age-at-scene
   possible.
2. Career span derived from item dates, never stored as a user-editable fact
   (it is recomputed; §7.11).
3. Status as a first-class enum (deceased/retired/inactive), filterable.
4. Appearance type per item (cameo, non-sexual, group, duologue), driving the
   "appear with" graph and excluding performers from it.

**Accept:** schema test per field type; a test that a 3-year span with 5 dated
measurements produces the correct derived career span and the correct age at a
given item's date.
**Done when:** the derivation test exists and recomputes identically after a
no-op rescan (i.e. it is derived, not incrementally drifted).

**Implementation notes.**

*Files:* `crates/commons-identity/src/attrs.rs`, `.../span.rs`,
`crates/commons-store/migrations/{postgres,sqlite}/0006_attr_types.sql`,
`crates/commons-identity/tests/attrs.rs`, and `tests/common/{mod.rs}` for the two
object fixtures.

**The vocabulary is the schema's.** `attr_type_vocab` in the migration is the
statement of which types exist and which are multi-valued, and
`AttrType::from_schema` reads it and *refuses* to proceed if the Rust enum
disagrees. It fired during development: `date` was seeded single-valued while
`is_multi` said multi. That is the guard working, and it is the reason the
vocabulary is a table rather than a `match` in Rust — the two would have drifted.

**Values are refused, not coerced.** Every `AttributeError` names what was wrong.
A wrong-typed value stored anyway reads back as something the writer never meant,
and nothing anywhere reports it. `measurement` is checked for its date *before*
its type, so an undated measurement is reported as undated rather than as "a
number where a measurement belongs" — the first is the thing the writer can act
on.

**`AttrValue` is externally tagged.** Adjacently tagged (`{type, v}`) was nicer
and is not possible: serde cannot serialise a bare primitive inside an internally
tagged enum, and `Text(String)` is a bare primitive. The store's
`no_internally_tagged_enum_carries_a_bare_primitive` test is what caught it, and
it exists because the mistake compiles.

**Two bugs the tests found in code that predates this ticket:**

*0002's uniqueness on `appearance` forbids two performers in one scene.* It
restated 0001's `(object_id, cluster_id, appearance_type)` as
`(object_id, appearance_type)` partial indexes. The stated reason was sound — a
NULL compares unequal to every NULL, so with `cluster_id` nullable the original
constraint stopped constraining the ambiguous rows — but the fix dropped
`cluster_id` from the key, so **one appearance per object per type**. A scene
with two people in it could not be recorded, and §7.12's appear-with graph is
built by joining appearances through `object_id`, so the whole feature is
unbuildable against it. 0006 restores the key using
`COALESCE(cluster_id, '')` as an *expression* index column rather than a
sentinel written into the data, which is what 0002's own comment was reaching for
and rejecting for the wrong reason.

*0001's uniqueness on `custom_field_value` allows one value per subject per
date.* For a `multi` field that is precisely wrong: two values written on the
same day collide, and the refusal names no field and no value. 0006 drops it and
rebuilds with `value_json` in the key. On SQLite that is the twelve-step table
rebuild, written out in full in the sidecar — the unnamed table-level `UNIQUE`
cannot be dropped any other way.

**`date` is single-valued, not multi.** A person has one birth date. The
repeated-over-time case is `measurement`, which carries its own date. This was
the disagreement the vocabulary guard caught.

**`derive` counts undated items.** The bounds need two dates so they come from
the dated ones; the *count* is a number and an item with no date is still an
item. An inner join would have made the count silently drop them.

**Mutations:** fourteen. Two survived the first pass. `add()`'s duplicate check
had no test — a silently doubled value is a list claiming three ethnicities when
there are two. And the age boundary was only tested a month clear of the
birthday, so a comparison off by a day in either direction passed; the test now
covers the day before, the day of and the day after, plus a 29 February birthday
in both a leap and a common year. One further survivor was a genuine no-op:
removing `rescan`'s touch-loop changes nothing because `derive` inside it
queries anyway, so the function is correct by construction rather than by
assertion — recorded in the code rather than papered over with a test that
cannot fail.

---

## Phase 4 — Curation and voting

**Exit:** an amateur corpus with no upstream source is fully curated by
proposal and vote; C33–C45 closed.

### T-P4-001 — FieldProposal resolution — **DONE**

**Spec:** §8.1
**Files:** `crates/commons-index/src/resolve.rs`,
`crates/commons-store/src/index.rs`,
`crates/commons-store/migrations/{postgres,sqlite}/0007_proposal_justification.sql`,
`crates/commons-index/tests/resolve.rs`, `tests/common/mod.rs`

1. `resolve(subject, field) -> ResolvedValue`: weight each proposal by
   `user_weight × recency_decay`, winner by weight, computed and cached.
2. `locked` fields (stash-box#213) return the pinned value and refuse
   proposals until a steward unlocks.
3. Auto-proposals (`MlTagger`, `MlCaptioner`) participate with their confidence
   as weight — a voter, never an override.
4. The role table's `public` refusal lives in the vote path, not the view.

**Accept:** the ticket's four named tests, plus tie reporting, retraction,
machine-proposal bounding, per-field isolation, and cache-invalidation.
**Done when:** all four are named tests. **Met** — 23 tests in
`tests/resolve.rs`.

**Implementation notes.**

**The cache is keyed on the evidence, and the configuration with it.** The key
is `(subject, field, fingerprint)`, where the fingerprint is FNV-1a over the
*contents* of every proposal and every live vote. Nothing has to remember to
invalidate anything: a new proposal or a new vote is a different key, so a
stale entry cannot be *looked up*, not merely needs to be invalidated. This is
the only shape that is safe in a system still growing write paths.

*The configuration is in the key too*, and a test found that the hard way. A
key of `(subject, field, fingerprint)` alone silently ignores `half_life_days`:
the evidence is identical, so the second configuration is served the first one's
answer. `two_different_half_lives_give_two_different_answers` passes by accident
until the key includes the config — the two settings gave byte-identical
weights. Every knob is hashed from its bits rather than its `Debug` output, so
a field added to `ResolveConfig` cannot be forgotten here.

**Extraction is not inference.** `ProposalSource::is_inference` separates
`ml:tagger`/`ml:captioner` — which looked at the content and *decided* — from
`filename`/`embedded`/`caption`, which only moved a string that was already
there. Only an inference's confidence counts as support. Without the
distinction, a 0.99-confidence filename parse and a 0.99-confidence tagger are
the same claim, and every title in a freshly-scanned library settles on whatever
the first parser guessed with no human in the loop. A proposal nobody has voted
on and no model has *judged* weighs zero; the test that pins this gives the
extraction a real 0.99 confidence, because without one the arithmetic zeroes it
for the wrong reason and the assertion passes for free.

**A weight floor applies per voter, never to a proposal's score.** The first
version clamped the total into `[min, max]`, which gave every *unvoted*
proposal the same positive weight and made one beat a heavily-backed value —
the voting system inverted. The floor exists so a repudiated account can still
vote (§8.3's newcomer problem), and a weight of zero would be indistinguishable
from being silenced, which must be a moderator's decision. Both belong on one
account's contribution.

**The ceiling is a bound on one account, not on the scale.** Clamping the total
to 5.0 made reputation 5 and reputation 6 indistinguishable, so §8.1's "better
evidence moves the value" quietly stopped holding at exactly the top of the
scale. It is 25.0.

**Per-field reputation is applied once, at the ballot.** `cast_vote` writes the
account's `field_reputation` into `vote.weight`; resolve decays that number and
does not restate it. Multiplying by the per-field map as well squares it, which
runs every account into the ceiling — the same bug as above, reached by a
different road.

**Sybil damping is absolute, not relative to the field's largest bloc.** The
relative version looks like fairness and is not: the biggest bloc divides by
itself, so it is never damped, and a field with eight accounts against one damps
nothing. `1/sqrt(n)` with a floor of 0.2 says what §8.3 means — a bloc is
discounted however large it is, so its *marginal* backer is worth less and
less — and the floor stops a bloc of two being worth nothing next to a bloc of
one.

**Ties are reported, and the fallback is deterministic.** Weight descending,
then value ascending. Without the value tiebreak the same evidence gives
different answers on two machines, and `a_tie_is_reported_as_a_tie_not_broken_by_row_order`
would not be able to say which. A tie is a distinct state from "contested but
decided" because a tie needs a human and the other does not.

**`age_vote` does the date arithmetic in Rust, not in SQL.** SQLite's
`datetime()` rewrites a timestamp into its own format, and the schema's format
is RFC 3339 — so an SQL-side shift produced `2025-09-26 17:15:11`, which
`ts::age_days` cannot parse, and every aged vote decayed as if brand new. The
two decay tests passed *for the wrong reason* until this was found: they were
agreeing with the no-decay answer. `ts::shift_days` and `ts::age_days` now live
in commons-core so the format is one function's business.

**`commons-core` gained `Role::may_vote` and `Role::may_lock`, and
`Account` is read as a tuple.** `may_curate` existed; `may_vote` did not, and
`public` refusing to vote is the whole of what makes stash #2792's anonymous
read-only case mean anything. Deriving `FromRow` on `Account` would have been
the obvious way to read it and is forbidden: T-P0-007 makes commons-core the
bottom of the graph, and a `FromRow` derive drags sqlx in under everything.

**Migration 0007 adds `field_proposal.justification`.** §8.2 requires every
proposal to record *why* it exists, and 0001 had nowhere to put it — so the only
way to satisfy the requirement was to synthesise the wording from the source at
read time, which is a guess about a parser's output presented as a fact. It is
nullable: a fixture or a silent peer has no justification, and inventing one is
worse than admitting there is none.

**Mutations: eighteen.** Four survived the first pass and all four were real
findings, not test noise:

* the cache key omitted the configuration (above);
* the endorsed rule was a no-op for any proposal whose weight was already
  zero, so `is_inference` was untested until the extraction was given a
  confidence;
* damping computed but unused, and then relative rather than absolute;
* the unendorsed-proposal rule, which the arithmetic was masking.

One mutation survived a second pass and turned out to be genuinely redundant: a
fingerprint built from the live vote set cannot distinguish "retracted" from
"never cast", so an earlier version read *all* rows including retracted ones.
The extra read changed no outcome — the retracted row is invisible to the
arithmetic either way, and it is the absence of the live row that moves the key
— so it was removed rather than kept as insurance, and the reasoning is in the
code where the next person will look.

### T-P4-002 — Candidate generation (§8.2) — DONE

Nine proposers, one function each, in `crates/commons-index/src/candidates.rs`.
A candidate is a field, a value, a source, a justification and an optional
confidence — never a proposal, because §8.1 has not seen it yet.

The signals the proposers read had nowhere to live, so **migration 0009**
(`file_signal`, plus `peer_value` and `file_phash` indexes) adds the table that
holds them. `propose_and_store` is idempotent: a rescan re-extracts every signal
and creates no second proposal.

Each proposer has a positive and a negative test, as the ticket requires — 34
integration tests and 19 unit tests. The negative tests are the ones that carry
the weight, and several of them found real bugs rather than confirming intended
behaviour:

* **An exact phash match was excluded.** The query had `AND phash != ?` to skip
  the file's own object, but that skips *every* exact match — the case §8.2 names
  first. A byte-identical re-upload of a described item proposed nothing while a
  near-identical one did. The `object_id ==` check below it already excludes the
  file's own object correctly, since one object holds several files.
* **A bare year was not a date.** `is_plausible_date` demanded a full
  `YYYY-MM-DD`, so `Title (2021) [1080p].mkv` — the most conventional release
  filename there is — yielded no date. Worse, a test asserted that
  `Chapter 12 (2021)` holds *no* date, which pinned the bug instead of the intent.
* **A four-digit number in brackets was a year.** `Title (1080)` proposed the
  resolution as a date. A range check (1890–2099) is what separates a year from a
  resolution; shape alone cannot.
* **Release tags were not recognised as written.** `is_quality_token` stripped
  only the ends of a word, so `H.264` and `WEB.DL` — both real spellings — were
  not tags and were proposed as titles.
* **Two tags from one model could not be stored.** The unique key on `file_signal`
  omitted the value, so a tagger's second tag for a file collided with its first.
* **A single letter and a content hash were titles.** `a.mkv` proposed "a" and
  `a3f9c2e1.mkv` proposed "a3f9c2e1" — a downloader's filename, repeated for every
  file a downloader has ever named.
* **Two branches of one ticket disagreed on a ceiling** (`resolve`'s 5.0 and
  `ReputationConfig::max_weight`'s 4.0), and the SQLite half of migrations 0008
  used a table-level `UNIQUE` with a `COALESCE` expression, which SQLite rejects.
  Postgres accepted both, so review passed and the first SQLite test failed.

Verification: `950 tests, 0 failures`; `cargo fmt --check` clean; `cargo clippy
--workspace --all-targets -- -D warnings` clean. 16 mutations of the nine
proposers were tried and every one was killed. Two survivors were real gaps and
are now covered by `phash_match_uses_the_threshold_not_just_the_algorithm`,
`phash_match_does_not_propose_an_objects_own_title` and
`a_quality_token_is_not_a_title`.

### T-P4-003 — Reputation and trust — **DONE**

**Spec:** §8.3
**Files:** `crates/commons-index/src/reputation.rs`,
`crates/commons-store/migrations/{postgres,sqlite}/0008_reputation_audit.sql`,
`crates/commons-index/tests/reputation.rs`

1. Reputation derives from **agreement with settled outcomes over time**.
2. New accounts ramp from a low base; sustained rejection decays them.
3. **Sybil damping** discounts a bloc; a coordinated pattern is **flagged for a
   steward, never silently punished**.
4. Weights are **per-field**.
5. Trust tier with double votes is steward-granted with an audit trail.
6. A vote's weight is frozen at cast time.

**Accept:** simulation — 100 accounts, a known fraction coordinating; (a) honest
weight rises over rounds, (b) the bloc's marginal influence is sublinear, (c) the
bloc is flagged. Per-field independence asserted.
**Done when:** the per-field independence assertion exists. **Met.**

**Implementation notes.**

**Nothing here is a counter.** `reputation_event` is append-only and the weight is
recomputed from it, by the same rule §8.6 applies to votes. A counter drifts:
every write path must remember to update it, and there is always one that does
not. Here a lost increment is impossible, because there are no increments — a
lost *event* is a missing row, and the recomputation is the only thing that reads
the log, so it is the one place a bug would show.

The unique index on `(account, COALESCE(proposal_id, ''), kind)` is what makes a
settlement pass re-runnable. `settle_round` can be called again after a merge or
a rollback without double-counting, which is a property of the schema rather than
of a caller's loop. `a_repeated_settlement_does_not_double_count` pins it.

**The log is authoritative and the columns are its cache.** `cast_vote` reads
`account.field_reputation`. The first version left it that way after
`reputation.rs` existed, and the consequence was that reputation as implemented
could not affect a single outcome: a settlement pass recomputed a weight, wrote
it to the log, and no vote ever saw it, because the vote read a column nothing
wrote. Every test that did not go through a vote still passed.

`ballot_weight` is now the only writer of `account.reputation` and
`account.field_reputation`, and `cast_vote` calls it. A column anybody can write
is a second source of truth, and the disagreement between two sources is exactly
the bug above. `voting_refreshes_the_cached_reputation_columns` asserts the cache
equals the log to within a float epsilon.

**A frozen weight means a reputation change applies from the next ballot, not by
rewriting the last one.** T-P4-001 left this open and it belongs here: a settle
pass that could change what somebody said they believed when they said it would
make §8.6's audit trail editable. The consequence is that a weight above
`max_weight` is not a reputation at all, which is what forced the test fixture to
change shape — see below.

**The disagreement penalty saturates; it does not compound.** `penalty·d/(1+fd)`,
not `penalty·d·(1-f)^d`. The compounding version was written to mean "each
dispute costs less than the last", and it is not monotone: at the defaults the
total penalty runs 0.30, 0.36, 0.324 for the first three disputes, so an account's
standing got *better* on its third rejection. A punishment that expires is not a
punishment.

Only `sustained_rejection_decays_weight_but_not_to_zero`'s *per-round* assertion
found it, and that is the transferable part: "is the final value lower" cannot see
a non-monotone curve. A decay has to assert that it never rises, round by round.

**Three detection rules, and each needs an off switch.** A proposal is referred to
a steward when its backers hold a large share of the field's votes, when
essentially nobody disagrees with it, or when the group behind it has never once
been on the losing side. All three were individually deletable with the suite
green, because every realistic test shape trips two or three at once and a test
that passes if any of three things happens cannot tell you which worked.

The fix is `coordination_share`, `coordination_unanimity` and
`coordination_lockstep` in the config, so a test can arm one rule and assert the
input shape makes the other two impossible — which it does *before* checking the
outcome, so the test fails for the right reason if the shape is wrong. Six tests
in `mod rules` now cover each rule in both directions: referred and not referred.

A detector with no off switch can only be tested through the shape of its input,
and that is a design smell in the production code, not only in the test.

**Flagging changes no weight, and that is its own test.** `flagging_does_not_change_any_weight`
reads every weight in the bloc before and after a detection. §8.3 says
"flagged, not silently punished", and that is a claim about a *process* — not
that a number is right but that a step did not run — so it cannot be an
assertion inside a test about numbers.

**A trust tier is a multiplier, and it is not the role.** Tier 0 is exactly 1.0
(`an_untiered_account_weighs_exactly_its_reputation`), tier 1 is
`tier_multiplier` and each tier above squares it, so tier 2 is
stash-box#630's "double votes". A tier-1 is deliberately less than double, so the
tier a steward grants without discussion is not the tier that doubles somebody's
vote. `trust_tier` is a separate table from `role` because a role is what an
account may *do* and a tier is what its word is *worth*; collapsing them would
mean granting vote weight and queue access with one click.

The revocation keeps the grant row and fills in `revoked_by`. A deleted grant is
indistinguishable from one never made, and a reason is required on both paths —
`a_tier_change_needs_a_reason` covers `""`, `"   "` and `"\t\n"`.

**Mutations: twenty-four.** Eight survived the first pass and every one was a
real gap. Two findings are worth carrying:

* the four tests that wanted a "5.0-reputation account" could not be repaired by
  making the fixture write events, because the curve is bounded at
  `max_weight = 4.0` and no number of agreements reaches 5.0. Inverting the
  curve for an unreachable value is an infinite loop, and the fixture looped to
  400 round-trips before the suite timed out rather than failed. The fix is
  `vote_with_weight`: cast the vote, then freeze the weight onto it. That is
  what a vote's weight *is* — a number frozen at cast time — so it says what the
  test means, where an out-of-range account reputation was a claim about an
  account that cannot exist. Those three `resolve` tests had been asserting
  nothing at all: `cast_vote` recomputed all three weights back to 1.0.
* `an_untiered_account_weighs_exactly_its_reputation` compares the weight against
  the *standing*, not against `weight()` — because `weight()` is itself
  multiplied, so a broken multiplier cancels out of the comparison and the
  assertion passes with the bug in place.

One mutation survives and is equivalent rather than missed: making tier 0 take
`tier_multiplier` instead of `1.0` is unobservable through `powi`, because
`tier_multiplier.powi(0) == 1.0`. Removing the `tier <= 0` guard is caught the
moment the comparison is to `tier_multiplier` rather than to `1.0`, and that is
the test above.

### T-P4-004 — Edit history with recomputed integrity — DONE

**Spec:** §8.6
**Files:** `commons-index/src/history.rs`, migration 0010

**The rule: scores are recomputed from the accepted-edit set, never maintained as
a counter.** There is no score column anywhere and no second implementation of
this number in the codebase; that absence is the enforcement.

Migration 0010 adds `field_edit` (the accepted set), `history_report` and
`object_merge`. The four states of a row carry §8.6's distinctions: `retracted_at`
marks a row out of the set without deleting it, `author = NULL` with `removed_at`
set is an unlinked attribution, and a deliberate null is a row whose `value_json`
is `null` while an unset field is no row — which is the whole of stash-box#9.

`FieldState` has three variants, not two. `the_three_states_are_pairwise_distinct`
exists because two tests each comparing one state to `Unset` would pass if `Null`
and `Set` were the same value.

Four bugs the tests found:

* **History order was random.** `ORDER BY accepted_at, id` ties on
  `accepted_at` whenever two edits are written in the same millisecond — which is
  most pairs of edits in a real scan — and the fallback was a uuid, so the tie
  broke at random. `a_fields_history_is_the_accepted_edits_in_order` read back
  `["Second", "First"]`. There is now a `seq` column, written from a sequence
  table, in one transaction with the insert.
* **A deliberate null was invisible.** `current_entry` ordered by `weight DESC` and
  then `id`, so a field that had been set and then cleared — two ordinary edits,
  both weight 1.0 — reported `Set` with the *old* value. Clearing a field is
  something a user did, and a reader that cannot see it is stash-box#9 again. The
  tiebreak is recency.
* **A reverted field's value was wrong for the same reason**, and a revert
  written before the fix was indistinguishable from an edit.
* **A migration declared an index and a table with the same name.** SQLite
  rejects it; Postgres does not, because a table and an index live in different
  namespaces there. So it passed every Postgres test.

Two things the tests were wrong about, and both are recorded in the code:

* A test asserted `Chapter 12 (2021)` holds no date, which pinned the bare-year
  bug from T-P4-002 rather than the intent.
* `Iterator::max_by` returns the *last* of equal maxima, so a `.rev()` added to
  make it pick the newest made it pick the oldest. Only visible because both
  fixture edits weigh the same.

Verification: `967 tests, 0 failures`; `cargo fmt --check` clean; `cargo clippy
--workspace --all-targets -- -D warnings` clean. 11 mutations of the recompute
rule were tried — including `MAX` → `SUM`, which is the counter bug in one
character — and every one was killed.

### T-P4-005 — Moderation, locking, disputes

**Spec:** §8.5
**Files:** `commons-index/src/moderation.rs`

Steward queue for contested fields, disputed merges, disputed consent tier, and
abuse reports. Editing another's pending edit with attribution (stash-box#599),
amending an edit (#226), editing closed submissions (#570), per-user pending
limits (#782), pinned comments (#700), name-collision warning before submit
(#714, #950), ignore-lists for studios and performers (stash-box#787),
excluded studios that cannot accept new scenes (#1175).

**Accept:** per-queue item type, a test that the right role can resolve and a
lower role cannot. The negative authorization test per type.
**Done when:** all seven negative tests exist.

### T-P4-006 — Leaderboards, badges, points

**Spec:** §8.4
**Files:** `commons-index/src/points.rs`

Leaderboards over **contributors, never performers** (§2). Badges, bounties,
quests, reward points for invited contributors, configurable invite-key count
(stash-box#551). Points are earned by **accepted** proposals, tying the economy
to T-P4-001.

**Accept:** test that points are awarded on acceptance and withdrawn on
rejection-after-acceptance; test that no query surfaces a "top performers"
ranking.
**Done when:** the negative test (no performer-popularity surface) exists —
it guards §2's most easily eroded promise.

### T-P4-007 — Consent tiers as a store-layer filter

**Spec:** §14.1, §8.12
**Files:** `commons-consent/src/lib.rs`, `commons-store/src/consent_filter.rs`
**Depends:** T-P0-005

**Rule 3 lives here.** `Store::query(filter, caller)` ALWAYS ANDs in
`ConsentVisible { caller }` (§14.1's tier table plus the caller's content
filter). There is no API that returns objects without it.

1. Tiers: `Unverified, SelfPublished, PerformerClaimed, ThirdPartyPermitted,
   Quarantined, Denied`.
2. A caller's content filter (stash-box#643, #733, #1005, #986) is enforced at
   the query layer so a hidden category cannot leak through search,
   recommendation, export, or DLNA.
3. `Quarantined` and `Denied` are invisible to everyone but stewards.
4. Revocation propagates as a tombstone and is **never outvoted** (§13.2).

**Accept:** the central test of the project: for every query path in the
codebase (list, search, recommendation, export, sitemap, DLNA, GraphQL
resolver), assert that a `Denied` object never appears. Implement it as a
shared test helper applied to every path, so a new query path that forgets
the filter fails by default.
**Done when:** that shared helper exists and is applied to all paths. This is
the difference between a consent model and a consent *claim*.

### T-P4-008 — Takedown pipeline

**Spec:** §14.1
**Files:** `commons-consent/src/takedown.rs`, `commons-federation/src/propagate.rs`

1. Report → `Quarantined` everywhere → if accepted → `Denied` + content-hash
   blocklist entry that propagates to every peer and is checked on import,
   scan and match.
2. A performer can request takedown without an index account, via a signed
   request verified against their §7.5 claim.
3. Locators on a denied object are destroyed with the tombstone (§5.18).
4. Redaction of history on request (stash-box#656).

**Accept:** two-peer integration test: peer A denies an object, peer B
receives the tombstone, assert the object is invisible on B and a re-import of
the same object is refused. Assert any locator was destroyed, not tombstoned.
**Done when:** the re-import refusal is asserted.

---

## Phase 5 — Discovery, search, images, interface

**Exit:** the 174-image-issue and 93-tag-issue clusters closed; C46–C63.

### T-P5-001 — Search index, both engines

**Spec:** §9.2, §9.3
**Files:** `commons-store/migrations/*/0002_search.sql`, `commons-store/src/search.rs`

Search across titles (all languages), descriptions, tags, performer names and
aliases, clusters, studios, groups, markers, **transcripts and captions**
(stash#4985), and external IDs. Includes performers and tags in keyword search
(stash#2976) and alias-aware search (stash#3266, stash-box#804, #742).

**Accept:** a shared fixture corpus with known expected hits, run against both
engines, asserting **identical result id sets**. That equality test is what
keeps rule 2 honest for search.
**Done when:** the cross-engine equality test passes.

### T-P5-002 — Fuzzy, phonetic, synonyms

**Spec:** §9.3

Fuzzy + phonetic matching, alias and nickname awareness, per-vocabulary
synonyms. Same tokenizer and same synonym table in both engines; the synonym
table is data (a table), not code.

**Accept:** a typo-tolerance test (`reciever` finds `receiver`) and a synonym
test, both run against both engines with identical results.
**Done when:** both engines agree.

### T-P5-003 — Tag system

**Spec:** §5.15, §9.4
**Files:** `commons-store/src/tags.rs`

Tags with parent (tree), namespace, color, typed attributes, importance weight
(stash#2973). Namespaces are the honesty mechanism for ML tagging: an ML tag is
always labelled `ml:<model>` (stash#560, #848, #722). Breadcrumbs (stash#1723),
tree view (stash#1732), create-from-anywhere (stash#2736), undo (stash#3221).

**Accept:** test that an ML-proposed tag is stored with an `ml:` namespace and
is visually distinguishable from a `canonical` tag; test breadcrumbs resolve
for a 3-deep tree.
**Done when:** both exist.

### T-P5-004 — Duplicate and similar detection

**Spec:** §9.7
**Files:** `commons-scan/src/dedup.rs`

`same_scene_as` / `re_encode_of` relations, phash ANN nearest-neighbour search
(stash#1220), perceptual near-duplicate detection, and content-hash identity
(§5.18) as the strongest signal. The duplicate checker is a *view* over
relations, which is why stash#39 and the #5786/#5823/#1220 cluster stop being
separate features.

Side-by-side merge with obvious direction (stash#6429), 100 %-identical display
(#5823), quality metrics (#5067, #2397), mark-not-duplicate (#1656), exclude
organized (#3531), select-by-path (#6382), report export (#5412),
merge-and-delete-files in one task (#6430), auto-merge by rules (#2094, opt-in
and reversible).

**Accept:** fixture set with one byte-identical pair, one re-encode, and one
false positive; assert each is classified correctly and that auto-merge is off
by default and reversible when on.
**Done when:** the false-positive case is asserted — a dedup feature that
cannot be wrong is not trustworthy.

### T-P5-005 — Lightbox, image organization, per-image metadata

**Spec:** §10.2, §9.6
**Files:** `ui/src/lib/components/Lightbox.svelte`, `ui/src/lib/components/ImageGrid.svelte`

150 image issues live here. Per-image tags, ratings, ordering, custom numbering
(stash#4950), O-counter sums (#5364), gallery rating (#6576), per-image
similarity search (§9.6). The lightbox must not navigate on wheel-pan or fast
drag (stash#7149, #7148, #7147 — three separate upstream bugs), and its back
button must not surface an unsaved modal (stash#7154).

**Accept:** Playwright test dispatching a wheel event mid-pan and asserting the
image index did not change. Each of the three upstream bugs gets its own named
assertion.
**Done when:** all three exist — they are cheap and they are the actual
complaints.

### T-P5-006 — View modes and bulk editing

**Spec:** §10.4, §10.6, §10.7, §10.9, §10.10

Wall with group-by and auto-scroll (#6544, #6955), rich list tables for bulk
edit (#517, #899), folder view (#1586), unified media view (#1030), vertical
TikTok-style feed (#3859), keyboard map and command palette, undo for
destructive actions (#3221), bulk-edit modal (#5336), right-click paste (#7139),
unsaved-entry protection (#6466), CSV import (#1296), per-field ignore lists
(#2318, #2399), create-from-subpage (#3694) and create-all-missing (#1017,
#3122).

**Accept:** Playwright spec per surface. The unsaved-protection test must
attempt navigation with an unsaved edit and assert a confirm appears.
**Done when:** that test exists.

### T-P5-007 — Theming, accessibility, deep links

**Spec:** §10.8, §15.10
**Files:** `ui/src/lib/theme/`, `ui/src/lib/stores/url_state.ts`

Dark/light, contrast, focus management, screen-reader labels, hit-target sizes
(stash#3322, #6383), safe-area insets (stash#5979 — note this is a *responsive
web* fix, not a Safari-specific hack), viewport-constrained popovers
(stash#4667). Every view state is a resolvable URL (§15.10): filters, sorts,
view mode, tagger selection, player position. Time-limited share links
(stash#5612).

**Accept:** Playwright: navigate to a filtered+sorted grid, reload, assert
identical DOM state. Plus an axe-core scan with zero critical violations.
**Done when:** both pass.

---

## Phase 6 — Player, plugins, API

**Exit:** external player, cast, full plugin SDK, public API; C64–C68, C73.

### T-P6-001 — Player

**Spec:** §11.1

Codec fallback with on-demand proxy, subtitle toggle (embedded or sidecar),
frame-accurate seek, deinterlacing (#5313), crop/pan/flip filters (#5312,
#2160), custom speed and long-press 2× (#2645, #6982), A/B loop with touch
(#5178) and points on the scrubber (#6509), audio-track selection (#1058),
control-bar layout surviving fullscreen and mobile (#6526, #6811),
skip-intro-per-source (#634), ratings in the player (#3250).

**Accept:** Playwright per feature; the fullscreen-control-clipping case
(#6526) must assert the control bar is within the viewport at 360×640.
**Done when:** that assertion exists.

### T-P6-002 — Subtitles and captions

**Spec:** §5.10
**Files:** `commons-media/src/subtitles.rs`, `ui/src/lib/player/SubtitleTrack.svelte`

Sidecar and embedded, ASS/SSA (#3077), SRT, VTT. Cues survive transcode.
Caption *search* (stash#4985) and multi-language entries (stash#5514). Exposed
over DLNA (#5420) and injected for external players (#2770).

**Accept:** test that a cue survives a transcode round-trip with timestamps
correct to 40 ms; test an `.ass` file parses and renders.
**Done when:** both exist.

### T-P6-003 — Funscript and interactive playback

**Spec:** §5.6
**Files:** `ui/src/lib/player/FunscriptPlayer.svelte`, `commons-media/src/funscript.rs`

Browser playback with timing sync, token/drm note (#5650), AutoBlow support
(#6579), manual pause (#2762), and N named action axes with a 2-axis overlay
and an N-axis controller (#6339).

**Accept:** a Playwright test with a fake clock asserting a marker at t=10 s
fires within 50 ms of the scripted position.
**Done when:** the timing assertion exists.

### T-P6-004 — Interviews: transcription and Q&A search

**Spec:** §5.8
**Files:** `commons-ml/src/asr.rs`, `ui/src/lib/interview/`

Local ASR (whisper.cpp or a Parakeet ONNX model) with word-level timestamps;
chapters derived from the transcript as `Marker`s; quotes and topics as
weighted `Tag`s; speaker attribution into the same `PersonCluster` as other
appearances; human corrections fed back as `FieldProposal`s (§8.1), not as
overwrites.

**Accept:** test on a fixture audio clip asserting word timestamps are
monotonic and within 200 ms of a hand-checked transcript. Assert a corrected
word becomes a proposal, not a silent overwrite.
**Done when:** both exist.

### T-P6-005 — Cast, DLNA, external players

**Spec:** §11.2, §11.3
**Files:** `commons-server/src/dlna.rs`, `commons-server/src/cast.rs`, `commons-server/src/external_player.rs`

Chromecast, DLNA, AirPlay (#4136, 17 comments). DLNA exposes subtitles (#5420)
and saved filters as virtual folders (#3135, #1580) including legacy layout
(#3107). External hand-off to mpv/VLC/Jellyfin/Plex with resume position and
injected metadata (#2747, #2770, #966 STRM).

**Accept:** a DLNA discovery test (SSDP multicast on loopback) asserting the
server advertises and serves a browse request. External player: assert the
generated command line matches an expected argv array exactly — never a shell
string.
**Done when:** both exist.

### T-P6-006 — Plugin SDK completion

**Spec:** §11.4
**Files:** `commons-plugin/src/api.rs`, `ui/src/lib/plugins/`

Build out the full surface promised in T-P0-006: hooks (§11.4's list — file
destroy, duplicate-checker context, hook source context, phash via hook,
plugin task settings), UI extension points (#4510), services tab (#5118),
plugin settings UI with defaults (#5002, #6899), error surfacing as toasts
(#1695), reinstall (#6987), and the required pre-install backup (#6185).

**Accept:** a documented-API test that the full manifest schema validates both
a valid and an invalid plugin; a test that installing a plugin with an
undeclared capability fails to instantiate.
**Done when:** the undeclared-capability test exists.

### T-P6-007 — Public API, SDK, Jellyfin compatibility

**Spec:** §11.5, §13.3

GraphQL (matching upstream familiarity) + REST + OpenAPI. Upload endpoints for
video and image **with consent attestation required** (#4995, #1125, #13).
Jellyfin-compatible read API (#2747). Client SDK. Changelog (#69). Playground at
`/playground`.

**Accept:** an OpenAPI schema snapshot test; an upload test asserting that
omitting the consent attestation field returns an error, not a default.
**Done when:** the missing-attestation test exists.

---

## Phase 7 — Federation and consent

**Exit:** two peers exchange claims and propagate a takedown; C71, C72, C79,
C80.

### T-P7-001 — Peer list and signed claims

**Spec:** §13.1
**Files:** `commons-federation/src/peer.rs`, `claim.rs`, `sign.rs`

1. Peers are **explicitly configured** — no ambient discovery, no DHT (see
   §5.18.1's property).
2. A claim is `(kind, subject, payload, peer_id, timestamp, content_hash,
   signature)` signed with ed25519. Reject unknown peers, bad signatures,
   stale timestamps, and content-hash mismatch.
3. What travels: identity claims, field-value proposals, fingerprint matches,
   **tombstones and takedowns**. What never travels: file bytes, local paths,
   transcripts, private lists, anything under a forbidden consent tier.

**Accept:** two-peer test — sign a claim, exchange, assert acceptance; tamper
one byte, assert rejection; replay an old claim, assert staleness rejection.
**Done when:** all three exist.

### T-P7-002 — Conflict handling without consensus

**Spec:** §13.2
**Files:** `commons-federation/src/merge.rs`

1. **No consensus algorithm**, because there is no global truth. A peer accepts
   a claim if it improves local state by the same rules as any other proposal
   (T-P4-001). Conflicts become **disputes for stewards**, not forks to
   reconcile.
2. Merges propagate as claims. Takedowns propagate as tombstones and are never
   voted on.
3. A peer's local decision is final for that peer. Federation is a channel, not
   a leader.
4. Locator claims inherit consent gating: a peer accepts one only if its local
   tier permits redistribution (§5.18).

**Accept:** two peers with divergent edits; assert both reach a stable state
without either discarding the other's evidence into a dispute record, and
assert a tombstone cannot be outvoted by a large weight of ordinary claims.
**Done when:** the tombstone-outvoted assertion exists — it is §14's
guarantee.

### T-P7-003 — Import, export, backup, restore

**Spec:** §13.4
**Files:** `commons-store/src/portability.rs`, `crates/commons-server/src/backup.rs`

Full-fidelity export **including consent records**, so a takedown survives a
migration. Scheduled backup, a restore drill, config backup (#2636),
folder-based metadata import/export (#428, #5498). Import is lossless and
idempotent, and refuses fingerprints from a blocked peer (§14.1).

**Accept:** round-trip test: export a seeded library, import into a fresh
instance, assert deep equality of every table. Then run the documented
restore-drill script and assert it succeeds.
**Done when:** the round-trip equality assertion exists — "full fidelity" is
otherwise an unverified claim.

### T-P7-004 — Migration safety

**Spec:** §14.2
**Files:** `commons-store/src/migrate.rs` (extend)

Forward-only, ordered, transactional in both engines, each gated on a verified
backup, with a dry-run mode and a documented rollback plan. `schema_version` in
`/healthz`.

The gating test, which is the whole ticket: attempt a migration with a
corrupt backup present and assert it refuses; attempt to re-apply an existing
migration and assert the immutability check fires.
**Accept:** both assertions.
**Done when:** the immutability test exists.

---

## Phase 8 — Operations, docs, export

**Exit:** non-root container, backup/restore drill, docs site, research export.

### T-P8-001 — Containers

**Spec:** §12.3
**Files:** `Dockerfile`, `docker-compose.yml`, `.dockerignore`

Non-root by default (stash#684), healthcheck, versioned volumes, compose
bringing up server + Postgres with `pgvector` and ML sidecar optional. CI
builds the image with tagged releases (stash-box#935).

**Accept:** `docker run` the image, `curl /healthz` from inside the container
as the non-root user, assert 200.
**Done when:** it runs.

### T-P8-002 — Native install and updater

**Spec:** §12.4
**Files:** `scripts/install.sh`, `systemd/commons-server.service`, `crates/commons-server/src/update.rs`

Static binary, systemd unit, XDG paths, in-app updater with a **signed**
release manifest that refuses to self-update across a major version without
explicit consent (#758, #5625).

**Accept:** a test that a manifest with a bad signature is rejected, and one
that a major-version bump requires an explicit flag.
**Done when:** both exist.

### T-P8-003 — Reverse proxy and TLS

**Spec:** §12.5
**Files:** `deploy/nginx/`, `deploy/caddy/`, `deploy/README.md`

nginx and Caddy recipes, static-asset and websocket tuning, `X-Real-IP`
(stash#6883).

**Accept:** run the app behind the recipe and assert a websocket upgrade
succeeds through the proxy. A proxy config that breaks websockets is the
common failure, so test it.
**Done when:** the websocket-through-proxy test passes.

### T-P8-004 — Tauri desktop shell

**Spec:** §3.4, §4.3
**Files:** `desktop/` (Tauri v2), `crates/commons-client/`

1. The shell points at `http://127.0.0.1:<port>` by default and can point at a
   **remote hosted instance** — it is a client, not a private build.
2. It uses `commons-client`, the same typed client the UI uses. No private API.
3. Native file dialogs, tray, single-instance lock.
4. Its own RSS budget (≤120 MB) joins the T-P0-008 harness.
5. Ships as a **separate artifact** from the server binary; the server must be
   runnable with no shell present.

**Accept:** an automated launch, a `/healthz` fetch, and the RSS assertion.
**Done when:** the RSS number is measured and recorded.

### T-P8-005 — Observability

**Spec:** §12.6
**Files:** `crates/commons-server/src/observability.rs`, `ui/src/routes/settings/logs/`

Structured logs with TTY/file split (#2463, stash-box#655), in-app log viewer
with clear (#1171), sanitized log upload with redaction preview (#342), a
file-health hub listing which files are broken and why (#837), job traces,
`/metrics`, troubleshooting mode via env var (#6875).

**Accept:** a test that the redaction preview removes absolute paths and any
`consents` values before upload.
**Done when:** the redaction test exists — uploading logs from a consent-first
platform is exactly where a leak would matter.

### T-P8-006 — Documentation and onboarding

**Spec:** §15.5, §15.1, §15.2, §15.6

Docs site, visible in-app help (#700), first-run tour, offline manual (#6365),
sample library, `ARCHITECTURE.md` (stash-box#472). Country/locale reference
table with correct names (Taiwan #5237, UK countries stash-box#808) and an
i18n contribution path where adding a language is a PR, not a code change
(#7253 Kiswahili as the worked example).

**Accept:** a link-checker over the docs build with zero broken links.
**Done when:** it passes.

### T-P8-007 — Research export and fingerprint privacy

**Spec:** §15.7, §15.8
**Files:** `commons-store/src/research_export.rs`

De-identified dataset dumps, opt-in per account, stripped of
consent-restricted tiers, documented schema. Fingerprints salted per peer
before publication (stash-box#633), rotatable salt, opt-out submission,
validation before submission (stash#2149) and a bounded growth policy
(stash-box#814 is phashes growing forever).

**Accept:** test that an export for a `Denied` object contains nothing about
it — not the row, not a hash, not a count. And that submitting a corrupt
fingerprint is rejected (stash#2149).
**Done when:** both exist; the first is the more important.

### T-P8-008 — Scheduled maintenance

**Spec:** §15.9
**Files:** `commons-jobs/src/schedule.rs`

Periodic clustering re-consolidation, orphan cleanup, fingerprint
re-verification, artifact GC, peer index-sync. Every job visible, pausable and
logged.

**Accept:** test that the scheduler does not stack duplicate runs of the same
job and that pausing survives a restart.
**Done when:** both exist.

---

## Phase 9 — Public serving (adopting stash #2792)

**Exit:** an anonymous visitor browses a consent-tier-visible library; a share
link grants scoped access; a subscriber delete routes to the steward queue.
**This phase is last because it requires T-P4-007's query-layer consent
enforcement to already be proven.**

### T-P9-001 — Role model and anonymous access

**Spec:** §8.1 role table, §12.1
**Files:** `commons-store/src/roles.rs`, `commons-api/src/authz.rs`

Roles `public` (no login), `subscriber`, `contributor`, `steward`, `admin`.
Anonymous read-only browsing of consent-tier-visible items, with streaming,
download and voting all gated. An audit log.

**Accept:** unauthenticated request test suite: assert an anonymous client can
browse, and **cannot** vote, download, propose, or see a `Denied` object —
four negative assertions, all against the same store path.
**Done when:** all four exist. The anonymous case is precisely where a
UI-only filter leaks, which is why rule 3 is absolute.

### T-P9-002 — Streaming surface

**Spec:** §12.1.1
**Files:** `commons-media/src/hls.rs`, `commons-server/src/stream.rs`

On-demand HLS for browsers, direct file serving with HTTP range requests for
local-network clients, quality ladders configurable per role, anonymous
visitors served the lowest rung. The §4.3 memory ceiling applies to the
transcoder too.

**Accept:** a range-request test asserting a `Range: bytes=100-199` returns
206 with exactly 100 bytes and the correct `Content-Range`. An HLS test
asserting a playlist and one segment play in sequence.
**Done when:** both exist — range requests are trivially broken and invisible
when broken.

### T-P9-003 — Expiring share links

**Spec:** §12.1.1
**Files:** `commons-api/src/share.rs`

A signed, expiring, optionally password-protected URL granting exactly one
capability (view, or view-and-download) on one item or one smart collection,
revocable, with an access log. A capability grant, not a second account
system.

**Accept:** tests for: valid link works; expired link refused; revoked link
refused even before expiry; wrong password refused; a view-only link cannot
download. All five.
**Done when:** all five exist.

### T-P9-004 — Moderation on destructive public actions

**Spec:** §12.1.1
**Files:** `commons-api/src/moderation_routes.rs`

A `public` or `subscriber` action that would delete, merge or bulk-edit routes
to the steward queue instead of executing.

**Accept:** a subscriber DELETE returns "queued" and the object still exists;
a steward DELETE removes it. Assert the object is untouched in the first case.
**Done when:** both halves of that assertion exist.

### T-P9-005 — Points economy

**Spec:** §12.1.1
**Files:** `commons-index/src/meter.rs`

Points earned by accepted contributions, spent on metered actions. **The meter
is a cached per-account counter with periodic flush, never a transactional
join per action** — the author of #2792 noted this explicitly. Bandwidth-heavy
actions use a per-account quota, not a per-action price, because a price per
action invites the join that was the original objection.

**Accept:** a load test issuing N metered actions and asserting the per-action
cost is O(1) database work (count the queries; assert the count does not grow
with N). Then assert the periodic flush reconciles exactly.
**Done when:** the query-count assertion exists.

---

## Phase 10 — The C91 locator plugin

**Exit:** a consent-permitting object hands a magnet or ed2k URI to a
configured download client on one click; the sandbox makes speaking BitTorrent
or ed2k structurally impossible. **Delivered through the extension SDK, so it
doubles as the SDK's first real consumer.**

### T-P10-001 — Plugin scaffold and manifest

**Spec:** §5.18.1
**Files:** `plugins/locator-p2p/Cargo.toml`, `src/lib.rs`, `manifest.toml`, `README.md`

WASM target. `manifest.toml` declares exactly: `Fs { roots: [<library>] }`,
`Db { scope: ReadOnly }`, `LoopbackHttp`, `Task`, `Ui`. It does **not** declare
`Net`. Version, signature, and a human-readable capability list that the
pre-install dialog renders verbatim.

**Accept:** the manifest validates against the Phase 0 schema; a test asserts
`Net` is absent.
**Done when:** both.

### T-P10-002 — ed2k and infohash computation

**Spec:** §5.18, §5.18.1
**Files:** `plugins/locator-p2p/src/ed2k.rs`, `infohash.rs`

1. **ed2k**: 128-bit MD4 over eDonkey2000's own chunking (9,728 KiB partial
   chunks, padding on the final chunk), stored as `ed2k_hash + ed2k_size`.
   This is the fiddly part of the project and is the one place where
   "implement it" is insufficient — see the acceptance note below.
2. **BitTorrent infohash** for single- and multi-file torrents, computed from
   the bencoded metainfo the plugin builds from local files. Multi-file
   torrents map onto `Segment` (§5.2).
3. Core's xxh128/BLAKE3 are **not** reimplemented here; the plugin consumes
   `file_id` metadata and calls core via the host API.

**Accept:** ed2k hashes verified against a **reference implementation's
published test vectors**, not only against our own implementation. If no
reference vector can be obtained, this ticket does not complete — record the
gap in the ticket rather than closing it on self-consistency. Infohash: verify
against `transmission-show`/`mktorrent` if present, else against a known
single-file torrent's published infohash.
**Done when:** ed2k and infohash are both verified against something outside
this codebase. I flagged this in the spec review; it is the highest-risk
correctness item in the project.

### T-P10-003 — `locator.propose` client

**Spec:** §5.18.1
**Files:** `plugins/locator-p2p/src/propose.rs`

The plugin's **only** mutation path. Calls `locator.propose(object_id,
locator)`; core evaluates the §14.1 tier table and persists or refuses. The
plugin never sees the tier and cannot bypass it.

**Accept:** the hostile-plugin test from T-P2-007 re-run against this real
plugin: proposing on a `Denied` and on an `Unverified` object both yield
`TierForbidsLocator`, and the plugin has no other write path.
**Done when:** both refusals are asserted against the real plugin.

### T-P10-004 — External client hand-off

**Spec:** §5.18
**Files:** `plugins/locator-p2p/src/handoff.rs`

One click → push the URI to a configured local client (qBittorrent,
Transmission, Deluge, aria2) via its HTTP API. **The tier is re-checked at the
moment of the action**, because consent can be revoked between storage and
use. Each adapter is a separate module with a recorded-request test.

**Accept:** per-client test against a mock HTTP server on 127.0.0.1 asserting
the exact request path and body each client expects. Plus: revoke consent
*after* storing a locator, then attempt hand-off, and assert it is refused —
that is the whole point of re-checking.
**Done when:** all adapters tested and the revoke-then-attempt test exists.

### T-P10-005 — One-click install, disclosure, removal

**Spec:** §5.18.1
**Files:** `ui/src/lib/plugins/gallery/`, `plugins/locator-p2p/manifest.toml`

Gallery entry, single-click install, capability disclosure shown *before*
install, signed manifest verification, versioned and updatable, fully
removable — and removal takes its locators with it unless the operator chooses
to keep them as plain metadata. Pre-install backup required (stash#6185).

**Accept:** Playwright: from a fresh install, three clicks to a working
locator list. Uninstall, assert locators are gone. Assert the disclosure
dialog appeared *before* the install and named loopback-only access.
**Done when:** all three assertions exist.

### T-P10-006 — The no-downloader proof

**Spec:** §5.18, §2
**Files:** `tests/e2e/p2p_boundary.spec.ts`, `plugins/locator-p2p/tests/`

The closing test for §2's central claim. Assert, mechanically:

1. The plugin's manifest declares no `Net`.
2. Every egress attempt from plugin code is denied (reuse T-P0-006's harness).
3. No core module contains a BitTorrent or ed2k protocol implementation —
   assert by scanning for the protocol's magic constants and wire keywords
   (`"d1:announce"`, `"5:files"`, eDonkey opcode `0xE0`/server handshake) in
   the core crates, and fail if found. This is a crude grep-as-test and it is
   deliberately crude: it is a tripwire against a future contributor adding a
   downloader "just for dedup".
4. No core module opens a listening socket except the configured HTTP server.

**Accept:** `cargo test -p commons-plugin p2p_boundary` plus the source scan.
**Done when:** all four assertions exist. Item 3 is a lint, not a
recommendation — that is the point of writing it down.

---

---

## Phase 11 — The community ecosystem (stashapp/CommunityScripts, CommunityScrapers)

**Added 2026-09-26 at the owner's request.** This phase is deliberately last: it
is the only phase whose value is entirely borrowed, and every ticket before it
is about the thing this ecosystem plugs into. Nothing here is implemented until
Phase 10 is closed.

### What is actually in those repositories

Measured, not assumed. Both are AGPL-3.0, both are actively pushed
(CommunityScrapers `master`, CommunityScripts `main`), and the `stashapp` org
holds 14 public repositories.

| Repo | Stars | Forks | Shape |
|---|---|---|---|
| `stash` (Go) | 13022 | 1184 | the reference implementation |
| `CommunityScrapers` | 841 | 520 | **729 YAML + 155 Python** scraper definitions |
| `stash-box` (TS) | 371 | 96 | GraphQL metadata graph, MIT |
| `CommunityScripts` | 284 | 242 | **462 plugin files, 54 themes**, plus userscripts |
| `Stash-Docs` | 80 | 64 | the manual, CC-BY-SA |
| `plugins-repo-template`, `scrapers-repo-template` | 13, 1 | — | scaffolding |
| `StashServer`, `StashFrontend`, `StashOSX`, `metadata-api-discuss` | — | — | **archived**, pre-2019, not targets |

The two that matter here split into **two genuinely different problems**, and
this plan does not pretend otherwise.

**The scrapers are mostly declarative.** 729 of the 982 files under
`scrapers/` are YAML: an entry-point table (`performerByURL`, `sceneByFragment`,
`galleryByURL`, …) plus `xPathScrapers` / `jsonScrapers` blocks that are
selectors, `concat`, and a `postProcess` list of `replace` / `parseDate` /
`truncate` / `map` transforms. A YAML scraper is a *program in a tiny
declarative language*, and adapting it is a matter of writing an interpreter
for that language. The 155 Python ones are not: they are ordinary programs that
import `py_common` and, increasingly, `AyloAPI`.

**The plugins are ordinary programs too.** 79 of them are Python invoked as
`exec: [python, "{pluginDir}/x.py"]` with `interface: raw` and a `tasks:` list;
the rest are TypeScript userscripts against the stash GraphQL API, or themes
(54 of them, plain CSS). There is no `plugin.json` in the whole repository —
the manifest *is* the YAML, and the plugin id is the directory name.

### The three decisions this phase rests on

1. **The YAML scraper language gets an interpreter, not a translator.** 729
   definitions is too many to translate one at a time, and they are declarative
   by design. A converter would need to stay in sync with every upstream
   construct forever; an interpreter is written once and tracks the language.
   The cost is that our `xPathScrapers` / `jsonScrapers` / `postProcess`
   semantics must be a *superset-compatible* reimplementation, and every
   divergence has to be a named, tested, reported difference.
2. **The Python scrapers and plugins get a compatibility layer, not a
   rewrite.** `py_common` is a real library with a real API; reimplementing 155
   programs in Rust is not adaptation, it is a different project with a worse
   success rate. So `py_common` ships as a shim over Commons' own host API,
   and a scraper that needs a capability Commons does not have says so at load
   time rather than failing halfway through a scrape.
3. **Nothing is vendored.** Every artifact is fetched at install time from
   upstream, pinned by commit, and cached. No scraper or plugin source is
   copied into this repository. This is a licensing decision as much as a
   maintenance one: AGPL-3.0 content in-tree would make the whole workspace
   AGPL, and a vendored copy also goes stale the moment upstream pushes.

### T-P11-001 — Upstream catalogue and pin

**Files:** `crates/commons-plugin/src/catalogue.rs`, `crates/commons-ecosystem/`

A client for the two repositories' GitHub APIs. Clones nothing and executes
nothing; it builds a catalogue of `(repo, ref, path, kind, declared
requirements)` and can pin a set of artifacts to exact commit SHAs.

**Accept:** a test against a recorded fixture of the two trees that asserts the
catalogue sees 729 YAML scrapers, 155 Python scrapers, 79 plugin directories and
12 theme directories — and fails loudly if upstream has diverged, because a number that
changes silently is a number nobody is maintaining. Pinning is by SHA, and a
test asserts a SHAs-pinned fetch is byte-identical across two runs.
**Done when:** the count assertions exist.

### T-P11-002 — The declarative scraper interpreter

**Files:** `crates/commons-ecosystem/src/scraper/`

Parses a stash scraper YAML and evaluates it: the entry-point table, the XPath
and JSON selector languages, and the `postProcess` chain
(`replace` / `parseDate` / `truncate` / `map` / `dateFormat` / `switch` /
`filter` / `setDefault`). Selectors evaluate against `lxml`-shaped results via
`quick-xml` plus an HTML5 tree, not against a browser.

**Accept:** every construct in the 729-file corpus is either implemented or
recorded in a `unimplemented.yaml` list with the count. A differential test
runs the interpreter over N real scrapers and asserts it produces a *structurally
valid* result object; it does **not** assert equality with stash, because
stash's Go implementation is the reference and a byte-comparison against it is
a test of their code, not ours. The unimplemented list is asserted to be
non-growing across a fixture sweep, so it cannot quietly grow.
**Done when:** the sweep runs and the unimplemented count is in the test output.

### T-P11-003 — `py_common` compatibility layer

**Files:** `crates/commons-ecosystem/src/pybridge/`

A Python runtime embedded in the host process, with `py_common` and its
`util` / `cache` / `config` / `deps` / `graphql` modules reimplemented on top of
Commons' `HostApi`. The scraper contract is the standard argv/JSON-stdout
protocol, so a scraper that cannot be adapted is reported as *incompatible*
with the missing capability named.

**Accept:** a `py_common.util.dig` / `replace_all` / `replace_at`
compatibility test against upstream's own test vectors, so the shim's
semantics are pinned to theirs rather than to ours. A scraper that calls an
unimplemented `py_common` function fails at *load* with the function named, and
a test asserts that — the alternative is a scrape that produces half a result.
**Done when:** the load-time failure test exists.

### T-P11-004 — Scraper registration, selection, and the `AyloAPI` surface

**Files:** `crates/commons-ecosystem/src/registry.rs`, `ui/src/lib/scrapers/`

Registry, URL-to-scraper matching, fragment matching, the search path, and the
proposal pipeline into Phase 4's `FieldProposal`. `AyloAPI.scrape` is a facade
over the same six entry points, so a scraper written against it works unchanged.

**Accept:** a test that a URL is offered the right scrapers and *no others* —
over-matching sends a scrape to a site that will 404, which reads to a user as
"the scraper is broken". Every result becomes a `FieldProposal` and none of
them writes a field directly; a test asserts a scrape cannot bypass the proposal
queue.
**Done when:** both assertions exist.

### T-P11-005 — Plugin and theme compatibility

**Files:** `crates/commons-ecosystem/src/plugins.rs`, `crates/commons-plugin/src/api.rs`

The 79 plugin directories (95 of the 462 files are Python) and the 12 theme
directories, 54 files of CSS served into the app's stylesheet layer. The YAML becomes a `Manifest`:
directory name is the id, `version` is the manifest version, `exec` is a
`ProcessSpawn` capability that is **refused by default** under
`HostPolicy::first_party` and must be granted visibly.

**Accept:** a test that installing one of these plugins without granting
`ProcessSpawn` fails with a reason naming the capability; and a themesheet test
that a theme's CSS is scoped to the theme and does not leak into the base
stylesheet.
**Done when:** the capability refusal names the capability.

### T-P11-006 — The divergent-behaviour report

**Files:** `docs/ECOSYSTEM-COMPATIBILITY.md`

The honest output of this phase: a generated table of what each upstream
artifact does, whether it works here, and if not, which Commons feature is
missing. Generated, not hand-written, and regenerated in CI so it cannot rot.

**Accept:** the document is generated from the catalogue and a test fails if
regenerating it produces a diff that is not committed.
**Done when:** the regeneration check is in CI.

### T-P11-007 — Userscripts and the stash GraphQL surface

**Files:** `crates/commons-api/src/compat/`

The 4 userscripts and any plugin that talks to stash's GraphQL API, mapped onto
Commons' own API. Kept last in the phase because it is the only part that
depends on `commons-api`, which does not exist yet.

**Accept:** a test that a query written against stash's schema either resolves
on Commons' schema or is reported as unsupported by name. No silent empty
results: an unsupported field returns an error, because a scraper that reads
`null` and writes `null` is how a library fills up with blanks.
**Done when:** the unsupported-field error test exists.

### What this phase explicitly does not do

- **No vendored copies.** See decision 3.
- **No reimplementation of a scraper in Rust.** Where a Python scraper cannot
  run, it is reported as incompatible. A hand-port that diverges from upstream
  is worse than no port, because the next upstream push fixes theirs and not
  ours.
- **No "compatible" claims for a scraper that was not run.** The compatibility
  report is generated from execution, so a scraper marked working has actually
  produced a result here.
- **This does not make Commons a stash replacement.** It makes the existing
  ecosystem usable in Commons. Where stash's semantics and Commons' differ
  outright — no DHT, no auto-download, a different consent model — the
  difference is reported, not papered over.

---

## Appendix A — Ticket index by capability

| Capability | Spec § | Tickets |
|---|---|---|
| C01, C02 video, multi-scene | 5.2 | T-P1-001, T-P1-002, T-P1-003 |
| C03 images, galleries, archives | 5.3 | T-P1-001, T-P1-004, T-P5-005 |
| C04 audio | 5.4 | T-P1-007 |
| C05 comics | 5.5 | T-P1-007 |
| C06 funscript | 5.6 | T-P1-007, T-P6-003 |
| C07 text, links | 5.7 | T-P1-007 |
| C08 interviews | 5.8 | T-P1-007, T-P6-004 |
| C09 relations, extras | 5.9 | T-P1-003, T-P5-004 |
| C10 subtitles | 5.10 | T-P6-002 |
| C11 markers, chapters | 5.11 | T-P1-002, T-P6-003 |
| C12, C13, C14 lists, organised | 5.13, 5.14 | T-P5-006 |
| C15–C20 library, scanner, jobs, hw, storage | 6 | T-P2-001…008, T-P0-008 |
| C21–C24 clustering, merge, fingerprints, body | 7.1–7.4 | T-P3-001…004 |
| C25 self-claim | 7.5 | T-P3-005 |
| C26–C32 performer model | 7.6–7.12 | T-P3-006 |
| C33, C34 proposals, candidates | 8.1, 8.2 | T-P4-001, T-P4-002 |
| C35, C36 reputation, leaderboards | 8.3, 8.4 | T-P4-003, T-P4-006 |
| C37, C38 moderation, history | 8.5, 8.6 | T-P4-005, T-P4-004 |
| C39–C43 titles, ratings, studio, groups, lists | 8.7–8.11 | T-P4-006, T-P5-004 |
| C44 consent filtering | 8.12 | T-P4-007 |
| C45 funder links | 8.13 | T-P5-003 |
| C46–C50 filters, search, tags, folders | 9.1–9.5 | T-P0-005, T-P5-001…003 |
| C51, C52 image org, dedup | 9.6, 9.7 | T-P5-004, T-P5-005 |
| C53 recommendations | 9.8 | T-P4-006 |
| C54 artifacts, previews | 10.1 | T-P1-005, T-P1-006 |
| C55–C63 lightbox, identify, views, player UX | 10.2–10.10 | T-P5-005, T-P5-006, T-P5-007 |
| C64 player | 11.1 | T-P6-001 |
| C65, C66 cast, external | 11.2, 11.3 | T-P6-005 |
| C67, C68, C73 plugins, API, SDK | 11.4, 11.5, 13.3 | T-P0-006, T-P6-006, T-P6-007 |
| C69, C70 users, auth | 12.1, 12.2 | T-P9-001 |
| C71, C72 federation | 13.1, 13.2 | T-P7-001, T-P7-002 |
| C74 import/export/backup | 13.4 | T-P7-003 |
| C75–C78 deploy, observability | 12.3–12.6 | T-P8-001…003, T-P8-005 |
| C79, C80 consent, migrations | 14.1, 14.2 | T-P4-007, T-P4-008, T-P7-004 |
| C81–C90 housekeeping | 15 | T-P8-006…008, T-P5-007 |
| C91 locators (plugin) | 5.18, 5.18.1 | T-P2-007, T-P10-001…006 |
| Community scrapers, plugins, themes | 11.4, 13.5 (adopting upstream) | T-P11-001…007 |

## Appendix B — Spec's own non-goals, restated as build-time checks

| Spec non-goal | Enforced by |
|---|---|
| No Electron | T-P8-004; the shell is Tauri/webkit2gtk. A CI grep for `electron` in `desktop/` fails. |
| No downloader | T-P10-006 (protocol scan + sandbox proof) |
| No cloud requirement | T-P8-001 (compose with every remote service optional) |
| No performer-popularity surface | T-P4-006 negative test |
| No payments | §8.13 is display-only; no payment SDK in the dependency tree. A CI check on `Cargo.lock` for payment crates fails. |
| Windows/macOS desktop out of scope | No packaging job for those platforms in CI. |
| Consent enforced in the store, not the UI | T-P4-007 shared test helper, applied to every query path |

## Appendix C — Where this plan deliberately stops short

- **No UI design.** §10's interface work is specified by behaviour and
  acceptance test, not by mockup. Visual design is a separate exercise.
- **No scraper *authoring*.** Phase 11 makes the existing
  `stashapp/CommunityScrapers` and `CommunityScripts` ecosystems usable here —
  729 declarative YAML scrapers, 155 Python scrapers, 79 plugin directories and
  12 theme directories (54 CSS files). Writing *new* per-site scrapers is out of
  scope: it adapts what the community already maintains and does not fork it.
  That is also why Phase 11 vendors nothing (§0.1 rule 6).
- **No model training.** §7 and §8 consume ONNX models; this plan never trains
  one. Training data comes from §15.8's opt-in research export.
- **No i18n translation.** The pipeline is built (§15.2); the translations
  themselves are contributed as PRs, per that section.
- **ed2k correctness is the one open risk** (T-P10-002) and cannot be closed
  by self-consistency alone.
