# T-P6-001 — the player

**Plan entry:** `docs/plans/implementation-plan.md` §T-P6-001
**Spec:** §11.1 (C64), §11.5 (on-demand proxy), §12.1.1 (streaming surface)
**Status:** **partially implemented** — the transport and the state, not the
feature set. Tags `phase-6-001-player-schema`, `phase-6-001-proxy`,
`phase-6-001-player-client`. See §8.1 for what §7 still requires.

§1's table below is kept **as it was on 2026-09-27, before the work**, because
the point of measuring first is that you can check afterwards what the
measurement said. What each row says now is in §8.1.

---

## 1. What exists, measured rather than assumed

Checked against the tree on 2026-09-27:

| Piece | State |
|---|---|
| `crates/commons-server/src/range.rs` | **exists** — `ByteRange` (half-open), `RangeSpec`, `RangeError`, `resolve` |
| `GET /media/:object_id` | **exists** — full and partial responses, `content_type_for` by extension |
| `crates/commons-media::probe` | **exists** — `MediaInfo`, `VideoStream`, `AudioStream`, `Chapter`, `Prober` |
| `crates/commons-media::encode` | **exists** — `OutputFormat`, `EncodeSettings`, `args`, `validate` |
| `crates/commons-media::hwaccel` | **exists** — acceleration planning |
| Player (any) | **does not exist** — no `Player`, no playback state, no route |
| Playback persistence | **does not exist** — no table, no migration |
| Subtitle handling | **does not exist** |
| Proxy / transcode endpoint | **does not exist** — `encode` is not reachable over HTTP |
| `ui/src/routes` player route | **does not exist** |

### 8.1 What §7 still requires, measured against the tree

§7 asks for "a Playwright test per feature in the ticket's list", and the
ticket's list is long. Being precise about which parts exist, because a
spec marked *implemented* while nine features are missing is worse than one
marked partially done — the next reader trusts the wrong one.

**Built and covered:**

| Requirement | Where | Tests |
|---|---|---|
| playback state, both engines, idempotent put, default get | `commons-store/src/playback.rs`, `0020_playback_state.sql` | `playback_pure.rs`, `playback_db.rs`, `playback_store.rs` |
| GET-200-for-unknown | `commons-server/src/playback.rs` | `an_unplayed_object_gets_a_fresh_state_rather_than_404` |
| the `a < b` CHECK, exercised by a zero-length loop | store + route | `a_zero_length_loop_is_refused_by_the_check` |
| on-demand proxy | `commons-media/src/transcode.rs`, `commons-server/src/proxy.rs` | `transcode.rs`, `proxy_route.rs` |
| codec decision, one implementation | `GET /media/:id/caps` | `caps agrees with the proxy rather than deciding twice` |
| A/B loop, points on the scrubber, save as NULL not 0 | `player.ts`, `Player.svelte` | `player.test.ts`, `player.spec.ts` |
| control bar, fullscreen clipping at 360×640 | `Player.svelte` | `player.spec.ts` |
| an unplayable source says so | `Player.svelte` | `an unplayable source says so rather than showing a black rectangle` |

**Not built — §7 is not satisfied until these are:**

| Requirement | Ticket | Note |
|---|---|---|
| frame-accurate seek (the *landing*) | — | §8.2: the setting is set and asserted; the landing is **blocked on the environment** |
| subtitle toggle | §5.10 | **T-P6-002 owns it outright** (§4 says so) |
| deinterlacing | #5313 | needs a filter in `transcode.rs`, not a control |
| crop / pan / flip | #5312, #2160 | same, plus a UI to set them |
| custom speed, long-press 2× | #2645, #6982 | `playbackRate`, plus a gesture |
| audio-track selection | #1058 | `AudioStream` exists; no picker, and no proxy support for it |
| skip-intro-per-source | #634 | needs an intro-detection source, which does not exist |
| ratings in the player | #3250 | needs the ratings table, which is not until T-P7 |

**So the ticket's *core* — a player that serves range requests, knows where it
got to, and falls back to a proxy when the browser cannot play the file — is
done and tested. The feature list on top of it is not.** Six of the eight
remaining items need a server-side filter or a table that belongs to another
ticket, which is why they were sequenced after the core rather than skipped
silently.

