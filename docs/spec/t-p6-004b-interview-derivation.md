---
title: T-P6-004b — Interview derivation: quotes, topics, speaker attribution
date: 2026-09-28
status: proposed
ticket: T-P6-004b
parent: T-P6-004
---

# T-P6-004b — Interview derivation: quotes, topics, speaker attribution

**Status: not started.** Written after T-P6-004's stated "Done when: both
exist" was met and verified (both accept criteria pass), so this is the
remainder the plan names rather than a re-opening of the ticket.

**Parent spec:** `docs/spec/t-p6-004-interviews.md` — read **§3.4** (what gets
derived, and how) and **§5** (the five ways this is wrong) before implementing.
**Parent commit:** `phase-7-050-interview-routes` (`07619b0`).

---

## 1. Why this is a separate ticket

T-P6-004's two accept criteria are met: word timings monotonic and within
200 ms of a hand-checked transcript (`asr_timing`, 7 tests), and a correction
becoming a `FieldProposal` with the model's words intact (`interview_db`, 24
tests). What remains is **derivation** — turning a transcript into the things
a person actually wants from an interview — and none of it is on the path the
ticket was scoped to prove. Three independent pieces, three different failure
modes, one data source.

Building them inside T-P6-004 would have meant shipping a derivation nobody had
asked to verify yet, which is the mistake the parent ticket's own §5 warns
about in a different guise: cheap to get wrong, expensive to discover from a
transcript.

## 2. What exists to build on

| Thing | Where | State |
|---|---|---|
| words, windows, provenance | `commons-store/src/interview.rs` | done, 24 tests × 2 engines |
| read routes | `commons-server/src/interview.rs` | done, 12 tests |
| `Marker` (the chapter type) | `commons-store/src/marker.rs` | done, single forward pass, min/max/silence |
| `Tag` | `commons-store/src/tag.rs` | exists, **unweighted** |
| `PersonCluster` | `commons-identity`, `commons-core::domain` | exists, used for other appearances |

**`Marker.object_id` is a `String`, not a `Uuid`.** The scanner mints
`o-pending-<uuid>` ids. Do not reintroduce a parse — that bug is what
`phase-7-050-interview-routes` fixed, and it made every marker unreadable.

## 3. The three deliverables

### 3.1 Quotes as weighted tags

A quote is a span of words worth resurfacing, with a weight saying how much.
Weight is not decoration: an untagged "derived artefact" cannot be ranked, and
a tag that cannot be ranked is a list someone has to read in insertion order.

**The weight column already exists: `tag.importance REAL NOT NULL DEFAULT
1.0`** (`migrations/*/0001_core.sql:196`). This ticket does not add a `weight`
column to `tag`; giving one table two columns that mean the same thing makes
"which one does this query read?" unanswerable from the schema. A quote's own
table carries its own weight, and the mapping onto `importance` is explicit.

There is also a `vote.weight` on a different table — a voter's reputation at
cast time. Different concept; not a candidate for reuse.

- **A quote is a `(start_ms, end_ms, text, weight)` span over `interview_word`,
  so it must survive a re-transcription** the way `FieldProposal` does: keyed
  on the **object**, not the transcript id.
- **Weight comes from where the quote came from**, and the sources are not
  comparable: a human marking a quote is a stronger signal than a model's
  salience score. Normalise onto one scale explicitly and record which is which
  in the quote's own table, or the ranking is a mixture of two different units.
  This is §4.2 and it is the most important thing in the ticket.

### 3.2 Topics as weighted `Tag`s

Same storage, different derivation: clustering over a transcript's words
produces topic tags with a weight each.

**A topic is a proposal until a person accepts it.** A model's opinion about
what an interview is about is exactly the §5.2 failure mode — a model update
silently rewriting the library — with the added property that a wrong *topic*
is more damaging than a wrong *word*, because a client renders it as fact.

### 3.3 Speaker attribution into `PersonCluster`

The engine's own diarisation gives speaker labels; the library already has a
`PersonCluster` for appearances elsewhere. An interview's speaker should land
in the same cluster as that person's other appearances, or the library has two
truths about one person.

**The hard part is not the write, it is the merge.** Two transcripts of the
same person, diarised by different engines, produce labels that do not
correspond. Merging is a proposal too — automatic cluster merging is how a
library ends up asserting that two different people are one.

## 4. The four ways this is wrong, and what each costs

1. **A re-transcription silently drops every quote and topic.** → key derived
   artefacts on the object, not the transcript id, exactly as `FieldProposal`
   does. The parent has a test for the correction half of this
   (`a_correction_survives_a_re_transcription_but_the_words_do_not`); this is
   the same trap in a new place.
2. **A weight scale that mixes a human's 1–5 with a model's 0.0–1.0.** → the
   ranking is dominated by whichever source has the wider numbers, and the
   interface shows the best model output below the worst human one. Both
   halves are in the same column, so nothing catches it.
3. **Topic tags presented as fact.** → a model's topic is a `FieldProposal`
   with `source = Model` until accepted, and the client renders it as
   "suggested" because the namespace is what says so to a reader.
4. **Diarisation merges two people into one `PersonCluster`.** → a proposal,
   never an automatic merge. Automatic merging of clusters is unrecoverable
   once a client has shown the merged card.

## 5. What deliberately does not happen here

- **No Q&A page.** The parent split it out for a reason and that reasoning has
  not changed. The searchable word-level transcript now exists and is served,
  so this is the follow-on that is finally unblocked — but it is a separate
  ticket with its own design, not a tail on this one.
- **No cloud models.** §6.5 requires offline capability, and an interview is
  the most sensitive media in a library.
- **No streaming transcription.** Out of scope in the parent and unchanged: it
  needs partial results and a mutable transcript, which fights the
  "derived artefact with provenance" rule.

## 6. Accept criteria

- **A quote survives a re-transcription**; the words it was cut from may
  change, the quote and its weight do not disappear. → `interview_db.rs`, new
  test, both engines.
- **A weight from a human and a weight from a model are distinguishable, and a
  test asserts their orderings do not interleave.** → the test that matters
  most here, because §4.2 is a silent failure: nothing errors, the ordering is
  just wrong.
- **A model topic is a proposal, not a tag**; accepting it is an explicit act,
  and rejecting it leaves no trace in the tag list.
- **Diarisation proposes a cluster merge and does not perform one.**

Each criterion runs on **both engines** — every bug in the parent's §6a was
found by the two-engine test rather than by reading, and every one of them
produced a wrong answer rather than an error.
