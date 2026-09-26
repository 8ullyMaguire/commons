# T-P5-005 — Lightbox, image organization, per-image metadata

**Spec:** §10.2 (lightbox), §9.6 (image and gallery organization)
**Files:** `ui/src/lib/components/Lightbox.svelte`, `ui/src/lib/components/ImageGrid.svelte`
**Plan ticket:** T-P5-005 in `docs/plans/implementation-plan.md`

---

## 1. What this ticket is

Two surfaces and one rule that connects them.

- **The lightbox** (§10.2, C55) — the full-screen viewer. Its whole reason for
  existing separately from a grid is that it is the one place a user looks at
  *one* thing closely enough to pan it. The ticket's own accept criterion is a
  test that panning does not change the image.
- **Per-image organization** (§9.6, C51) — tags, ratings, ordering, custom
  numbering, and similarity search over image embeddings. The image-side
  counterpart of §7.4: in an amateur corpus the face is often the only thing
  that links two sets.

The rule connecting them: **the lightbox is a view over the grid's selection,
not a separate navigation model.** Every bug in this ticket is a symptom of
those two things disagreeing about which image is current.

### 1.1 The three upstream bugs, and what each one actually was

The plan calls these "three separate upstream bugs" and says they are "cheap
and they are the actual complaints". They are cheap as *assertions*. They are
not cheap as *fixes*, and the reason is the same in all three: a trackpad
produces a stream of events, and every one of these bugs is a handler that
cannot tell an intentional gesture from its own momentum.

| # | Symptom | Real cause | The rule |
|---|---|---|---|
| #7147 | Dragging to pan skips to the next image | a fast drag's final `pointermove` lands after `pointerup`, and the release is read as a *flick* | a release only navigates if the pointer moved less than `FLICK_SLOP` **and** the gesture took longer than `FLICK_MS` |
| #7148 | Trackpad two-finger scroll changes image | `wheel` is bound to navigation, and on a trackpad `wheel` *is* how you pan | `wheel` pans; only a wheel event with no pan handler bound may navigate |
| #7149 | Zoomed-in image scrolls to the next | same as #7148, at a zoom level where the pan range is small and the wheel delta is large | the wheel navigates only when the image is **not** pannable (fit-to-screen) |

`#7147` and `#7148` are not the same bug, which is why the plan says three and
not two. #7147 is about the *end* of a drag; #7148 is about a device that
emits no drag at all. A fix for one does not fix the other: #7147 needs slop and
duration, #7148 needs the wheel to not navigate at all.

### 1.2 #7154, the back button

Not in the accept criterion, in the ticket body, and it is the one that is a
design decision rather than a threshold. The browser back button cannot know
that a modal is open. So the lightbox pushes a history entry when it opens, and
`popstate` closes it. The bug: the *unsaved* state. An unsaved modal is not
something back should dismiss silently — the user would lose work with no undo.
So `popstate` while dirty **re-pushes** the entry and refuses to close, which is
a deliberate loop, and the only correct behaviour: the alternative is either
losing the edit or a back button that silently does nothing forever.

---

## 2. What exists and what is missing

| Piece | State |
|---|---|
| `VirtualGrid.svelte` | exists, 410 lines, virtualized, fixed row height (§4.2) |
| `KeysetStore` | exists, `api/keyset.ts`, 261 lines |
| `ViewState` | exists, `api/view.ts`, 107 lines |
| `ObjectRow` | exists — `id, kind, title, date, rating, organized, coverPath, width, height, durationMs` |
| `ImageGrid.svelte` | **absent** |
| `Lightbox.svelte` | **absent** |
| a gesture/pan module | **absent** — this is where the three bugs are actually solved |
| Playwright wheel-mid-pan test | **absent** — the accept criterion |

`ObjectRow` has `rating` and `organized` and nothing for custom numbering or
per-image ordering, so §9.6's numbering is a **view concern** until a later
ticket adds the column. It is designed for here and stubbed to the existing
row rather than guessed at.

## 3. Design decisions

### 3.1 The gesture recognizer is a pure module, not a component

Every threshold in §1.1 is a *pure function of an event stream*. A `wheel`
event and a `pointerdown/move/up` triple tell you what the user did with no DOM
at all. So the recognizer is `api/gestures.ts` with no imports from
Svelte, and it is unit-tested directly with synthetic event sequences.

