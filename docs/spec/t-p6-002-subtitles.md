# T-P6-002 — Subtitles and captions

**Plan entry:** `docs/plans/implementation-plan.md` §T-P6-002
**Spec:** §5.10 (C10), §11.1
**Status:** §1–§4 implemented (parsers, schema, store, both engines, probe,
extractor, sidecar reading). §5–§7 not started.

---

## 1. What exists, measured on this machine on 2026-09-27

Not a guess about what a subtitle feature would need. What is here, what was
measured about it, and what is missing.

| Piece | State |
|---|---|
| `ffprobe -show_streams` | **returns subtitle streams** — measured, see below |
| `commons-media::probe` | **enumerates them** — `SubtitleStream` in `MediaInfo` |
| `commons-media::probe` subtitle model | **done** — index, codec, language, size, start |
| sidecar reading (`.srt` beside the video) | **done** — `Extractor::read_sidecar`, no process |
| sidecar *discovery* (finding the file) | **does not exist** — §5 |
| extraction (ffmpeg → cues) | **done** — `Extractor::extract_stream` |
| `commons-media::transcode` | **`-sn`, deliberately.** `transcode.rs:232` defers to this ticket |
| ffmpeg `ass` filter | **present** |
| ffmpeg `subtitles` filter | **present** |
| ffmpeg `ass`/`ssa`/`subrip` decoders | **present** |
| ffmpeg `webvtt` encoder + muxer | **present** |
| ffmpeg `srt` muxer | **present** |
| `SubtitleTrack.svelte` | **does not exist** |
| caption text in any index | **does not exist** — so #4985 search has nothing to search |
| caption tables | **does not exist** |

### What ffprobe actually hands us

Measured on a real H.264/AAC MKV with one `ass` stream muxed in at index 2,
`language=eng`, `disposition.default=1`:

```
codec_name  ass
tags        { language: eng, ENCODER: …, DURATION: … }
disposition { default: 1, forced: 0, hearing_impaired: 0, captions: 0,
              visual_impaired: 0, descriptions: 0, … }
extradata_size  (non-zero for ASS — the style/script header)
```

Three things this settles that a plan would otherwise leave to taste:

