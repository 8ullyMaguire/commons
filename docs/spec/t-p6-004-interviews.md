# T-P6-004 — Interviews: transcription and Q&A search

Spec §5.8. Local ASR on interview videos, with chapters, quotes, topics and
speaker attribution feeding the same proposal and identity machinery the rest of
the application already uses.

**Status:** spec written, not implemented. This file is the contract the
implementation is written against.

---

## 1. Why this is one ticket and not five

Transcription on its own produces a text file nobody can search. What the ticket
actually asks for is a *chain*, and every link in it is a place the naive version
loses information:

```
audio ──▶ 16 kHz mono ──▶ ASR ──▶ word timestamps ──▶ transcript
                                                          │
              ┌───────────────────────────────────────────┼───────────────┐
              ▼                                           ▼               ▼
         chapters (Markers)                        quotes + topics    speakers
         -> the existing Marker model              -> weighted Tags   -> PersonCluster
                                                            │               │
                                                            └──────┬────────┘
                                                                   ▼
                                                          FieldProposal (§8.1)
```

The load-bearing decision is at the bottom right: **a human correction becomes a
`FieldProposal`, never an overwrite.** That is §8.1 and the ticket says so
explicitly. Writing the corrected word straight into the transcript is the
obvious implementation and it destroys the evidence for every other correction —
once the model output is gone, nobody can tell a misheard word from a typo, and a
user who fixes the transcript a second time has nothing to compare against.

The second load-bearing decision is that **the transcript is a derived artefact
with a provenance record**, not a fact about the media. The ASR engine, the
model, its digest and the ffmpeg command are stored, because "the transcript
says the interviewee is called Sarah" is only as good as the ability to reproduce
it. An unreproducible transcript is a rumour.

## 2. What exists to build on

| Thing | Where | Why it is reused rather than replaced |
|---|---|---|
| `ProposalSource::Transcript` | `commons-core/src/enums.rs:261` | Already there, waiting. Using `MlTagger` instead would lose the ability to say *where* a value came from, which §8.2 requires the UI to show. |
| `FieldProposal` | `commons-core/src/domain.rs:231` | Carries `justification: Option<String>`, which is exactly "asr=whisper.cpp, model=base.en, digest=…, cmd=…". |
| `PersonCluster` | `commons-identity/src/lib.rs` | Speaker attribution must land in the *same* cluster an appearance does, or the same person ends up in two clusters. |
| `Marker` | `commons-core/src/domain.rs` | Chapters are markers. A new chapter type would need a new UI, a new index and a new export format for no gain. |
| `ModelError` | `commons-ml/src/model.rs` | Split by cause on purpose: `Unavailable` is a feature not enabled, `ChecksumMismatch` is a corrupted download *or an attack*. The caller's response differs and the error type already knows that. |
| ffmpeg wrapper | `commons-media/src/extract.rs` | `COMMONS_FFMPEG` is resolved in tests. Audio extraction is one more `-map`/`-vn` invocation against the same binary. |

## 3. Architecture

### 3.1 `commons-ml/src/asr/` — the engine seam

The engine is a trait, not a choice:

```rust
/// A word with its timing. Times are milliseconds from the start of the
/// media, NOT from the start of the chunk, so a caller can concatenate the
/// output of a chunked run without an offset table.
pub struct TimedWord {
    pub text: String,
    pub start_ms: u32,
    pub end_ms: u32,
    /// Engine's own confidence, 0.0..=1.0. `None` when the engine does not
    /// score words, which is different from scoring every word 0.5.
    pub confidence: Option<f32>,
    /// Set when the engine attributes a word to a speaker.
    pub speaker: Option<SpeakerId>,
}

pub trait AsrEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn transcribe(&self, audio: &Pcm16kMono, sink: &mut dyn FnMut(TimedWord)) -> Result<()>;
}
```

Two engines, both local, both behind the trait:

