# Commons — Spec

**Status:** proposed, not adopted. No code, no repo, no schema written. This is
the spec to review before anything is built.
**Owner:** Alvaro
**Date:** 2026-09-26
**Upstream read:** `stashapp/stash` (Go, 13.0k stars, AGPL-3.0) and
`stashapp/stash-box` (TypeScript + Postgres, MIT), both at `develop`, 2026-09-26.
850 open issues total (673 + 177) — every one is mapped in Appendix A.
**Related:** [[2026-09-25T130540+0200-tauri-ebook-library-spec]] — shares the
Tauri-desktop and community-instance vocabulary, not code. Nothing here touches
[[lorehaven]].

---

## 0. TL;DR

One application that does what stash does (organize a local library) and what
stash-box does (hold a crowd-curated metadata index) — because those are two
halves of the same idea, split across two codebases, two languages, two
licenses. Plus the third thing neither does: **curate the long tail**.

The premise. Both upstream projects are downstream-biased. stash is superb at
driving metadata *into* a local library from commercial catalogs, and has no
crowd-curation story at all. stash-box is a crowd-curated index whose entire
corpus is commercial studio output; its own issue tracker carries a request
(#643) to let users hide specific content, which is an admission that the index
has no way to describe its own edges. The result is that a large, real,
globally-distributed corpus of amateur material has no good home: no official
metadata source exists, so both tools go silent, and the person is left
unlinked across their own appearances.

This platform treats that corpus as first-class. When no upstream source
exists, the platform curates it itself from local signals and from the
community, and it never requires the person to be identified as a content
creator in order for their scenes to be linked (§7.1, §8).

Six decisions shape everything else:

1. **One process, three roles.** A single Rust binary is the local library
   engine, the optional public index server, and the federation peer. Not three
   products, and not "local app plus separate website" — the same binary serves
   both, so a self-hoster's index and their private library are the same
   deployment. This is what makes the two upstream projects one project.
2. **Tauri desktop, never Electron.** The local app is a webkit2gtk shell
   around the same SvelteKit UI the hosted site serves. No Chromium is bundled;
   idle RAM is a design constraint with a number attached (§4.3).
3. **Unsupervised identity clustering is the core feature, not a feature.**
   Face embeddings cluster every appearance in a library with no upstream
   source, no name, no studio, and no consent to be a "performer". A cluster is
   an anonymous identity that can be linked to later, split, merged, or
   self-claimed (§7.1). This is the answer to "all scenes with the same person
   should be linked even if people don't manage to identify that person as a
   content creator".
4. **Curation is a voting system with field-level granularity.** Titles,
   descriptions, tags, studio, performer name, everything, is proposed by
   anyone (human or automatic proposer) and settles by weighted vote, with
   reputation-weighted ballots, decay, and a locked-field escape hatch for
   contested values (§8.1–8.5). A machine proposal is just another voter.
5. **The index is federated, and consent is load-bearing.** Anyone can run a
   public index; the protocol is federation-capable from day one, so no single
   operator becomes a chokepoint. Every item carries a consent tier, an
   attestation trail, and a takedown pipeline that propagates across peers
   (§13, §14). Amateur material is not a grey area here; it is the case the
   consent model was designed around.
6. **Local ML by default, remote assist opt-in.** All automatic curation,
   face clustering and tag inference runs on CPU via ONNX by default, uploading
   nothing. A user-configurable remote endpoint is available for hard cases and
   is opt-in per item, never silent (§6.5).

**Deliberately not this:** a chat app, a social feed, a downloader, a tag
manager with a database attached. Content acquisition is out of scope (§0.7).

The one boundary this list draws and then redraws: stash #2792 asked to serve
a library publicly, and I originally wrote that as a non-goal. It is not. A
self-hosted index with an anonymous read-only tier, expiring share links and
moderated destructive actions is in scope (§12.1.1), and it is the feature
that made the maintainers close #2792 as "not what this project is for" —
which is the correct reason to build it somewhere else.

---

## 1. Why these two projects should be one

The split is historical, not conceptual, and reading both trackers makes the
seam visible. Three concrete proofs from the 850 open issues:

- **Both have a performer-identity problem and neither solves it.** stash
  #1220 (PHASH-based similar lookup), stash #3617 (manually link performers),
  #7132 (performer evolution), stash-box #850 (vectors for identifying
  performers), #299 (mark performers as ambiguous), #846 (streamlining support
  for nameless performers). Both sides are reaching for unsupervised
  identity and neither has it.
- **Both have the same "where does the metadata come from" hole.** stash #5625
  (centralized automated update checker), #2914 (per-scraper rate limiting),
  stash-box #95 (query URL/request scrape), #1165 (filter the stash-box query
  based on existing metadata), #870 (findUpdatedScenes). When a file is from no
  known studio, both tools stall.
- **The two trackers are complements, not duplicates.** stash's largest cluster
  is images/galleries (150 issues); stash-box's largest is performer identity
  and voting. Merging them gives each what it lacks: stash gains a crowd and a
  curation engine, stash-box gains a filesystem, a player, a scanner, and a
  desktop app.

Concretely, the merge is a capability inventory, and Appendix A maps all 850
open issues onto 91 designed capabilities. Twenty-three of those capabilities
have **no** upstream issue behind them at all (C12, C13, C14, C21, C24, C31,
C35, C51, C53, C55, C56, C58, C59, C65, C66, C68, C71, C72, C80, C84, C88,
C89, C91) — they are the answer to the parts of the request that upstream never
addressed.

## 1.1 The amateur-content gap, stated precisely

stash-box is described in its own README as "an OpenSource video indexing and
Perceptual Hashing MetaData API server for porn", populated in the
MusicBrainz-crowd-sourcing mould. Its corpus is studio output, because that is
what upstream scrapers can find. The consequence, visible in the tracker:

- No representation for material that has no studio, no title, no release date.
  stash-box #550 (allow scene without date) and #549 (allow scene without
  studio) are the minimal asks; neither addresses a whole class of content.
- No representation for the person who is not a performer. #846 asks for
  "nameless performers"; the platform needs the stronger primitive: a
  **cluster** (§7.1) that is not yet a person, a studio, or a credit.
- No representation for a creator. Both projects assume the producer of an
  item is a studio. For amateur material the producer is a person, sometimes
  several, sometimes the same person as a participant.
- Issue #643 asks users to *hide* categories of content, and #637 asks to
  *adapt for hentai*, #733 asks to *block content by tags*. Those are the
  symptoms: the index can describe its interior but not its boundaries or its
  non-studio provenance.

So the amateur gap is not a missing feature list. It is a missing data model,
and §5, §7 and §8 are written around it rather than bolted onto it.

---

## 2. Non-goals

Named explicitly, because a spec this size accumulates unstated scope.

- **Not a downloader.** The platform organizes and curates content the user
  already has, plus link objects for content hosted elsewhere (§5.7). It never
  fetches item bodies. It *does* serve its own library to viewers over the web
  (§12.1.1, adopting stash #2792) — that is streaming what you hold, not
  acquiring what you don't. It also stores P2P locators and hands them to a
  download client the user already runs (§5.18) — that is naming a file the
  user may already have, not acquiring one they don't. There is no code path
  in which the platform itself fetches a file body over BitTorrent, ed2k, or
  any other network. Concretely: the locator capability is not in core at all
  but in a one-click plugin (§5.18.1) whose sandbox makes the property
  structural rather than aspirational.
- **Not a chat or social product.** Comments on entities are
  discussion-about-metadata only (answers a voting question, disputes a
  merge), scoped and rate-limited — not a messaging surface, no DMs, no
  notifications about other users' activity. Answers stash-box #574 in part.
- **Not a performer-discovery site.** No ranking of people by popularity as a
  browsable surface, no "top performers" front page. Popularity appears only
  as a tie-breaker in search ordering and as a leaderboard of *contributors*
  (§8.4). Handles are pseudonymous by default and self-claim is opt-in (§7.5).
- **Not a paywall or tip jar.** Funder links are display-only metadata;
  §8.13 links an existing profile, it does not process money.
- **No Electron, ever.** Not now, not as a fallback, not "just for Windows".
  X02 in Appendix A.
- **No required cloud service.** Every feature works with no remote dependency.
  Remote inference is opt-in and per-item (§6.5).
- **Not a clone with a skin.** Where a decision here differs from upstream
  behaviour it is stated in the relevant section with the reason, not hidden
  behind "compatible".
- **Windows and macOS desktop are out of scope** (X01). The server runs
  anywhere; the desktop app targets Linux. Mobile web is supported as a
  responsive layout, not a native app.

## 2.1 Name and identity

"Caelus" was considered and is too cute. The working name is **Commons**,
meaning the shared, community-curated layer — which is precisely the half that
stash lacks and the half that carries the amateur corpus. Neutral, descriptive,
no claim on anyone's brand. Rename later is a one-line change (§15.1).

---

## 3. Architecture

```
                    ┌──────────────────────────────────────────┐
   browsers ───────▶│  reverse proxy (nginx/Caddy)            │
   Tauri shell ────▶│    TLS, static assets, websocket tuning │
                    └──────────────────┬───────────────────────┘
                                       │
                    ┌──────────────────▼───────────────────────┐
                    │  commons-server (single Rust binary)     │
                    │  ├─ HTTP + GraphQL/REST + websocket       │
                    │  ├─ index mode   (public, multi-tenant)   │
                    │  ├─ library mode (local filesystem)       │
                    │  └─ peer mode    (federation client)       │
                    ├──────────────────────────────────────────┤
                    │  job engine  scan · generate · match ·   │
                    │              cluster · transcode · vote   │
                    ├──────────────────────────────────────────┤
                    │  ML runtime (ONNX, CPU default, CUDA opt)│
                    │  face embed · tag infer · phash · ASR    │
                    ├──────────────────────────────────────────┤
                    │  federation  signed claims · merge ·     │
                    │               tombstone propagation       │
                    └──────────────────┬───────────────────────┘
                                       │
              ┌────────────────────────┴────────────────────┐
              │  Postgres (index)      │  embedded store (library)  │
              │  pgvector + pg_trgm +  │  SQLite or DuckDB — same   │
              │  pgcrypto              │  logical schema, no server │
              └───────────────────────────────────────────┘
```

### 3.1 One binary, three modes

The single most consequential structural decision. Modes are runtime
configuration of the same artifact, not separate builds:

| Mode | Postgres | Filesystem | Purpose |
|---|---|---|---|
| `library` | optional, embedded store instead | full read/write | The desktop app's backend. Private by default. |
| `index` | required | metadata + generated only, optional media | A public index server: accounts, voting, browsing, submissions. |
| `peer` | as `index` | same | Federation: exchanges signed claims with other indices. |

A self-hoster runs `--mode index`; the same deployment also holds their private
library if they want. The desktop app speaks the same protocol as the hosted
site, so "local" and "hosted" are the same software at different scales — the
reason the website and the local app cannot drift apart.

### 3.2 Why Rust, and why not Go

stash is Go. The choice here is not "better language", it is the license and
the ecosystem:

- **License.** stash is AGPL-3.0. Reusing stash's code obliges any networked
  deployment to publish modifications. stash-box is MIT (permissive) but is a
  thin API over a Postgres schema. A from-scratch implementation against a
  documented behaviour set keeps the platform permissively licensed. The
  upstream projects stay usable and untouched (§16).
- **Ecosystem.** The ML runtime (ONNX Runtime, ort crate), the ANN index
  (usearch), the media pipeline (ffmpeg sidecar, not a binding) and the Tauri
  shell are all better-served in one language than across a Go backend plus a
  TS frontend plus a second Go/TS service.
- **Practicality.** One static-ish binary with no runtime, no interpreter, no
  npm tree at deploy time.

What is deliberately *not* reimplemented: ffmpeg is used as an external
process, not embedded (§11.5), and the Postgres schema is not copied —
§15 defines it fresh.

### 3.3 Frontend

SvelteKit 2 + a Rust WASM-free split: the UI is a normal Svelte app, with the
desktop shell (Tauri) providing native file dialogs, tray, and single-instance
semantics. The UI talks to one GraphQL API plus a websocket event stream; there
is no second private API for the desktop app, which is what keeps the two
deployment modes from diverging.

SvelteKit is chosen over upstream's React for one reason that matters here: the
desktop app ships the same bundle, so bundle size is a RAM and startup cost.
The UI never holds the library in memory — it queries, and the server streams
paginated windows of virtualized rows (§10.4).

### 3.4 Tauri

Chosen over the bare-binary-plus-PWA option. The trade is deliberate and worth
stating: Tauri costs a webkit2gtk build dependency and per-platform packaging
signing, and buys true native dialogs, tray, single-instance lock, and a real
window with no browser chrome. Both are fine; Tauri was chosen because the
"native dialogs for a filesystem-first app" case is real and PWA file pickers
remain weaker on some engines, and because the user asked for a real local
app. The bare binary remains the server and is always runnable without the
shell — the shell is a client, not a dependency.

Consequences to design for: Tauri means webview2/webkit2gtk, so the *server* and
the *shell* ship separately; the shell must handle being pointed at a remote
hosted instance, not only `localhost`; and the shell's own process count is
two (shell + server), not the five to ten an Electron app costs.

## 3.5 Storage: two engines, one logical schema

The schema in §15 is defined once, in terms of entity types, fields and
relations, and instantiated twice:

- **Index mode** — Postgres with `pgvector` (face/body embeddings, tag
  embeddings), `pg_trgm` (fuzzy name and title search), `pgcrypto` (hashed
  credentials). This is what a hosted index runs on.
- **Library mode** — an embedded store (SQLite by default; DuckDB where the
  workload is analytics-shaped) with the same logical tables, the same
  relations, and vectors in a sidecar ANN file. A private library must not
  require a database server, and a desktop app that needs "start postgres
  first" is a desktop app nobody runs.

The consequence to accept: two SQL dialects, and the portable-subset discipline
of §15.2. One logical schema, two physical ones, and a migration suite that
proves they agree (Phase 0, §17).

## 3.6 Data flow: a file arrives

1. **Scan** — the watcher enqueues the path; the scanner stats, hashes
   (xxh128 + BLAKE3), and identifies type by content, not by extension.
2. **Extract** — ffprobe yields container, streams, duration, chapters, and
   embedded metadata. Chapter titles become marker seeds.
3. **Artifacts** — sprite, poster, transcript. All keyed on
   `(file_id, mtime, size, generator_version)` so a replaced file invalidates
   only its own artifacts.
4. **Match** — phash lookup against the local store and, if the user allows it,
   against known index peers. A match is a *proposal*, not an assignment
   (§8.2).
5. **Propose** — ML proposes title, description, tags, performers; vote
   weighting decides the settled value (§8.1).
6. **Cluster** — face embeddings assign this item's people to existing
   identity clusters, or create new anonymous ones (§7.1).
7. **Settle** — the item is now described, tagged, and linked, with or without
   any upstream source ever existing.

Step 7 is the point of the whole project.

## 3.7 Process and service supervision

One systemd unit for the server (`commons-server.service`), one for the
desktop shell (user unit, autostart-optional). Docker and compose recipes ship
in-repo (C75) with a non-root user by default — answers stash #684. Upgrade
path: a versioned binary plus forward-only migrations gated on a verified
backup (§14.2). The in-app updater (C76) checks a signed release manifest and
refuses to self-update across a major version without consent.

---

## 4. Technology decisions, with reasons

### 4.1 Data: Postgres in index mode, embedded in library mode

Index mode needs `pgvector` and `pg_trgm` for identity and fuzzy search; those
are the two features that justify Postgres for a hosted service. Library mode
needs zero administration. See §3.5 for the shared logical schema and §15 for
the entities.

### 4.2 Frontend: SvelteKit 2, virtualized everything

Virtualized grid and list (§10.4) are non-negotiable for 100k+ item libraries.
The UI holds no more than a window of rows; the server does sort, filter and
paginate. A consequence: every view state is a URL (§15.10), because a
virtualized view cannot hold state in memory reliably.

### 4.3 Memory budget: a number, not a hope

The user's requirement — local Linux app without Electron's RAM cost — becomes
a budget with a test:

| Process | Idle RSS | Peak RSS |
|---|---|---|
| `commons-server` (library mode, 5k items) | ≤ 90 MB | ≤ 700 MB during a scan |
| Tauri shell (webkit2gtk) | ≤ 120 MB | ≤ 250 MB |
| **Total idle, app open** | **≤ 210 MB** | — |

The budget is enforced by (a) not loading the library into memory — SQLite
reads pages, Postgres pages, artifacts stream from disk; (b) a configurable
memory ceiling on the ML workers (§6.4, C18); (c) no Chromium. A regression
test in CI measures idle RSS of a booted server and fails the build above
budget (§17, Phase 0). This is the requirement, expressed so it can fail.

## 4.4 Everything else, briefly

| Concern | Decision | Reason |
|---|---|---|
| Media processing | ffmpeg/ffprobe as external processes | Never embed; the codec surface is maintained upstream |
| ML runtime | ONNX Runtime (Rust `ort`) | CPU-first, CUDA optional, one artifact format |
| Vector search | `usearch` or hnswlib binding | One ANN index serves faces, bodies, tags, text |
| GraphQL | `async-graphql` | Matches upstream's API familiarity (C73) |
| Websocket | `tokio-tungstenite` | Live job progress + live vote updates |
| Auth | argon2id + passkeys + optional TOTP | Stash #7135 (max password length) is a spec bug, fixed by having no max length beyond a sane cap |
| Scraper sandbox | WASM (wasmtime) for scrapers, no ambient authority | Scraper runs untrusted remote HTML/JS; the plugin API is the attack surface (C67) |
| i18n | Fluent (`ftl`) with compile-time extraction | Answers stash #5514 multi-language and #7253 Kiswahili |
| Frontend build | pnpm, hoisted linker | Local convention |
| Migrations | `sqlx` offline-checked, forward-only | Fails the build on a bad query at compile time |

---

## 5. Content model

Five object types, not one. Upstream has three (scene, image, gallery) and the
tracker shows what that costs: #1258 audio, #1659 manga, #1006 PDF, #1259 text,
#6088 O-Counter feature requests, #1028 "built for a particular type of
conventional studio-produced porn". Each new type in upstream is a bolt-on;
here each is a first-class citizen of one polymorphic model.

### 5.1 The object model

```
Object ──┬── File (0..n)          one or more source files
         ├── PersonCluster (0..n)  via Appearance (§7)
         ├── Tag (0..n)
         ├── Studio | Creator (0..1)   producer, commercial or person
         ├── Group (0..n)          release / series / compilation
         ├── Marker (0..n)         timeline points, §5.11
         ├── ObjectRelation (0..n)  extras, compilations, parts, §5.9
         ├── FieldProposal (0..n)  one per (object, field), §8.1
         ├── Rating (0..n)
         └── Consent (1)           §14.1, mandatory
```

`Object` is polymorphic on `kind`: `scene`, `image`, `gallery`, `audio`,
`comic`, `text`, `interview`. Gallery is a *containment* relation over images
(§5.3), not a separate type with a copy of every image field.

`FieldProposal` is the key structural choice. Metadata is not a column per
field; it is a set of proposals per (object, field) that vote to a settled
value (§8.2). A plain `title` column would be the wrong shape for a
crowd-curated index, and getting this wrong early is the single most expensive
mistake available in this project.

### 5.2 Video (C01, C02)

Containers: mp4, mkv, webm, avi, mov, wmv, m4v, ts. Type by content sniff
(ffprobe), not extension — answers #6577 (.webm classified as video when it is
an image clip) and #5111 (GIF in zip is image vs video) and #5185
(`.forcegallery`).

