# T-P6-003 — Funscript and interactive playback

Spec §5.6. Answers stash#3031 and the family: in-browser playback with timing
sync, token/DRM note (#5650), AutoBlow (#6579), manual pause (#2762), and N
named action axes with a 2-axis overlay and an N-axis controller (#6339).

## What already exists, and what this ticket adds

`crates/commons-scan/src/funscript.rs` (T-P1-007) already parses, normalises and
discovers. It is a **scan-side** module, and the layering table forbids
`commons-media` from depending on `commons-scan`, so the player cannot reach the
parser from there.

This ticket adds the four missing pieces, and none of them is "parse JSON"
again:

| Piece | File | Why it is not a copy |
|---|---|---|
| the **row** | `crates/commons-store/src/funscript.rs` | The path, size, hash and mtime. **Not** the actions: a 20,000-action script as 20,000 rows is unreadable, and the timeline is a pure function of the file, so it is recomputed rather than stored. |
| the **timeline** a player samples | `crates/commons-scan/src/funscript_timeline.rs` | Owns interpolation and a sampled index, over the parser T-P1-007 already wrote. |
| the **server routes** | `crates/commons-server/src/funscript.rs` | Owns the consent gate, the status codes, and the path containment check. |
| the **player** | `ui/src/lib/player/funscript.ts` + `FunscriptPlayer.svelte` | Owns sync, and every decision in it is a pure function. |
| the **e2e** | `ui/e2e/funscript.spec.ts` | The ticket's Done-when. |

### Two places this spec was wrong, and why the timeline is in `commons-scan`

The spec first put the timeline in `commons-media` on the reasoning that the
player lives there. It does not: the player is TypeScript in the UI, and
`commons-media` has no caller for a timeline at all. Worse, the layering table
forbids `commons-server` from reaching `commons-scan`, so a timeline in
`commons-media` could not be used by the route that serves it without amending
the table in the wrong direction.

So the timeline went to `commons-scan`, next to the parser it reads, and
`commons-server` grew a `commons-scan` edge — the same edge it already had for
`commons-media`, and the layering test (`commons-store --test layering`) holds
with it.

## The two decisions that are not obvious

### A funscript is a step function, and a player samples it 60 times a second

`Axis::action_at` is a binary search and returns the last action at or before
`t`. That is correct but not sufficient for a player: at 60 Hz a 20,000-action
script is 1.2 million binary searches a minute, and each one returns a step —
so the value visibly **jumps** at action boundaries instead of moving.

[`Timeline::sample`] therefore *interpolates linearly between the bracketing
actions* and exposes a precomputed sample index so the player does O(1) work
per frame. Interpolation is the difference between a device that glides and one
that stutters, and it is a judgement call the format does not make for us —
which is exactly why it is a named, tested function rather than an inline
expression.

### Manual pause (#2762) is not `video.pause()`

A funscript that keeps running while the video is paused drives the device
through a scene the viewer is not watching, and for a toy that is a real safety
concern, not a cosmetic one. So the player exposes three states, not two:

| State | Video | Device |
|---|---|---|
| `playing` | playing | moving |
| `paused` | paused | **held** |
| `manual-pause` (#2762) | paused | **held, and the position is not advanced** |

The third is distinguishable because the position is *frozen* rather than
merely idle, and a test asserts the freeze rather than the pause — a pause test
passes against an implementation that stopped nothing.

## Done when

The ticket's own Done-when is a Playwright test asserting a marker at t=10 s
fires within 50 ms of the scripted position. `ui/e2e/funscript.spec.ts`
asserts exactly that — on a **real** clock.

This spec originally said "with a fake clock", and that was wrong. A fake
`requestAnimationFrame` is the right tool for making a *frame count*
deterministic, and it is the wrong tool for a claim about *drift*: freezing
rAF removes the very scheduling jitter that drift is made of, and freezing it
also freezes the `timeupdate` and the resume-retry that the player depends on,
so the seek races a resume and the test fails on a clock that snaps back to 0.
That failure is indistinguishable from a player that ignores the scrubber.

So the split is: the **rate** is proved arithmetically by `driftAfter()` in
`funscript.ts` (a unit test can hold a claim about ten minutes), and the
e2e proves the **wiring** on a real clock, within the ticket's 50 ms.

The e2e also asserts the two things the 50 ms assertion cannot see: that the
interpolated value at the midpoint of two actions is the midpoint, and that
manual pause freezes the device clock while the video moves.

## What the browser found

Three failures during the e2e were bugs in existing code, not in the feature.
They are recorded here because the shape of each one — a silent, plausible
wrong answer rather than an error — is the thing worth remembering.

- **`Player.svelte`'s `on:play` never cleared `paused`.** A video that had been
  paused once was reported paused for ever after. Nothing visible read both
  variables, so it was latent until the funscript needed the distinction.
- **A media stub without `accept-ranges: bytes` cannot be seeked.** Chromium
  accepts the assignment, fires no error, reports `readyState` 4 on a fully
  buffered element, and does not move. The symptom is a player that appears to
  ignore every seek.
- **`/media/<id>` 404s as a *page* with HTTP 200** in a static build, so every
  assertion fails on "element not found" and nothing says "wrong route". The
  player is at `/play?o=<id>`.

## Not claimed here

- **Caption/plot search over a funscript** — §6, not §5.6.
- **Device transport** (Bluetooth, USB, OSC). The player produces a *position*;
  something else has to move a device. The spec §5.6 scope is the browser half.