- **whisper.cpp** — GGML. Best accuracy-per-watt, and it is what most people have
  a model for. Invoked as a subprocess with `--output-json`, because binding a
  C library into the workspace for this is a large amount of build complexity for
  one feature.
- **parakeet** — ONNX, through the existing `ModelError` verification path. Runs
  in-process, which is what makes it usable for a library scan; whisper.cpp is
  what makes it usable for a one-off.

**Why a subprocess for whisper.cpp and in-process for parakeet.**

**Revised, and the revision is the point.** The original text above chose
in-process for parakeet "because the memory is bounded by chunk size". That was
reasoning about the model, not about the repository, and it is the kind of
reasoning that is never re-checked because it sounds like engineering.

The parakeet path is a **subprocess too** — a Python sidecar speaking ONNX
Runtime over a pipe. The reasons, in the order they mattered:

1. **It can be verified.** whisper.cpp's JSON parser is a pure function, and it
   is tested on this machine with no model present. An in-process ONNX path
   cannot be tested that way: its correctness is a property of a model file
   nobody can fetch, behind a runtime nobody has installed. Shipping it would
   mean shipping code whose only evidence is that it compiles.
2. **It can be killed.** The original argument — that a subprocess cannot wedge
   a scan — applied to whisper.cpp and was not applied to parakeet, for no
   reason other than that the reasoning ran out. A pathological 90-minute input
   can allocate until the OOM killer arrives either way; being in-process only
   means the OOM killer takes the *library* with it, so the scan is not
   resumable. That is a worse failure than the one the design was trying to
   avoid, not a better one.
3. **The dependency is not free.** The in-process route needs an ONNX runtime
   bound into the workspace. `tract-onnx` pulls ~20 transitive crates including
   a full NNEF and transformer stack, for a feature that one user with one
   model will use. The sidecar needs Python, which the user already needs to
   have for the model anyway.

What is **kept** from the original is the reason the model file is trustworthy:
both engines verify the digest through `ModelSource::open_with_sha256` before
loading, and neither is opened on a mismatch.

The cost, stated plainly: a Python sidecar is a dependency the user must
install, it is slower to start, and it will not work on a machine with no
Python. That is a real cost and it is why the second engine is optional and
whisper.cpp is the default.

### 3.2 Chunking

A 90-minute interview is not transcribed in one call.

- Chunk: **30 s**, with **no overlap**.
- Overlap is the thing to be careful about. The obvious implementation (overlap
  each chunk by 1 s and dedupe) produces doubled words at every boundary, and the
  dedupe that fixes it is a heuristic that eats real words at boundaries where
  the speaker pauses. So: no overlap, and instead a **VAD (voice activity
  detection)** pass that trims silence off each chunk's edges before the window
  moves, so the model is not spending most of its context on pauses.
- `start_ms`/`end_ms` on every word are absolute, computed from the chunk index.
  This is why the field is documented as absolute above: a relative timestamp
  that a caller has to correct with an offset table is an offset table that will
  be wrong somewhere.

### 3.3 Audio extraction

```
ffmpeg -nostdin -v error -i <input> -map 0:a:0 -vn -ac 1 -ar 16000 -f s16le -
```

`-ac 1 -ar 16000` is not negotiable: every ASR model in this space expects it,
and resampling in the model wrapper instead means a second resampler.

The output is read **streaming**, in 32 KiB reads, and never written to a
temporary file. An interview is 2 hours of 16 kHz mono = 230 MB; a temp file that
size is a disk-full crash on a library machine, and it is also a plaintext copy
of someone's voice sitting in `/tmp` after the process exits.

### 3.4 What gets derived, and how

**Chapters.** A `Marker` per chapter, `start_ms` from a segment boundary, plus
a title. Titles come from the first N words of the segment, and the rule is that
**a chapter with no title is not created** — a chapter list of blank rows is
worse than no chapter list, because the user cannot tell "no chapters detected"
from "chapters exist and their titles failed".