Multi-scene files (C02) get a first-class answer, which is the issue with 38
comments upstream (#3530): a file may hold N scenes, so `File` and `Object` are
already 1:n and a **Segment** row describes `(file, start, end)`. Multi-part
scenes (#2276), compilations (#2511, #2085), and virtual compilations all fall
out of the same primitive. Markers copy to segments on split (#5089).

### 5.3 Images and galleries (C03)

The single largest issue cluster in the whole corpus: 174 issues (150 stash +
24 stash-box). What they collectively ask for is "images are first-class":

- Per-image metadata: tags, ratings, O-counter-equivalent, ordering, custom
  numbering (#4950), per-image markers. `Image` is an `Object`, so it votes like
  one.
- Galleries: zip, rar, 7z, gz, tar.gz (#233), sub-galleries (#1127), page
  ordering, custom covers from any image (#6045), metadata inheritance
  gallery→image (#2902) and scene↔gallery linkage by folder (#1161, #5468).
- Formats: HEIC/HEIF with live-photo pairing (#6732), WebP, AVIF, animated.
- O-counter on galleries with sums (#5364) and rating on galleries (#6576).
- Zip: compression-method display, safe extraction (answers #7240 Zip-Slip),
  per-image delete inside archive (#7106), `.nogallery` handling (#7179).

Archive handling gets a hard rule: extract to a temp dir, validate every
member path against the destination root, never write outside it.

### 5.4 Audio (C04)

Answers #1258 (45 comments, the most-discussed single issue in the corpus).
Audio is an `Object` of kind `audio` with waveform, replay-gain metadata, and
audio scrapers. Audio-track selection is a separate concern from the audio
object and lives in the player (#1058). Waveforms render beside the player
(#5144) and are generated as artifacts. Fades and crossfades for compilation
playlists.

### 5.5 Comics, manga, doujin (C05)

Answers #1659 (15 comments). CBZ, CBR, CB7, PDF (also #1006), and image
sequences. Needs: page ordering that respects filename numbering, right-to-left
reading, spreads, per-page metadata, cover extraction, and a page-sprite
artifact. Doujin is not a special case of comic; it is a comic with a
`circle` producer, which the `Creator` type models directly (§5.12).

### 5.6 Funscript and interactive (C06)

Answers #3031 and the family: sidecar discovery, in-browser playback with
timing sync, token/drm note (#5650), AutoBlow toy support (#6579), the full
"missing funscript" list in #3031, and manual pause (#2762). Funscript
metadata (stash-box #851) is a typed object: axes, script metadata, and
provenance.

Interactive-axes labelling (multi-axis interactive scenes, #6339) is part of
this section: the platform models an item as having N named action axes, and
the UI can render a 2-axis overlay or an N-axis controller.

### 5.7 Text, story posts, links (C07)

Answers #1259. Text objects (captions, stories, posts) and link objects for
content hosted elsewhere, with per-site metadata and a stable local identity
that survives the remote item moving. Link objects never fetch the body;
screenshot/preview is a user-supplied or consented artifact.

### 5.8 Performer interviews (C08, owner-added)

Not in upstream's vocabulary; added because the user asked for it. An
`interview` is an `Object` with a transcript, chapters, quotes, and topic
links, and it is a *first-class appearance venue*: a person in an interview is
an appearance (§7) exactly as in a scene, so interview appearances contribute
to identity clustering and to a person's filmography. This is the one content
type that mostly does not exist in either upstream project, and it is the
clearest example of a capability with no upstream issue behind it.

- ASR transcription locally (whisper.cpp or a Parakeet ONNX model) with
  word-level timestamps, plus human correction feeding back into the
  transcript field as a proposal (§8.1).
- Chapters derived from the transcript, as `Marker`s.
- Quotes and named topics as `Tag`s with weights, voted like any other tag.
- Q&A indexing: search finds a moment in a conversation, not just a title.
- Interview-specific: speaker attribution, so a person's spoken words are
  attributed to the same cluster as their other appearances.

### 5.9 Extras, related items, and relations (C09)

Answers #2296 (Plex-style extras) and the rest. `ObjectRelation` is typed and
directed: `extra_of`, `part_of`, `compilation_of`, `same_scene_as`,
`re_encode_of`, `unrelated_to`. The duplicate checker and the "related items"
surface are both views over this table, which is why #39 (similar/related tab)
and the duplicate-checker improvements (#5786, #5823, #1220) stop being
separate features.

### 5.10 Subtitles and captions (C10)

Sidecar and embedded subtitles, ASS/SSA (#3077), SRT, VTT. Cues survive
transcode (re-muxed or re-derived from transcript). Subtitle *and* caption
search (#4985) and multi-language entries (#5514). Exposed over DLNA
(#5420) and to external players via injected metadata (#2770).

### 5.11 Markers and chapters (C11)

41 issues on stash alone — the third-largest cluster. Markers: named, tagged
(#765 performer attachment, #2121, #5750), rated (#3197), with O-counter
(#6057), with thumbnails generated on creation (#2954, #5783), with live
preview in the player (#5448), AB-loop with points on the scrubber (#6509,
#5178), and marker→playlist creation (#2647). Marker ranges render in
thumbnails for scenes on mobile (#6811) and in the duplicate checker.

### 5.12 Producers: studio, circle, creator (C41, C42)

One `Producer` type with a `kind` discriminator (`studio`, `circle`,
`individual`, `collective`, `unknown`) instead of separate `Studio` and
"imagine a group" tables. This is what lets doujin circles, amateur uploaders,
and studios share one metadata shape and one voting system.

Studio-specific work, all of it: founding/closure dates and defunct status
(stash-box #1279), aliases with URL-scoped ownership history and studio codes
(#820), non-unique names (#5210), more info fields (#676), studio-level tags
auto-applied to items (#641), logo images with transparency preserved (#605,
#279), favicons, child-studio inheritance of posters (#1863), and duplicate
merging (§C83, #6360).

Groups (#663 release/scene groups, #832, #1624, #398) are collections of
objects with a group cover, a description, a code, and optional subgroups
(#6897, #6076, #6944, #5394). Full-length/feature marker (#779) and
compilations (#585, #2511, #2085) live here.

### 5.13 Favourites, organized, and bulk state (C13, owner-added)

The `organized` flag exists upstream (#4970, #497, #1170, #1624) but only as a
per-object boolean with no bulk story and no history. Here it is a tri-state
(`unreviewed`, `organized`, `favourite`) on every object, with bulk operate
(§15.6), a safeguard flag against deleting favourites (#2808), "play next after
delete" (#7157), and the organized-icon legibility fix (#7160). Watch Later
(#7134) and Watchlist queue (#5450) are `List` objects, not flags.

### 5.14 Lists, playlists, and smart collections (C12, C14, owner-added)

Three distinct objects that upstream conflates:

- **List** — an ordered, explicit set of objects. Private or shared. Templates
  (stash-box #834, #226), Wantlist/collection (stash-box #458), playlists with drag
  reorder and shuffle, m3u export (#1939), offline download (#3073).
- **Playlist** — a list with a playback order, a next-track rule, and player
  integration; m3u round-trip.
- **Smart collection** — a saved query, re-evaluated on every open. Stash has
  collections and saved filters; a smart collection is the saved filter
  promoted to a first-class object with a cover, a description, and a
  subscription (notify when new items match). This is the answer to #1029
  (folder-like structure) without pretending folders are the right model.

## 5.15 Tag system (C49)

Upstream's tag rewrite (#3400) asks for tag attributes; #3469 for tag groups;
#1723, #1732, #5992 for the tree; #3200 for colors; #2736 for create-anywhere;
#3221 and #2494 for undo. All of it: tags have a parent (tree), a namespace
(source: `canonical`, `site:<host>`, `ml:<model>`, `user:<id>`), a color, an
optional typed attribute set, and an importance weight (#2973). Tag
namespaces are also the honesty mechanism for ML tagging — an ML tag is always
labelled as such (#560, #848, #722 auto image tagging).

## 5.16 Filters, saved views, query language (C46, C90)

One filter model, not six. The upstream filter refactor discussion (#2122,
28 issues touch filters) is answered by a single typed filter AST:

- Facets per object kind, with typed operators (`in`, `contains`, `is null`,
  `between`, `matches`) and multi-select of the same type (#5159).
- The whole filter serializes into the URL (§15.10) — shareable, bookmarkable,
  restorable on back-navigation (#7142, #1896, #1335).
- Saved filters, with a dedicated list view for the tagger (#2305), and
  "save filter as tag" (#2182).
- The stash query language (#185, #2816 StashID query) is accepted verbatim as
  an input mode, parsed to the same AST.
- Text: a real query surface over the search index with numeric indices,
  NULL semantics (#6970, #3159), and regex with a case-insensitive toggle
  (#1111).

## 5.17 Ratings and organization views (C40, C57)

Ratings: 1–5 per user on any object, star and numeric (#3250 in the player,
#5616 keyboard shortcut in the lightbox, #1400 ratings for tags, #3197 markers,
#6576 gallery rating, #3197). Average performer rating derived from rated items
(#552) and automatic ratings for entities from related content (#1680) are
*derived* values, not stored writes, so they cannot drift.

Organization views: wall (#6544 group-by with collapsible sections, #6955 auto
scroll), list (#517 rich tables for bulk edit, #899 custom headings), folder
view (#1586), media view unifying scenes+images (#1030), tag view (#773),
all virtualized with per-user density (C57).

## 5.18 Locators and peer exchange — the P2P idea (C91, owner-added)

From `YurikaL` in stash #2792: *"it would be nice if Stash nodes could connect
to each other via some kind of P2P, and share content metadata along with its
hash ID, such as magnet links or ed2k links. And then Stash should forward
these links to external download clients."* The 850-issue corpus contains
almost nothing here — one issue, stash-box #440 ("share database through
torrent") — so this is a gap, not a re-implementation.

The idea has two halves that are usually confused, and separating them is the
whole design:

**Half one — locator metadata.** An object may carry *locators*: BitTorrent
infohashes and magnet URIs, eDonkey2000/ed2k hashes and names, and (as plain
text, not a P2P protocol) HTTP(S) source URLs. This is metadata, in the same
column-family as a studio URL or a funder link, and it does three jobs:

1. **Content-identity dedup.** Two libraries holding the same file identify it
   by content hash, not filename. The locator set is the strongest available
   cross-instance identity signal, which is what makes §13's federation
   meaningful for large media.
2. **Hand-off to a client you already run.** One click pushes a magnet or ed2k
   URI to a configured local client (qBittorrent, Transmission, Deluge, aria2)
   over its API. This is a *user action against an external program*, and it
   is the only acquisition-adjacent behaviour in the platform.
3. **Nothing else.** The platform never speaks BitTorrent or ed2k itself. It
   is not a downloader, has no swarms, no peer list, no DHT, no incoming
   connections. There is no code path in which the platform fetches a file
   body over a P2P network.

**Why the distinction matters for consent.** A magnet link is a redistribution
channel. Storing one is a distribution decision, not a metadata decision, so
locators are gated by the §14.1 consent model and cannot be attached to a
tier that does not permit redistribution:

| Consent tier | Locator allowed |
|---|---|
| `unverified` | no — rejected |
| `self_published` | no by default; the creator must attach one explicitly |
| `performer_claimed` | no |
| `third_party_permitted` | yes, if the stated basis includes redistribution |
| `quarantined` / `denied` | no, and any existing locator is destroyed with the tombstone |

This is the reason the feature is safe to have at all: an unverified amateur
item can never become a redistribution pointer, no matter who proposes it. The
gate is enforced in the data layer, like every other consent rule (§14.1), and
a hand-off to an external client additionally re-checks the tier at the moment
of the action, because consent can be revoked between storage and use.

**What is exchanged between peers (§13).** Locators federate as ordinary
signed claims (§13.1) and therefore inherit consent gating: a peer accepts a
locator claim only if its own local tier for that object permits redistribution.
A peer that receives a locator for a `denied` object stores a tombstone, not
the locator. Nothing about a magnet reveals its content, but a magnet plus a
size plus a name is a strong join key, so locators are excluded from
fingerprint publication (§15.7) and from research exports (§15.8) by default.

**Wire format.** A locator is `{scheme, uri, infohash?, size?, name?, added,
source}` with `scheme ∈ {magnet, ed2k, infohash, http}`. The API exposes
listing and adding; there is no "resolve" endpoint, because resolving means
fetching. Duplicate detection uses content hash first, infohash second, and
filename+size last, with the match shown to the user before any merge (§9.7).

**ed2k specifics.** ed2k hashes are 128-bit MD4 over a specific chunking
scheme, so they are computed from a file in eDonkey2000's own segmentation and
stored as `ed2k_hash + ed2k_size`. Segmented multi-file torrents and ed2k
"part" collections map to §5.2's `Segment` model, so a collection is one object
with N file locators rather than N objects with a parent hack. Where a file's
ed2k hash and BitTorrent infohash both exist, both are stored; they are
different protocols' names for the same bytes and neither is authoritative.

### 5.18.1 C91 ships as a one-click plugin, not as core

The whole of §5.18 is **one installable plugin**, offered in the plugin gallery
and installed with a single click, and it is not built into the server. The core
never learns what a magnet is. Three reasons, in order of weight:

1. **It keeps the no-downloader property true by construction.** The plugin
   runs in the WASM sandbox of §11.4 with a declared capability set, and its
   network capability is **loopback only**. It can reach a qBittorrent or aria2
   API on `127.0.0.1` and it cannot resolve a public hostname, open an outbound
   socket to a peer, speak the BitTorrent or ed2k wire protocols, join a swarm,
   run a DHT, or accept an inbound connection. "No swarms, no peer list, no DHT,
   no incoming connections" stops being a commitment in a design document and
   becomes a property the host can prove, because the capability is absent
   rather than unused.
2. **The consent gate must not live in the plugin.** A tier check inside a
   third-party component is a tier check that can be buggy, disabled, or
   hostile. So the split is: **the host owns the gate, the plugin only
   requests.** The plugin calls `locator.propose(object, locator)`; core
   evaluates §14.1's tier table and either persists it or refuses. The plugin
   has no write path to the consent tier, the object, or the database. A
   malicious plugin can ask for a locator on a `denied` object and get a
   refusal, which is the same answer a well-behaved one gets.
3. **It exercises the extension SDK against a real consumer.** §11.4's plugin
   API is a stability promise that upstream has asked for 34 times. Shipping
   the platform's own advertised capability *through* that API is the only way
   to find out whether the API is actually adequate, before a third party
   depends on it.

**What is core regardless.** Content hashing (xxh128 + BLAKE3) stays in core,
because §6.2's incremental-correctness rule depends on it and so does
duplicate detection (§9.7). Only the *protocol-specific* hashes — ed2k's MD4
segmentation, BitTorrent infohash for multi-file torrents — are the plugin's
job, added to a file's locator set when the plugin is installed and absent
entirely when it is not.

**Install experience.** One click from the plugin gallery, with a capability
disclosure shown *before* the install that names exactly what the plugin will
do: read file headers on your library, store locators, make loopback HTTP
calls to a download client you configure. No other network access. That
disclosure is itself a consent moment (§14.1's spirit applied to code), and it
is signed-manifest verified, versioned, updatable, and fully removable — with
removal taking its locators with it unless the operator chooses to keep them as
plain metadata. The pre-install backup that #6185 asks for is required here
rather than optional.

**Consequence for the roadmap.** Because a headline capability is delivered as
a plugin, the extension SDK stops being a Phase 6 polish item and becomes
load-bearing infrastructure: the sandbox and capability-declaration properties
must exist early, and the full SDK must be complete before Phase 10. This is
recorded as a design consequence, not a preference — a platform that advertises
a feature it can only deliver in-core has mis-stated its own architecture.
---

## 6. Library, scanning, and the job engine

### 6.1 Scanner throughput (C15)

The most-repeated complaint class in the corpus. 15 issues on throughput
alone, plus 6 on SQL/query performance.

| Problem | Upstream symptom | Design |
|---|---|---|
| Large libraries | #2824 slow scanning, #6455 SQL for larger DB, #6390 pagination caching | Watcher-driven incremental scan; scan checkpoints per library; every list query keyset-paginated, never `OFFSET`; composite indexes chosen from real query plans in Phase 0 |
| Removed drives | #5683 looping read access when a scene's drive is gone, #5036 rescanning `*nix` paths breaks galleries | The file record is stateful: `present | missing | unreadable | remote`. A missing volume never triggers a rescan storm and never logs per read; the path is greyed once, and the item is skipped by bulk operations with a visible reason |
| Slow metadata lookups | #6452 tagger jumps position, #7142 scroll lost, #7222 studios page slow on large image libraries | Stash-box as a *query cache*, not a per-lookup HTTP call. Batch lookups by fingerprint set; a 60 s TTL for studio pages |
| Rclone/NFS/SMB | #7130 "Loading forever" | Remote mounts get a polled scan with an explicit interval and a progress bar. A watcher is never trusted across a network boundary |
| Custom fields | #6795, stash-box #823 | Typed custom fields, first-class from day one, filterable and votable |

Scan is idempotent and resumable. A scan interrupted by a crash resumes from
its checkpoint; it never re-hashes an unchanged file (§6.2).

### 6.2 Incremental correctness (C16)

- `(mtime, size)` is a *hint*; content hash is truth, but only recomputed when
  the hint changes. This is the fix for #7155 (stale artifacts after a
  same-path content change) and #2773 (image replaced under the same name).
- Moves and renames are detected by content hash, then the path is rewritten
  without re-extracting metadata.
- `.forcegallery` and `.nogallery` marker files are honoured, including fixing
  `.nogallery` not removing an existing folder gallery (#7179).
- Selective scan can pick a studio (#2679) and accept zip files (#2548).
- Per-volume opt-out of cleanup (#314) for hot-swap users, plus XDG paths
  (#2814).
- Deleting a file while generating for it must not lock the scanner (#5953):
  generation holds a file lease; a lost lease cancels the job cleanly.
- A fileless object is a legal state (#3220) and the queue must skip, not
  stall, such items (#6814).
- Fileless and remote-path libraries are first-class (#3171, #2548).

### 6.3 The job engine (C17)

Every slow thing is a job: scan, generate (thumbnails, sprites, previews,
covers, phash, transcript), identify/match, cluster, transcode, autotag,
cluster-refresh, backup, index-sync.

One durable queue, not one per subsystem. Required properties, each traceable
to an issue:

| Property | Issue |
|---|---|
| Bounded worker pool, one job type per file | #2824, #5709 (zombie Python), #6814 |
| Per-item skip list — a poisoned file cannot loop the queue | #2913 (queue looping), #6837 (skip failed generation) |
| Resume after crash | #1445 |
| Progress, cancel, and retry with backoff | #3237, #1445 |
| Suppress sleep during tasks | #5517 |
| Queue UI as a nav popover with live progress | #2871, #3220 |
| Task-level timeout and cancellation | #3237, #6837 |
| Plugin tasks share the same queue | #5944 |

Generation task knobs: ffmpeg thread count (#819), encoder choice (#894),
max memory (#5762), preview skip threshold (#6193), preview skip on known
failure (#6837), marker preview options (#5783), generate-group cover from
children (#6657), and failed-task info surfaced with the action needed
(#6446).

### 6.4 Hardware acceleration (C18)

Answers #5731 (10 comments), #5769, #5681, #7007, #7239. The generation path
detects VA-API / NVENC / QSV / VideoToolbox, reports what it found and what
it could not use *with a reason* (#7239 asks for exactly that), and uses it for
sprite, preview and phash generation where ffmpeg supports it. A documented
CPU fallback always exists, and the Docker image ships with the acceleration
hooks wired but not privileged (#7007). Configuration is respected, not
guessed.

### 6.5 Remote inference, and the line it may not cross (C05 answer)

All ML is local by default: ONNX Runtime, CPU, nothing uploaded. A remote
endpoint is configurable and **opt-in per item**, with three states — `never`,
`ask`, `always` — and the per-item choice is recorded on the item's consent
record (§14.1) so it is auditable later.

The line: even in `always` mode, what may be sent is a *fingerprint* (phash,
face embedding, coarse tag vector, title) and never the file bytes, never
frames, never a transcript, unless the user separately enables transcript
assist. This is the anonymized-fingerprint concern from stash-box #633, taken
as a design constraint rather than a feature request. Fingerprints themselves
are salted per-peer before publication (§15.7), so a published fingerprint
cannot be replayed against a local file to confirm its contents.

### 6.6 Storage accounting (C20)

**"How big is this library" has three answers, and answering with the wrong one
is worse than not answering.**

The obvious implementation — sum `metadata.len()` — is wrong in three separate
ways, and each has produced a bug report upstream:

| quantity | definition | what it is not |
|---|---|---|
| **apparent** | `metadata.len()` | not what the file *costs*: a 1-byte file occupies a 4 KiB block |
| **allocated** | `blocks * 512` | not unique: two hardlinks to one file cost one file's worth of disk, not two |
| **physical** | allocated, each inode counted once | not per-path, so it under-reports what a naive `du` shows |

A library of 4 KiB JPEGs is nearly all block overhead; the apparent total can
understate the real one by 50 % or more. A library of hardlinked site rips
(stash#4409) overstates it by the number of links. §5.2's phrase "real on-disk
size" is the **physical** basis.

**The requirement.** Every size the system reports or persists names the basis
it is in. A rollup carries `total` *and* `basis`, and a per-file `FileSize`
carries all three quantities, so a caller cannot accidentally sum lengths and
call it disk usage. Hardlink identity is `(device, inode)`, not inode alone:
inode numbers are per-device, and two libraries mounted at once would otherwise
silently merge.

**`st_blocks` is in 512-byte units**, whatever the filesystem's block size.
This is POSIX, and `du`, `stat` and `ls` all agree. Multiplying by the
filesystem block size instead is off by 8 on a 4 KiB filesystem and disagrees
with `du` for a reason that looks like a bug in this code.

**Free space is two numbers.** `f_bfree` includes blocks reserved for root;
`f_bavail` is what an unprivileged process can actually write. A statistics
page shows the available figure and the gap, because a user told they have
40 GB free on a filesystem with 5 % reserved has been told something they
cannot use (stash#7194).

**Temporary files do not live under `generated/`.** They have different
lifetimes and different backup policy, and a library served from a read-only
mount — or one where `generated/` is a separate volume — cannot run a transcode
at all if they do (stash#5646). The temp root is configurable and defaults
under the OS temp area. Configuring it *into* `generated/` is refused, because
that reintroduces the bug by configuration.

**Cross-checking.** Each basis is verified against the `du` invocation that
computes that same basis, exactly rather than within a tolerance:

| basis | `du` |
|---|---|
| apparent, per path | `du -s -B1 --apparent-size --count-links` |
| allocated, per path | `du -s -B1 --count-links` |
| physical, per inode | `du -s -B1` |

`du -sb` is **not** a valid cross-check for any of them: it is apparent size
*per inode*, because `du` counts each inode once by default even under
`--apparent-size`. A tolerance would also hide the only class of failure that
matters, which is having summed the wrong field.

## 7. People, identity, and the cluster

This is the section that answers the user's central requirement, and the area
where upstream is furthest from a solution.

### 7.1 Unsupervised identity clustering (C21, owner-added)

**The primitive.** A `PersonCluster` is a set of face embeddings believed to be
one person, with no name required, no studio, and no credit. It has a stable
id, an optional handle, an optional avatar, and a confidence. It is created by
the clustering engine and is meaningless to a user until it has three or more
appearances, at which point the UI offers to name it.

**The pipeline.**

1. **Detect** — a face detector runs over generated keyframes and stills, plus
   any user-supplied headshots. Every face becomes a crop with its provenance
   (file, timestamp).
2. **Embed** — a local ONNX face embedding model produces a vector.
3. **Assign** — nearest-neighbour lookup in the ANN index decides, per face:
   join an existing cluster, create a new one, or attach as *ambiguous*. The
   ambiguous bucket is the important one: stash-box #299 asks for
   "mark performers as ambiguous" and #846 for nameless performers, and both
   are symptoms of needing a state where the system knows it does not know.
4. **Consolidate** — agglomerative passes merge clusters whose members are
   mutually consistent, with a guard against transitive over-merge (see §7.4).
5. **Name** — once a cluster has appearances, the user can attach a handle, an
   existing performer record, or nothing. Anonymous clusters are first-class;
   a cluster with no name is a valid, browsable state, and it is the *default*.

**Why this is the core feature.** Upstream can only link a person when a
scraper found a name. For amateur corpora there is no scraper, so the person
is fragmented across hundreds of items with no thread. The cluster is that
thread. Every appearance of a person, in any content type, including the
interview format (§5.8), lands in the same cluster graph, so a person's
scenes, gallery images and interview appearances are one entity.

**Linking across sources.** Within one library, clustering is local and
immediate. Across sources — a second library, an index peer, a remote
fingerprint match — clusters link by *proposal*, never by silent merge
(§13.2). A cluster merge that crosses a consent boundary is a moderation event,
not a background job.

**Ambiguity is displayed, not hidden.** A cluster with conflicting evidence
(the same embedding matched two named performers) is `ambiguous` in the UI
with both candidates and a "this is two people" action. Silently guessing is
the failure mode that would make people distrust the links.

### 7.2 Merge, split, alias, disambiguate (C22)

- **Merge** two clusters or two performers: reference re-pointing, a merge
  record, and every affected field re-voted (#943 edits lost on merge is fixed
  by re-deriving from the edit set, §8.6).
- **Split** a cluster: the offending face goes to a new cluster, and the
  original's centroid is recomputed so the error does not recur.
- **Alias** with studio/era scoping (#422, stash-box #818), a selectable
  primary name (#610), and validation that a new name is not an existing
  alias (stash-box #726) with a *warning, not a hard error* (#714 asks for the
  warning; the platform allows the collision and flags it, because in an
  amateur corpus a shared name is common and a hard error would block real
  data).
- **Disambiguate** by free-text disambiguation and by a `same_as`/`not_same_as`
  assertion, with automation (#760) as a proposal, never as an action.
- Non-ASCII names never fail auto-tag (#2293) and commas inside an alias never
  split it (#778, #5033) — both are parser bugs upstream, fixed at the parser.

### 7.3 Performer fingerprints (C23)

Beyond phash: a small set of embeddings per performer (face from several
angles, plus optionally a body embedding) used for cross-index identity
matching against stash-box #850's vectors and stash-box #1177's fingerprint
cluster view.
Fingerprints are removable per-item when wrong (stash-box #304), validated
before submission (#2149), anonymized before publication (#633, §15.7), and
generated for images as well as video (#6939, and #1179 gallery fingerprint
search). Additional hash types beyond oshash are supported (stash-box #530) —
xxh128 and BLAKE3, with oshash deprecated (stash-box #1115).

False positives are reported back to the index (stash #5643) and correction
propagates (§13.2).

### 7.4 Body and appearance similarity (C24, owner-added)

Faces fail in the cases that matter for amateur corpora: low resolution,
heavy compression, masks, and — most often — **the same person at different
weights over time**. A face-only system splits one person into three
clusters, which is worse than no clustering because the user then has to merge
manually and distrusts the result.

So identity uses a *composite* score: face embedding + a body/appearance
embedding (silhouette and proportions, which survive weight change) + optional
tattoo/piercing embeddings. The thresholds are per-decision, visible, and
configurable, and the UI shows *why* two items were linked ("same face, 0.87;
body consistent") — because a link the user cannot interrogate is a link they
will not trust. Lookalike and sibling cases get an explicit
`possibly_different` review state rather than a forced merge.

### 7.5 Self-service performer claim (C25, owner-added)

Pseudonymous by default; a person may privately verify a cluster. The flow:
a performer claims a cluster, the claim goes to moderation, and on approval
the performer gets a dashboard showing their appearances, and can correct
metadata on their own record. This is the "performer" concept stash cannot
offer its users at all, and it is the mechanism that makes §14's consent model
practical: the person in the content is the one who can consent to it, and a
verified claim is how a takedown request reaches the right person.

### 7.6 Performer field model (C26)

Everything both trackers ask for, as one typed model: gender (with a default
and dynamic field visibility per gender, #6866, #3751, #7204 sex/genitals
field, #3751), ethnicity, nationality, country (#808 specific UK countries,
#5237 Taiwan naming — a maintained locale table, §15.2), height, weight,
measurements (#1495 split into bust/cup/hip/waist, #422, #422, #422),
birthday and age-vs-birthday sorting (#6925), career span (§7.11), status
(§7.9), penis size / cut-uncut / length for male and trans performers
(stash-box #206, #1141, #784, #206), shoe size (stash-box #205), hair color
(multi-value, stash-box #210), additional male performer metadata
(stash-box #1141, #553), and custom fields (stash-box #823, stash #6795).
Measurements are multi-value over time (§7.7), so "career" is derivable.

### 7.7 Multi-valued attributes (C27)

Answers stash-box #234 (multi-select breast type, ethnicity, nationality) and
#210. Attribute types are declared in schema: `single`, `multi`, `range`,
`ordinal`, `boolean`, `text`, `date`, `measurement`. `measurement` is
date-stamped and multi-valued, which is what makes §7.11's automatic career
span and the age-at-scene calculation (#2956, #5748) possible.

### 7.8 Performer image sets (C28)

#571 (32 comments, "support for multiple performer images"). Every entity —
performer, studio, tag, group — takes N images with a category (headshot,
body, outfit, behind-the-scenes, verified, submitted), a crop, and an ordering
(#161, #237, #203). Images are set from a file, a URL (local image URL works
with auth enabled, #5538), a marker thumbnail, or a linked gallery's image
(#2185, #2032, #2170). Downscale must preserve transparency for studio logos
(#605) and studio logos/favicons are first-class (#92, #2082, #5399, #666).

### 7.9 Status markers (C29)

Deceased, retired, inactive — a badge on the card, a filter, and a
human-readable timeline entry (#5326, #5434, #1141). Stash's request for a
"passed away" icon is here as a first-class status, because a deceased
performer's scenes are a real category users browse and filter.

### 7.10 Stage names and aliases (C30)

§7.2. Aliases scoped to studio or era (#422, stash-box #818), a primary name
(#610), cross-studio alias correlation, and alias-aware search (#3266,
#804, #742 — filter a performer's scenes by alias).

### 7.11 Automatic career span (C31, owner-added)

First and last appearance derived from item dates and markers, editable,
never destructive (#619, stash-box #619). The career length is a *derived*
value shown on the record; age at scene is computed from it (#5748), and
measurements over time (§7.7) chart against it.

### 7.12 Group appearance credit (C32)

Not every appearance is the same kind: cameo, non-sexual presence, group
scene, duologue, background. Per-item appearance type (#342, #5444 exclude
from "appear with"), performer re-ordering in scene cards (#6218), and the
"appear with" graph as a first-class browse surface (#1961). Scene-group
semantics (#342) and O-counter per appearance are part of this.

## 8. Metadata, voting, and curation

The other half of what upstream splits between the two projects, designed once.

### 8.1 Typed field voting (C33)

**The model.** For each `(object, field)` the platform holds a set of
`FieldProposal`s, each with a value, a proposer (a user or an automatic
proposer), provenance, and a timestamp. The field's *displayed* value is the
winner by weighted vote, computed and cached. A field may be `locked`
(stash-box #213), which pins the value and refuses further proposals until a
steward unlocks it.

**Read-only public access.** A tiered role model sits alongside the consent
tier (§14.1) and is what makes stash #2792 work:

| Role | Browses | Votes | Curates | Extra |
|---|---|---|---|---|
| `public` (no login) | yes, within consent tier | no | no | consent-tier-visible items only |
| `subscriber` | yes | yes | no | tier-filtered items |
| `contributor` | yes | yes | yes | full tier filter |
| `steward` | yes | yes | yes | + moderation queue |
| `admin` | yes | yes | yes | + settings, roles, peer config |

A `public` role with no login is explicitly supported: read-only browsing of
consent-tier-visible items, with streaming, download and voting all gated.
This is the "let non-authenticated users view content without being able to
modify it" case from #2792, and it is exactly why §14.1's tier model must be
enforced in the query layer and not the UI — an anonymous viewer is the case
where a UI-only filter leaks.

This replaces the upstream model where a field is a column and edits are
incidents. It is also the only shape in which "users vote on the best title"
(the user's request) is a native operation rather than a bolt-on.

Vote weights (§8.3) and decay mean a settled value can change when a better
proposal arrives, which is the correct behaviour for a living index: the
displayed value is a function of the evidence, not a stored opinion.

### 8.2 Candidate generation (C34, owner-added)

Where do proposals come from when no upstream source exists? From local
signals and from the model, each a first-class proposer with its own trust:

| Proposer | Signal |
|---|---|
| `filename` | Parsed from the filename (per-studio parsers, #2680, #484 date) |
| `embedded` | Container/metadata tags, JPEG EXIF/IPTC (#2719) |
| `phash-match` | An item with the same perceptual hash as a described item |
| `transcript` | Local ASR on audio/interviews → keywords, chapter titles (#8) |
| `caption` | Sidecar caption files, parsed |
| `ml:tagger` | Local tag inference (§8.2.1) |
| `ml:captioner` | Local image/video captioning → title and description candidates |
| `user` | A human, or a human's script |
| `peer:<id>` | A federated index's settled value (§13) |
| `scraper` | An upstream source, when one exists (the stash path, kept) |

Every proposal records *why* it exists, so the UI can show "title proposed
from filename" beside "title proposed by 4 users", and a user can accept a
source wholesale or field by field.

#### 8.2.1 Local tag inference (answers stash #7250 explicitly)

The request "use AI to tag videos, marking sexual positions and scenarios" is
in scope as a local model. A tag-inference model over keyframes proposes
structured tags — including pose and scenario taxonomies — each tagged with
its namespace (`ml:tagger`) and confidence (§5.15). ML tags are proposals,
never settled values on their own, and the confidence is visible. This is the
"amateur content gets curated even with no official metadata source" promise,
mechanically.

### 8.3 Reputation and trust (C35, owner-added)

Vote weight is a function of reputation, and reputation is a function of
*agreement with settled outcomes* over time, not of volume. Design points:

- New accounts have a low weight and ramp as their proposals are confirmed
  (the newcomer problem, #743 notes the current method is flawed).
- Weight decays if a user's proposals are repeatedly rejected.
- Sybil damping: many accounts voting the same way is discounted, and
  coordinated patterns are flagged (not silently punished).
- Double-vote trust tier (stash-box #630) as a manual steward-granted status,
  with an audit trail.
- Votes are per-field, so agreeing about titles says nothing about tags.

### 8.4 Leaderboards and badges (C36)

Contributors, not performers (§2). Leaderboard by contribution points
(#595, stash-box #595, #417), badges (#417, #2734), bounties and quests
(#569), and reward points for invited contributors (#600). Points are earned
for *accepted* proposals, which ties the economy to §8.1. Invite-key count is
configurable (stash-box #551). No popularity front page (§2).

### 8.5 Locking and moderation (C37)

A steward queue for contested fields (#213), disputed merges, disputed consent
tier, and abuse reports. Editing another's pending edit is allowed with
attribution (#599). Amending an edit (#226), editing closed submissions (#570),
limits on pending edits per user (#782), and pinned comments (#700) are all
moderation-surface features. A name-collision warning before submission
(#714, #950) and an ignore/exclude list for studios and performers
(stash-box #787, #787) are also here.

### 8.6 Edit history and integrity (C38)

Per-field history with revert and blame (#552 performer modification history,
#6677 history entry dates, stash-box #656 remove-info-and-report on history).
The correctness rule, which fixes the two real bugs in this area: **scores are
recomputed from the accepted-edit set**, never maintained as a counter, so
merges (#943), field deletions (#9: NULL vs unset are distinct states), and
amendments stop corrupting vote totals.

### 8.7 Alternate titles (C39)

N titles per object with language and source (#345), a primary title, and
alias-aware search over all of them.

### 8.8 Ratings and recommendations (C40, C53)

Ratings (§5.17) and a recommendation engine: content-based (similar items by
tag, performer, cluster) and collaborative (co-rated). #3074, #559 (similar
performer recommendations), #474. Recommendations are a browse surface, not a
popularity ranking of people, and they are computed locally by default.

### 8.9 Studio and producer metadata (C41)

§5.12. Studio founding/closure and defunct status (stash-box #1279), ownership
history and codes (#820), aliases with URL correlation (#818), more info
fields (#676), studio-level auto-tags (#641), non-unique names (#5210),
URL pattern matching (#5966, #7200 overlap warnings), custom URL
mapping (#2048), and per-studio default tab/ordering (#6860, #6076).

### 8.10 Release groups (C42)

§5.12. Group types: release, series, compilation, franchise, and the
scene-group relationship of stash-box #663.

### 8.11 User lists (C43)

§5.14. Shared and private lists, templates (stash-box #834), Wantlist and
collection (#458), and "add to collection" from any surface.

### 8.12 Consent tiers and content filtering (C44)

Answers stash-box #643 (hide categories), #733 (block by tag/studio/star),
#1005 (filter by tag exclusion), #986 (filter by multiple tags), #787
(exclude studios and performers), #1175 (exclude studios from accepting
scenes), #540 (per-instance image toggle).

Every object carries a consent tier (§14.1) and every user carries a content
filter that is enforced at the query layer, not in the UI: a filter that hides
a category actually removes those rows from every query, search result,
recommendation and export, so a hidden category cannot leak through a
different surface.

### 8.13 Funder and subscription linking (C45)

Display-only links to existing funder/subscription profiles (#5601 Bluesky
icon, #5399 favicons, #666 stacked icons, #664 default ordering, stash-box
#1161 defunct links). No payments, no ranking by funder, no tip jar (§2).

## 9. Discovery and search

### 9.1 Filter UI (C46)

§5.16. One typed filter AST, facets per kind, multi-select, NULL semantics,
URL-serialized, saved as smart collections.

### 9.2 Search breadth (C47)

One search index over: titles (all languages), descriptions, tags, performer
names and aliases, clusters, studios, groups, markers, **transcripts and
captions** (#4985, and §5.8's Q&A moment search), and external IDs. Search
includes performers and tags in keyword search (#2976) and links on
sub-pages (#3266).

### 9.3 Fuzzy and synonym search (C48)

Phonetic and fuzzy matching, alias and nickname awareness, and per-vocabulary
synonyms (so "cunt" finds the tag, "ass" finds both meanings). FTS in both
Postgres and the embedded store, with the same tokenizer and the same
synonym table — one behaviour across both engines (§3.5).

### 9.4 Tag organization (C49)

§5.15. Tree, namespaces, colors, groups, importance weight, and a full
taxonomy browser.

### 9.5 Folder-like organization (C50)

§5.14. Virtual folders over saved queries, breadcrumbs (#1723), plus the
FUSE-style read-only export (#1385) for tools that want a real filesystem,
and a semantic view of the library mounted as a filesystem.

### 9.6 Image and gallery organization (C51, owner-added)

Per-image tagging, ratings, ordering, custom numbering, and similarity search
over image embeddings — the image-side counterpart of §7.4, since in an
amateur corpus the face is often the only thing that links two sets.

### 9.7 Similar and duplicate detection (C52)

`same_scene_as` and `re_encode_of` relations (§5.9), phash ANN search (#1220),
perceptual near-duplicate detection, and — strongest of all — **content-hash
and infohash identity** (§5.18), so two copies of one file in two libraries
are recognised even when the perceptual hashes differ. A duplicate checker is a
view over these relations: side-by-side merge with direction made obvious
(#6429),
100%-identical display (#5823), bitrate and quality metrics (#5067, #2397),
highest-bitrate selection (#5067), mark-as-not-duplicate (#1656), exclude
organized items (#3531), select-by-path (#6382), report export (#5412),
merge-and-delete-files in one task (#6430), and auto-merge by rules (#2094,
which is opt-in and reversible).

### 9.8 Recommendations (C53)

§8.8. Local by default; collaborative when the index has enough signal.

## 10. Images, previews, and interface

### 10.1 Preview and sprite pipeline (C54)

The generated-artifact pipeline: thumbnails (WebP/AVIF, alpha preserved,
progressive, #3038, #5850, #1585), scrubber sprites, posters, responsive
thumbnails, waveform (#5144), marker thumbnails, VR flatten options (#5275),
and a cache keyed on content (§6.2) so a stale artifact is impossible. Cover
updates immediately on generation (#2227). Set a scene cover from file, URL,
marker, or linked gallery (#5819, #2170, #2032). Gallery cover from children
(#6657). Tag images inherited from tagged items (#6254).

### 10.2 Lightbox (C55, owner-added)

The full-screen viewer, done properly: mouse-wheel and drag panning that do
not accidentally navigate (#7149, #7148, #7147), touch controls (#2538),
back-button that does not surface an unsaved modal (#7154), keyboard
shortcuts including ratings (#5616), image-clip video controls (#5584), and a
compare mode for two images.

### 10.3 Identify via image (C56, owner-added)

The identify task accepts an image, a frame, or a gallery as its seed
(#6134, #5869), so a user who screenshots a performer can identify from that
screenshot. This is the workflow an amateur corpus needs and a studio-keyed
scraper cannot serve.

### 10.4 View modes (C57)

Wall with group-by and auto-scroll (#6544, #6955), rich list tables for bulk
edit (#517, #899), folder view (#1586), unified media view (#1030), tag
view (#773), secondary-sort correctness (#7068, #1508), per-user density,
and virtualization throughout (§4.2).

### 10.5 Play from grid (C58, owner-added)

Inline playback in the grid with hover-scrub and no page hop (#3350, 15
comments), plus "play next" behaviour across deletes (#7157) and queues
(§5.14).

### 10.6 Vertical view (C59, owner-added)

The TikTok-style full-bleed vertical feed (#3859), which is how a large
amateur library is actually browsed on a phone, with swipe, autoplay-on-focus,
and preloading of the next few items.

### 10.7 Keyboard and power use (C60)

A complete shortcut map, a command palette, and undo for destructive actions
(#3221, #1052, #647, #2833 collision handling, #2542 Esc closes modals,
#6218/#5587 ordering shortcuts). All destructive actions confirm (#594 cancel-confirmation, stash-box
#594, #3221 undo) and long tasks confirm with their scope spelled out.

### 10.8 Theming and accessibility (C61)

Dark/light, contrast, focus management, screen-reader labels, hit-target sizes
(#3322, #6383, #6823, #6816), safe-area insets for notched displays (#5979),
viewport-constrained popovers (#4667), and a lightbox animation toggle
(#6864). Mobile web is a supported layout (#771, #6335).

### 10.9 Cards and profiles (C62)

Performer/studio/group cards with consistent buttons (#6823), direct profile
links (#5400, #3617, #2528 StashID badge), a performer link panel with
favicons and sorting (#5587, #5399), full path in the tagger (#2866), and
scene-index alignment (#5397).

### 10.10 Bulk editing (C86)

The bulk-edit modal as a first-class surface (#5336), right-click paste
(#7139), unsaved-entry protection (#6466, #3253), CSV and paste-parse import
(#1296, #431 bulk tag input), per-field ignore lists for the tagger (#2318,
#2399), and create-from-subpage (#3694) and create-all-missing (#1017, #3122)
actions.

## 11. Player and integration

### 11.1 Player (C64)

Codec fallback for exotic formats (on-demand proxy, §11.5), subtitle toggle
from the embedded track or sidecar, frame-accurate seek, deinterlacing (#5313),
crop/pan/flip video filters (#5312, #2160), custom playback speed and
long-press 2× (#2645, #6982), A/B loop with touch support (#5178), audio-track
selection (#1058), and control-bar layout that survives fullscreen and mobile
(#6526, #6811). Skip-intro per source (#634). Ratings in the player (#3250).

### 11.2 Cast and network playback (C65, owner-added)

Chromecast, DLNA and AirPlay (#4136, 17 comments). DLNA exposes subtitles
(#5420) and saved filters as virtual folders (#3135, #1580), including the
legacy-folder layout (#3107). Bitcode/hardware paths for cast targets.

### 11.3 External players (C66, owner-added)

Send any item to mpv, VLC, Jellyfin or Plex with resume position and injected
metadata (#2747, #2770, #966 STRM files), and the same for a Tauri-local
"open in system player" action.

### 11.4 Plugin and scraper SDK (C67)

The plugin API is a stability promise (34 "plugin idea" issues, #4510's UI
plugin API discussion, #5002 plugin settings UI, #4998 hook source context, #5118 services
tab, #3001 file-destroy hook, #2381 duplicate-checker hook, #4998 hook source
context, #6874 patch-hook safety, #1828 phash via hook, #5944 plugin task
settings, #6987 reinstall, #6185 backup before plugin install, #1695 errors as
toasts, #6899 plugin setting defaults, #5118). Plugins run in a WASM sandbox
(§4.4) with declared capabilities: filesystem scope, network scope, hook
subscriptions, UI extension points, and a task type. Scrapers are plugins:
per-scraper rate limits (#2914), pinned/favourite scrapers (#5460), arbitrary
grouping (#6283), rename-in-app (#1446), overlap warnings (#7200), and a
scraper health dashboard (#837, #7258 discoverability).

**Capability declaration and network scoping** are part of the SDK, not
per-plugin hints. A plugin manifest declares the capabilities it needs
(`fs` with a path scope, `net` with a scope, `http` host list, `loopback_http`,
`db` read-only or scoped-write, `hooks`, `ui`, `task`), the host enforces them
at instantiation, and the declared set is displayed to the user before install
(§5.18.1). The `loopback_http` scope exists specifically so a plugin can talk
to a local service — a download client, an encoder, a media server — without
being granted general egress. A plugin that asks for `net` at all is shown to
the user as asking for internet access, and plugins that do (scrapers) are
expected to; the distinction the API must make crisp is *which* network, not
whether.

The SDK is load-bearing rather than decorative: the platform ships its own
C91 locator capability as a one-click plugin through this API (§5.18.1), so the
API's adequacy is proven against a real consumer before a third party depends
on it.

### 11.5 API and external clients (C68, C73)

A documented public API — GraphQL for UI parity with upstream, plus REST for
scripts and a small OpenAPI surface for external tools. Upload endpoints for
video and image from the API (#4995, #1125, #13) with consent attestation
required (§14.1). A Jellyfin-compatible read API (#2747) so existing
clients work. A client SDK. §13.4 covers import/export.

## 12. Accounts, deployment, operations

### 12.1 Multi-user (C69)

Accounts with roles and per-library permission (#2337, 27 comments), steward
and moderator roles (stash-box #100 more user roles), an audit log, SSO
(stash-box #953), and per-user settings that are data, not config files
(#1335, #1896).

### 12.1.1 Serving a library to the public — stash #2792 (adopted, not_planned upstream)

stash #2792 asked for exactly this platform in 2022: serve a stash instance
over the web, Plex-style, with moderation on deletes and merges, admin-chosen
moderators, and a points economy for uploads and curation. It was closed
`not_planned` on 2026-05-02 — the maintainers' objection was that a public
serving stack is a different product from a personal organizer, and
`pickleahead`'s closing advice was to build it as a separate project. This
platform is that separate project, so the request is adopted. What it requires,
beyond §8.1's role table:

**Streaming surface.** Transcoded-on-demand HLS for browsers, direct file
serving with HTTP range requests for local-network clients, and the DLNA and
cast targets from §11.2 so the same library reaches a TV. Bandwidth is the
real constraint, not disk: quality ladders are configurable per role
(§4.3's memory budget applies to the transcoder too), and a `public` visitor
is served the lowest rung by default.

**Time-limited share links.** #2792's actual use case was `holly-hacker`'s:
sharing a limited amount of content with specific people. So share links
(#5612) are first-class: a signed, expiring, optionally password-protected URL
grants exactly one capability — view, or view-and-download — on one item or
one smart collection, revocable at any time, with an access log. It is a
capability grant, not a second account system.

**Granular per-item access.** #2792's later comments asked to allow or block
access to particular items. Consent tier (§14.1) plus a share scope covers it:
an item is visible to a role, to a share link, or to nobody.

**Moderation on destructive public actions.** A `public` or `subscriber`
action that would delete, merge, or bulk-edit routes to the steward queue
(§8.5) rather than executing. This is what #2792 meant by "waiting for
moderation approval on deletes, merges".

**Points economy.** #2792 proposed points for uploads and curation, spent on
watching and downloading, with the author's own note that a per-action database
join would not survive load. The design: points are earned by *accepted*
contributions (§8.4) and spent on *metered* actions, where the meter is a
cached counter per account with a periodic flush, not a transactional join.
Bandwidth-heavy actions additionally have a configurable per-account quota
rather than a per-action price, which is the honest way to do it — a price
per action invites the join the author was worried about, a quota does not.

**Creator self-hosting.** The thread's deeper argument — that creators should
be able to host their own audience rather than depend on a centralized
platform — is why §0.6 makes the index self-hostable and federation-capable,
and why the funder-link model (§8.13) is display-only: the platform hosts the
metadata and the audience relationship, the creator keeps their existing
subscription channel. This is stated because it is a design boundary, not an
oversight: no payment processing, no payouts, no paywall.

### 12.2 Auth hardening (C70)

Passkeys first, TOTP second, argon2id passwords with a sane maximum and a
documented minimum (stash-box #583, #809, #660 field length limits; stash
#7135). Session management and revocation, rate-limited login, and password
reset that survives any character (stash-box #809).

### 12.3 Containers (C75)

Non-root by default (#684), healthchecks, versioned volumes, and a compose
file that brings up server + Postgres with `pgvector` and the ML sidecar
optional. CI builds the image (#935 tagged releases).

### 12.4 Native install (C76)

Static binary, `systemd` unit, XDG paths (#2814), an in-app updater with
signed manifests (#758, #5625), a hardware-accelerated image variant
(#7007), and the Tauri shell as a separate, optional artifact (§3.4).

### 12.5 Reverse proxy and TLS (C77)

nginx and Caddy recipes, static-asset and websocket tuning, `X-Real-IP`
(#6883), behind-proxy logging, and dev-proxy examples (stash-box #6).

### 12.6 Observability (C78)

Structured logs with a readable shell mode (#2463, stash-box #655), an
in-app log viewer with clear button (#1171), sanitized log upload (#342), a
file-health hub (#837, "which files are broken and why"), job traces,
`/healthz` and `/metrics`, a troubleshooting mode via env var (#6875), and
XDG-compliant config and cache paths.

## 13. Federation

The decision from §0: anyone can run a public index, and the protocol is
federation-capable from day one, so no single operator is a chokepoint.

### 13.1 The protocol (C71)

Peers exchange **signed claims**, not database rows. A claim is a statement
about an entity or a field value, signed by the asserting peer, carrying the
peer's identity, a timestamp, and a content hash. Peers are configured
explicitly (a peer list), and there is no ambient discovery.

What travels: entity identity claims, field-value proposals, performer
fingerprint matches, and — critically — **tombstones and takedowns**
(§14.1). What never travels: file bytes, local paths, transcripts, private
lists, and anything under a consent tier that forbids it.

### 13.2 Merge and conflict (C72)

No consensus algorithm, because there is no global truth to converge on. Each
peer accepts a claim if it improves its local state by the same rules as any
other proposal (§8.1), and conflicting claims become a *dispute* surfaced to
stewards, not a fork to reconcile. Merges propagate as claims. Takedowns
propagate as tombstones and are never voted on. A peer's local decision is
always final for that peer; federation is a channel, not a leader.

### 13.3 Open API and SDK (C73)

§11.5. One documented API served by both modes, versioned, with a changelog
(#69) and a playground (`/playground`, matching upstream's developer
experience).

### 13.4 Import, export, backup (C74)

Full-fidelity export of everything (including consent records, so a takedown
survives a migration), scheduled backup, a restore drill, config backup
(#2636), and metadata import/export to a folder (#428, #5498). Import is
lossless and idempotent, and refuses to import a fingerprint set from a
blocked peer (§14.1).

## 14. Consent, safety, and takedown

This is not an appendix. It is the constraint the data model was designed
around, and it is why the platform can hold amateur material at all.

### 14.1 Consent and takedown pipeline (C79)

Every object carries a **consent record**: tier, attestation (who attested
consent, when, on what basis), and an audit trail. Tiers:

| Tier | Meaning |
|---|---|
| `unverified` | Scanned locally; consent not established. Private by default. Not publishable. |
| `self_published` | The uploader asserts they are the creator and the subject consents. Publishable to opted-in peers. |
| `performer_claimed` | A verified performer claim (§7.5) covers it. Strongest tier. |
| `third_party_permitted` | Licensed/permitted by a studio or the subject under a stated basis. |
| `quarantined` | Reported or contested. Hidden everywhere, pending review. |
| `denied` | Takedown accepted. Permanently blocked by hash across all peers. |

Rules, all enforced in the data layer, not the UI:

- `unverified` items never leave the local library without an explicit,
  per-item, per-peer publish consent.
- A takedown request moves an object to `quarantined` everywhere, and if
  accepted, to `denied`, which adds a content-hash blocklist entry that
  propagates to every peer and is checked on import, scan, and match.
- A performer can request takedown of their own appearances without an account
  on the index (§7.5) — via a signed request verified against their claim.
- Consent can be revoked; revocation propagates as a tombstone, not a vote
  (§13.2), and revocation is never outvoted by contribution points.
- Performer name and likeness are protected: linking a person's appearances
  across sources is a *proposal* (§7.1) that a person can decline, and
  declining is recorded and respected.

This is the difference between a hobby index and something that can hold
material people did not upload. The consent record is the reason the rest of
the design is allowed to be ambitious.

### 14.2 Migration and upgrade safety (C80, owner-added)

Forward-only, ordered, transactional migrations in both engines (§3.5), each
gated on a verified backup; a dry-run mode; a documented rollback plan;
schema-version in the health endpoint; and the standing rule that an **applied
migration is immutable in content and position** — corrections ship as new
migrations. This mirrors a hard-won rule from another project of this owner's
and is written down so it is not relearned.

## 15. Data model, taxonomy, and housekeeping

### 15.1 Naming and entities (C81)

One vocabulary for tagging concepts across the UI, scrapers, plugins, and API
(#4336 rename/relabel sweep). The entity set, defined once for both engines:

`Object` (polymorphic: scene, image, gallery, audio, comic, text, interview) ·
`File` · `Segment` · `Image` (as object) · `Archive` · `Marker` ·
`ObjectRelation` · `PersonCluster` · `Appearance` (object↔cluster, with type) ·
`Performer` (a named, claimed profile layered over a cluster) · `Producer`
(studio/circle/individual/collective) · `Group` · `Tag` · `TagNamespace` ·
`FieldProposal` (polymorphic value) · `Vote` · `Rating` · `Consent` ·
`List`/`Playlist`/`SmartCollection` · `Account`/`Role` · `Peer` · `Claim` ·
`Job` · `Artifact` · `ExternalId` · `CustomField`/`CustomFieldValue` ·
`Marker`, `Subtitle`/`Transcript`, `Funscript`, `Award`, `Bookmark`,
`Locator` (magnet, ed2k, infohash, http — §5.18, consent-gated).

### 15.2 Locale and country reference data (C82)

A maintained, versioned country/locale table with correct names (Taiwan #5237,
UK constituent countries #808, state/province where relevant), i18n via
Fluent, compile-time message extraction, and a contribution path for new
locales (#7253 Kiswahili is the example — adding a language is a PR, not a
code change).

### 15.3 Duplicate-entity hygiene (C83)

Detect and merge duplicate producers, tags, and performers, with reference
re-pointing, tombstones, and a scan that skips tombstoned rows (the current
tagger matches deleted studios, stash-box #1007). Scene-level: exclude
organized items from duplicate checking (#3531), mark-as-not-duplicate (#1656).

### 15.4 Notifications and activity (C84, owner-added)

A watched-entity model: watch a performer, cluster, studio, tag, or object, and
get notified when its metadata changes materially — a settled value flips, a
new scene appears, a claim is filed. Digest by default, not a firehose.
Pinned comments (#700), notification badges on pages (#822, #5626), and a
"what changed since I was here" activity feed. This is a metadata-activity
notification system, not a social feed (§2).

### 15.5 Documentation and onboarding (C85)

A docs site, an in-app help section that is actually visible (#700), a
first-run tour, an offline manual (#6365), a sample library, and an
architecture document (stash-box #472). Documentation is a deliverable, not an
afterthought — the platform is unusable without it.

### 15.6 Bulk data entry (C86)

§10.10. CSV import, paste-parse, bulk edit, ignore-lists, and undo. In an
amateur corpus the metadata is entered by hand at scale, so this is
load-bearing, not a convenience.

### 15.7 Fingerprint privacy (C87)

Fingerprints are salted per-peer before publication (#633), the salt is
rotatable, submission is opt-out, and validation-before-submission prevents
corrupt fingerprints from poisoning the index (#2149, stash-box #814
bad-phash growth is fixed by a validation gate plus a bounded growth policy).

### 15.8 Research export (C88, owner-added)

De-identified dataset dumps for ML and academic use — explicitly opt-in per
account, stripped of consent-restricted tiers, with a documented schema. This
is the legitimate research path that does not require scraping the live index.

### 15.9 Scheduled maintenance (C89, owner-added)

A maintenance scheduler: periodic clustering re-consolidation, orphan cleanup,
re-verification of old fingerprints, artifact GC, and index-sync with peers.
Every maintenance job is visible, pausable, and logged.

### 15.10 Deep links (C90)

Every view state is a resolvable URL: filters, sorts, view mode, tagger
selection, player position. A share link reproduces a view exactly
(#337, #5612 for time-limited share links, #185 query syntax, and the
external-ID registry #1790).

### 15.11 The documented non-goals from the issue corpus

Three issues are not implemented, with reasons:

- **#5736 (Support Handy firmware 4)** — a USB vibrator/dongle integration.
  Out of scope; a hardware protocol for a specific device. (X05)
- **#5979 (navbar cut off on notched mobile displays)** — a mobile-browser
  viewport bug on iOS Safari. The platform's mobile story is responsive web on
  standards-compliant engines; a Safari Dynamic Island workaround is not a
  platform feature. (X01)
- **stash-box #551 (configure the number of invite keys)** — invite-key policy
  is operator configuration in this platform (§8.4), not a per-instance
  database setting. Folded, not dropped. (X01)

Everything else in the 850 is either mapped to a capability in Appendix A or
is answered by one of the ninety-one capabilities. The full per-issue mapping,
with the capability and section for each, is Appendix A.

## 16. What this borrows, and under what terms

Nothing is copied. Both upstreams are read as behaviour specifications:

- **stash** (AGPL-3.0) — read for the file model, the scanner/generate/identify
  pipeline, the tagger UX, the marker/sprite concepts, the GraphQL shape, and
  the issue corpus. No code is taken. The license is incompatible with the
  intent here (a permissively licensed platform), and the architecture differs
  enough that a clean-room implementation from the documented behaviour is
  both feasible and correct.
- **stash-box** (MIT) — read for the metadata schema shape, the fingerprint
  concept, the voting model, the submission/moderation flow, and the GraphQL
  API. No code is taken; the schema is designed fresh in §15 because the
  upstream schema cannot express an anonymous cluster or a consent tier.

Both remain usable. A user can keep a stash install and point this platform at
it as a peer; the import path (§13.4) reads a stash database and a stash-box
instance's public API.

## 17. Phases, and how the 850 get closed

| Phase | Scope | Exit criterion |
|---|---|---|
| 0 | Foundations: schema in both engines, migration suite, scanner, job engine, budget-enforced binary, Tauri shell, CI with the RSS test, **and the plugin sandbox skeleton** (WASM host, capability declaration, `loopback_http` scope) | A file scanned, generated, and browsed; idle RSS within §4.3 budget; migrations proven in both engines; a test plugin that declares `loopback_http` can reach `127.0.0.1` and is provably refused `net` |
| 1 | Content types: video, images/galleries, audio, comics, funscript, text, interviews (§5) | All seven types scan, generate, play/browse; C01–C14 closed |
| 2 | Library and performance (§6): watcher, throughput, hardware accel, remote-mount handling | 100k-item library scan and browse within a stated budget; C15–C20 closed. Locators (§5.18) are generated here, since ed2k and infohash computation belongs to the scan/generate pipeline and is what makes §9.7's content-hash identity work |
| 3 | Identity engine (§7): clustering, merge/split, fingerprints, self-claim | A person with no name is linked across every appearance in a test corpus; C21–C32 closed |
| 4 | Curation and voting (§8): proposals, weights, reputation, moderation, history | Amateur corpus with no upstream source is fully curated by proposal+vote; C33–C45 closed |
| 5 | Discovery, search, images, interface (§9, §10) | The 150-image-issue and 93-tag-issue clusters closed; C46–C63 closed |
| 6 | Player, plugins, API (§11) | External player, cast, plugin sandbox, public API; C64–C68, C73 closed. **The sandbox and capability-declaration properties are promoted from here to Phase 0**, because §5.18.1 delivers a headline capability as a plugin and the roadmap depends on the SDK existing before it |
| 7 | Federation and consent (§13, §14) | Two peers exchange claims and propagate a takedown; C71, C72, C79, C80 closed |
| 8 | Operations, docs, export (§12, §15) | Non-root container, backup/restore drill, docs site, research export; C69, C70, C74–C78, C81–C90 closed. C91's locator *storage* and *federation* ship here; only the client hand-off waits for phase 10 |
| 9 | Public serving (§12.1.1, adopting stash #2792) | An anonymous visitor browses a consent-tier-visible library over the web; an expiring share link grants scoped access; a `subscriber` delete routes to the steward queue. This phase is last because it is the one that requires §14's consent model to already be proven — serving a library publicly before the consent tiers are enforced in the query layer is the failure mode #2792's thread warned about |
| 10 | External client hand-off (§5.18, as a one-click plugin per §5.18.1) | A consent-permitting object hands a magnet or ed2k URI to a configured qBittorrent/Transmission/aria2 on one click, the tier is re-checked at the moment of the action, and the sandbox makes it structurally impossible for the plugin to speak BitTorrent or ed2k, join a swarm, or accept an inbound connection. Delivered *through* the extension SDK, so it doubles as the SDK's first real consumer. Last of all, because it is the only outward-facing action in the platform and it should ship after §14 is proven by phase 7 |

Each phase closes its slice of Appendix A. The matrix is the acceptance
criterion, not a wishlist: a phase is done when its rows are closed or carry a
documented reason.

---

## Changelog

- **v1 (2026-09-26)** — Initial spec. Merges `stashapp/stash` and
  `stashapp/stash-box` into one design and adds the amateur-curation,
  unsupervised-identity, and federation capabilities neither has. All 850 open
  issues mapped to 90 capabilities in Appendix A; three documented
  non-goals. Owner decisions recorded in §0: Tauri desktop, all seven content
  types including performer interviews, public index with day-one
  federation capability, pseudonymous identity with private self-claim, local
  ML with opt-in remote assist.
- **v1.1 (2026-09-26)** — Owner directed that closed issues count too.
  Adopted **stash #2792 "Serve adult website"** (closed `not_planned`
  2026-05-02) as a first-class feature: §12.1.1 adds the public-serving
  surface, the `public`/`subscriber` read-only roles (§8.1), expiring share
  links, per-item access scope, moderation on destructive public actions, and
  the points economy, and §2's downloader non-goal is narrowed to say so
  explicitly. New Phase 9. This is the strongest available evidence that the
  merge is the right project: the feature that makes stash and stash-box one
  product is the feature upstream declined to build.
- **v1.2 (2026-09-26)** — Owner adopted `YurikaL`'s P2P idea from the
  #2792 thread. New **§5.18** (capability C91) defines P2P locators —
  BitTorrent infohash and magnet, eDonkey2000/ed2k, plus plain HTTP source
  URLs — as *metadata* that federates as ordinary signed claims, dedups
  content across instances, and hands off to a download client the user
  already runs. Owner decisions recorded: the platform is primarily a
  website; both ed2k and BitTorrent/magnet are supported; acquisition stops
  at hand-off, so the "not a downloader" non-goal is narrowed rather than
  deleted (§2) and there is no code path that fetches a file body over a P2P
  network; and locators are refused on any consent tier that does not permit
  redistribution (§14.1), with the tier re-checked at the moment of hand-off.
  Corridates §9.7 dedup, §15.1's `Locator` entity, and §6.1's scan pipeline
  (ed2k MD4 segmentation, infohash computation). New Phase 10, deliberately
  after §14's consent model is proven. The 850-issue corpus contains one
  related issue (stash-box #440), so this is a gap rather than a
  re-implementation.
- **v1.3 (2026-09-26)** — Owner directed that §5.18 (C91) be delivered as
  a **one-click installable plugin** rather than in core. New **§5.18.1**
  makes that explicit, and the design consequences are recorded rather than
  waved at: the plugin's `loopback_http`-only sandbox turns "no swarms, no
  peer list, no DHT, no incoming connections" from a promise into a property
  the host can prove, because the capability is absent rather than unused; the
  **consent gate stays in core** (§14.1) with the plugin only able to
  *propose* locators, so a buggy or hostile plugin cannot bypass the tier
  table; and content hashing (xxh128 + BLAKE3) stays in core because §6.2 and
  §9.7 depend on it, leaving only the protocol-specific hashes to the plugin.
  §11.4 gains capability declaration and network scoping, including a
  dedicated `loopback_http` scope for talking to a local download client
  without general egress. Roadmap consequence: the extension SDK is promoted
  from Phase 6 polish to Phase 0 infrastructure, since a headline capability
  now depends on it.
---

# Appendix A — Full issue matrix (850 open issues)

Generated 2026-09-26 from the GitHub API against `stashapp/stash` (develop,
673 open issues) and `stashapp/stash-box` (develop,
177 open).
Each row: issue → capability id → spec section. All 850 are mapped;
3 are documented non-goals
(X-codes, explained in §15.11). Two further issues are cited in the prose that
are **closed** upstream and are adopted here anyway, because a closed tracker
row is evidence of intent, not of scope: **stash #2792** (serve a library
publicly → §12.1.1, Phase 9) and its `YurikaL` comment proposing P2P
magnet/ed2k exchange with external-client hand-off (§5.18, delivered as a
one-click plugin per §5.18.1, Phase 10).

| # | Issue title | Capability | Spec |
|---|---|---|---|
| stash#12 | Account system | C69 Multi-user & permissions | §12.1 |
| stash#13 | Scene upload from UI | C77 Reverse proxy & TLS | §12.5 |
| stash#39 | Similar/related scenes tab based on scene details | C52 Similar-item detection | §9.7 |
| stash#47 | Implement ajax load/infinite scroll pagination functionality | C36 Leaderboards & badges | §8.4 |
| stash#118 | Alphabetical performer list inside sidebar | C33 Typed field voting | §8.1 |
| stash#185 | Query syntax | C26 Performer field model | §7.6 |
| stash#226 | Custom scenes and playlists | C11 Markers & chapters | §5.11 |
| stash#233 | Support additional gallery archive formats (7z, gz, tar.gz) | C03 Image & gallery sets | §5.3 |
| stash#245 | Auto Tag "extra settings" | C09 Scene extras & related clips | §5.9 |
| stash#260 | Use markers to create scene chapters/bookmarks | C11 Markers & chapters | §5.11 |
| stash#284 | Navigation using next/previous buttons for performers | C77 Reverse proxy & TLS | §12.5 |
| stash#314 | Add functionality to prevent cleaning on particular drives (Hot swap drive user) | C16 Incremental scan correctness | §6.2 |
| stash#325 | Ability to "Select All" objects across all pages | C46 Filter UI redesign | §9.1 |
| stash#342 | Ability to upload sanitized logs to external pastebin | C77 Reverse proxy & TLS | §12.5 |
| stash#398 | Groups section Suggested Improvements | C33 Typed field voting | §8.1 |
| stash#422 | Enhanced performer aliases with studio association | C30 Stage/alias naming with studio | §7.10 |
| stash#428 | Import/Export scene metadata to/from same folder | C50 Folder-like organization | §9.5 |
| stash#484 | Support date with Auto Tag task | C41 Studio model & extras | §8.9 |
| stash#517 | List view overhaul; rich tables for easier bulk and individual metadata editing and more | C57 Grid/list view modes | §10.4 |
| stash#552 | Calculate average performer rating from rated scenes | C40 Ratings & recommendations | §8.8 |
| stash#571 | Support for multiple performer images | C09 Scene extras & related clips | §5.9 |
| stash#591 | XPath scraper shouldn't remove newlines for Details field | C67 Plugin & scraper SDK | §11.4 |
| stash#634 | Ability for previews to skip the intro of scenes from specific sources | C54 Preview & sprite pipeline | §10.1 |
| stash#647 | Add keyboard shortcuts to focus selector fields | C36 Leaderboards & badges | §8.4 |
| stash#684 | Non-privileged user in Docker build | C01 Video container support | §5.2 |
| stash#691 | SOCKS5 proxy support for scraping | C19 Transcode & proxy pipeline | §6.5 |
| stash#700 | Make in-app help section more noticeable to new users | C85 Documentation & onboarding | §15.5 |
| stash#710 | Store and display corrupt files | C61 Theming & accessibility | §10.8 |
| stash#715 | Assign default scrapers to individual fields and add 'Scrape From All Sources' button on | C63 Confirmation on cancel | §10.10 |
| stash#758 | Automated client-side update mechanism | C76 Native install path | §12.4 |
| stash#761 | Common field post-processing scraper enhancement | C03 Image & gallery sets | §5.3 |
| stash#765 | Ability to attach performer to markers | C11 Markers & chapters | §5.11 |
| stash#773 | Add tag view mode to display all tags in a single page | C11 Markers & chapters | §5.11 |
| stash#779 | Mark Scene as Full Movie | C42 Release groups / scene groups | §8.10 |
| stash#819 | Configure number of threads for ffmpeg transcodes | C01 Video container support | §5.2 |
| stash#821 | Support for multiple 'is missing' filters | C46 Filter UI redesign | §9.1 |
| stash#837 | Log potential issues with files and show in dedicated information hub | C03 Image & gallery sets | §5.3 |
| stash#861 | Image Filename Parser tool | C03 Image & gallery sets | §5.3 |
| stash#872 | Add "set this image as..." in image operations menu | C03 Image & gallery sets | §5.3 |
| stash#894 | Allow to change encoder for FFmpeg -c:v parameter in Configuration for Preview Generatio | C15 Scanner throughput | §6.1 |
| stash#899 | Custom headings in the "List" view and easier sorting | C57 Grid/list view modes | §10.4 |
| stash#966 | Support external playback via STRM file | C64 Player & subtitle UX | §11.1 |
| stash#972 | "Watch Preview" Button | C54 Preview & sprite pipeline | §10.1 |
| stash#1006 | PDF Support | C07 Text, story posts, links | §5.7 |
| stash#1010 | "Search All" functionality for scene tagger view | C23 Performer fingerprints | §7.3 |
| stash#1017 | One-click button to create all missing objects in "Scene Scrape Results" dialog | C33 Typed field voting | §8.1 |
| stash#1024 | Performer-specific Tag-field in Scene Edit tab | C86 Accessibility of metadata entry | §15.6 |
| stash#1028 | Stash built for a particular type of conventual studio-produced porn, management of porn | C41 Studio model & extras | §8.9 |
| stash#1029 | Folder-like structure for organizing content | C46 Filter UI redesign | §9.1 |
| stash#1030 | Add "Media" tab which combines scenes and images | C06 Funscript & interactive | §5.6 |
| stash#1052 | Keyboard shortcut to create scene marker while watching a video | C11 Markers & chapters | §5.11 |
| stash#1058 | Ability to select different audio track during playback | C04 Audio & music | §5.4 |
| stash#1111 | Checkbox to enable case insensitive RegEx filtering | C46 Filter UI redesign | §9.1 |
| stash#1125 | Upload image from the UI | C03 Image & gallery sets | §5.3 |
| stash#1127 | Support for sub-galleries | C03 Image & gallery sets | §5.3 |
| stash#1129 | Browse and filter scenes by alphabet letter  | C46 Filter UI redesign | §9.1 |
| stash#1161 | Option to link scenes to galleries if they are in the same folder | C03 Image & gallery sets | §5.3 |
| stash#1165 | Filter the stash-box query based on existing metadata | C41 Studio model & extras | §8.9 |
| stash#1170 | Add organized flag to performers | C67 Plugin & scraper SDK | §11.4 |
| stash#1171 | Add clear logs button | C78 Observability & logging | §12.6 |
| stash#1173 | In-app editor for config.yml file | C85 Documentation & onboarding | §15.5 |
| stash#1182 | Add "Test" button to "Chrome CDP Path" config option | C67 Plugin & scraper SDK | §11.4 |
| stash#1183 | Add `{studioName}` and `{studioURL}` placeholder fields for sceneByFragment scrapers | C41 Studio model & extras | §8.9 |
| stash#1220 | Ability to lookup similar scenes by PHASH in scene duplicate checker | C01 Video container support | §5.2 |
| stash#1246 | Search Bar for Settings | C33 Typed field voting | §8.1 |
| stash#1253 | Separating tags for higher-level objects | C03 Image & gallery sets | §5.3 |
| stash#1258 | Support audio files/object type | C04 Audio & music | §5.4 |
| stash#1259 | Support text files/object type | C04 Audio & music | §5.4 |
| stash#1280 | Ability to configure default tab for object that have attached files | C11 Markers & chapters | §5.11 |
| stash#1290 | Ability to Move Between Media in Collection | C03 Image & gallery sets | §5.3 |
| stash#1296 | Ability to parse a list of tags to add them in bulk to an object | C40 Ratings & recommendations | §8.8 |
| stash#1317 | Global Filters (aka Libraries) | C39 Alternate titles | §8.7 |
| stash#1334 | Unified Scene and Image View on Performer page | C03 Image & gallery sets | §5.3 |
| stash#1335 | Persistent scene filters settings | C03 Image & gallery sets | §5.3 |
| stash#1341 | Global option to only accept StashID in Scene Tagger | C23 Performer fingerprints | §7.3 |
| stash#1365 | Contextual filtering across all objects | C41 Studio model & extras | §8.9 |
| stash#1367 | Extend Performer/Scene Tagger for other scrapers | C74 Import / export / backup | §13.4 |
| stash#1385 | Semantic FUSE filesystem to mount organised version of contents of library in host files | C50 Folder-like organization | §9.5 |
| stash#1400 | Ratings for Tags | C40 Ratings & recommendations | §8.8 |
| stash#1445 | Resume Interrupted Task Queue | C17 Background job engine | §6.3 |
| stash#1446 | Allow in-app renaming of scrapers | C77 Reverse proxy & TLS | §12.5 |
| stash#1459 | Add multiple new movies with url list | C41 Studio model & extras | §8.9 |
| stash#1460 | Ability to add a tag to a specific performer on object pages instead of to the object it | C03 Image & gallery sets | §5.3 |
| stash#1463 | Ability to map scraped tag to multiple Stash tags | C30 Stage/alias naming with studio | §7.10 |
| stash#1464 | Feed / Editing View for Collections (Galleries, Images, Scenes, etc) | C03 Image & gallery sets | §5.3 |
| stash#1495 | Split performer MEASUREMENTS value into distinct BUST, CUP, HIP, and WAIST values | C22 Performer identity merge/split | §7.2 |
| stash#1508 | Support Multiple Sorts and apply them consecutively | C36 Leaderboards & badges | §8.4 |
| stash#1545 | Organize Scene Filter Dropdown with added category sections | C33 Typed field voting | §8.1 |
| stash#1580 | DLNA folders Recently added, Recently viewed, Unplayed | C50 Folder-like organization | §9.5 |
| stash#1585 | Convert thumbnails to support progressive image loading or support lazy loading | C03 Image & gallery sets | §5.3 |
| stash#1586 | Folder View mode for browsing | C50 Folder-like organization | §9.5 |
| stash#1599 | Ability to Merge existing tags in Scene Scrape Results modal | C83 Duplicate-entity hygiene | §15.3 |
| stash#1624 | Add oragnized flag to groups (aka movies) | C39 Alternate titles | §8.7 |
| stash#1652 | Improve/modernize consistency of formatting in scene detail | C86 Accessibility of metadata entry | §15.6 |
| stash#1656 | Ability to mark scenes as not duplicate for scene duplicate checker | C52 Similar-item detection | §9.7 |
| stash#1659 | Add a Manga/Doujin section | C05 Comics / manga / doujin | §5.5 |
| stash#1680 | Calculate automatic ratings for Performers, Tags & Groups based on related content | C40 Ratings & recommendations | §8.8 |
| stash#1695 | Display plugin errors as toasts | C67 Plugin & scraper SDK | §11.4 |
| stash#1723 | Show Breadcrumb for Nested Tags | C49 Tag groups & attributes | §9.4 |
| stash#1732 | Tag tree view mode for Nested Tags | C57 Grid/list view modes | §10.4 |
| stash#1790 | Generalized support for external IDs | C90 Deep-linkable URLs | §15.10 |
| stash#1811 | Faster/more intuitive UI/UX for adding to galleries | C03 Image & gallery sets | §5.3 |
| stash#1828 | Getting Phash with a hook plugin | C47 Full-text search breadth | §9.2 |
| stash#1863 | Child studios should Inherit posters from their parent | C41 Studio model & extras | §8.9 |
| stash#1867 | Duplicate Checker tool for Images | C03 Image & gallery sets | §5.3 |
| stash#1896 | Remember sort/filter settings when navigating outside the page | C40 Ratings & recommendations | §8.8 |
| stash#1914 | Option to display linked scene title instead of gallery title | C03 Image & gallery sets | §5.3 |
| stash#1924 | i18n: Streamline Singular/Plural Nouns | C33 Typed field voting | §8.1 |
| stash#1939 | Ability to export selected scenes to a .m3u playlist file | C46 Filter UI redesign | §9.1 |
| stash#1961 | Performers sub-page and performer cards include objects from other studios instead of on | C41 Studio model & extras | §8.9 |
| stash#1981 | Update performer scraper interface to parity with scenes | C47 Full-text search breadth | §9.2 |
| stash#2032 | Set tag image based on a marker image or scene image | C11 Markers & chapters | §5.11 |
| stash#2045 | Scraper option to choose the encoding of the URL | C47 Full-text search breadth | §9.2 |
| stash#2048 | Custom URL mapping for a single URL | C01 Video container support | §5.2 |
| stash#2049 | Request for Submissions: Stash Logo | C78 Observability & logging | §12.6 |
| stash#2055 | Extend Auto Tag task sources by title and description | C34 Candidate generation | §8.2 |
| stash#2067 | Scan task button label change | C16 Incremental scan correctness | §6.2 |
| stash#2078 | Improve and add more classnames to elements in card-sections | C62 Card & profile detail | §10.9 |
| stash#2080 | Backup functionality should not work concurrently to other tasks | C17 Background job engine | §6.3 |
| stash#2082 | Ability to upload favicons to studios | C41 Studio model & extras | §8.9 |
| stash#2085 | Link parts of a compilation to their original scenes with markers | C11 Markers & chapters | §5.11 |
| stash#2094 | Automatically delete duplicate scenes based on pre-defined rules | C79 Consent & takedown pipeline | §14.1 |
| stash#2121 | Add markers tab to performer page | C11 Markers & chapters | §5.11 |
| stash#2122 | Filter Functionality UI/UX Refactor Discussion | C44 Content filtering & consent tiers | §8.12 |
| stash#2123 | Improve Gallery Tab in Scene Details Page | C03 Image & gallery sets | §5.3 |
| stash#2142 | Change thumbnail slider to button | C54 Preview & sprite pipeline | §10.1 |
| stash#2149 | Phash validation | C87 Fingerprint privacy | §15.7 |
| stash#2152 | De-duplicating auto-taggable strings | C30 Stage/alias naming with studio | §7.10 |
| stash#2160 | Ability to flip/mirror scenes in player on horizontal axis | C01 Video container support | §5.2 |
| stash#2165 | Auto-populate metadata between scene/gallery relationship | C03 Image & gallery sets | §5.3 |
| stash#2170 | Set Scene Image using linked gallery images | C03 Image & gallery sets | §5.3 |
| stash#2178 | Add language selector to the setup process | C82 Country & locale reference data | §15.2 |
| stash#2182 | Save Filter as Tag | C46 Filter UI redesign | §9.1 |
| stash#2185 | Set Image for Performers via images/gallery | C03 Image & gallery sets | §5.3 |
| stash#2186 | Show stats for selected items | C40 Ratings & recommendations | §8.8 |
| stash#2227 | "Generate thumbnail" should update scene cover image immediately | C03 Image & gallery sets | §5.3 |
| stash#2248 | Move logging prefix to its own HTML element | C67 Plugin & scraper SDK | §11.4 |
| stash#2276 | Handling multi-part scenes, "part X" field and/or UI indicator? | C02 Multi-scene single file | §5.2 |
| stash#2293 | Non-ASCII performers fail to be tagged with Auto Tag | C16 Incremental scan correctness | §6.2 |
| stash#2296 | Support for scene extras similar Plex's movie extras | C09 Scene extras & related clips | §5.9 |
| stash#2297 | Include studio value in Scrape query search string | C41 Studio model & extras | §8.9 |
| stash#2305 | Dedicated Saved Filter list for tagger view | C17 Background job engine | §6.3 |
| stash#2318 | Add ability to ignore more fields when using scene tagger | C86 Accessibility of metadata entry | §15.6 |
| stash#2337 | Support multiple users with configurable permissions | C69 Multi-user & permissions | §12.1 |
| stash#2350 | Flag previously deleted files | C50 Folder-like organization | §9.5 |
| stash#2359 | [Meta] Update Stash to be inline with Stash-Box | C30 Stage/alias naming with studio | §7.10 |
| stash#2381 | Add DuplicateChecker context to on delete hook | C67 Plugin & scraper SDK | §11.4 |
| stash#2397 | Add additional metrics about video quality in Duplicate Checker | C01 Video container support | §5.2 |
| stash#2399 | Scene Tagger - Add option to exclude specific metadata fields from search query | C41 Studio model & extras | §8.9 |
| stash#2420 | Collapse button for excluded tag patterns | C77 Reverse proxy & TLS | §12.5 |
| stash#2463 | Improve readability when logging to shell | C78 Observability & logging | §12.6 |
| stash#2464 | Change default setting of PHash generation to ON for Scans | C74 Import / export / backup | §13.4 |
| stash#2479 | Add tag to objects based on scraping method | C01 Video container support | §5.2 |
| stash#2492 | Sort scenes by date when they were rated | C74 Import / export / backup | §13.4 |
| stash#2494 | Option to reset metadata on an object | C38 Edit history & rollbacks | §8.6 |
| stash#2507 | Support performer alias(es) in Auto Tag task | C30 Stage/alias naming with studio | §7.10 |
| stash#2511 | Create virtual files for compilations | C09 Scene extras & related clips | §5.9 |
| stash#2521 | Ability to bulk submit scenes to stash-box | C74 Import / export / backup | §13.4 |
| stash#2523 | Ability to change default quality when using live transcode | C01 Video container support | §5.2 |
| stash#2528 | Show badge on Performer card in Scene Details Page if performer has StashID | C36 Leaderboards & badges | §8.4 |
| stash#2538 | Support mobile controls inside lightbox | C03 Image & gallery sets | §5.3 |
| stash#2540 | Image HTTP request referrer behavior | C03 Image & gallery sets | §5.3 |
| stash#2542 | Allow `Esc` key to close any and all modal windows | C60 Keyboard shortcuts & power use | §10.7 |
| stash#2548 | Select .zip files in Selective Scan | C86 Accessibility of metadata entry | §15.6 |
| stash#2626 | Add Missing Scraper Fields | C03 Image & gallery sets | §5.3 |
| stash#2633 | Tag edit option to hide tag from dropdown lists | C77 Reverse proxy & TLS | §12.5 |
| stash#2636 | Backup the Stash config | C67 Plugin & scraper SDK | §11.4 |
| stash#2645 | Custom playback speed (or just bring 4x back) | C15 Scanner throughput | §6.1 |
| stash#2647 | Ability to create a playlist from multiple markers | C11 Markers & chapters | §5.11 |
| stash#2651 | Ability to Merge existing Performers in Scene Scrape Results modal | C41 Studio model & extras | §8.9 |
| stash#2662 | Create menu item for Interactive Options in settings | C06 Funscript & interactive | §5.6 |
| stash#2679 | Option to choose Studio during selective scan | C32 Group appearance credit | §7.12 |
| stash#2680 | Option to Scope Scene Filename Parser to Specific Studio | C41 Studio model & extras | §8.9 |
| stash#2719 | Integrated way to import embedded metadata from JPEG | C09 Scene extras & related clips | §5.9 |
| stash#2722 | Gallery Filename Parser tool | C03 Image & gallery sets | §5.3 |
| stash#2723 | Create second database within the program and fast switch between databases | C50 Folder-like organization | §9.5 |
| stash#2734 | Achievements | C36 Leaderboards & badges | §8.4 |
| stash#2736 | Allow creating a tag from anywhere tags can be added | C01 Video container support | §5.2 |
| stash#2742 | Studio logo on cards visibility options | C03 Image & gallery sets | §5.3 |
| stash#2747 | Jellyfin-like external remote player support | C01 Video container support | §5.2 |
| stash#2762 | Ability to manually pause interactive funscript from the scene player | C06 Funscript & interactive | §5.6 |
| stash#2765 | Lightbox image changes on rating/o-counter value change | C03 Image & gallery sets | §5.3 |
| stash#2770 | Inject metadata into raw streams (for external players) | C64 Player & subtitle UX | §11.1 |
| stash#2773 | Stash is unaware when an image has been replaced if the name is the same | C03 Image & gallery sets | §5.3 |
| stash#2780 | Transfer metadata between linked galleries and scenes | C03 Image & gallery sets | §5.3 |
| stash#2808 | Add additional flag to objects to safeguard against deleting favorite content | C09 Scene extras & related clips | §5.9 |
| stash#2814 | Follow XDG directory specifications | C50 Folder-like organization | §9.5 |
| stash#2816 | Query in Scene Tagger using StashID | C23 Performer fingerprints | §7.3 |
| stash#2824 | Slow scanning with huge amounts of videos | C01 Video container support | §5.2 |
| stash#2833 | `e` keyboard shortcut collides with subpages that use the same shortcut | C60 Keyboard shortcuts & power use | §10.7 |
| stash#2836 | Add O-Counter to Galleries and allow Sorting Galleries by O-Counter | C03 Image & gallery sets | §5.3 |
| stash#2858 | Save selected scraper inside saved filter on scene tagger | C44 Content filtering & consent tiers | §8.12 |
| stash#2866 | Show full path for scenes in scene tagger | C41 Studio model & extras | §8.9 |
| stash#2871 | Move task queue to a nav bar icon popover | C17 Background job engine | §6.3 |
| stash#2879 | Add settings option to hide scene tabs sidebar by default | C52 Similar-item detection | §9.7 |
| stash#2902 | Add option for images to inherit metadata from the gallery they are in | C03 Image & gallery sets | §5.3 |
| stash#2903 | Ability to check stash-box matches based on last updated date in scene tagger | C79 Consent & takedown pipeline | §14.1 |
| stash#2905 | Ability to pre-trancode video file and store it for playback in lower quality | C01 Video container support | §5.2 |
| stash#2913 | Queue Looping | C01 Video container support | §5.2 |
| stash#2914 | Ability to to configure delay/rate limit on per scraper basis | C85 Documentation & onboarding | §15.5 |
| stash#2942 | Parse scene details field when using auto tag task | C01 Video container support | §5.2 |
| stash#2944 | Add user interface to move files using `moveFiles` | C11 Markers & chapters | §5.11 |
| stash#2954 | Generate marker preview immediately after marker is created | C11 Markers & chapters | §5.11 |
| stash#2956 | Add the ability to sort Images and Galleries by Performer Age | C03 Image & gallery sets | §5.3 |
| stash#2960 | Make "Create galleries from folders containing images" more robust to specify gallery fo | C03 Image & gallery sets | §5.3 |
| stash#2973 | Give a measure of importance to tags | C33 Typed field voting | §8.1 |
| stash#2976 | Include performers and tags in scene keyword searching | C40 Ratings & recommendations | §8.8 |
| stash#3001 | Adding `File.Destroy.Post` hook | C16 Incremental scan correctness | §6.2 |
| stash#3031 | Funscript related ideas/goals | C06 Funscript & interactive | §5.6 |
| stash#3038 | Optimize/compress images in both generated folder and database | C03 Image & gallery sets | §5.3 |
| stash#3065 | Make Stash more suitable for JAV | C33 Typed field voting | §8.1 |
| stash#3073 | Allow downloading/caching of video(s) for offline use | C01 Video container support | §5.2 |
| stash#3074 | Content recommendations based on recent history | C07 Text, story posts, links | §5.7 |
| stash#3077 | Advanced SubStation Alpha (ASS) subtitle format support | C10 Subtitles & captions | §5.10 |
| stash#3078 | Add Quit Stash option from the Web UI | C36 Leaderboards & badges | §8.4 |
| stash#3107 | DLNA legacy folders | C04 Audio & music | §5.4 |
| stash#3122 | Create All/New/Missing on Scene Tagger page | C40 Ratings & recommendations | §8.8 |
| stash#3130 | Scene/Performer Tagger Configuration UI Refactor | C33 Typed field voting | §8.1 |
| stash#3135 | Expose Saved Filters over DLNA | C46 Filter UI redesign | §9.1 |
| stash#3159 | Improve filtering in presence of NULL values | C46 Filter UI redesign | §9.1 |
| stash#3171 | Synology NAS and folders table | C01 Video container support | §5.2 |
| stash#3172 | Stash icon almost invisible on windows 10 dark mode | C03 Image & gallery sets | §5.3 |
| stash#3189 | Fallback to scene cover in Duplicate Checker tool if no scrubber sprites are generated | C01 Video container support | §5.2 |
| stash#3197 | Add ratings to markers | C11 Markers & chapters | §5.11 |
| stash#3200 | Tag colors | C49 Tag groups & attributes | §9.4 |
| stash#3219 | Add toggles in settings to hide fields on edit pages | C10 Subtitles & captions | §5.10 |
| stash#3220 | Option to convert scene to fileless scene if attached primary file is deleted | C78 Observability & logging | §12.6 |
| stash#3221 | Add a button inside toasts to quickly undo accidentally created tags | C03 Image & gallery sets | §5.3 |
| stash#3232 | Merge tag to scene button & functionality for performers and studios tags | C41 Studio model & extras | §8.9 |
| stash#3237 | Request sub-task within Generate to run Identify on a file | C15 Scanner throughput | §6.1 |
| stash#3238 | Allow different time units for minimum play percent option | C01 Video container support | §5.2 |
| stash#3250 | Add ratings directly to the scene player | C01 Video container support | §5.2 |
| stash#3253 | Video.js Tags Quick Add Dialouge | C01 Video container support | §5.2 |
| stash#3266 | Add alias field keyword searching for groups | C30 Stage/alias naming with studio | §7.10 |
| stash#3272 | Sort scenes by studio or performer rating from scenes page | C40 Ratings & recommendations | §8.8 |
| stash#3281 | Smarter Tagging Overhaul | C78 Observability & logging | §12.6 |
| stash#3296 | Ability to select multiple studios | C01 Video container support | §5.2 |
| stash#3299 | Native Remote UI | C33 Typed field voting | §8.1 |
| stash#3303 | Remove `^https?:\/\/(www\.)?` and `\/$` from all <a> HTML tags displaying link text URLs | C77 Reverse proxy & TLS | §12.5 |
| stash#3312 | Dynamically load more rows on front page when reaching the end of items | C46 Filter UI redesign | §9.1 |
| stash#3318 | Studio Code display improvement | C33 Typed field voting | §8.1 |
| stash#3322 | Increase the checkbox size for selecting multiple items in scene duplicate checker | C61 Theming & accessibility | §10.8 |
| stash#3333 | Saved Filters: Tag-item badge below toolbar not updating | C36 Leaderboards & badges | §8.4 |
| stash#3336 | metadataScan flag to disable/ignore hooks | C67 Plugin & scraper SDK | §11.4 |
| stash#3350 | Ability to play scene directly from the scenes grid page | C03 Image & gallery sets | §5.3 |
| stash#3361 | Add POST support to scraper queries | C73 Open API & SDK | §13.3 |
| stash#3364 | Tagging Flow/Interface to facilitate content tagging | C74 Import / export / backup | §13.4 |
| stash#3366 | Add metadata in multiple languages | C86 Accessibility of metadata entry | §15.6 |
| stash#3371 | Rename 'Merge' to 'Merge Metadata' in scene duplicate checker tool | C01 Video container support | §5.2 |
| stash#3382 | Expose Named Capture Groups in Scene filename parser | C01 Video container support | §5.2 |
| stash#3384 | Ungreedy Repetitions in Scene filename parser | C41 Studio model & extras | §8.9 |
| stash#3400 | Rewamp tagging system to support tag attributes | C49 Tag groups & attributes | §9.4 |
| stash#3412 | Streamline the scene merging process in scene duplicate checker | C15 Scanner throughput | §6.1 |
| stash#3422 | Ability to upload and set scene cover from remote server | C03 Image & gallery sets | §5.3 |
| stash#3426 | Anamorphic videos previews are not normalized | C01 Video container support | §5.2 |
| stash#3431 | Boolean Filter Wrappers | C46 Filter UI redesign | §9.1 |
| stash#3450 | Ability to use relative dates for date-specific filters | C46 Filter UI redesign | §9.1 |
| stash#3468 | Return more search results and restructure results presentation in performer tagger | C03 Image & gallery sets | §5.3 |
| stash#3469 | Add the ability to group Tags into Tag Groups | C39 Alternate titles | §8.7 |
| stash#3478 | Recent Activity sorts for Performers, Studios, and Tags | C03 Image & gallery sets | §5.3 |
| stash#3481 | Import tags from sidecar text files | C07 Text, story posts, links | §5.7 |
| stash#3485 | Perform database integrity check on startup | C03 Image & gallery sets | §5.3 |
| stash#3486 | Option to performer scheduled automatic database backups | C50 Folder-like organization | §9.5 |
| stash#3496 | Adding Hashes to Galleries for Potential Stash-Box Integration | C03 Image & gallery sets | §5.3 |
| stash#3505 | Similar Performers Tab | C52 Similar-item detection | §9.7 |
| stash#3529 | Create new object for characters | C86 Accessibility of metadata entry | §15.6 |
| stash#3530 | Support multiple scenes in a single file | C02 Multi-scene single file | §5.2 |
| stash#3531 | Exclude organized scenes from scene duplicate checker | C83 Duplicate-entity hygiene | §15.3 |
| stash#3573 | Exclude directories in scene duplicate checker | C46 Filter UI redesign | §9.1 |
| stash#3602 | Add scene field to track uncredited performers | C03 Image & gallery sets | §5.3 |
| stash#3617 | Ability to Manually Link Performers and show direct link to their profiles | C22 Performer identity merge/split | §7.2 |
| stash#3625 | Support funscripts for Kiiroo interactive toys | C06 Funscript & interactive | §5.6 |
| stash#3637 | Add Aspect Ratio to scene File Info tab | C46 Filter UI redesign | §9.1 |
| stash#3651 | Configurable browser path and CLI arguments in Desktop Integration | C01 Video container support | §5.2 |
| stash#3655 | Option to display tag image next to the tag name in card popovers | C03 Image & gallery sets | §5.3 |
| stash#3664 | Long filenames cause cryptic deletion errors | C81 Tag/field naming consistency | §15.1 |
| stash#3685 | Custom menu items for saved scene/marker filters | C11 Markers & chapters | §5.11 |
| stash#3692 | Improve log settings | C33 Typed field voting | §8.1 |
| stash#3693 | Smart Tags (Organized Saved Filters) | C46 Filter UI redesign | §9.1 |
| stash#3694 | Allow creating `New` objects from subpages | C86 Accessibility of metadata entry | §15.6 |
| stash#3700 | 'Add to Gallery' Option for Selected Images | C03 Image & gallery sets | §5.3 |
| stash#3711 | Replace Tags field dropdown with Checkbox Tree | C11 Markers & chapters | §5.11 |
| stash#3722 | pHash Improvement for Short Durations | C03 Image & gallery sets | §5.3 |
| stash#3734 | Setting to replace scene cover with sprites used for scene scrubber  | C54 Preview & sprite pipeline | §10.1 |
| stash#3738 | Update "Updated At" date when funscript is attached to the scene | C06 Funscript & interactive | §5.6 |
| stash#3741 | Scene detail screen loads full size thumbnails of all items in the queue | C03 Image & gallery sets | §5.3 |
| stash#3749 | .forceGallery metadata & include scenes on scan | C03 Image & gallery sets | §5.3 |
| stash#3750 | Ability to set viewport dimensions for CDP scraper | C67 Plugin & scraper SDK | §11.4 |
| stash#3751 | Option to set default gender for new performer | C26 Performer field model | §7.6 |
| stash#3769 | Auto Tag might be sped up by keeping tags in RAM | C30 Stage/alias naming with studio | §7.10 |
| stash#3773 | Persistent settings/toggles via cookies | C03 Image & gallery sets | §5.3 |
| stash#3781 | Display filtered object counts in subpages | C03 Image & gallery sets | §5.3 |
| stash#3790 | Update Filters in Search View, Add IsHaving along with isMissing and make it a checkbox  | C46 Filter UI redesign | §9.1 |
| stash#3809 | Distribute as Flatpak on Linux | C76 Native install path | §12.4 |
| stash#3819 | Make lightbox slideshow options aware of image clips that have duration | C03 Image & gallery sets | §5.3 |
| stash#3824 | Display most common scene tags per performer | C03 Image & gallery sets | §5.3 |
| stash#3825 | Scene aliases for Performers "Jane Doe as Jane" | C30 Stage/alias naming with studio | §7.10 |
| stash#3837 | Expand `VR tag` settings option to support multiple tags | C77 Reverse proxy & TLS | §12.5 |
| stash#3849 | Wrong order of images in galleries on identical files | C03 Image & gallery sets | §5.3 |
| stash#3855 | Change O-Counter value via keybaord shortcut during video playback | C03 Image & gallery sets | §5.3 |
| stash#3859 | New Scene View Mode TikTok style | C01 Video container support | §5.2 |
| stash#3861 | Localization setting to display units in either metric, imperial units, or both | C26 Performer field model | §7.6 |
| stash#3866 | User configurable keyboard shortcuts for VideoJS | C01 Video container support | §5.2 |
| stash#3867 | Option to search subfolders when generating galleries | C03 Image & gallery sets | §5.3 |
| stash#3869 | Use videojs-vr player by default in VR tagged scenes & default videojs-vr rotation | C64 Player & subtitle UX | §11.1 |
| stash#3871 | Ability to add custom country/state codes for Country field | C82 Country & locale reference data | §15.2 |
| stash#3875 | Extract and display embedded subtitles | C10 Subtitles & captions | §5.10 |
| stash#3917 | Include short "Scenes" (pre-clipped files) in the Marker browser by tag | C11 Markers & chapters | §5.11 |
| stash#3921 | Gallery update when galleries change from ZIP based to folder based | C03 Image & gallery sets | §5.3 |
| stash#3923 | Ability to play and navigate markers from a playlist | C11 Markers & chapters | §5.11 |
| stash#3949 | Add a flag to hide individual images on specific galleries | C03 Image & gallery sets | §5.3 |
| stash#3950 | Hide header and footer inside lightbox by default | C03 Image & gallery sets | §5.3 |
| stash#3957 | Ability to merge details on individual fields in Scene Scrape Results dialog | C78 Observability & logging | §12.6 |
| stash#3958 | Add gallery tagger view mode | C03 Image & gallery sets | §5.3 |
| stash#3961 | Ability to configure default tab for each object page | C03 Image & gallery sets | §5.3 |
| stash#3962 | Ability to favorite scenes, images and galleries | C03 Image & gallery sets | §5.3 |
| stash#3967 | Display zoom level percentage inside a lightbox | C03 Image & gallery sets | §5.3 |
| stash#3994 | Filter for Is Image Clip: True/False | C03 Image & gallery sets | §5.3 |
| stash#4002 | Associate audio files used by e-stim toys to matching scenes | C04 Audio & music | §5.4 |
| stash#4019 | Sync or Export/Import across devices | C40 Ratings & recommendations | §8.8 |
| stash#4041 | Checking for StashID in scene duplicate checker | C46 Filter UI redesign | §9.1 |
| stash#4042 | Allow selective tasks input to parse delineated list of directories | C15 Scanner throughput | §6.1 |
| stash#4053 | Support TIF/TIFF image format | C03 Image & gallery sets | §5.3 |
| stash#4067 | Ability to generate scene markers immediately after creation | C11 Markers & chapters | §5.11 |
| stash#4070 | Ability to scan image clips from archives | C03 Image & gallery sets | §5.3 |
| stash#4077 | Filter menu: Add rename option; Consolidate icon buttons into vertical overflow menu | C46 Filter UI redesign | §9.1 |
| stash#4080 | Run tasks from system tray | C17 Background job engine | §6.3 |
| stash#4083 | AHash fingerprint support | C04 Audio & music | §5.4 |
| stash#4084 | Show visual indication if scene tagger returns multiple results | C62 Card & profile detail | §10.9 |
| stash#4089 | Allow scraping scene index (Group Scene Number) by scene scrapers | C32 Group appearance credit | §7.12 |
| stash#4115 | Support HEIC image format | C03 Image & gallery sets | §5.3 |
| stash#4136 | Can't cast any video to Chromecast | C01 Video container support | §5.2 |
| stash#4155 | DLNA lists for extended rating system | C40 Ratings & recommendations | §8.8 |
| stash#4160 | Pass more metadata for sceneByFragment scrapers | C03 Image & gallery sets | §5.3 |
| stash#4163 | Deleting a duplicated gallery makes all images of the remaining gallery disappear | C03 Image & gallery sets | §5.3 |
| stash#4167 | Support multiple studio codes | C03 Image & gallery sets | §5.3 |
| stash#4168 | Allow user to define a custom start/end time per scene | C11 Markers & chapters | §5.11 |
| stash#4174 | Filter to exclude by duration in scene duplicate checker | C46 Filter UI redesign | §9.1 |
| stash#4175 | Ability to change default live transcode method | C01 Video container support | §5.2 |
| stash#4207 | Ability to move queued tasks up/down to change priority | C17 Background job engine | §6.3 |
| stash#4219 | Add/expose `ID` tags on HTML elements | C03 Image & gallery sets | §5.3 |
| stash#4221 | Make internal scene ID a searchable field | C46 Filter UI redesign | §9.1 |
| stash#4231 | Set performer image from existing images | C03 Image & gallery sets | §5.3 |
| stash#4233 | Rotation information is ignored for determining if a video is portrait | C01 Video container support | §5.2 |
| stash#4239 | Make "Scrape With" (on scene page) function identically to "Scene Tagger" | C52 Similar-item detection | §9.7 |
| stash#4274 | Add dedicated page that tracks Watch History | C07 Text, story posts, links | §5.7 |
| stash#4300 | Docker container overhaul | C01 Video container support | §5.2 |
| stash#4306 | Allow user to define TagFilterType for various input forms | C03 Image & gallery sets | §5.3 |
| stash#4318 | Scene gallery view | C02 Multi-scene single file | §5.2 |
| stash#4326 | Ability to browse related content during video playback without leaving fullscreen mode | C01 Video container support | §5.2 |
| stash#4332 | Make "exclusions" regex field with more user friendly UI | C77 Reverse proxy & TLS | §12.5 |
| stash#4336 | Renaming and presentation of all tagging/scraper components and related labels | C73 Open API & SDK | §13.3 |
| stash#4351 | Add transformational filters to images | C03 Image & gallery sets | §5.3 |
| stash#4353 | Auto-save metadata fields that already include a confirmation | C63 Confirmation on cancel | §10.10 |
| stash#4366 | Run backup as a queued task | C17 Background job engine | §6.3 |
| stash#4383 | Ability to filter Groups by Alias | C30 Stage/alias naming with studio | §7.10 |
| stash#4384 | Multiple image support for tags | C03 Image & gallery sets | §5.3 |
| stash#4409 | Hardlink Duplicates in scene duplicate checker | C20 Storage accounting | §6.6 |
| stash#4415 | Scrapers that build a queryURL | C41 Studio model & extras | §8.9 |
| stash#4418 | Plugin page disclaimer | C25 Self-service performer claim | §7.5 |
| stash#4433 | Ability to set settings in scraper yaml configuration file | C67 Plugin & scraper SDK | §11.4 |
| stash#4457 | Scene Tagger regex specify replacement instead of space in blacklist field | C47 Full-text search breadth | §9.2 |
| stash#4458 | Scene Tagger blacklist regex enhancements | C47 Full-text search breadth | §9.2 |
| stash#4465 | Task Queue Improvements | C11 Markers & chapters | §5.11 |
| stash#4467 | Ability to scrape scene markers | C09 Scene extras & related clips | §5.9 |
| stash#4499 | Sort performers by play duration | C62 Card & profile detail | §10.9 |
| stash#4505 | Improved Tag Selection inside dropdowns | C01 Video container support | §5.2 |
| stash#4510 | Suggestions for UI Plugin API improvements | C33 Typed field voting | §8.1 |
| stash#4513 | Ability to sort and/or filter library list | C15 Scanner throughput | §6.1 |
| stash#4523 | Add new field to support performer role in relation to the scene | C69 Multi-user & permissions | §12.1 |
| stash#4536 | Casting the video never loads or plays while the casting is activated | C01 Video container support | §5.2 |
| stash#4549 | Video controls and filters do not reset on queue change | C01 Video container support | §5.2 |
| stash#4556 | Prevent installing multiple themes simultaneously  | C17 Background job engine | §6.3 |
| stash#4560 | Blob remains in use and prevents performer image from being replaced/deleted | C03 Image & gallery sets | §5.3 |
| stash#4586 | Save default caption settings / Language rulesets | C10 Subtitles & captions | §5.10 |
| stash#4589 | Custom label for captions/subtitles | C10 Subtitles & captions | §5.10 |
| stash#4594 | Ability to define Clips/Sub-Scenes from Scenes | C52 Similar-item detection | §9.7 |
| stash#4619 | Tags Page > Tag Item > Additional Performers (Not named like that) Nav Item for Performe | C86 Accessibility of metadata entry | §15.6 |
| stash#4640 | Add Manga (2-image) layout option to Image lightbox | C05 Comics / manga / doujin | §5.5 |
| stash#4642 | Allow playback of secondary files | C20 Storage accounting | §6.6 |
| stash#4647 | Option to delete linked objects | C03 Image & gallery sets | §5.3 |
| stash#4651 | Global search bar | C44 Content filtering & consent tiers | §8.12 |
| stash#4656 | Support for galleryByQueryFragment for image galleries | C03 Image & gallery sets | §5.3 |
| stash#4667 | Popovers may appear outside the viewport | C73 Open API & SDK | §13.3 |
| stash#4673 | Add data-values attributes to div.scene-specs-overlay span elements; add span.overlay-du | C01 Video container support | §5.2 |
| stash#4680 | O-Counter history for Images and Performers | C07 Text, story posts, links | §5.7 |
| stash#4698 | Track and display statistics about scene duplicate checker actions | C20 Storage accounting | §6.6 |
| stash#4739 | Inherit tag images from tagged images | C03 Image & gallery sets | §5.3 |
| stash#4746 | Mark Studio As Network | C41 Studio model & extras | §8.9 |
| stash#4756 | Ability to merge all the scenes with the same Stash ID in Scene Duplicate Checker | C01 Video container support | §5.2 |
| stash#4771 | Ability to set subtitle offset | C10 Subtitles & captions | §5.10 |
| stash#4779 | Add more id and data attributes | C61 Theming & accessibility | §10.8 |
| stash#4815 | Aliases not changed on performer scrape before alias uniqueness is tested | C30 Stage/alias naming with studio | §7.10 |
| stash#4816 | Add play count to scene cards | C01 Video container support | §5.2 |
| stash#4823 | Add full list of editable fields in the edit modal for images | C03 Image & gallery sets | §5.3 |
| stash#4827 | Add Filter by Tag in scene duplicate checker tool | C20 Storage accounting | §6.6 |
| stash#4831 | Breast/Cup Size Sort on Performers Page | C62 Card & profile detail | §10.9 |
| stash#4834 | Editable saved filter names | C46 Filter UI redesign | §9.1 |
| stash#4853 | Ability to increase O-Count on all images inside a gallery with a single click | C05 Comics / manga / doujin | §5.5 |
| stash#4860 | Add collapsible elements to identify modal | C03 Image & gallery sets | §5.3 |
| stash#4916 | Ability to configure displayed tabs in Scene page | C52 Similar-item detection | §9.7 |
| stash#4917 | Support for file size scene filter | C46 Filter UI redesign | §9.1 |
| stash#4919 | Create/link ScrapedStudio Parent Studio on Scrape | C41 Studio model & extras | §8.9 |
| stash#4933 | Show related scenes directly on the Appears With page instead of just performers | C69 Multi-user & permissions | §12.1 |
| stash#4950 | Add Custom Image Numbering and Sorting for Galleries | C03 Image & gallery sets | §5.3 |
| stash#4970 | Add Organized flag to all objects | C03 Image & gallery sets | §5.3 |
| stash#4985 | Ability to search/query the text of caption files | C10 Subtitles & captions | §5.10 |
| stash#4995 | Ability to Upload Videos/Images from the GraphQL API | C03 Image & gallery sets | §5.3 |
| stash#4998 | Expand hook context to determine source | C67 Plugin & scraper SDK | §11.4 |
| stash#5002 | Plugin settings UI/UX | C67 Plugin & scraper SDK | §11.4 |
| stash#5033 | Scene Tagger/Scrape with.../Scrape query for stash-box parses comma separated aliases in | C30 Stage/alias naming with studio | §7.10 |
| stash#5036 | Rescanning windows on *nix paths breaks galleries | C03 Image & gallery sets | §5.3 |
| stash#5067 | Option to select highest bitrate in scene duplicate checker | C01 Video container support | §5.2 |
| stash#5089 | Copy existing markers on newly created scene from split | C11 Markers & chapters | §5.11 |
| stash#5094 | Multiple image support for all objects using corresponding user-defined image attributes | C03 Image & gallery sets | §5.3 |
| stash#5101 | Support for min/max stroke length slider for Handy integration | X05 Upstream tracker | §15.11 |
| stash#5107 | Ability to limit Auto Tag scraper in scene tagger to only apply to specific objects | C41 Studio model & extras | §8.9 |
| stash#5111 | GIF files in ZIP archives are images vs video | C03 Image & gallery sets | §5.3 |
| stash#5118 | Plugin Services available in Stash->Services tab | C67 Plugin & scraper SDK | §11.4 |
| stash#5144 | Add option to generate/scrub waveforms and display them alongside the player | C06 Funscript & interactive | §5.6 |
| stash#5159 | Support multiple filters of the same type | C03 Image & gallery sets | §5.3 |
| stash#5178 | A>B Loop Controls do not work on Apple Touch Devices | C01 Video container support | §5.2 |
| stash#5185 | Treat videos in folders that have `.forcegallery` as image clips | C03 Image & gallery sets | §5.3 |
| stash#5193 | Make O-Counter on Performer Card clickable | C62 Card & profile detail | §10.9 |
| stash#5210 | Support non-unique studio names | C41 Studio model & extras | §8.9 |
| stash#5237 | Naming issue for Taiwan in list of countries | C81 Tag/field naming consistency | §15.1 |
| stash#5238 | Sort URLs alphabetically with optional ability manually re-order | C30 Stage/alias naming with studio | §7.10 |
| stash#5273 | Enhancements to the Tags System, File Details and Settings | C33 Typed field voting | §8.1 |
| stash#5275 | Option to flatten VR scene preview and markers previews | C11 Markers & chapters | §5.11 |
| stash#5312 | Adding crop & pan for video filters | C03 Image & gallery sets | §5.3 |
| stash#5313 | Support deinterlacing during playback | C01 Video container support | §5.2 |
| stash#5317 | Sometimes images are placed outside the viewport in lightbox on Android | X01 Windows-only requirement | §15.11 |
| stash#5326 | Add icon to performer profile/card to indicate performer has passed away | C29 Performer death/status markers | §7.9 |
| stash#5329 | Improving the merge modal window layout | C85 Documentation & onboarding | §15.5 |
| stash#5336 | Edit Modal - Improving bulk editing workflow | C74 Import / export / backup | §13.4 |
| stash#5352 | Option to show full size images in gallery preview scrubber | C03 Image & gallery sets | §5.3 |
| stash#5364 | Show O-Count on gallery cards (using sum of image O-Counts) | C03 Image & gallery sets | §5.3 |
| stash#5384 | Ability to add images and galleries to groups | C03 Image & gallery sets | §5.3 |
| stash#5390 | Regenerate scene markers and scrubber sprites after merging | C11 Markers & chapters | §5.11 |
| stash#5394 | Add "Include sub-group content" toggle on groups page in scenes tab | C33 Typed field voting | §8.1 |
| stash#5397 | Middle Align scene_index on scene cards | C03 Image & gallery sets | §5.3 |
| stash#5399 | Ability to add icons/favicons for performer links | C03 Image & gallery sets | §5.3 |
| stash#5400 | Open performer link directly if it only has single link of that type | C45 Funder / subscription linking | §8.13 |
| stash#5412 | Scene Duplicate Checker report | C20 Storage accounting | §6.6 |
| stash#5419 | Ability to set marker offset  | C11 Markers & chapters | §5.11 |
| stash#5420 | Expose scene subtitles over DLNA | C10 Subtitles & captions | §5.10 |
| stash#5430 | Count Sub-tag scenes in the Tag View when "Display subtag content" is turned on | C78 Observability & logging | §12.6 |
| stash#5434 | Add icon/field to performer profile/card to indicate performer activity status | C03 Image & gallery sets | §5.3 |
| stash#5444 | Add flag to scenes to exclude those performers from "Appear With" tab | C86 Accessibility of metadata entry | §15.6 |
| stash#5448 | Live marker preview | C11 Markers & chapters | §5.11 |
| stash#5450 | Watchlist queue | C17 Background job engine | §6.3 |
| stash#5460 | Ability to Pin/Favorite scrapers | C67 Plugin & scraper SDK | §11.4 |
| stash#5468 | Option to link galleries to scenes if they are in the same folder | C03 Image & gallery sets | §5.3 |
| stash#5498 | Add IMPORT option to import tags, galleries, performers, and studios | C03 Image & gallery sets | §5.3 |
| stash#5500 | Ability to filter scenes by scene_index | C42 Release groups / scene groups | §8.10 |
| stash#5514 | Multi-language entries | C82 Country & locale reference data | §15.2 |
| stash#5517 | Inhibit system sleep while tasks are running | C01 Video container support | §5.2 |
| stash#5518 | Tagger view for groups with batch tasks | C49 Tag groups & attributes | §9.4 |
| stash#5532 | Main page horizontal customization | C46 Filter UI redesign | §9.1 |
| stash#5534 | Offload Thumbnail Generation to External Computer | C11 Markers & chapters | §5.11 |
| stash#5536 | Migrate Scene Previews To Its Own Directory  | C01 Video container support | §5.2 |
| stash#5538 | Performer Image Malformed via Local Image URL when API/Creds Enabled | C03 Image & gallery sets | §5.3 |
| stash#5541 | Ability to manually trigger Auto Tag on single performer alias | C03 Image & gallery sets | §5.3 |
| stash#5584 | Video / "Image Clip" controls for lightbox | C03 Image & gallery sets | §5.3 |
| stash#5587 | Setting to sort Performer Credits in TabPanel & CardPopover | C03 Image & gallery sets | §5.3 |
| stash#5593 | Support multiple aliases for groups | C30 Stage/alias naming with studio | §7.10 |
| stash#5595 | Ability to edit scene number from scene tagger on scenes sub-page | C02 Multi-scene single file | §5.2 |
| stash#5596 | Client Side Hamming Distance Settings for Identify Tasks | C81 Tag/field naming consistency | §15.1 |
| stash#5601 | Add Bluesky Icon for performer links | C45 Funder / subscription linking | §8.13 |
| stash#5612 | Create a sharable link to give temporary access to a scene | C50 Folder-like organization | §9.5 |
| stash#5616 | Implement ratings keyboard shortcut for images in lightbox mode | C40 Ratings & recommendations | §8.8 |
| stash#5625 | Centralized Automated Update Checker | C67 Plugin & scraper SDK | §11.4 |
| stash#5626 | Main Nav Bar Notification System | C67 Plugin & scraper SDK | §11.4 |
| stash#5627 | Ability to Merge existing Performer aliases in Performer Scrape Results modal | C30 Stage/alias naming with studio | §7.10 |
| stash#5631 | `moveFiles` should optionally clean up empty directories | C16 Incremental scan correctness | §6.2 |
| stash#5643 | Ability to report false-positive fingerprint back to stash-box instance | C23 Performer fingerprints | §7.3 |
| stash#5646 | Add new /temp/ Application Path instead of using /generated/temp/ for temporary files | C20 Storage accounting | §6.6 |
| stash#5650 | Support for funscript tokens (aka DRM) | C06 Funscript & interactive | §5.6 |
| stash#5667 | Add "Ignore Auto Tag" box to scraped performer create dialog | C41 Studio model & extras | §8.9 |
| stash#5681 | Support Hardware Acceleration (Intel Integrated Graphics) When Building Video Previews | C01 Video container support | §5.2 |
| stash#5683 | High CPU / looping read access when loading scene associated with removed external hard  | C01 Video container support | §5.2 |
| stash#5709 | Zombie process left (Python defunct) | C15 Scanner throughput | §6.1 |
| stash#5711 | Image relationships feature to allow linking related images  | C10 Subtitles & captions | §5.10 |
| stash#5731 | Hardware decoding in generation tasks | C18 Hardware decode for generation | §6.4 |
| stash#5736 | Support Handy Firmware 4 | C76 Native install path | §12.4 |
| stash#5748 | Consider career start/end dates when dispalying performer age | C03 Image & gallery sets | §5.3 |
| stash#5750 | Ability to view markers based on secondary tags | C11 Markers & chapters | §5.11 |
| stash#5758 | Scene save activity causing re-queries for plugin | C07 Text, story posts, links | §5.7 |
| stash#5762 | Configure max memory usage | C15 Scanner throughput | §6.1 |
| stash#5769 | Support hardware acceleration for phash/sprite/preview generation tasks | C15 Scanner throughput | §6.1 |
| stash#5772 | Make director/photographer field a list | C03 Image & gallery sets | §5.3 |
| stash#5783 | Marker Preview Generation Options | C11 Markers & chapters | §5.11 |
| stash#5785 | Settings option to display favourited tags first | C46 Filter UI redesign | §9.1 |
| stash#5786 | [meta] Duplicate Checker improvements | C01 Video container support | §5.2 |
| stash#5788 | [meta] Sorting Feature Requests | C40 Ratings & recommendations | §8.8 |
| stash#5792 | "Play Next" in marker view | C11 Markers & chapters | §5.11 |
| stash#5795 | Add URLs to scenes "Details" tab | C41 Studio model & extras | §8.9 |
| stash#5806 | Improve the layout for comparing existing and new values when scraping via scene tagger | C86 Accessibility of metadata entry | §15.6 |
| stash#5819 | Set scene preview from file or URL | C03 Image & gallery sets | §5.3 |
| stash#5823 | Show 100% identical duplicates on Scene Duplicate Checker | C20 Storage accounting | §6.6 |
| stash#5850 | Thumbnails generated as JPEG drop transparency resulting in black thumbnails | C03 Image & gallery sets | §5.3 |
| stash#5869 | Add gallery support to identify task | C03 Image & gallery sets | §5.3 |
| stash#5873 | Filter Markers tab by "Is missing thumbnail/preview" | C11 Markers & chapters | §5.11 |
| stash#5893 | Filter by File Modification Time for Scenes and Images | C46 Filter UI redesign | §9.1 |
| stash#5939 | Ability to run scene filename parser on selected scenes | C03 Image & gallery sets | §5.3 |
| stash#5942 | Ability to edit scene marker directly from scene card | C11 Markers & chapters | §5.11 |
| stash#5944 | Include settings when executing plugin task | C67 Plugin & scraper SDK | §11.4 |
| stash#5953 | Deleting a file while generating for it locks up scan/generate | C40 Ratings & recommendations | §8.8 |
| stash#5966 | Pattern Match Studio URLs | C33 Typed field voting | §8.1 |
| stash#5970 | GQL sorting by custom field value | C36 Leaderboards & badges | §8.4 |
| stash#5979 | Top navigation bar cut off on mobile devices with notch/floating island | C61 Theming & accessibility | §10.8 |
| stash#5987 | Upgrade React + Dependencies | C36 Leaderboards & badges | §8.4 |
| stash#5992 | Add "Add Child Tag" and "Add Parent Tag" Buttons in Single Tag View | C78 Observability & logging | §12.6 |
| stash#5998 | Ability to configure specific custom fields to be hidden from object pages | C67 Plugin & scraper SDK | §11.4 |
| stash#6008 | Support for scanning Encryped Archives | C20 Storage accounting | §6.6 |
| stash#6013 | sys.stdin.read function response is too long | C03 Image & gallery sets | §5.3 |
| stash#6045 | Ability to set external image as gallery cover | C03 Image & gallery sets | §5.3 |
| stash#6050 | Ability to start marker in AB Loop mode if it has start/stop time | C11 Markers & chapters | §5.11 |
| stash#6052 | Ability to run Bulk Auto Tag from Tags page | C74 Import / export / backup | §13.4 |
| stash#6057 | Add O-Counter to markers | C11 Markers & chapters | §5.11 |
| stash#6062 | Permit GraphQL API to update video captions | C10 Subtitles & captions | §5.10 |
| stash#6071 | Support immersive VR video from browser via WebXR API | C01 Video container support | §5.2 |
| stash#6076 | Add option to sort sub-groups by number of the sub-group item | C78 Observability & logging | §12.6 |
| stash#6078 | Add studio code field for Groups | C41 Studio model & extras | §8.9 |
| stash#6080 | Enable installation as Windows service | C78 Observability & logging | §12.6 |
| stash#6088 | [meta] O-Counter feature requests | C08 Performer interviews | §5.8 |
| stash#6112 | Support for sub-scraper post process option for scrapeJson scrapers | C47 Full-text search breadth | §9.2 |
| stash#6121 | Display resolution of scraped images across all scraper modals | C03 Image & gallery sets | §5.3 |
| stash#6134 | Add image support to identify task | C03 Image & gallery sets | §5.3 |
| stash#6183 | Redirect old scene page to merged scene page after merge | C02 Multi-scene single file | §5.2 |
| stash#6185 | Create automatic backup upon installing a new plugin to protect from malicious plugins | C63 Confirmation on cancel | §10.10 |
| stash#6193 | Ability to Skip Preview Generation based on set duration threshold | C01 Video container support | §5.2 |
| stash#6218 | Ability to re-order performers in scene cards | C26 Performer field model | §7.6 |
| stash#6231 | Add keyboard shortcut to trigger "Merge..." action from scenes page | C33 Typed field voting | §8.1 |
| stash#6246 | Scene Tagger Navbar floats out of position | C61 Theming & accessibility | §10.8 |
| stash#6254 | Inherit tag images from tagged scenes | C03 Image & gallery sets | §5.3 |
| stash#6274 | [meta] "short form" video content | C11 Markers & chapters | §5.11 |
| stash#6283 | Ability to arbitrarily group/organize scrapers | C77 Reverse proxy & TLS | §12.5 |
| stash#6311 | Multi axis Funscript and Serial Connections (OSR2+ & SR6) | C06 Funscript & interactive | §5.6 |
| stash#6335 | Allow tablet view to be full desktop view instead of mobile view | C36 Leaderboards & badges | §8.4 |
| stash#6339 | Filter for multi-axis interactive scenes | C06 Funscript & interactive | §5.6 |
| stash#6357 | Filter based on related objects | C11 Markers & chapters | §5.11 |
| stash#6360 | Ability to merge studios | C41 Studio model & extras | §8.9 |
| stash#6365 | Replace in-app manual with offline version of StashDocs | C16 Incremental scan correctness | §6.2 |
| stash#6382 | Add option to select every file by path in scene Duplicate Checker | C20 Storage accounting | §6.6 |
| stash#6383 | Make the selectable item area bigger in scene duplciate checker | C20 Storage accounting | §6.6 |
| stash#6390 | Improving pagination performance by caching results of larger queries | C15 Scanner throughput | §6.1 |
| stash#6394 | Allow scrapers to return custom fields | C86 Accessibility of metadata entry | §15.6 |
| stash#6421 | Rename o-counter concept into "Hearts" | C15 Scanner throughput | §6.1 |
| stash#6422 | Option to apply scene filters only to primary file | C03 Image & gallery sets | §5.3 |
| stash#6429 | Improve visual clarity on merge direction in scene duplicate checker | C64 Player & subtitle UX | §11.1 |
| stash#6430 | Option to merge metadata and delete files in a single task in scene Duplicate Checker | C20 Storage accounting | §6.6 |
| stash#6446 | Make thumbnail placeholders more informative by mentioning the task required to generate | C11 Markers & chapters | §5.11 |
| stash#6452 | Tagger View Jumps Position | C57 Grid/list view modes | §10.4 |
| stash#6454 | Ability to cutomize the prefix for path links on File Info tab | C01 Video container support | §5.2 |
| stash#6455 | SQL Query Improvments for Larger DB | C77 Reverse proxy & TLS | §12.5 |
| stash#6456 | bfcache not used because WebSocket connection is not closed | C73 Open API & SDK | §13.3 |
| stash#6457 | Update API to scan in file(s), add metadata on scan | C50 Folder-like organization | §9.5 |
| stash#6459 | Expand scene captions filter to support all languages | C10 Subtitles & captions | §5.10 |
| stash#6460 | Add configuration option on scene tagger to set studio code | C41 Studio model & extras | §8.9 |
| stash#6466 | Unsaved entries intermittently lost in Scene Edit Tags or Performers boxes | C01 Video container support | §5.2 |
| stash#6490 | Ability to open tag page on click instead of applying object-specific tags filter | C46 Filter UI redesign | §9.1 |
| stash#6509 | Highlight AB loop points on the scene scrubber bar | C11 Markers & chapters | §5.11 |
| stash#6516 | Add duplicated filter to galleries | C03 Image & gallery sets | §5.3 |
| stash#6526 | Player bottom controls are clipped (fullscreen missing) + menus overlap/are occluded (z- | C01 Video container support | §5.2 |
| stash#6539 | Scraper postprocess option to decode HTML entities to Unicode | C67 Plugin & scraper SDK | §11.4 |
| stash#6544 | "Group By" View mode with collapsible sections | C40 Ratings & recommendations | §8.8 |
| stash#6564 | Stash Object Sync Project | C03 Image & gallery sets | §5.3 |
| stash#6576 | Ability to filter images based on gallery rating | C03 Image & gallery sets | §5.3 |
| stash#6577 | No way to prevent .webm files from being categorized as "videos"/scenes instead of image | C01 Video container support | §5.2 |
| stash#6579 | Support funscripts for AutoBlow interactive toys | C06 Funscript & interactive | §5.6 |
| stash#6645 | Reorganise generated artifacts | C03 Image & gallery sets | §5.3 |
| stash#6657 | Ability to generate group cover from attached scenes if no image is set | C03 Image & gallery sets | §5.3 |
| stash#6668 | Sort scenes by file by creation time in filesystem | C47 Full-text search breadth | §9.2 |
| stash#6669 | Merge tags on alias collision  | C26 Performer field model | §7.6 |
| stash#6672 | X-Ray Style Performer Overlay in Fullscreen Image & Video Viewer | C03 Image & gallery sets | §5.3 |
| stash#6677 | Log date to history tab when tag was applied to an object | C07 Text, story posts, links | §5.7 |
| stash#6719 | GraphQL SFW shadowed query fields | C64 Player & subtitle UX | §11.1 |
| stash#6732 | HEIC/HEIF Image Format Support with Live Photo Pairing | C03 Image & gallery sets | §5.3 |
| stash#6744 | Option to check specific folder for subtitles | C10 Subtitles & captions | §5.10 |
| stash#6745 | Ability to link scraped studio to existing studio from scrape dialog window | C03 Image & gallery sets | §5.3 |
| stash#6781 | [epic] src/ui dependency update spree | C37 Field locking & moderation | §8.5 |
| stash#6793 | Add similar markers filters that exist on scenes | C11 Markers & chapters | §5.11 |
| stash#6795 | Create and manage Custom Fields to be used across all Scenes | C26 Performer field model | §7.6 |
| stash#6811 | Show marker ranges on scene player control bar on mobile screens | C11 Markers & chapters | §5.11 |
| stash#6814 | Queue gets stuck on fileless scenes | C01 Video container support | §5.2 |
| stash#6816 | Reduce size of the action buttons on performer page | C03 Image & gallery sets | §5.3 |
| stash#6823 | Standardize Buttons on Performers Cards | C03 Image & gallery sets | §5.3 |
| stash#6837 | Ability to skip generation tasks on scenes where it failed | C17 Background job engine | §6.3 |
| stash#6860 | Allow setting default tab on studio details page on a per-studio basis | C03 Image & gallery sets | §5.3 |
| stash#6861 | Cover Image usability improvements + metadata | C03 Image & gallery sets | §5.3 |
| stash#6864 | Move disableAnimation lightbox option from GraphQL to UI config | C36 Leaderboards & badges | §8.4 |
| stash#6866 | Dynamically hide non applicable performer fields based on gender | C26 Performer field model | §7.6 |
| stash#6871 | Add paths and behavior flags to findFolders() and findFiles() | C33 Typed field voting | §8.1 |
| stash#6873 | Add Image(s) to scrapeSingleTag | C03 Image & gallery sets | §5.3 |
| stash#6874 | safe catch/ bypass of PluginApi.patch | C67 Plugin & scraper SDK | §11.4 |
| stash#6875 | Enter troubleshooting mode via ENV/ argument | C17 Background job engine | §6.3 |
| stash#6883 | Log X-Real-IP from reverse proxy | C19 Transcode & proxy pipeline | §6.5 |
| stash#6897 | Sub-Groups without scenes not displaying initial page-load properly | C42 Release groups / scene groups | §8.10 |
| stash#6899 | Add defaults for plugin settings | C67 Plugin & scraper SDK | §11.4 |
| stash#6925 | Performer Age vs. Birthday Sort | C26 Performer field model | §7.6 |
| stash#6939 | Phash Generation task does not generate for videos classified as images in Stash | C03 Image & gallery sets | §5.3 |
| stash#6944 | Add/Remove/Reorder scenes from Group page | C42 Release groups / scene groups | §8.10 |
| stash#6945 | Add Scenes field to ScrapedGroup schema | C36 Leaderboards & badges | §8.4 |
| stash#6949 | Animated Images using VideoFile in GQL | C03 Image & gallery sets | §5.3 |
| stash#6953 | Improve dense scene marker readability | C11 Markers & chapters | §5.11 |
| stash#6955 | Settings option to enable automatic scrolling in wall view | C15 Scanner throughput | §6.1 |
| stash#6956 | Improve "Clear Date Data" popups | C07 Text, story posts, links | §5.7 |
| stash#6970 | Add IS_NULL and NOT_NULL modifiers to duration field for Group filter | C03 Image & gallery sets | §5.3 |
| stash#6978 | HEVC Video in MKV playback freezes when skipped to another part of the video on Firefox | C01 Video container support | §5.2 |
| stash#6979 | Ability to add tags to exclusion list from tags page | C15 Scanner throughput | §6.1 |
| stash#6982 | Ability to fast forward/rewind at 2x speed using long press | C01 Video container support | §5.2 |
| stash#6987 | Ability to reinstall plugin from installed plugins section | C67 Plugin & scraper SDK | §11.4 |
| stash#7007 | Official Docker image with hardware acceleration support | C03 Image & gallery sets | §5.3 |
| stash#7013 | Avoid unnecessary cover image updates when re-scraping scenes | C03 Image & gallery sets | §5.3 |
| stash#7020 | Image view counter | C03 Image & gallery sets | §5.3 |
| stash#7028 | Inconsistent File Info shortcut on gallery/image pages conflicts with Add Filter shortcu | C03 Image & gallery sets | §5.3 |
| stash#7036 | Add "Primary Source" as default/top of list in scrape dialog | C03 Image & gallery sets | §5.3 |
| stash#7052 | Link O history to multiple scenes (O Assists) | C02 Multi-scene single file | §5.2 |
| stash#7058 | Do not pre-select scene match when multiple scenes are found in scene tagger | C02 Multi-scene single file | §5.2 |
| stash#7068 | Secondary sort issue when sorting by descending date | C03 Image & gallery sets | §5.3 |
| stash#7071 | Add VR specific fields to `VideoFile` | C47 Full-text search breadth | §9.2 |
| stash#7086 | Add `{inputName}` placeholder for searchByName scrapers | C46 Filter UI redesign | §9.1 |
| stash#7106 | Deleting an image in a zip gallery silently fails | C03 Image & gallery sets | §5.3 |
| stash#7118 | Duration filtering for scene identification | C46 Filter UI redesign | §9.1 |
| stash#7130 | App not responding on RClone drive - "Loading ..." forever | C63 Confirmation on cancel | §10.10 |
| stash#7132 | Performer evolution | C26 Performer field model | §7.6 |
| stash#7133 | Using "excludes" or "is not" with the disambiguation filter hides non-disambiguated perf | C22 Performer identity merge/split | §7.2 |
| stash#7134 | Watch Later | C40 Ratings & recommendations | §8.8 |
| stash#7135 | Unmentioned/bugged max password length | C63 Confirmation on cancel | §10.10 |
| stash#7136 | JSON based scrapers fail with Chrome CDP | C63 Confirmation on cancel | §10.10 |
| stash#7139 | Lack of right-click paste option in multi-edit boxes like Tags, Performers, Groups in  E | C49 Tag groups & attributes | §9.4 |
| stash#7142 | Scroll position is lost when navigating back to a list | C57 Grid/list view modes | §10.4 |
| stash#7145 | Remote image downloader sends no User-Agent, causing HTTP 418 | C03 Image & gallery sets | §5.3 |
| stash#7147 | Fast dragging motions in the lightbox/image-viewer navigate to the next/previous image | C03 Image & gallery sets | §5.3 |
| stash#7148 | Drags that overshoot the image boundaries result in the lightbox/image-viewer being dism | C03 Image & gallery sets | §5.3 |
| stash#7149 | Panning images in lightbox with the mouse wheel can result in navigation to the next/pre | C03 Image & gallery sets | §5.3 |
| stash#7152 | Studio Tagger batch update panics when scraped studio has nil StoredID | C36 Leaderboards & badges | §8.4 |
| stash#7154 | Lightbox back button behavior cause unsaved modal to show | C07 Text, story posts, links | §5.7 |
| stash#7155 | Stale sprite/preview/cover/transcode after a same path file content change | C01 Video container support | §5.2 |
| stash#7157 | Option to play next scene after deleting | C50 Folder-like organization | §9.5 |
| stash#7160 | [UI/UX] Organized icon state is hard to distinguish on scene detail pages | C29 Performer death/status markers | §7.9 |
| stash#7165 | Plugin settings should be able to add connect-src CSP sources | C33 Typed field voting | §8.1 |
| stash#7173 | Generate failing make previews | C01 Video container support | §5.2 |
| stash#7175 | Ability to attach performer to a group | C41 Studio model & extras | §8.9 |
| stash#7179 | `.nogallery` does not remove an existing folder-based gallery during Clean | C11 Markers & chapters | §5.11 |
| stash#7187 | No way to return "No images found" from scraper script? | C63 Confirmation on cancel | §10.10 |
| stash#7192 | Option to disable automatic file hash merging for Images/Galleries and allow duplicate f | C03 Image & gallery sets | §5.3 |
| stash#7194 | Show free/available disk space on the Statistics page | C20 Storage accounting | §6.6 |
| stash#7197 | Add plugin media-src CSP support | C67 Plugin & scraper SDK | §11.4 |
| stash#7198 | Updating scrapers does not install new requirements | C63 Confirmation on cancel | §10.10 |
| stash#7200 | Warn about overlapping URL patterns in scrapers | C03 Image & gallery sets | §5.3 |
| stash#7202 | Tagger should not cut off vertical cover images (for the local scene) | C03 Image & gallery sets | §5.3 |
| stash#7204 | Add Sex/Genitals Field for Performers | C26 Performer field model | §7.6 |
| stash#7209 | Freeones: `Could not parse career length 2016-now` | C63 Confirmation on cancel | §10.10 |
| stash#7212 | `UNIQUE constraint failed` errors during scene identify | C01 Video container support | §5.2 |
| stash#7216 | Windows FFmpeg download uses the "essentials" build, which has no libdav1d (AV1 decoding | C15 Scanner throughput | §6.1 |
| stash#7217 | Scene preview videos play with 20-30s delay / choppy in Firefox on Linux — correlates wi | C54 Preview & sprite pipeline | §10.1 |
| stash#7222 | Studios page extremely slow on large image libraries | C03 Image & gallery sets | §5.3 |
| stash#7228 | Image scrapers should be able to return a `Galleries` field | C03 Image & gallery sets | §5.3 |
| stash#7229 | Preview generation fails when video stream has a non-zero start offset | C01 Video container support | §5.2 |
| stash#7230 | Display ZIP compression method in gallery file info, and optionally allow converting a g | C03 Image & gallery sets | §5.3 |
| stash#7231 | Can't scrape any male or trans performers using freeones and all other community scraper | C47 Full-text search breadth | §9.2 |
| stash#7234 | Details for Performers being shown below Picture on Safari | C03 Image & gallery sets | §5.3 |
| stash#7236 | Heavy load time on Safari for Scene pages | C15 Scanner throughput | §6.1 |
| stash#7238 | paths.funscript should use signed URLs when authentication is enabled | C10 Subtitles & captions | §5.10 |
| stash#7239 | Hardware transcode: [InitHWSupport] Supported HW codecs [0] gives no actionable reason w | C01 Video container support | §5.2 |
| stash#7240 | [security] Zip-Slip arbitrary file write in import and package install | C63 Confirmation on cancel | §10.10 |
| stash#7247 | VR videos get super bright and washed out when played in VR mode | C01 Video container support | §5.2 |
| stash#7250 | Use AI technology to tag videos, marking different sexual positions and scenarios. | C26 Performer field model | §7.6 |
| stash#7253 | Add Kiswahili (Swahili, sw-KE) language translation | C54 Preview & sprite pipeline | §10.1 |
| stash#7256 | Input file buttons in firefox unresponsive | C03 Image & gallery sets | §5.3 |
| stash#7258 | Improve scraper discoverability | C41 Studio model & extras | §8.9 |
| stash-box#6 | Traefik Reverse Proxy for Dev instance | C19 Transcode & proxy pipeline | §6.5 |
| stash-box#9 | [Bug Report] Fields updated to NULL are ignored | C57 Grid/list view modes | §10.4 |
| stash-box#62 | [Feature] Share Markers via Stash-Box | C11 Markers & chapters | §5.11 |
| stash-box#69 | [Feature] Changelog on StashDB for bulk activity and site/network additions | C38 Edit history & rollbacks | §8.6 |
| stash-box#75 | [Feature] search for duration | C41 Studio model & extras | §8.9 |
| stash-box#92 | [Feature] Studio Logo's/Images | C03 Image & gallery sets | §5.3 |
| stash-box#95 | [Feature] Query URL/Request Scrape | C17 Background job engine | §6.3 |
| stash-box#100 | [RFC] More user roles | C23 Performer fingerprints | §7.3 |
| stash-box#116 | [RFC] Scene import process | C33 Typed field voting | §8.1 |
| stash-box#161 | [Feature] Add the posibility to order performer images | C03 Image & gallery sets | §5.3 |
| stash-box#203 | [Feature] Add image categorization | C03 Image & gallery sets | §5.3 |
| stash-box#205 | [Feature] Add performer shoe size | C86 Accessibility of metadata entry | §15.6 |
| stash-box#206 | [Feature] Add penis size, for male and trans performers | C26 Performer field model | §7.6 |
| stash-box#210 | [Feature] Make the hair color field a multi-value field | C27 Multi-valued attributes | §7.7 |
| stash-box#211 | [Feature] Include image guidelines directly on Stashbox | C03 Image & gallery sets | §5.3 |
| stash-box#212 | [Feature] Add cropperjs to the add image section | C03 Image & gallery sets | §5.3 |
| stash-box#213 | [Feature] Lock fields | C17 Background job engine | §6.3 |
| stash-box#226 | [Feature] Amending edits | C48 Smart search & typo tolerance | §9.3 |
| stash-box#234 | [Feature] Multiple-select field values (breast type, ethnicity, nationality, etc) | C26 Performer field model | §7.6 |
| stash-box#237 | [RFC] Performer Image Categorization | C03 Image & gallery sets | §5.3 |
| stash-box#279 | [Feature] Ability to upload multiple images for studios | C03 Image & gallery sets | §5.3 |
| stash-box#283 | [Feature] Add the ability to search through edits | C26 Performer field model | §7.6 |
| stash-box#299 | [Feature] Make it possible to mark performers as ambiguous | C22 Performer identity merge/split | §7.2 |
| stash-box#303 | [Feature] Disallow the submission of thumbnails generated by stash | C54 Preview & sprite pipeline | §10.1 |
| stash-box#304 | [Feature] Ability to remove incorrect fingerprints from scenes | C23 Performer fingerprints | §7.3 |
| stash-box#318 | Update performer unique name constraint | C22 Performer identity merge/split | §7.2 |
| stash-box#334 | [Feature] dvd cover art archive  | C43 User-created lists | §8.11 |
| stash-box#335 | [Feature] Performer Alias Disambiguation | C22 Performer identity merge/split | §7.2 |
| stash-box#336 | [Feature] Allow a pending performer to be added to a scene (placeholder) | C86 Accessibility of metadata entry | §15.6 |
| stash-box#337 | [Bug] Scene edits from favorited networks don't appear in the Edits view | C41 Studio model & extras | §8.9 |
| stash-box#338 | [RFC] Collect more info with file hashes (fingerprints) | C01 Video container support | §5.2 |
| stash-box#342 | [Feature] Cameo/NonSex tag or list for performers in scene | C09 Scene extras & related clips | §5.9 |
| stash-box#345 | [Feature] Alternate Titles | C39 Alternate titles | §8.7 |
| stash-box#364 | [Feature] List of filters for a user | C46 Filter UI redesign | §9.1 |
| stash-box#417 | [Feature] Gamification/badges | C36 Leaderboards & badges | §8.4 |
| stash-box#431 | [Feature] Bulk tag input on scene form | C30 Stage/alias naming with studio | §7.10 |
| stash-box#436 | [Feature] Image voting system | C03 Image & gallery sets | §5.3 |
| stash-box#440 | [Feature] Automatically share database through torrent | C69 Multi-user & permissions | §12.1 |
| stash-box#458 | [Feature] Add 'Add to Collection/Wantlist' and Collection/Wantlist viewing functionality | C50 Folder-like organization | §9.5 |
| stash-box#459 | [Feature] Movie section functionality on StashDB? | C47 Full-text search breadth | §9.2 |
| stash-box#472 | [Feature] Add `ARCHITECTURE.md` | C33 Typed field voting | §8.1 |
| stash-box#474 | [Feature] Rating and recommendation system | C40 Ratings & recommendations | §8.8 |
| stash-box#476 | [Feature] Adding fingerprints manually | C23 Performer fingerprints | §7.3 |
| stash-box#477 | [Feature] Differentiate files | C23 Performer fingerprints | §7.3 |
| stash-box#488 | [Feature] Add more performer filters | C26 Performer field model | §7.6 |
| stash-box#494 | [Feature] change edit default order | C33 Typed field voting | §8.1 |
| stash-box#497 | [Feature] Set Organised Status | C03 Image & gallery sets | §5.3 |
| stash-box#525 | [Bug Report] Attempting to edit an edit with a deleted image, crashes | C03 Image & gallery sets | §5.3 |
| stash-box#530 | [Feature] Other hash types | C23 Performer fingerprints | §7.3 |
| stash-box#540 | [Feature] Make images optional for each instance | C73 Open API & SDK | §13.3 |
| stash-box#541 | [Feature] List view | C57 Grid/list view modes | §10.4 |
| stash-box#542 | [Feature] Associate duplicate and non-duplicate scenes. | C46 Filter UI redesign | §9.1 |
| stash-box#549 | [Feature] Allow scene without studio. | C33 Typed field voting | §8.1 |
| stash-box#550 | [Feature] Allow scene without date | C33 Typed field voting | §8.1 |
| stash-box#551 | [Feature] Configuration to set the number of invite keys | C69 Multi-user & permissions | §12.1 |
| stash-box#552 | [Feature] Performer modification history | C09 Scene extras & related clips | §5.9 |
| stash-box#553 | [Feature] Some additional performer data fields | C26 Performer field model | §7.6 |
| stash-box#559 | [Feature] Similar Performer Recommendations | C40 Ratings & recommendations | §8.8 |
| stash-box#560 | [Feature] Give a measure of relevance to the tags of a scene | C33 Typed field voting | §8.1 |
| stash-box#569 | [Feature] Quest / Bounty Gamification | C33 Typed field voting | §8.1 |
| stash-box#570 | [Feature] Allow editing closed submissions. | C33 Typed field voting | §8.1 |
| stash-box#574 | [Feature] Comment section for each Performer, Studio, Tag, and Scene or a basic forum wi | C41 Studio model & extras | §8.9 |
| stash-box#583 | [Bug Report] Password length limit | C70 Auth hardening | §12.2 |
| stash-box#585 | [Feature] Submit a compilation scene | C42 Release groups / scene groups | §8.10 |
| stash-box#586 | [Feature] Submit tags specific for each file | C46 Filter UI redesign | §9.1 |
| stash-box#592 | [Bug Report] Replacing Performer in Scene is Added Instead | C79 Consent & takedown pipeline | §14.1 |
| stash-box#594 | [Feature] Confirmation on cancel | C63 Confirmation on cancel | §10.10 |
| stash-box#595 | [Feature] Leaderboard | C33 Typed field voting | §8.1 |
| stash-box#596 | [Feature] Awards Database | C43 User-created lists | §8.11 |
| stash-box#599 | [Feature] Allow other users to modify an edit | C33 Typed field voting | §8.1 |
| stash-box#600 | [Feature] Reward points based on the points gained by people invited | C36 Leaderboards & badges | §8.4 |
| stash-box#605 | [Bug Report] Downscaled Studio Logos Lose Transparency | C03 Image & gallery sets | §5.3 |
| stash-box#610 | [Feature] Select primary name from list of aliases | C30 Stage/alias naming with studio | §7.10 |
| stash-box#612 | [Feature] Add user toggle to blur images | C03 Image & gallery sets | §5.3 |
| stash-box#619 | [Feature] Make Career start and Career end automatic | C26 Performer field model | §7.6 |
| stash-box#621 | [Bug Report] "Failed to load edits." after deleting a 'site' that is used in a pending e | C79 Consent & takedown pipeline | §14.1 |
| stash-box#624 | [Feature] Sort scenes by duration | C42 Release groups / scene groups | §8.10 |
| stash-box#627 | [Feature] Edit Dependencies or Multi Edit Submissions | C33 Typed field voting | §8.1 |
| stash-box#628 | [Feature] Filter in GraphQL Last Updated | C09 Scene extras & related clips | §5.9 |
| stash-box#630 | [RFC] Add Trusted Users with Double Votes | C33 Typed field voting | §8.1 |
| stash-box#633 | [RFC] Anonymize Fingerprints for User Privacy | C23 Performer fingerprints | §7.3 |
| stash-box#637 | [RFC] Adapt for hentai/animated content | C86 Accessibility of metadata entry | §15.6 |
| stash-box#641 | [Feature] Add studio level tags which are automatically applied to scenes | C36 Leaderboards & badges | §8.4 |
| stash-box#643 | Allow Users to Hide Specific Content (e.g. Gay, Straight, Trans, BDSM) when using Stash- | C44 Content filtering & consent tiers | §8.12 |
| stash-box#648 | [Feature] Alternate Scene Covers | C03 Image & gallery sets | §5.3 |
| stash-box#649 | [Bug Report] Bad behaviour when image backend or image location not specified | C03 Image & gallery sets | §5.3 |
| stash-box#651 | [Feature] Administrative section for blacklisting domains from link fields | C46 Filter UI redesign | §9.1 |
| stash-box#655 | [RFC] Logging improvements | C78 Observability & logging | §12.6 |
| stash-box#656 | [Feature] Ability to remove info from edit history and report button | C07 Text, story posts, links | §5.7 |
| stash-box#660 | [Bug Report] Limit Field Lengths on Form Fields | C77 Reverse proxy & TLS | §12.5 |
| stash-box#661 | [Feature] Flag Links as Archives | C41 Studio model & extras | §8.9 |
| stash-box#663 | [RFC] Release Groups / Scene Groups | C04 Audio & music | §5.4 |
| stash-box#664 | [Feature] Default Ordering of Links | C74 Import / export / backup | §13.4 |
| stash-box#665 | [Feature] Increase margin between URL favicons | C61 Theming & accessibility | §10.8 |
| stash-box#666 | [Feature] Visually stack URL icons from the same domain/category to reduce overcrowding | C62 Card & profile detail | §10.9 |
| stash-box#676 | [Feature] Add more info fields to studios | C41 Studio model & extras | §8.9 |
| stash-box#700 | [Feature] Pinned comments | C41 Studio model & extras | §8.9 |
| stash-box#703 | [Bug Report] Unable to Update Merges | C63 Confirmation on cancel | §10.10 |
| stash-box#713 | [Feature] Import scenes from stash-box to stash. | C74 Import / export / backup | §13.4 |
| stash-box#714 | [Feature] Warn about entity name collisions before submitting edits | C40 Ratings & recommendations | §8.8 |
| stash-box#722 | [Feature Request] Automatic Image Tagging | C03 Image & gallery sets | §5.3 |
| stash-box#726 | [Feature] Add validation for name field against aliases | C03 Image & gallery sets | §5.3 |
| stash-box#727 | [Bug Report] GQL imageCreate schema still accepts `url` | C28 Performer image sets & categories | §7.8 |
| stash-box#729 | [Bug Report] nil pointer dererence on SceneEditUpdate mutation | C36 Leaderboards & badges | §8.4 |
| stash-box#733 | [Feature] Block content on website by tags, studios, stars, gender... | C26 Performer field model | §7.6 |
| stash-box#734 | [Bug Report] SMTP over TLS is unsupported | C77 Reverse proxy & TLS | §12.5 |
| stash-box#738 | Submitting draft with existing image causes pq error | C03 Image & gallery sets | §5.3 |
| stash-box#742 | [Feature] Filter Performer's scenes by alias | C30 Stage/alias naming with studio | §7.10 |
| stash-box#743 | [RFC] Improved edit count calculation; current method is flawed | C36 Leaderboards & badges | §8.4 |
| stash-box#745 | [Feature] Modifiable/Expanded Image Upload Extensions | C03 Image & gallery sets | §5.3 |
| stash-box#760 | [RFC] Automate performer disambiguation | C03 Image & gallery sets | §5.3 |
| stash-box#771 | [Feature] Make UI more mobile-friendly / less fixed-width | C70 Auth hardening | §12.2 |
| stash-box#772 | [Feature] Validate user hasn't forgotten to click 'add' for a link | C41 Studio model & extras | §8.9 |
| stash-box#778 | [Bug Report] Commas in Aliases split the alias on call to ScrapeSinglePerformer | C30 Stage/alias naming with studio | §7.10 |
| stash-box#782 | [Feature] Limit pending edits per user | C17 Background job engine | §6.3 |
| stash-box#783 | [Feature] Links to related scenes | C02 Multi-scene single file | §5.2 |
| stash-box#784 | Add cut/uncut and penis length to stastdh | C26 Performer field model | §7.6 |
| stash-box#787 | [Feature] Exclude (= ignore) studios and performers | C41 Studio model & extras | §8.9 |
| stash-box#790 | [Feature] Performer tags | C26 Performer field model | §7.6 |
| stash-box#794 | [Feature] Studio page performer tab, add a "view more" scene card for each performer | C41 Studio model & extras | §8.9 |
| stash-box#802 | [Bug Report] Removing category is not a valid change | C79 Consent & takedown pipeline | §14.1 |
| stash-box#804 | [Feature] Filter Performer's scenes by alias | C30 Stage/alias naming with studio | §7.10 |
| stash-box#808 | [Feature] Add support for specific UK countries | C82 Country & locale reference data | §15.2 |
| stash-box#809 | [Bug Report] Certain characters break password reset | C30 Stage/alias naming with studio | §7.10 |
| stash-box#814 | [RFC] Scenes with 100's of bad phashes keep growing faster without stopping | C23 Performer fingerprints | §7.3 |
| stash-box#815 | [Feature] Submitter User Rating Display | C33 Typed field voting | §8.1 |
| stash-box#818 | [RFC] Correlate aliases to studios | C30 Stage/alias naming with studio | §7.10 |
| stash-box#820 | [Feature] Studio ownership history and studio code format | C07 Text, story posts, links | §5.7 |
| stash-box#822 | [Feature] Performer page notification badge/banner for duplicate performer based on URL  | C22 Performer identity merge/split | §7.2 |
| stash-box#823 | [Feature] Custom fields for all objects | C41 Studio model & extras | §8.9 |
| stash-box#829 | [Bug Report] Some Performer Filter Criterions Do Not Work | C46 Filter UI redesign | §9.1 |
| stash-box#832 | [Feature] Groups in stash-box | C86 Accessibility of metadata entry | §15.6 |
| stash-box#834 | [Feature] User-Created Lists | C36 Leaderboards & badges | §8.4 |
| stash-box#846 | [RFC] Streamlining Support for Nameless Performers | C77 Reverse proxy & TLS | §12.5 |
| stash-box#848 | [RFC] More Accurate Tags by Using Tag Namespaces/Sources | C33 Typed field voting | §8.1 |
| stash-box#850 | [Feature] Add Vectors To DB for Identifying performers | C52 Similar-item detection | §9.7 |
| stash-box#851 | [Feature] Funscript metadata | C06 Funscript & interactive | §5.6 |
| stash-box#867 | [Feature] Add backend support for AutoCrop for automated headshots | C33 Typed field voting | §8.1 |
| stash-box#870 | [Feature] findUpdatedScenes | C70 Auth hardening | §12.2 |
| stash-box#879 | [Bug Report] Deleted fields in edits are reset when updating the edit | C26 Performer field model | §7.6 |
| stash-box#916 | [RFC] Notification Enhancements | C33 Typed field voting | §8.1 |
| stash-box#926 | [Feature] [My Fingerprints] Filter for entries with more than 1 value per hash | C03 Image & gallery sets | §5.3 |
| stash-box#929 | [Feature] Remember draft entries when leaving or refreshing the page. | C86 Accessibility of metadata entry | §15.6 |
| stash-box#930 | [Feature] Sort performer scenes based on scene length | C54 Preview & sprite pipeline | §10.1 |
| stash-box#935 | [docker] tagged releases | C75 Docker & compose | §12.3 |
| stash-box#938 | [Feature] Ability to filter "My fingerprints" by submission date | C23 Performer fingerprints | §7.3 |
| stash-box#939 | [Feature] Fingerprints submitted / "Owned" filter on various pages | C23 Performer fingerprints | §7.3 |
| stash-box#941 | [Bug Report] voting yes after voting no should clear notification | C33 Typed field voting | §8.1 |
| stash-box#943 | [Bug Report] Edits are not updated when entities are merged | C36 Leaderboards & badges | §8.4 |
| stash-box#948 | [Bug Report] Some images do not get saved | C03 Image & gallery sets | §5.3 |
| stash-box#950 | [Bug Report] No warning when creating a performer with same name+disambiguation | C22 Performer identity merge/split | §7.2 |
| stash-box#953 | [Feature] SSO/discourse account linking | C62 Card & profile detail | §10.9 |
| stash-box#954 | [Feature] Browser notification support | C73 Open API & SDK | §13.3 |
| stash-box#956 | [Bug Report] registration form still requiring invite key with with require_invite false | X01 Windows-only requirement | §15.11 |
| stash-box#969 | [Feature] Sort list of tags by scene count (and display count next to each) | C46 Filter UI redesign | §9.1 |
| stash-box#973 | [Bug Report] Valid URL is not accepted | C25 Self-service performer claim | §7.5 |
| stash-box#974 | [Bug Report] Network page lists scenes but no performers | C41 Studio model & extras | §8.9 |
| stash-box#986 | [Feature] Filter by multiple tags | C03 Image & gallery sets | §5.3 |
| stash-box#1005 | [Feature] Filter by tag exclusion | C46 Filter UI redesign | §9.1 |
| stash-box#1007 | [Bug Report] Studio Tagger ignores duplicate names + still matches deleted studios | C41 Studio model & extras | §8.9 |
| stash-box#1023 | [Feature] Stashbox statistics page | C41 Studio model & extras | §8.9 |
| stash-box#1060 | [Bug Report] Fingerprint edit filter masked by favorites | C23 Performer fingerprints | §7.3 |
| stash-box#1090 | Import scenes from stash-box to stash | C74 Import / export / backup | §13.4 |
| stash-box#1115 | [RFC] Removing OSHASH | C23 Performer fingerprints | §7.3 |
| stash-box#1141 | [Feature] Additional metadata for male performers | C11 Markers & chapters | §5.11 |
| stash-box#1161 | [Feature] Ability to mark performer link as defunct | C86 Accessibility of metadata entry | §15.6 |
| stash-box#1175 | [Feature] Optional flag to exclude specific studios from accepting scenes | C41 Studio model & extras | §8.9 |
| stash-box#1177 | [Bug Report] Fingerprint cluster view hash list population error | C23 Performer fingerprints | §7.3 |
| stash-box#1178 | [Feature] Add "Trailer" / "abridged" scene type | C09 Scene extras & related clips | §5.9 |
| stash-box#1179 | [Feature] Stash-box gallery fingerprint searching | C03 Image & gallery sets | §5.3 |
| stash-box#1194 | [Feature] (De-)Localize links / language | C63 Confirmation on cancel | §10.10 |
| stash-box#1197 | [Feature] Copy StashID to Clipboard button for scenes and performers | C23 Performer fingerprints | §7.3 |
| stash-box#1205 | [Bug Report] Low quality Performer image prioritization | C03 Image & gallery sets | §5.3 |
| stash-box#1244 | [RFC] Performer submission form regrouping and decluttering | C26 Performer field model | §7.6 |
| stash-box#1276 | [Feature] Adding popularity fields to performers and scenes | C23 Performer fingerprints | §7.3 |
| stash-box#1277 | [Bug Report] Unclear error message for email cooldown | C79 Consent & takedown pipeline | §14.1 |
| stash-box#1279 | [RFC] Studio Founding and Closure Dates + Defunct Status | C33 Typed field voting | §8.1 |

## A.1 Issues resolved by the same underlying fix

Distinct issues frequently collapse into one implementation. Recorded so the
matrix is honest that a single change closes many rows. Every reference below
is verified open at the time of writing.

| Capability | Closes | The one fix that does it |
|---|---|---|
| C15 Scanner throughput | #2824, #5683, #6455, #6390, #7130 | inotify watcher + content-hash change detection; one bounded worker pool with a single job type per file; `UNIQUE (object_type, object_id, path)`; per-library scan checkpoints. Remote mounts (rclone/NFS) fall back to a polled full scan, because a watcher cannot be trusted there. |
| C15 Artifact invalidation | #7155, #2773 | Every generated artifact is keyed on `(file_id, mtime, size, generator_version)`. A file replaced at the same path invalidates its own sprites, previews and hashes, and nothing else. |
| C20 Storage accounting | #7194, #5646, #819, #894 | **Met** (§6.6, T-P2-006). `files` carries all three sizes with the basis named; a per-library rollup reports physical total, hardlink savings and per-file detail; free/available disk space via `statvfs`; the temp root is configurable and refuses any path under `generated/`; ffmpeg encoder, quality and thread count are configurable per format. |
| C17 Job queue durability | #5709, #6814, #2913, #1445 | Process supervision with zombie reaping, a per-item skip list so one poisoned file cannot loop the queue, and resume-after-crash from the persisted job table. |
| C18 Memory ceilings | #5762 | A configurable memory ceiling the thumbnail and sprite workers respect via a token-bucket semaphore: the last in-flight frame is dropped and retried later rather than OOM-killing the process. |
| C54 Image pipeline | #3038, #5850, #1585 | Generate WebP/AVIF with alpha preserved and served progressively; `generated/` splits into `cache/` → `cache/objects/`. Stash's JPEG transparency loss is a symptom of the wrong intermediate format, not a separate bug. |
| C56 Identify via image | #5869, #6134 | Identify and generate tasks consult the same artifact cache as the UI, so "is this generated?" is one query for API, GraphQL and UI, and an image or gallery can seed a match the same way a video does. |
| C52 Similar-item search | #1220 | Perceptual-hash nearest-neighbour search reuses the same ANN index as the face embeddings, with a configurable Hamming threshold. |
| C03 Gallery archive handling | #7230 | Archive reading reports its compression method, supports 7z/gz/tar.gz, and converts on request rather than only at import. |
| C83 Studio hygiene | #6360, #1007 | Studio merge with reference re-pointing, and matching that skips tombstoned studios — the current tagger matches deleted rows. |
| C33 Field history integrity | #743, #9 | One edit engine for all objects: votes are recomputed from the accepted-edit set rather than a running counter, so merged and deleted-field edits stop corrupting scores. Handles null-vs-unset as distinct states. |
| C30 Alias integrity | #726, #335, #318, #778, #5033, #2293 | Aliases are first-class rows with a scoped uniqueness rule, a selected primary name, and a parser that never splits a comma inside an alias or drops non-ASCII names during auto-tag. |

## A.2 Per-capability issue counts

| Capability | Count | Capability | Count | Capability | Count |
|---|---|---|---|---|---|
| | C01 Video container support | | 62 | | C02 Multi-scene single file | | 8 | | C03 Image & gallery sets | | 174 |
| | C04 Audio & music | | 7 | | C05 Comics / manga / doujin | | 3 | | C06 Funscript & interactive | | 12 |
| | C07 Text, story posts, links | | 11 | | C08 Performer interviews | | 1 | | C09 Scene extras & related clips | | 11 |
| | C10 Subtitles & captions | | 13 | | C11 Markers & chapters | | 43 | | C15 Scanner throughput | | 15 |
| | C16 Incremental scan correctness | | 6 | | C17 Background job engine | | 14 | | C18 Hardware decode for generation | | 1 |
| | C19 Transcode & proxy pipeline | | 3 | | C20 Storage accounting | | 12 | | C22 Performer identity merge/split | | 8 |
| | C23 Performer fingerprints | | 18 | | C25 Self-service performer claim | | 2 | | C26 Performer field model | | 22 |
| | C27 Multi-valued attributes | | 1 | | C28 Performer image sets & categories | | 1 | | C29 Performer death/status markers | | 2 |
| | C30 Stage/alias naming with studio | | 21 | | C32 Group appearance credit | | 2 | | C33 Typed field voting | | 37 |
| | C34 Candidate generation | | 1 | | C36 Leaderboards & badges | | 20 | | C37 Field locking & moderation | | 1 |
| | C38 Edit history & rollbacks | | 2 | | C39 Alternate titles | | 4 | | C40 Ratings & recommendations | | 19 |
| | C41 Studio model & extras | | 42 | | C42 Release groups / scene groups | | 6 | | C43 User-created lists | | 2 |
| | C44 Content filtering & consent tiers | | 4 | | C45 Funder / subscription linking | | 2 | | C46 Filter UI redesign | | 35 |
| | C47 Full-text search breadth | | 10 | | C48 Smart search & typo tolerance | | 1 | | C49 Tag groups & attributes | | 5 |
| | C50 Folder-like organization | | 12 | | C52 Similar-item detection | | 8 | | C54 Preview & sprite pipeline | | 8 |
| | C57 Grid/list view modes | | 7 | | C60 Keyboard shortcuts & power use | | 2 | | C61 Theming & accessibility | | 6 |
| | C62 Card & profile detail | | 7 | | C63 Confirmation on cancel | | 13 | | C64 Player & subtitle UX | | 5 |
| | C67 Plugin & scraper SDK | | 23 | | C69 Multi-user & permissions | | 6 | | C70 Auth hardening | | 3 |
| | C73 Open API & SDK | | 6 | | C74 Import / export / backup | | 10 | | C75 Docker & compose | | 1 |
| | C76 Native install path | | 3 | | C77 Reverse proxy & TLS | | 14 | | C78 Observability & logging | | 11 |
| | C79 Consent & takedown pipeline | | 6 | | C81 Tag/field naming consistency | | 3 | | C82 Country & locale reference data | | 4 |
| | C83 Duplicate-entity hygiene | | 2 | | C85 Documentation & onboarding | | 4 | | C86 Accessibility of metadata entry | | 17 |
| | C87 Fingerprint privacy | | 1 | | C90 Deep-linkable URLs | | 1 | | X01 Windows-only requirement | | 2 |
| | X05 Upstream tracker | | 1 | |   | |   | |   | |   |

---

*End of spec. 850 open issues mapped, 91 capabilities, 3 documented
non-goals, 2 closed-but-adopted features (stash #2792 §12.1.1 and its
`YurikaL` P2P comment §5.18), one of them delivered as a one-click plugin
(§5.18.1).*