This is the same decision as `keyset.ts` and `view.ts` already in `api/`: the
hard part is testable without a browser, so it lives where a browser is not
needed. A `Lightbox.svelte` with three inline threshold constants and no test
would be the alternative, and it is the thing that makes these three bugs
reappear every time someone tunes a number.

### 3.2 Thresholds are exported constants with their reasons

`FLICK_SLOP = 10`, `FLICK_MS = 300`, `WHEEL_NAVIGATE_DELTA = 120`. Not private
literals: these are the numbers a user reports as "it navigates when I don't
want it to", and they need to be greppable and named in a test failure.

### 3.3 A test dispatches a real `WheelEvent` mid-pan

The accept criterion says "Playwright test dispatching a wheel event mid-pan
and asserting the image index did not change". This is done literally, and the
*index* is read from a `data-index` attribute on the lightbox root rather than
from a Svelte store, because a test that reads the state under test is a test
of the state, and the complaint is about what the user sees.

### 3.4 `ImageGrid` composes `VirtualGrid`

It does not fork it. §10.4's "virtualization throughout" plus §4.2 means a
second grid is a second thing to keep correct, and `VirtualGrid` already solves
the hard part with a documented reason.

## 4. Acceptance, per the ticket

> Playwright test dispatching a wheel event mid-pan and asserting the image
> index did not change. Each of the three upstream bugs gets its own named
> assertion.

| # | Test | Asserts |
|---|---|---|
| 1 | `a_wheel_event_mid_pan_does_not_change_the_index` | the accept criterion, literally |
| 2 | `a_fast_drag_releases_without_navigating` | #7147 — slop + duration |
| 3 | `a_slow_short_drag_is_a_flick_and_does_navigate` | #7147's other half, so the fix cannot be "never navigate on release" |
| 4 | `a_wheel_event_on_a_pannable_image_only_pans` | #7148, #7149 |
| 5 | `back_does_not_dismiss_an_unsaved_modal` | #7154 |
| 6 | recognizer unit tests | every threshold, at the boundary: slop-1, slop, slop+1; ms-1, ms, ms+1 |

**Test #3 is the one that matters most.** #7147's fix is "do not navigate on a
fast drag", and the degenerate fix is "never navigate on release" — which
breaks the flick-to-navigate that the feature is for. #3 is what stops that.

**Boundary, not a comfortable distance.** Every threshold test is at slop±1 and
ms±1. A test at slop×10 would pass for a constant that is off by a factor of
ten, which is exactly the kind of regression a threshold gets.

## 5. Mutation

`scripts/mutate-gestures.py` applies 11 behaviour-changing mutations to
`gestures.ts` -- one per decision the recognizer makes -- and runs the unit
suite against each. All 11 are killed; there are no survivors and no
exemptions.

A survivor would mean a rule nothing tests. That number is the reason these
four functions were pulled out of the component at all: inline in a `.svelte`
file they could not be mutated, and "a threshold nobody tests" is exactly how a
threshold drifts.

The two that matter most, because they are the bugs the ticket names:

| Mutation | What it reproduces | Killed by |
| --- | --- | --- |
| `dx` measured as 0, i.e. the press ignored | #7147's mechanism: a drag that measures as no travel | `the distance is measured from the press, not from the moves` |
| the `dragging` branch removed | the accept criterion: a wheel mid-pan navigates | `a wheel event mid-drag is ignored, ...` |

The first matters because a suite that passed no pointer events at all still
catches it: `classifyDrag` is a function of two samples, so there is no move to
lose. The second matters because `pannable`-first would pass the pannable half
of that test and still fail the user's actual case -- a drag on a fitted image
has nowhere to pan.

---

## 5. Verification

| Step | Command | Expected |
|---|---|---|
| recognizer | `cd ui && node ./tests/run-tests.mjs` | all green, incl. #6 |
| type check | `cd ui && pnpm exec svelte-check --threshold error` | 0 errors |
| build | `cd ui && pnpm run build` | clean |
| browser | `cd ui && pnpm run test:e2e` | #1–#5 pass |
| everything | `bash scripts/verify.sh` | ALL GREEN |

**A mutation per test, as with every ticket in this repo.** The thresholds are
the mutation targets: moving `FLICK_SLOP` by one pixel must fail a test, or the
constant is decorative.