**Quotes.** A `Tag` with a weight. A quote is a sentence-shaped span between two
long pauses (≥ 400 ms of silence) with a minimum length. The weight is the
product of the mean word confidence and a length factor, so a long confident
sentence outranks a short confident one. This is a `Tag` and not a new entity
because a quote that cannot be searched by weight is a quote you can only find by
reading every one.

**Topics.** Noun-phrase-ish n-grams, 1–3 words, scored by frequency × inverse
document frequency across the *library's* transcripts, stored as weighted `Tag`s.
The IDF part is what stops "the" and "and" becoming topics on every video; a
transcript corpus where every transcript has the same top-3 keywords is a corpus
with no topics.

**Speakers.** `SpeakerId`s are per-*transcript*, not global. The cluster is
attached to the `PersonCluster` through the same path an appearance uses. The
ticket's words: "into the same `PersonCluster` as other appearances", which is a
requirement about the *link*, not a claim that diarisation alone is reliable
enough to merge clusters — merging is a proposal, and §8.1 applies to it too.

## 4. Storage

Migration `0023_interviews.sql`, both engines.

```
interview_transcripts
  id, object_id UNIQUE, engine, model_id, model_sha256,
  audio_command, sample_rate, created_at, updated_at,
  word_count, duration_ms, language

interview_words        -- the word-level output, one row per word
  id, transcript_id, ordinal, text, start_ms, end_ms,
  confidence REAL NULL, speaker TEXT NULL

interview_speakers
  id, transcript_id, speaker_key, label NULL, cluster_id UUID NULL
```

Decisions worth writing down:

- **`interview_words` is a table, not JSON in a column.** The ticket wants
  search, and `LIKE` over a JSON blob is not search. It costs rows, and the rows
  are the point.
- **`confidence` is nullable**, and `speaker` is nullable, for the same reason:
  "this engine does not score words" and "this engine scored it 0.5" are
  different facts and averaging over them is a lie.
- **`model_sha256` is stored** even though `ModelError` verifies it at load time.
  Verification answers "is this file what the manifest says"; the column answers
  "which model produced *this transcript*", which is a different question and the
  one a user asks when the transcripts changed after a model update.
- **`ordinal` is explicit** rather than relying on insertion order. Word
  sequence is data, and a `SELECT` without `ORDER BY` is not ordered.

## 5. The five ways this is wrong, and what each costs

Written down before implementation because each is cheap to get right later and
expensive to discover from a transcript.

1. **Timestamps drift across chunks.** The output is a word at 29.5 s in one
   chunk and 30.2 s in the next, meaning a word was split. → absolute times
   computed from the chunk index, and a test that asserts monotonicity across a
   chunk boundary.
2. **A model update silently rewrites the library.** → the transcript records
   the model digest, and a re-run that produces different text is a *proposal*,
   not a replacement.
3. **A correction overwrites the model output.** → `FieldProposal`, always. The
   store method is named `propose_correction` and there is no
   `update_word`.
4. **The temp audio file is left behind.** → streaming, no temp file, so this
   cannot happen.
5. **A 90-minute interview OOMs a scan.** → subprocess for whisper.cpp, chunk
   size bounded, and the scan is resumable at chunk granularity.

## 6. The accept criteria, and the tests that will prove them

The ticket's own criteria:

- **Word timestamps monotonic and within 200 ms of a hand-checked transcript**
  on a fixture clip. → `crates/commons-ml/tests/asr_timing.rs`: asserts
  non-decreasing `start_ms`, asserts `end_ms >= start_ms`, and compares against
  a checked-in expected transcript in `tests/fixtures/interview.srt`. The
  comparison is a *word-alignment* distance, not a raw string compare, because
  the point is timing accuracy and a string compare conflates it with wording
  differences between model versions.
- **A corrected word becomes a proposal, not a silent overwrite.** →
  `crates/commons-store/tests/interview_db.rs`: record a transcript, apply a
  correction, assert the original word is still in `interview_words` and a
  `FieldProposal` with `source = Transcript` exists referring to it.