**So the ticket is not "build a player from nothing": it is a thin control layer
over range serving that already works, plus the parts that genuinely do not
exist — playback state, subtitles, and the on-demand proxy.**

The three hardest things in the ticket are the three that do not exist. The
easy-looking part (a `<video>` element and some controls) is the easy-looking
part in every player, and it is the part that gets built last because it is the
part you can see.

---

## 2. The one decision the spec leaves open

The platform spec says exactly one thing about persistence in §11.1 — external
players get "resume position" — and says nothing about schema. So the shape of
playback state is a decision, and it is mine to make.

**Decision: playback state is per-object, not per-(user, object), and it is a
`playback_state` table keyed by `object_id`.**

Reasoning, and the alternative I rejected:

- The library is currently **single-user** (§12.1's multi-user is a later phase —
  T-P9-001 owns roles). A per-(user, object) key is therefore a key with one
  permanent member, which is a constraint the schema carries and nothing
  constrains. It is not wrong; it is *premature*, and the migration to add the
  user column later is cheap when the table is small.
- Putting it in `INSERT ... ON CONFLICT DO UPDATE` on the existing `object` row
  is the cheaper option and I rejected it: `object` is the library's central
  table, and a player's write path should not be able to block a scan, a dedup
  pass, or a metadata write. A separate table with its own row is also what
  makes "which objects have a resume position" a query rather than a full scan
  with a `NULL` filter.

**The consequence to write down:** when T-P9-001 lands, this table gains
`user_id` and its primary key becomes `(user_id, object_id)`. That is a
migration over a small table, and it is recorded here so the next agent does not
have to rediscover that the single-column key was a decision rather than an
oversight.

---

## 3. Schema

One migration, `0020_playback_state.sql`, in **both** engines —
`migrations/sqlite/0020_playback_state.sql` and
`migrations/postgres/0020_playback_state.sql`.

`scripts/sync-migrations.py` and `tests/migration_parity.rs` both enforce that
the pair stays in step, so writing one and forgetting the other fails the build
rather than shipping a Postgres that cannot read the player's state.

```sql
-- position_ms is the resume point. NULL means "never played", which is
-- different from 0: 0 is a position the user chose by seeking to the start.
CREATE TABLE playback_state (
    object_id      TEXT    NOT NULL PRIMARY KEY,
    position_ms    INTEGER NOT NULL DEFAULT 0,
    duration_ms    INTEGER,
    -- A/B loop points. NULL on either side means the loop is not set;
    -- a loop with a == b would be a zero-length loop that never fires, so
    -- the CHECK forbids it rather than letting it sit there looking armed.
    loop_a_ms      INTEGER,
    loop_b_ms      INTEGER,
    completed      INTEGER NOT NULL DEFAULT 0,
    updated_at     INTEGER NOT NULL DEFAULT 0,
    CHECK (loop_a_ms IS NULL OR loop_b_ms IS NULL OR loop_a_ms < loop_b_ms)
);
```

**Correction, found while implementing.** The first draft of this spec gave
`updated_at` as `BIGINT` millis and said Postgres would use `TIMESTAMPTZ`. Both
wrong: plan §0.4 requires ISO-8601 UTC **TEXT** in both engines, so comparison
and sort need no timezone function and the two agree. A millisecond integer
would have made every "what changed recently" query engine-specific, and it is
the one thing in this spec that contradicted the portable-SQL rules the other 19
migrations follow. The migration as written uses TEXT in both, and
`migration_parity.rs` passes.

**Correction 2, and the one worth keeping.** Postgres `INTEGER` is INT4 and
decodes as `i32`; SQLite's is 8 bytes and decodes as `i64`. Writes bind `i64`
happily in both — sqlx widens — so the mismatch appears only on the *read*, as
`Rust type i64 (as SQL type INT8) is not compatible with SQL type INT4`. Any
store method that decodes a row must pick `i32`, and the test that proves it is
`playback_db.rs`.

The SQLite mirror is **generated**, not hand-written: `python3
scripts/sync-migrations.py`, and the generated file says so in its own header.

**Indexes.** The primary key covers the only query the player makes (read by
`object_id`). No secondary index: there is no "all watched items" query in this
ticket, and adding an index for a query nobody runs is a write cost on every
resume save.

---

## 4. The endpoints

