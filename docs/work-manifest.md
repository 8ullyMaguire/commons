# Work manifest v1

**Why this exists.** The fiction estate (darkhold, lorehaven, gravity cards, ficnexus exports, the AO3
mirror) each holds a version of "a work", so every import, dedupe and tag-normalisation bug is fixed three
times. A shared *database* would be worse — four products, four release cadences. A shared **format** is
the cheap version of the same win: anyone can produce it, anyone can consume it, nobody has to migrate.

**What it is.** A JSON document describing a work: title, authors, chapters, tags, warnings, series
position, provenance, and a `content_hash` per chapter. It carries **metadata, never text**. That
restriction is the whole safety argument: a manifest can be federated, mailed or committed without
distributing somebody's work.

**What it is not.** Not a database, not a transport, not a recommendation format. Not canonical: source
tags pass through verbatim, and normalisation is the consumer's problem (AO3's tag namespace is the
practical reference, and the mirror already has it).

## Rules

1. **`tags` must not be lossy.** Store what the source said. A consumer that wants canonical tags maps
   them itself and keeps the original alongside.
2. **`content_hash` is how two parties agree without exchanging anything.** sha256 of extracted plain
   text, per chapter. Same hash = same chapter; no hash = unknown, never "probably the same".
3. **`extra` is where disagreements live.** Anything source-specific goes there; consumers must ignore
   what they cannot parse. Adding a field to the top level is a version bump.
4. **`manifest_version` is checked first.** A consumer that does not know the version refuses, loudly.
5. **Rights note, not rights claim.** `source.rights_note` records what the producer believes about
   permission. It is a note to a human, never an authorisation to a machine.

## Producers and consumers today

| side | who | what they do |
|---|---|---|
| producer | `python/fic-crawler`, AO3 mirror exports | describe crawled works |
| producer | `ficnexus` (post-2026-10-28, export-only) | describe what it just exported |
| consumer | `lorehaven` / `darkhold` | import a work, skip chapters whose hash it already has |
| consumer | `gravity` | render a work card without hosting text |
| consumer | `kindred` | seeds for recommendations by work id |

## Versioning

`manifest_version: "1"`. Additive fields go inside `extra`; anything else bumps to `"2"` and the schema
ships beside it. Schema: `work-manifest.schema.json` in this directory.
