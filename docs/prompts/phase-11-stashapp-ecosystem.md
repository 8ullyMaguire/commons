# Phase 11 — the stashapp ecosystem, adapted

**The prompt.** This file is the brief an agent executes. It is written to be
self-contained: an agent with no memory of this conversation should be able to
work through it top to bottom.

**Status:** not started. Nothing in this phase is implemented.

**Precondition:** the rest of the project is complete and verified. This phase is
**last** — see §0.

**Scope note:** the organisation holds **14** public repositories. Two are the
work; the other twelve are read, consumed, or explicitly out of scope, each with
a reason in §5. The first draft of this brief listed thirteen, because a GitHub
*search* for the org missed `website` — the authoritative listing is
`gh api orgs/stashapp/repos`, and that is what the counts below come from.

---

## 0. Why this phase is last, and what "last" means

The owner's instruction: *every repo in `https://github.com/stashapp/`, and
specially `CommunityScripts` and `CommunityScrapers`, adapted to our
implementation — and only after everything else is complete.*

Two things follow from that, and they are not the same requirement:

**Ordering.** Nothing in this phase may start until the rest of the project is
implemented and verified. The hard dependency inside the phase is `commons-api`,
which does not exist yet — T-P11-007 is last even within the phase.

**Not a fork.** The aim is that the ecosystem *works here*, not that Commons
becomes a drop-in stash. Where stash's semantics and ours differ outright — no
DHT, no auto-download, a different consent model — the difference is **reported**,
not papered over. A scraper marked "working" must have actually produced a result
in this codebase; a compatibility claim nobody executed is worse than no claim.

---

## 1. The scope, measured rather than assumed

Every number below came from the GitHub trees API on 2026-09-27, not from the
project's own earlier notes. Upstream pushes daily, so **re-measure before
trusting any of it** — and that is now mechanical rather than a matter of
discipline:

```
./scripts/measure-ecosystem.py            # print
./scripts/measure-ecosystem.py --write    # rewrite docs/ecosystem-2026-09-27.json
./scripts/measure-ecosystem.py --check    # fail if stale — runs in verify.sh
```

The fixture is `docs/ecosystem-2026-09-27.json`, and `verify.sh` runs `--check`,
so the documents cannot silently drift from upstream. This is a *staleness* check
rather than a correctness one: the numbers *will* change, and the only thing that
must not happen is the prose disagreeing with reality. T-P11-001 turns the fixture
into assertions against the live trees.

That script earned its place by being wrong first: it reported 231 YAML scrapers
and a confident 484-file total before the layout trap in §1.1 was understood.

| Repository | State | What it is |
|---|---|---|
| `stashapp/CommunityScrapers` | active, AGPL-3.0 | 981 files under `scrapers/`: **727 YAML**, 163 Python, 91 other (66 `.md`, 7 `.rb`); 9 `py_common`; 19 in `lib/` |
| `stashapp/CommunityScripts` | active, AGPL-3.0 | 462 plugin files across **79 plugin directories** (95 Python, 84 `.md`, 79 `.yml`, 58 `.js`, 55 `.css`); **11 theme directories**, 53 theme files, 28 CSS; 2 userscripts; 19 archived |
| `stashapp/stash-box` | active, TypeScript/Go | The metadata + perceptual-hash API. **A service, not an artifact** — see §5 |
| `stashapp/stash` | active, Go, 13k★ | The reference implementation. Not a target |
| `stashapp/website`, `StashDB-Docs`, `.github` | active, dormant | Marketing, docs, org defaults. Out of scope |
| `stashapp/Stash-Docs` | active | Community documentation. Documentation, not code |
| `stashapp/StashServer`, `StashFrontend`, `StashOSX` | **archived**, pre-2019 | Ruby / TS / Swift servers and clients. Out of scope — see §5 |
| `stashapp/plugins-repo-template`, `scrapers-repo-template` | active | Source-index templates. Consumed by T-P11-001's index format |
| `stashapp/metadata-api-discuss` | dormant since 2019 | Issue threads. Historical context only |