And these, which the ticket implies:

- A transcript is reproducible: same engine, same model digest, same audio
  command → the stored provenance is complete.
- `view`-scope versus download-scope is not re-litigated here; it was
  T-P9-003 and it is done.
- An unavailable model produces `ModelError::Unavailable`, not a panic, and the
  caller can tell it apart from `ChecksumMismatch`.

## 6a. What the storage layer actually found

Written down because each of these was found by the two-engine test rather than
by reading, and each is a shape this schema has now been bitten by twice before.

- **`?` sent to Postgres is a syntax error at an offset that names nothing.**
  Three `DELETE`s were written with a literal `?` because that is what the
  SQLite arm wanted. Postgres answered `syntax error at end of input` at
  position 51 — an offset into a statement that has nothing wrong at 51. Every
  statement is now built from a marker.
- **`placeholders(n, …)` returns the whole comma-joined list.** Writing one
  `{p}` per column expands a 12-bind INSERT into 144 placeholders. The
  convention is a single `{p}` for a VALUES clause, and `placeholder(i, …)`
  for a specific parameter in a predicate.
- **A marker that is a PREFIX of another marker is rewritten by its
  replacement.** `{p}` and `{p:i}` in one statement: the `{p}` replacement
  matched the prefix inside `{p:i}` and both range bounds became `$2`. The
  query returned *zero rows with no error* — the worst failure shape, because a
  chapter search that returns nothing looks like a library with no chapters in
  it. The markers are now `{a}` / `{b}` / `{c}` and the order is irrelevant.
- **An untyped `$n` against an `INTEGER` column is inferred as `text`.**
  `start_ms >= $2` is `integer >= text` and the query is refused at plan time
  on the arm that runs the comparison. A test that only ever lists every word
  never reaches it. Fixed with an explicit `CAST`.
- **`INTEGER` is INT4 on Postgres and INT8 on SQLite.** `word_count` was
  declared `INTEGER` and decoded as `i64`: green forever on SQLite, a read-time
  type error on Postgres, and invisible to `migration_parity` because that test
  compares column *names*. Third occurrence in this repository.
- **A replacement arriving under a NEW transcript id leaves the previous run's
  words behind.** The children were deleted by the incoming id, which matches
  nothing on a re-run that reuses the row and only the new words on one that
  does not. The delete is now keyed on both the id and the object, and the
  parent is an `ON CONFLICT (object_id) DO UPDATE`.

That is six, and every one of them produced a wrong answer rather than an
error. The parities that catch them are the two-engine test and the round trip;
neither is optional.

## 7. Fixtures

Per the standing rule: **each fixture creates its own instance, never a named
row.** A shared object row, and a shared transcript row, are both a collision
waiting for a second suite. The audio fixture is generated by ffmpeg at test
time (a tone plus a silence, `COMMONS_FFMPEG` is already resolved in the harness)
rather than checked in, so the repository does not carry a binary that every
clone must fetch.

`tests/fixtures/interview.srt` is the exception and is checked in: it is a
*hand-checked* transcript, and a hand-checked thing cannot be generated by the
thing it is checking.

## 8. What this ticket deliberately does not do

- **No cloud ASR.** §6.5 requires offline capability, and an interview is the
  most sensitive media in a library.
- **No speaker *diarisation* model beyond what the engine ships.** Clustering
  two speakers into one `PersonCluster` is a proposal; the ticket asks for
  attribution, and attribution is what the engine's own diarisation gives.
- **No streaming/transcription-while-playing.** Out of scope, and it is a
  different design (it needs partial results and a mutable transcript, which
  fights the "derived artefact with provenance" rule above).
- **No Q&A search UI.** The ticket names "Q&A search", but the deliverable that
  makes it possible is the searchable transcript; the UI is a separate page
  against the same data and would be speculative before any transcript exists.
  Recorded in the plan as the follow-on rather than silently dropped.
