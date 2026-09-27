# T-P6-003 — Funscript and interactive playback

Spec §5.6. Answers stash#3031 and the family: in-browser playback with timing
sync, token/DRM note (#5650), AutoBlow (#6579), manual pause (#2762), and N
named action axes with a 2-axis overlay and an N-axis controller (#6339).

## What already exists, and what this ticket adds

`crates/commons-scan/src/funscript.rs` (T-P1-007) already parses, normalises and
discovers. It is a **scan-side** module, and the layering table forbids
`commons-media` from depending on `commons-scan`, so the player cannot reach the
parser from there.

This ticket adds the three missing pieces, and none of them is "parse JSON"
again:

| Piece | File | Why it is not a copy |
|---|---|---|
| the **timeline** a player samples | `crates/commons-media/src/funscript.rs` | Owns interpolation and a sampled index. Parsing is not duplicated: it re-exports the scan parser. |
| the **server routes** | `crates/commons-server/src/funscript.rs` | Owns the consent gate and the status codes. |
| the **player** | `ui/src/lib/player/funscript.ts` + `FunscriptPlayer.svelte` | Owns sync, and every decision in it is a pure function. |
| the **e2e** | `ui/e2e/funscript.spec.ts` | The ticket's Done-when. |

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

The ticket's own Done-when is a Playwright test with a fake clock asserting a
marker at t=10 s fires within 50 ms of the scripted position. `ui/e2e/
funscript.spec.ts` asserts exactly that, and also asserts the two things the
50 ms assertion cannot see: that the interpolated value at the midpoint of two
actions is the midpoint, and that manual pause freezes the device clock while
the video is paused.

## Not claimed here

- **Caption/plot search over a funscript** — §6, not §5.6.
- **Device transport** (Bluetooth, USB, OSC). The player produces a *position*;
  something else has to move a device. The spec §5.6 scope is the browser half.