**Licensing is a hard constraint, not a note.** Both artifact repositories are
AGPL-3.0. Consequences, all of them deliberate:

- **Nothing is vendored.** No scraper, plugin, theme or userscript source is
  copied into this repository. Every artifact is fetched at install time, pinned
  by commit, and cached outside the tree. AGPL content in-tree would make the
  whole workspace AGPL.
- A vendored copy is also stale the moment upstream pushes — which is daily.
- The compatibility **report** is generated from execution, so it can talk about
  artifacts it does not contain.

---

## 2. The three decisions this phase rests on

These were decided when the phase was planned, and the reasoning is the part that
matters — a later agent who disagrees should be able to see *why*, not just *what*.

### 1.1 `scrapers/` is TWO layouts, and assuming either one loses most of the corpus

Worth stating before any ticket touches the tree, because it is the kind of
structural fact that produces a confident wrong count rather than an error:

- **497** scrapers are flat: `scrapers/<Site>.yml`
- **232** are grouped: `scrapers/<Site>/<Site>.yml`, across 217 site directories

A glob of `scrapers/*.yml` reads 497 and misses a third. A recursive walk with a
fixed depth assumption is worse — the first version of the measurement script in
this repository reported **231** YAML scrapers and a confident 484-file total,
because it required two segments below the prefix. Nothing errored.

So: count at "one or more segments below `scrapers/`", never at a fixed depth, and
make the count a test. `scripts/measure-ecosystem.py` is the script that got this
wrong first and is the one to keep.

### 2.1 The YAML scrapers get an interpreter, not a translator

727 declarative definitions, each a program in a small language: an entry-point
table, XPath and JSON selector languages, and a `postProcess` chain
(`replace` / `parseDate` / `truncate` / `map` / `dateFormat` / `switch` /
`filter` / `setDefault`).

A per-file converter must track every upstream construct forever, and it breaks
on the first scraper that uses something the converter did not know about. An
interpreter is written once and tracks the language.

**The cost, stated honestly:** our `xPathScrapers` / `jsonScrapers` /
`postProcess` semantics must be *superset-compatible* reimplementations. Every
divergence is a named, tested, reported difference — not a silent difference.

### 2.2 The Python scrapers and plugins get a compatibility layer, not a rewrite

`py_common` is a real library with a real API. Reimplementing 163 working Python
programs in Rust is a different project with a worse success rate.

So `py_common` ships as a shim over Commons' own host API, and a scraper that
needs a capability Commons lacks is **reported as incompatible at load time**,
naming the capability — not discovered halfway through a scrape, which is how a
library fills up with half-populated records.

### 2.3 Nothing is vendored, ever

See §1. This is also §0.1 rule 6 of the implementation plan.

---

## 3. The seven tickets, in order

Full text, with per-ticket accept criteria, is in
`docs/plans/implementation-plan.md` under `## T-P11-001`. This is the sequence
and the intent; the plan is the authority on detail.