1. **The language is there, and it is a free BCP-47 tag.** `tags.language` is
   `eng` — a three-letter ISO 639-2 code, not `en`. Multi-language entries
   (#5514) and per-language filters (#6459) are therefore a normalisation
   question, not an extraction one, and the normalisation has to happen in
   exactly one place or "English" and "eng" and "en" become three languages.
2. **Which track is which is a disposition, not a guess.** `hearing_impaired`
   is how the container says "captions", and `forced` is how it says "burn this
   in if the user has no preference". Inferring these from a filename is how
   #4586's "language rulesets" turn into a mess, and the container is
   authoritative — it is what the muxer wrote.
3. **No cue text in the probe output.** `extradata_size` is 176 for the ASS
   stream — the style/script header — and `-show_packets` for the subtitle
   stream returns one packet of `size: 11` with `pts_time 0.0`,
   `duration_time 2.0` and no payload. So the **timings are discoverable and the
   text is not**, which splits the work cleanly: the probe can enumerate tracks
   and their extents, and extraction is a real decode step. That is why this
   ticket has a back end rather than being a `<track>` tag and nothing else.

### The ceiling, and it is the browser's

A browser will not render an ASS file. It will not render an SRT file either,
unless you convert it. The only subtitle input every browser implements is
**WebVTT**, via a `<track>` element or a text track on MSE.

So the spine of this ticket is forced, and it is worth being explicit that it is
forced by a platform fact rather than chosen: **whatever the library stores,
what the player consumes is WebVTT.** Everything else is about getting from
here to there without losing the things the formats can express that WebVTT
cannot — which is the real content of this ticket, and which is entirely
upstream of the browser.

The consequence, stated as a decision: **we do not reimplement ASS rendering.**
`ass` styling is a whole renderer — override tags, `\N` line breaks, karaoke,
`\move`/`\pos` animation, drawing commands. ffmpeg's `ass` filter does it, and
it is not our job to do it better than ffmpeg does it. What we do is *get the
text and the timings* out, and hand styling to whoever is going to draw it.

### What a cue is here, and why

A cue has: a start, an end, text, and **the styling it arrived with**. The
last part is the load-bearing one, because it decides the whole architecture:

- If cues are stored as plain text, ASS becomes a text blob, styling is lost,
  and a viewer with no libass gets flat white text. `ass` styling is most of
  what a fan-sub is *for*.
- If cues are stored as a **source document plus a parsed cue list**, then the
  raw file is the record and the cues are a derived index. The style survives
  because the original is still there; a cue-level style cache can be added
  later without a migration of the real data.

The second is the design, and the cost is stated: a cue row points at a
document rather than owning its text, so a query for "captions containing X"
(#4985) has to join or denormalise, and that is where the search ticket lands.

---

## 2. The one decision the spec leaves open

**Does a cue's text live in the database, or does the player fetch the file?**

The platform spec says cues survive transcode, and that search over caption
*text* is wanted. Search implies the text is somewhere queryable. So the
database, or an index of it — and the spec is silent about which, so:

**The source document is the record; the parsed cues are an index.** The file
is extracted once and stored as a document, cues are parsed from it and
persisted as rows, and the text is indexed for #4985 in the search ticket's
time. Neither is a guess about WebVTT: the document is what preserves ASS, and
the rows are what makes search possible. A sidecar file already on disk is
referenced by path in this ticker's first step, because a sidecar that is
already there should not be copied into the database to be found again.

---

## 3. Schema

**`subtitle_documents`** — one row per subtitle source.

| Column | Type | Notes |
|---|---|---|
| `id` | text PK | derived, not random: see below |
| `object_id` | text FK | the object it belongs to |
| `origin` | text | `embedded` or `sidecar` — **not** nullable, not optional |
| `stream_index` | bigint NULL | ffprobe's index; NULL for a sidecar |
| `path` | text NULL | the sidecar's path; NULL when embedded |
| `format` | text | `ass`, `ssa`, `srt`, `vtt`, `mov_text` — as ffprobe/extension report it |
| `language` | text NULL | **normalised BCP-47**, or NULL when absent |
| `is_default` | bool | from `disposition.default` |
| `is_forced` | bool | from `disposition.forced` |
| `is_hearing_impaired` | bool | from `disposition.hearing_impaired` |
| `sha256` | text | of the *raw document*, so a re-extract is a no-op |
| `byte_size` | bigint | |
| `extracted_at` | text | ISO-8601, like every other timestamp in this schema |

`CHECK (origin IN ('embedded','sidecar'))`, and
`CHECK ((origin = 'sidecar') = (path IS NOT NULL))` — the pair is redundant, so
one of them wrong would mean a document that is a sidecar with no path or an
embedded one with a path, and neither is a state anything wants.

**`subtitle_cues`** — the index. One row per cue, many per document.

| Column | Type | Notes |
|---|---|---|
| `id` | text PK | derived: `<document_id>:<seq>` |
| `document_id` | text FK | |
| `seq` | bigint | 0-based, **as the source ordered them** |
| `start_ms` | bigint | |
| `end_ms` | bigint | |
| `text` | text | the cue's text with its inline tags **stripped**, for search and for WebVTT |
| `style_json` | text NULL | the source styling, when the format has any |

Two columns that look redundant and are not, which is worth saying because the
temptation to collapse them is exactly the bug:

- `text` has tags **stripped**, because `<` in a WebVTT cue is a tag and a cue
  containing `<i>` verbatim will not render — and because "does this caption
  contain a slur I must not show" is a question asked of *text*, not of markup.
- `style_json` keeps the styling, because "stripped" is only ever right for
  rendering and is wrong for fidelity.

`seq` is stored rather than derived from ordering by `start_ms`, because a
subtitle file with overlapping or out-of-order cues is **normal** (ASS
dialogue is layered that way), and re-deriving `seq` from timestamps renumbers
the file — which changes what a diff shows and what a search highlights.

### The uniqueness key is the TRACK, not the digest

The table above does not say how a document is *identified*, and the answer is
load-bearing enough to belong in the spec rather than in a migration comment.

The obvious key is `(object_id, sha256)` — "these bytes are already stored" — and
it deletes a user's subtitle track without an error. A film with an English track
and a forced-signs track very often has **identical text in both files**: a
translation script emits the same cues, and a sign-language track is frequently a
copy. Under `(object_id, sha256)` the second write reads as "unchanged", is
skipped, and the player shows one language where the file has two. No error, no
log line, and a track that was there before the upgrade.

What identifies a track is **where it is**, not what it contains:

```
UNIQUE (object_id, origin, COALESCE(stream_index, -1), COALESCE(language, ''), format)
```

Re-extracting the same track replaces the row. A track that *moved* (a remux
renumbers stream indices) becomes a new row, and `delete_documents` collects the
stale one on the next re-scan. The two `COALESCE`s are not decoration: a NULL
never equals another NULL in a unique index on either engine, so without them
every sidecar of one language would collide with itself and with the others.

The store's skip check uses the same tuple, as a `TrackKey`. Keeping them as two
independent expressions is how they come to disagree, and the disagreement is
silent in one direction only: the constraint rejects a duplicate loudly, while a
skip that is too broad drops a track with no symptom at all.

**The test that found this is `two_languages_of_one_object_are_both_kept`, and it
was written before the constraint was.** That order is the point. A test written
after the schema encodes the schema; a test written before it encodes what the
schema is *for*.

### Two engines, one migration, and a type the parity test cannot see

`migration_parity` compares column **names** between the Postgres and SQLite
trees. It cannot see a type, because `INTEGER` is a valid type name on both
engines — and means INT4 on one of them. `stream_index` and `byte_size` were
`INTEGER`, decoded as `Option<i64>` in Rust, and so:

- applied cleanly on both engines
- wrote cleanly on both engines
- read cleanly on **SQLite**, where `INTEGER` is a 64-bit rowid
- failed at **read** time on Postgres, and nowhere else

Only the store test caught it, and only because it round-trips a row through both
engines. Every column the Rust side decodes as `i64` is `BIGINT` on Postgres; the
three booleans are INT4 in Rust and so are `INTEGER` here. That asymmetry is
recorded in the migration itself, because the mirror will look "wrong" to the
next person reading them side by side.

---

## 4. Extraction, and the 40 ms that must survive it

`commons-media/src/subtitles.rs`, and the honest shape of the work:

1. **Get the bytes.** Embedded: `ffmpeg -i <file> -map 0:<idx> -c copy -f
   <fmt> -`. Sidecar: read it.
2. **Parse to cues.** `srt`, `vtt`, and `ass`/`ssa` by hand — they are line
   formats and a hand parser is smaller and more predictable than shelling out
   per format. `mov_text` needs ffmpeg, and only `mov_text`.
3. **Keep the document.** Written as extracted, byte for byte, `sha256` recorded.

**On the 40 ms.** The plan's accept for this ticket is "a cue survives a
transcode round-trip with timestamps correct to 40 ms", and the number is not
arbitrary — it is roughly one frame at 25 fps, which is the point at which a
subtitle is still perceptually on the right line of dialogue. So the test
asserts |round trip − original| ≤ 40 ms **for every cue in the file**, not for
one chosen cue, because a per-file average hides the cue that moved and the cue
that moved is the one a user notices.

**Where the drift actually comes from**, so the tolerance is aimed at
something:

- **`mov_text` and `webvtt` round-trip through a time base.** ffmpeg's
  `webvtt` muxer writes milliseconds; a source in 90 kHz ticks needs a
  rescale, and a rescale is where the sub-40 ms error lives. Storing
  milliseconds as integers is what keeps it bounded — a float would
  accumulate.
- **ASS times are `H:MM:SS.cc`, centiseconds.** Round to ms, and the error is
  at most 0.5 ms, which is not the problem.
- **The end timestamp is the one that drifts**, because files disagree about
  whether an end is inclusive. Normalising to half-open `[start, end)` is what
  makes "does this cue contain time t" answerable at all, and it is the same
  convention `commons-media::range` already uses — so the two agree by
  construction rather than by luck.

### What implementing it found

Three things the analysis above did not say, each measured rather than
assumed, and each now pinned by a test.

**The WebVTT timestamp has no hours field, and the parser insisted on one.**
WebVTT's grammar is `MM:SS.mmm`; SRT's is `HH:MM:SS,mmm`. ffmpeg's `webvtt`
muxer writes the short form, so a cue extracted from a `mov_text` track
arrives as `00:01.337` — and a three-field parser read that as hours=0,
minutes=1, seconds=undefined. The result was not an error but a cue at
**0:00**, and since *every* cue took that path, a file extracted from
`mov_text` produced a document in which the entire track fires on the first
frame. The document parsed cleanly. Nothing anywhere reported a problem.

Fixed by reading the field **count** as the discriminator rather than a flag:
two fields are `MM:SS`, three are `HH:MM:SS`. A side effect worth stating,
because it is a constraint rather than an accident: in the short form minutes
are unbounded, because there is no hours field for the excess to live in.
`90:00.000` is ninety minutes and must parse. The SRT form still refuses it.

**A cue whose timestamp cannot be read was being invented, not reported.**
`parse_vtt` pushed `Cue::new(seq, 0, 0, text)` on an unusable timestamp, with
a comment saying this keeps "a missing caption from being invisible". It made
it worse: a zeroed cue is a caption that *exists*, is listed in the track, and
fires at 0:00, so a file whose timestamps were all unreadable rendered as a
stack of subtitles on frame one — and the parse still succeeded. Dropping the
cue would at least leave a visible gap. Now it is `ParseError::BadTimestamp`,
naming the line, because "bad timestamp" in a 4000-line file is not something
anyone can find by hand.

**A non-empty output that parses to zero cues is a failure.** Every parser here
returns `Ok` with no cues for input it does not recognise — `parse_ass` on an
SRT file finds no `[Events]` section and returns an empty document rather than
an error — so a format mismatch is silent by construction. `extract_stream`
now rejects that case. Without it, a track muxed in the wrong format produces
a subtitle track that is listed, selectable, and empty, which is the reading a
user takes: *this video has no subtitles*. The check is in the extractor
rather than the parsers, because a zero-cue document is a legitimate result
for a genuinely empty file and a parser that refused to produce one would make
"this file has no subtitles" unexpressible.

**Two smaller ones, recorded because both are the kind that look right:**

- `mov_text` is the one format that is **not** `-c copy`. It is a QuickTime
  timecode atom, and the WebVTT muxer refuses it outright — `supports only
  codec webvtt for type subtitle`, exit 234. It has to be transcoded with
  `-c:s webvtt`. The codec flags are a parameter for exactly this reason.
- `mov_text` **cannot represent overlapping cues**, because a track is a
  timeline. ffmpeg resolves an overlap by clamping the earlier cue's *end* to
  the later one's start, so an end of 61000 next to a start of 60500 comes
  back as 60500. This is a property of the format, not a bug: no
  implementation can meet a 40 ms budget for an overlapping cue, because the
  data is not there. The round-trip fixture therefore contains no overlapping
  cues, and the clamping is asserted separately rather than left implicit in a
  drift number.

**Measured drift, on this machine, 2026-09-27, ffmpeg 9.0.1.** Every cue,
each of the ten-fixture files, worst case in the set:

| Format | Worst drift | Note |
|---|---|---|
| `subrip` (MKV) | 0 ms | `-c copy`, nothing to rescale |
| `ass` (MKV) | ≤ 1 ms | centisecond source, rounds to 10 ms |
| `mov_text` (MP4) | 0 ms | the time-base case, and it is exact |

The `mov_text` result is the one worth noting. §4 predicted the sub-40 ms error
would live in a time-base rescale; on this machine there is none, because
milliseconds are integers end to end and the only rescale is exact. The 40 ms
budget is therefore not currently being spent, and the tolerance stays as a
regression guard against a build where it is.

---

## 5. The transcode question, which the plan states and does not answer

§5.10: "cues survive transcode (re-muxed or re-derived from transcript)."

Re-muxed means `-c:s copy` carrying the subtitle stream into the proxy. That
works and is cheap, and it is **not sufficient**, because a proxy exists
precisely when the browser cannot play the original — and in MKV, an ASS stream
in the original is likely part of why it cannot. A browser given a proxied
H.264 MP4 with an ASS stream still cannot display it, because the browser never
could.

So the proxy must also be able to **re-derive** WebVTT, and the honest design
is: `-c:s copy` when the container and codec can carry it to a place a browser
can read, and a re-encode to `webvtt` when they cannot. Both paths are
specified here, and the second is the one a plan that says "re-muxed" would
have missed.

`transcode.rs:232` currently passes `-sn`. This ticket replaces that with the
two cases above, and the reason is written down there rather than in this
document, because the next person to read that flag is the one who needs it.

---

## 6. The client, and its one hard ceiling

`SubtitleTrack.svelte` renders the cues. It:

- lists documents for the object, grouped by language with a **"(none)"** for
  the user who has none, because "no subtitles" is a fact worth showing and an
  empty dropdown looks like a failed request;
- renders the selected one as a `<track>` in **WebVTT**, converting `start_ms`
  to `HH:MM:SS.mmm` and escaping the two characters that break a cue (`&` and
  the bare `<` that is not a tag);
- exposes **offset** (#4771) as a signed millisecond nudge on the
  `track.cueChange` path, so it is applied at display time and does not mutate
  stored timestamps;
- remembers the choice per object in playback state — the same place the
  position and the loop already live, because a subtitle preference is a
  playback preference and splitting them means two writes and a race.

**The ceiling, stated so it is not mistaken for a bug:** a browser cannot draw
ASS. When the selected document is ASS or SSA, the cues render as **styled
text we cannot honour** — position, colour, karaoke, and animation are lost.
The alternative is to burn them in with ffmpeg's `ass` filter, which is
pixel-perfect, and which this ticket does **not** do, for a reason that belongs
in the spec rather than in a comment: burned-in subtitles cannot be turned off,
cannot be searched, and cannot be re-styled by the user, and a player that
offers "burn in" without offering "turn off" is worse than one that says the
limit plainly. Burning in is a **later, explicit** user choice — a second
rendered stream in the proxy — not a silent default.

So this ticket delivers: full text and timing for every format, style preserved
as data, flat rendering in the browser, and a stated limit. It does not deliver
ASS rendering, and the spec says which and why.

---

## 7. What is explicitly NOT in this ticket

Each is a real request, named so it is deferred rather than forgotten.

- **Caption text search (#4985)** — needs `subtitle_cues.text` indexed and the
  search ticket's plumbing. The column is there for it; the index is not.
- **Multi-language entries and per-language filters (#5514, #6459)** — the
  normalised `language` column is the prerequisite. Grouping in the client is
  the whole of what is built here.
- **DLNA exposure (#5420)** — T-P6-005.
- **External-player metadata injection (#2770)** — T-P6-005.
- **Custom labels for captions (#4589)** and **per-scene captions** (#6062) —
  the label is a field on `subtitle_documents` that this ticket does not add.
- **A folder checked for sidecars (#6744)** — this ticket does sidecars by
  *convention beside the file*, which is the cheap, reliable half.
- **Burned-in subtitles** — §6, with the reason.

---

## 8. The three things most likely to go wrong

1. **A sidecar is found twice** — `<name>.en.srt` beside `<name>.mp4` and
   `<name>.srt` beside `<name>.en.mp4` both "belong" to the object. So
   discovery is by explicit convention (`<stem>.<lang>.<ext>` and
   `<stem>.<ext>`), and the `sha256` makes a re-scan idempotent, because a
   duplicate row for the same bytes is a bug a user sees as a doubled entry in
   the track list.
2. **A cue containing `<` silently stops rendering.** The WebVTT is emitted and
   the track loads and *most* of it displays. A stripped-tags assumption that
   misses one case is invisible until a specific file is played, so the
   round-trip test asserts on **every** cue of a fixture that deliberately
   contains `<`, `&`, an empty cue, and overlapping cues.
3. **The proxy loses the subtitles, and it is invisible.** §5. The proxy
   passing `-sn` is not an error, it is the current code, and a proxy with no
   subtitles looks exactly like a proxy that worked. So the transcode test
   asserts a cue is *present in the output* — not that the command did not
   contain a flag.

---

## 9. Definition of done

| # | Item | State |
|---|---|---|
| 1 | `0021_subtitles.sql` in both engines; `migration_parity.rs` green | **done** |
| 2 | `commons-media/src/subtitles.rs`: `parse` for `srt`, `vtt`, `ass`/`ssa`, a **pure** cue model, and `to_webvtt`. Tested with no file and no process — a parser tested only by round-tripping through ffmpeg cannot tell a parser bug from an ffmpeg bug | **done** (35 tests) |
| 3 | `commons-store/src/subtitles.rs`: `put_document` (one transaction), `list_documents`, `get_document`, `list_cues`, `stored_hash`, `delete_documents`, `count_documents`, on both engines | **done** (7 tests × 2 engines) |
| 4 | `extract` — ffmpeg in, cues out | not started |
| 5 | Sidecar discovery by convention, and a test that the two ambiguous cases in §8.1 resolve to one document each | not started |
| 6 | `commons-media::transcode` replaces `-sn` with the two cases of §5, and a test asserting a **cue is present in the transcode output** — §8.3 | not started |
| 7 | `SubtitleTrack.svelte` with the list, the WebVTT conversion, and the offset, and a unit test for the conversion that includes `<`, `&`, and an empty cue | not started |
| 8 | A round-trip test asserting **every** cue within 40 ms of the original | not started |
| 9 | `docs/HANDOFF.md` updated; `phase-6-002-subtitles` tagged | on completion of #4–#8 |

Two of the nine are done and both were the ones with a silent failure mode,
which is the order the risk wanted them in: the parsers and the schema are where
a bug costs a user a subtitle track with nothing in the logs.
- Sidecar discovery by convention, and a test that the two ambiguous cases in
  §8.1 resolve to one document each.
- `commons-media::transcode` replaces `-sn` with the two cases of §5, and a
  test asserting a **cue is present in the transcode output** — §8.3.
- `SubtitleTrack.svelte` with the list, the WebVTT conversion, and the offset,
  and a unit test for the conversion that includes `<`, `&`, and an empty cue.
- A round-trip test asserting **every** cue within 40 ms of the original.
- `docs/HANDOFF.md` updated; `phase-6-002-subtitles` tagged.