Four, and no more. Each one exists because a named feature in §11.1 needs it.

| Route | Method | Why it exists |
|---|---|---|
| `/media/:object_id/playback` | GET | resume position, A/B loop points, completed |
| `/media/:object_id/playback` | PUT | save the above |
| `/media/:id/proxy.m3u8` | GET | on-demand proxy, §11.5 |

**Three, not four.** An earlier draft of this spec listed
`/media/:object_id/subtitles` here as well, on the reasoning that §6 warns
about `TextTrack`'s ceiling. That was wrong, and the plan already says so:
**T-P6-002 owns subtitles outright** — `commons-media/src/subtitles.rs`,
`ui/src/lib/player/SubtitleTrack.svelte`, ASS/SSA, SRT, VTT, and the
sidecar scan. Building the route here would have split one feature across two
tickets and left the harder half (parsing, cue survival across a transcode) with
no owner. The ticket's "subtitle toggle" line means the *toggle* appears in the
control bar here and it calls a route that exists by the time it is wired; the
route itself is T-P6-002.

**`playback` GET returns 200 with the default state for an unknown object**, not
404. The player asks every object it opens, and a not-yet-played object is the
common case, not an error. A 404 here would make every fresh object look broken
and would be indistinguishable from a missing object.

**`playback` PUT is idempotent and takes absolute values, not deltas.** A player
that scrubs sends a position; a player that has two tabs open sends two
positions and the last writer wins. Deltas would need a sequence number the
client does not have.

**Proxy transcodes to a fixed ladder, chosen from `MediaInfo`, and caches the
result on disk keyed by (object content hash, ladder rung).** Not keyed by
request — a proxy endpoint that re-transcodes per seek is a transcoder
stampede, and this is exactly the "bandwidth is the real constraint" line from
§12.1.1. The ladder is three rungs, and which rung a client gets is §12.1.1's
per-role policy, which is T-P9-002's job; this ticket serves one rung and says
so in the route's doc comment rather than implementing half a policy.

---

## 5. What is explicitly NOT in this ticket

Stating these is the point. Each is a real feature someone will ask for.

- **Cast / DLNA / AirPlay** — T-P6-005. §11.2.
- **External players (mpv, VLC, Jellyfin, Plex)** — T-P6-005. §11.3. The
  `playback` GET above is deliberately shaped to serve it later, and that is the
  only concession this ticket makes to it.
- **Jellyfin-compatible API** — T-P6-007. §11.5.
- **Per-role quality ladders** — T-P9-002. §12.1.1.
- **Multi-user playback state** — T-P9-001, and §2 above says what changes.
- **Funscript** — T-P6-003.

---

## 6. The three things most likely to go wrong

Written down because each has already happened somewhere in this project.

**A frame-accurate seek is a *time* claim, and a range request is a *byte*
claim.** Seeking to frame 900 of a 30fps video is 30000ms; the player converts,
and the server never sees a frame number. The player's own conversion must be
tested against a known frame rate from `MediaInfo`, and the scrubber must not
claim frame accuracy it does not have when `fps` is absent from the probe. When
`fps` is missing, fall back to time-seek and say so in the control bar — a
"frame accurate" label on a time seek is a lie the user can see.