| # | Ticket | What it delivers | Depends on |
|---|---|---|---|
| 1 | **T-P11-001** — Upstream catalogue and pin | A client for the two repos' GitHub APIs. Clones nothing, executes nothing. Builds a catalogue of `(repo, ref, path, kind, declared requirements)`; pins artifacts to exact commit SHAs. | — |
| 2 | **T-P11-002** — Declarative scraper interpreter | Parses a stash scraper YAML and evaluates it: entry-point table, XPath and JSON selectors, the `postProcess` chain. Selectors evaluate against `lxml`-shaped results via `quick-xml` plus an HTML5 tree — **not** against a browser. | 1 |
| 3 | **T-P11-003** — `py_common` compatibility layer | Embedded Python runtime with `py_common` and its `util` / `cache` / `config` / `deps` / `graphql` modules reimplemented over `HostApi`. Standard argv/JSON-stdout scraper protocol. | 1 |
| 4 | **T-P11-004** — Registration, selection, `AyloAPI` | Registry, URL→scraper matching, fragment matching, search path, and the proposal pipeline into Phase 4's `FieldProposal`. `AyloAPI.scrape` is a façade. | 2, 3 |
| 5 | **T-P11-005** — Plugin and theme compatibility | The 79 plugin directories and 11 theme directories. The YAML *is* the manifest — there is no `plugin.json` in the whole repository; the id is the directory name. `exec` is a `ProcessSpawn` capability **refused by default** under `HostPolicy::first_party`. | 1, 3 |
| 6 | **T-P11-006** — Divergent-behaviour report | `docs/ECOSYSTEM-COMPATIBILITY.md`: what each artifact does, whether it works here, and if not, which Commons feature is missing. **Generated, not hand-written**, regenerated in CI. | 2, 3, 4, 5 |
| 7 | **T-P11-007** — Userscripts and the stash GraphQL surface | The 2 userscripts and any plugin talking stash's GraphQL API, mapped onto Commons' API. A query either resolves or is **reported as unsupported by name**. | `commons-api` |

---

## 4. How to know each ticket is done

Each ticket in the plan carries an explicit `**Accept:**` and `**Done when:**`.
Three of them are the ones worth restating, because they are the difference
between a working phase and an impressive-looking one.

**T-P11-001 — the counts are a test, not a comment.**
A fixture of both trees, and a test that fails loudly if upstream has diverged.
"727 YAML scrapers" written in a comment is a number that was true once; the same
number as an assertion is a number somebody maintains. Pinning is by SHA, and a
test asserts a SHA-pinned fetch is byte-identical across two runs.

**T-P11-002 — coverage, asserted non-growing.**
Every construct in the 727-file corpus is either implemented or recorded in
`unimplemented.yaml` with a count. A differential test runs the interpreter over
real scrapers and asserts a **structurally valid** result object. It does *not*
assert equality with stash — stash's Go implementation is the reference, and a
byte-comparison against it tests their code, not ours. The unimplemented list is
asserted non-growing across a fixture sweep, so it cannot quietly grow.

**T-P11-003 — incompatibility is reported at load.**
A `py_common` compatibility test against **upstream's own test vectors**, so the
shim's semantics are pinned to theirs and not to ours. And a test asserting that a
scraper calling an unimplemented function fails **at load, naming the function**.
The alternative is a scrape that produces half a result and reports success.

**T-P11-005 — the capability refusal names the capability.**
Installing a plugin without granting `ProcessSpawn` fails with a reason naming it.
Plus a themesheet test: a theme's CSS is scoped to the theme and does not leak
into the base stylesheet.

**T-P11-006 — the report is generated and checked in CI.**
A test fails if regenerating `docs/ECOSYSTEM-COMPATIBILITY.md` produces a diff
that is not committed. A hand-maintained compatibility table is a table that
quietly becomes false.

---

## 5. The other repositories, and why they are not all work

The owner said *every* repo. Three of them are not adaptation work, and saying so
explicitly is better than silently skipping them or pretending to do them.

**`stashapp/stash-box` — a service, not an artifact.** It is a Go/TypeScript
metadata and perceptual-hash API. It appears 249 times in the spec already,
mostly as the *source of the data model* (`FieldProposal`, fingerprints, entity
schemas). The correct relationship is: Commons is a **client** of stash-box's
public API, consuming fingerprints and metadata proposals, and T-P11-007's GraphQL
surface is where a stash-shaped client is mapped onto ours. There is nothing to
"adapt" in the sense of making its code run here — it is a remote dependency.
Concretely: implement the client, not a port. If a later reading wants
stash-box's *schemas* mirrored, that is a spec question (C36), not a Phase 11
ticket, and it should be raised rather than assumed.

