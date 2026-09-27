# T-P6-001 — the player

**Plan entry:** `docs/plans/implementation-plan.md` §T-P6-001
**Spec:** §11.1 (C64), §11.5 (on-demand proxy), §12.1.1 (streaming surface)
**Status:** specified, not implemented.

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
| `/media/:object_id/subtitles` | GET | list embedded tracks + discovered sidecars |
| `/media/:id/proxy.m3u8` | GET | on-demand proxy, §11.5 |

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

**Codec fallback needs a *decision*, and a silent one is the failure.** If
`MediaInfo` says the browser cannot play the container, the player must either
use the proxy or refuse with a named reason. It must not construct a `<video>`
that never fires `canplay` and leave the user on a black rectangle with a
spinner. The refusal names the codec and offers the proxy.

**Subtitles: embedded tracks are a browser API with a hard ceiling, and sidecars
are a filesystem convention.** `TextTrack` cannot carry SSA/ASS styling, and the
list of embedded tracks the browser reports is not the list the file contains.
So the server reports both (from `MediaInfo` for embedded, from a sidecar scan
for external) and the client attaches what it can, naming what it dropped. A
subtitle toggle that silently shows nothing is the same failure as the black
rectangle.

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