**ffprobe's `format_name` is a LIST, and matching it as one name is a silent
cost.** An mp4 file reports `mov,mp4,m4a,3gp,3g2,mj2`; a matroska file reports
`matroska,webm`. Comparing that whole string against `"mp4"` never matches, so
*every file in the library* is judged unplayable and proxied — minutes of CPU and
a disk per file, for files that needed nothing. The proxy still works, so nothing
looks broken. The same trap has a second form: a **silent** file has an empty
`audio_codec`, which matches no codec whitelist either, so every clip with the
audio stripped is proxied for the same non-reason. Both are membership tests
("is `mp4` one of these comma-separated names", "is the audio codec absent *or*
recognised") and both are now pinned by tests that use ffprobe's real strings
rather than a tidy single name.

**Codec fallback needs a *decision*, and a silent one is the failure.** If
`MediaInfo` says the browser cannot play the container, the player must either
use the proxy or refuse with a named reason. It must not construct a `<video>`
that never fires `canplay` and leave the user on a black rectangle with a
spinner. The refusal names the codec and offers the proxy.

**Subtitles are T-P6-002, and the ceiling is a browser API rather than ours.**
`TextTrack` cannot carry SSA/ASS styling, and the list of embedded tracks the
browser reports is not the list the file contains. Both are true here and both
belong to the next ticket, so the *only* thing this ticket owes them is the
toggle's position in the control bar and a client that does not lie about it: a
track the browser refused to attach must be shown as unavailable rather than
offered and silently doing nothing. A subtitle toggle that silently shows nothing
is the same failure as the black rectangle. The route and the parsing are
T-P6-002's.

---

## 7. Definition of done

- `0020_playback_state.sql` in both engines; `migration_parity.rs` green.
- Four routes, with the GET-200-for-unknown behaviour above asserted.
- A store layer with a `put` that is idempotent, and a `get` that returns the
  default for an unknown object.
- `ui/src/routes/play/[id]/+page.svelte` with the control bar, and the
  fullscreen-clipping assertion from the ticket's `**Accept:**`: control bar
  within the viewport at **360×640**, asserted, not eyeballed.
- A Playwright test per feature in the ticket's list, and the A/B loop test
  must exercise the `a < b` CHECK by trying to store a zero-length loop.
- The three failure modes in §6 each have a test that asserts the *refusal or
  the named reason*, not merely that something rendered.
- `docs/HANDOFF.md` and `README.md` updated, `phase-6-001-player` tagged.

---

## 8. Amendment: what implementation changed, and why

Written after the code, because the spec's §7 is where it is wrong. Each item
is a decision the spec did not make rather than a surprise.

### The route is `/play?o=<object_id>`, not `/play/[object_id]`

The plan says the player lives at `/play/[object_id]`. It does not, and the
reason is `adapter-static`.

This is a static SPA: the build emits one HTML file per **route**, and the
server is not a Node process that can rewrite paths at request time. A
`/play/<id>` route would require either prerendering every possible object id —
which is the whole library, at build time, and goes stale the moment a file is
added — or an edge rewrite rule, which is a deployment decision that would then
have to be replicated in Docker, in the reverse proxy, and in every local
development setup, none of which can see each other's configuration.

`/play?o=<id>` keeps the object id in the query string, so the route is
`/play` and the build is a fixed set of pages. The id survives deep-linking,
reload, and the back button for free, which is the property the path form was
wanted for.

**The cost, stated plainly:** the player cannot be prerendered per object, so
it is a client-rendered shell. For a local media library served from the same
host that already streamed the bytes, that is a latency figure nobody will
notice. If this ever runs behind a slow network on a cold cache, the first paint
is one extra round trip that a prerendered page would not need.

### `GET /media/:id/caps` was added, and the spec did not anticipate it

§4 specified `playback` and `proxy.m3u8`. Building the client against those two
only ran into a hole: **the database has no codec columns.** `ObjectRow` does
not carry them, and the schema has nowhere to put them.

The client's options were to guess (a guess that is wrong for most of a
library, costing a transcode per file for files that need none) or to ask. So
the server asks, using the probe it already runs and the same `rung_for` the
proxy route uses. The point of reusing `rung_for` rather than computing the
answer twice is that **two implementations of the same decision will disagree**,
and disagreement in either direction looks like a broken player: a client that
takes the direct path into an unplayable codec gets a black rectangle, and a
client that takes the proxy path for a playable file pays a transcode for no
reason. `caps agrees with the proxy rather than deciding twice` asserts the
agreement in both directions, because a test of one direction passes for code
that only got one direction right.

### `deny_unknown_fields` on the playback body

Not in the spec, and the strongest single addition. Without it, a client that
sends `"loop"` instead of `"loop_points"` gets **200** and a response with the
loop silently gone — found by writing exactly that test, which is how it is
known. The cost is a redeploy on a field rename, and the reason that is the
right trade is that a rename which cannot be deployed in step with every client
is better left undone than shipped as a write that disappears.

### `duration_ms` and `completed` are carried from the store, and one of them was dropped

Both are in `PlaybackState` and both are load-bearing on the client. The first
version kept only `position_ms` and the loop, which meant **every video a user
had watched to the end resumed at its final frame** — `resumePosition` honours
`completed` and was simply never told. It was caught by the test asserting the
resume *decision* rather than the file, which is the only kind of assertion that
could have caught it: the file's own clock was the thing being wrong.