**`stashapp/Stash-Docs` — documentation.** Useful as a **behavioural
specification** when a scraper or a plugin's expected semantics are ambiguous:
it describes what the user is supposed to see. Read it; do not port it. One
useful concrete use: when the interpreter's semantics for a construct are
underdetermined, the docs are the tie-breaker between two defensible readings.

**`StashServer`, `StashFrontend`, `StashOSX` — archived, pre-2019.** Ruby, TypeScript
and Swift implementations of stash from before the Go rewrite. They are dead code
upstream. There is nothing to adapt and nothing to learn from that the current Go
implementation does not already model. Explicitly out of scope.

**`plugins-repo-template`, `scrapers-repo-template` — consumed, not adapted.**
They define the *source index* format that scraper and plugin authors publish
against. T-P11-001's catalogue should read that format, so that a third-party
source index is a working input rather than something that needs a bespoke
adapter. Small, but it is the difference between "we support the two official
repos" and "we support how these ecosystems actually distribute".

**`metadata-api-discuss` — dormant since 2019.** Issue threads, not archived —
the plan's first table called it archived, which it is not. It is historical
context for *why* stash-box's API is shaped the way it is. Read when a schema
decision is ambiguous; never a dependency.

**`website`, `StashDB-Docs`, `.github` — out of scope.** A marketing site, a
docs site, and an org defaults repository. Nothing to adapt.

---

## 6. Working agreement for whoever executes this

Carried forward from the phases that worked, because each one was learned by
getting it wrong:

**A test that states the answer, not that something happened.** Write the
assertion a user would check by looking. "Something happened" assertions pass on a
broken implementation and are the reason a green suite coexisted with a broken
setup path more than once in this project.

**Mutation-check the logic, not just cover it.** A surviving mutant is a claim the
tests do not pin. `scripts/mutate-*.mjs` in this repo do this, and `verify.sh`
now parse-checks them — a mutation script that fails to parse reads as "found
nothing" rather than "never ran".

**A blank page with HTTP 200 and a happy build is a real failure mode here.** It
has happened three times in this project: a `{@const}` inside a nested `each`, a
renamed loop variable, and a store getter read in a `$derived`. When something
renders empty, probe the console before reading the code.

**Pin wire shapes from the authoritative side.** The Phase 11 filters and
capability manifests are wire contracts. Where our code has to match an external
shape, emit the expected value as data from the language that owns it and assert
against it in both directions — which is what
`crates/commons-store/tests/filter_wire_shape.rs` does for serde, and what the
Media view does for `ObjectKind::as_str`.

**Report what is missing.** An artifact that cannot run here is reported with the
missing capability named. Inventing a substitute that diverges from upstream is
worse than reporting the gap: the next upstream push fixes theirs and not ours.

**Commit per ticket, tag per ticket, and do not batch the phase.** The milestone
tags are `phase-11-00N-<name>`, and the phase closes with `phase-11-complete`.
Update `docs/HANDOFF.md` and `README.md` on each, with measured numbers.

**Never vendor.** If a diff would add upstream source to this tree, the approach
is wrong, not the exception.

---

## 7. Definition of done for Phase 11

- All seven tickets implemented, no stubs, each with its `**Accept:**` criterion
  satisfied and its test present.
- `docs/ECOSYSTEM-COMPATIBILITY.md` generated from execution, checked in, and
  regenerated in CI with the diff check passing.
- The catalogue fixture re-measured against live upstream, with the count
  assertions current.
- No upstream source vendored; every fetch pinned by commit and reproducible.
- `scripts/verify.sh` green: `cargo fmt`, `cargo clippy -D warnings`, the full
  Rust suite, the UI build, the UI unit suite, and Playwright.
- `docs/HANDOFF.md` and `README.md` updated with **measured** counts, not
  remembered ones.
- `phase-11-complete` tagged and pushed.

And the honest end state, which is what §0 asks for: a person who already uses
stash's scrapers and plugins can point Commons at their library and have most of
it work, with a written, generated list of exactly what does not and why.