### The two loop tests the spec asked for, and one it did not

`a_zero_length_loop_is_refused_by_the_check` is §7's own requirement — exercise
the `a < b` CHECK by trying to store a zero-length loop — and it passes with its
neighbour, so the check is not refusing everything: a one-millisecond loop is
ordered and is stored.

`a_loop_from_zero_is_stored_and_reads_back_as_zero` is the case §7 did not
mention and the DB constraint allows: `a_ms: 0` is a **position**, not an
absence. A client that spells "unset" as `0` and then reads the stored `0` back
as "unset" silently drops a loop the user really set. This is the store-side
twin of the client-side bug in `isLoopArmed` that demanded `a_ms > 0` — the same
mistake, on both sides of the wire, and the round-trip test is what stops it
reappearing.

### `data-resume-target` is absent, not a sentinel

Exposed on the player element because the resume *decision* is the part that is
the client's own work, and the only part assertable in a browser that refuses
seeks. It is **absent** for "start at the beginning" rather than `-1` or `0`,
because a sentinel is indistinguishable from "not decided yet" — a test
asserting the sentinel passes against a player that has not finished asking the
server, then fails in a different test order, and gets deleted as flaky.
`data-resume-decided` carries the distinction absence cannot.

### What the environment cannot verify, stated so it is not mistaken for coverage

This environment's Chromium **will not seek a paused, never-played video at
all.** `currentTime = 2.5` reads back `0`, `readyState` is 4, the range is
buffered, and neither `error` nor `seeked` fires; the same seek while playing
advances playback and ignores the target. The component is correct about this
and holds a requested position for a bounded window before yielding to the
clock, and the test asserts the decision and the control's agreement with the
file rather than the element's clock.

What that means for the suite: the *resume arithmetic* is covered by
`ui/tests/player.test.ts` on the pure module, and the *element's clock* is
covered by nothing here. On a machine with a browser that seeks a paused video
— Firefox, or Chromium launched with autoplay allowed — the e2e assertions
could be tightened to `currentTime`, and should be. Recording it as uncovered
is the point; claiming coverage from a test that asserts a decision would be
worse than having no test.

Chromium here also has no VP8-in-MSE, so the e2e fixture is a real H.264 MP4
produced by ffmpeg rather than a synthetic blob. A fixture the browser rejects
produces a player stuck in its loading state, which then fails as a broken
resume, a broken scrubber, or a broken control bar — all three of the things
under test, and none of them the thing that is wrong.

### 8.2 Frame-accurate seek, and why it is blocked here rather than merely undone

The one remaining item that is *this ticket's* work rather than another
ticket's. It is not built, and the reason is worth recording precisely, because
"not built" and "cannot be verified here" are different states and only one of
them is permanent.

The product requirement is that the seek lands on a frame, not merely near one.
That is `video.fastSeek = false`, and it is now **set explicitly** in
`onLoadedMetadata` rather than inherited. Inherited, it was correct and
invisible — and an invisible default is one line of unrelated code away from
being changed, at which point a seek becomes a keyframe jump that looks fine on
a long clip and is visibly wrong on a music video. `a seek is frame-accurate, not
a keyframe jump` asserts the flag.

**What is not asserted, and why.** Not the landing position. Verifying that a
seek lands within one frame period requires a browser that seeks, and in this
environment it does not: `currentTime = 2.5` on a paused, never-played video
reads back `0`, at `readyState` 4, with the range buffered, and no event is
emitted either way — established by seeking from the test itself, not inferred
from the component. The same seek while playing advances playback and ignores
the target.

So the flag is asserted, the arithmetic that decides *where* to seek is covered
on the pure module (`ui/tests/player.test.ts`), and the landing is covered by
nothing here. **That is a real gap and it is the only part of T-P6-001 that is
blocked rather than unstarted.** On a machine with a browser that seeks — or a
Chromium launched with autoplay allowed — the test is a dozen lines: seek, poll
`currentTime`, assert it is within one frame period of the request. Written here,
it would pass because the seek never happened, which is the worst kind of test to
ship.

This is recorded in the spec rather than left as a comment on an absent
assertion, because the absence *is* the thing a future reader needs to know, and
an absent assertion has no comment on it.
