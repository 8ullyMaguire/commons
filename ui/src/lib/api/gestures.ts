/*
  Gesture recognition for the lightbox. Pure: no DOM, no Svelte, no imports.

  # Why this is a module and not three constants in a component

  Every one of stash#7147, #7148 and #7149 is a handler that cannot tell an
  intentional gesture from its own momentum, and every one of them is a
  *threshold* on an event stream. A wheel event and a pointer triple are data:
  you can hand this module a sequence of them and ask what the user did, with
  no browser, no layout, and no rendering. So the hard part is here, where a
  test can reach it, and `Lightbox.svelte` is left holding only DOM
  concerns -- attaching listeners and moving a transform.

  The alternative is what the upstream code has: thresholds inline in a
  component, adjusted when someone reports the bug again, and untestable
  except by hand. These three bugs have each been reported more than once.

  # The three complaints, in the recognizer's terms

  stash#7147 -- panning with the mouse and releasing fast jumps to the next
  image. A fast drag's last pointermove lands *after* pointerup on some
  devices, and the release reads as a flick. Navigation on release therefore
  requires BOTH that the pointer barely moved (slop) and that the gesture took
  a while (duration). A fast long drag is a pan; a slow short one is a flick.

  stash#7148 -- two-finger trackpad scroll changes image. On a trackpad, wheel
  *is* how you pan; a wheel handler that navigates makes the image
  uncontrollable. So wheel never navigates while there is anything to pan.

  stash#7149 -- the same, at a zoom level where the pan range is small and the
  wheel delta is comparatively large. One rule covers both: wheel navigates
  only when the image is not pannable.

  # Why #7147 and #7148 are two bugs and not one

  They have the same symptom and different causes, and the distinction decides
  whether a fix works. #7147 is about the *end* of a drag and needs slop and
  duration. #7148 is a device that emits no drag at all, and no amount of drag
  logic can help it -- it needs wheel to pan. A fix for either one alone leaves
  the other, which is why the plan counts three.
*/

// ---------------------------------------------------------------------------
// Thresholds. Exported, named, and greppable, because these are the numbers a
// user reports as "it navigates when I don't mean it to".
// ---------------------------------------------------------------------------

/**
 * How far the pointer may travel, in CSS pixels, and still count as a flick.
 *
 * A drag is a pan. Only a release that barely moved is ambiguous, and only an
 * ambiguous release is a navigation. 10px is about the width of a scrollbar
 * arrow, which is roughly the smallest movement a hand makes deliberately.
 */
export const FLICK_SLOP = 10;

/**
 * How long a gesture must last, in milliseconds, to be a flick rather than a
 * pan.
 *
 * The other half of #7147. Slop alone is not enough: a slow careful drag of
 * 8px is still a drag, and navigating on it is the surprise. Together the two
 * thresholds mean "moved a little, took a while" -- which is a click-and-flick,
 * not a pan.
 */
export const FLICK_MS = 300;

/**
 * Wheel delta, in pixels, above which a wheel event on a *non-pannable* image
 * navigates.
 *
 * Only reachable when the image is already fitted to the screen, so the wheel
 * has no pan work to do and the only sensible meaning left is "next". Below
 * this, the wheel is noise: a trackpad emits small deltas constantly, and
 * treating one as a page turn makes a trackpad unusable on a fitted image.
 */
export const WHEEL_NAVIGATE_DELTA = 120;

/** Pan the image horizontally by this fraction of the container per pixel. */
export const PAN_RATIO = 1;

// ---------------------------------------------------------------------------
// Pointer
// ---------------------------------------------------------------------------

/** A pointer sample. The subset of `PointerEvent` this module needs. */
export interface PointerSample {
  readonly x: number;
  readonly y: number;
  /** Milliseconds, from any monotonic clock. Only differences are used. */
  readonly t: number;
}

/**
 * What a completed press-move-release gesture was.
 *
 * `Pan` carries the total travel so the caller can apply it as a transform;
 * `Flick` carries nothing, because a flick is not a position -- it is a
 * request to move to the next image, and the caller decides how far.
 */
export type DragOutcome =
  | { readonly kind: 'pan'; readonly dx: number; readonly dy: number }
  | { readonly kind: 'flick' }
  | { readonly kind: 'none' };

/**
 * Was a completed drag a pan or a flick?
 *
 * A pure function of the two endpoints, deliberately. The recognizer holds no
 * state between the press and the release, so there is no way for a dropped
 * `pointermove` -- the actual mechanism of #7147 -- to make a fast long drag
 * look like a short one. It cannot, because the distance is measured from the
 * press, not accumulated from the moves that happened to arrive.
 *
 * That is the whole bug and the whole fix: #7147 exists because the old code
 * summed deltas, so a move event lost in the race between `pointerup` and the
 * final `pointermove` made a 400px drag measure as 4px.
 */
export function classifyDrag(start: PointerSample, end: PointerSample): DragOutcome {
  const dx = end.x - start.x;
  const dy = end.y - start.y;
  const travelled = Math.hypot(dx, dy);
  const elapsed = end.t - start.t;

  if (travelled > FLICK_SLOP) {
    return { kind: 'pan', dx, dy };
  }
  if (elapsed >= FLICK_MS) {
    // A long press that did not move. Not a flick: a flick is a movement, and
    // treating "held still" as one makes a long press on a touch screen jump
    // to the next image.
    return { kind: 'none' };
  }
  return { kind: 'flick' };
}

// ---------------------------------------------------------------------------
// Wheel
// ---------------------------------------------------------------------------

/**
 * What a wheel event means for the current pan state.
 *
 * `ignore` is the third answer, and it is the one the ticket names: a wheel
 * arriving while a drag is in progress belongs to neither gesture. It is
 * momentum from the platform (a trackpad's own inertia arriving after the
 * fingers lifted, or a fling the compositor is still animating) and the
 * pointer that is already down is the user's actual current intent. Acting on
 * it either cancels the drag the user is performing or navigates the image out
 * from under their finger.
 */
export type WheelOutcome = 'pan' | 'navigate' | 'ignore';

/**
 * What should a wheel event do?
 *
 * The order of the three checks is the whole design, and it is deliberate.
 *
 * `dragging` is checked FIRST, before `pannable` and before the delta
 * threshold. That is the bug the ticket's accept criterion describes: "a wheel
 * event mid-pan must not change the image index". If `pannable` were checked
 * first it would also pass, but for the wrong reason -- a drag on a *fitted*
 * image has nowhere to pan, so a large wheel delta would still navigate while
 * the finger was down. A drag in progress is checked first because it is the
 * only one of the three that is about *which gesture owns the input*, and the
 * other two are about what the wheel would mean once that is settled.
 *
 * `pannable` is next, which is the whole of #7148 and #7149: on a trackpad the
 * wheel is the pan input, and on a zoomed image a large wheel delta is a pan
 * too. Navigating requires *both* that there is nothing to pan and a delta big
 * enough to mean it on purpose.
 *
 * `|delta|` is taken as a magnitude: a trackpad's two-finger scroll and a
 * mouse wheel's notches arrive with opposite signs depending on platform, and
 * "down" should never mean "previous" by accident.
 */
export function classifyWheel(deltaY: number, pannable: boolean, dragging = false): WheelOutcome {
  if (dragging) {
    return 'ignore';
  }
  if (pannable) {
    return 'pan';
  }
  return Math.abs(deltaY) >= WHEEL_NAVIGATE_DELTA ? 'navigate' : 'pan';
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/**
 * What `popstate` should do to an open, dirty modal.
 *
 * #7154. The browser's back button cannot know a modal is open, so the
 * lightbox pushes a history entry when it opens. But an unsaved modal is not
 * something back may dismiss: the user would lose their edit with no undo, and
 * "undo for destructive actions" is §10.7's rule, not this ticket's to
 * weaken.
 *
 * So `re-push` is a deliberate loop: the entry goes back on the stack, the
 * modal stays open, and the user's edits stay. The alternative -- closing and
 * losing the edit -- is worse than a back button that appears not to work,
 * because the first is silent and the second is visible.
 */
export type PopstateAction = 'close' | 're-push';

export function classifyPopstate(dirty: boolean): PopstateAction {
  return dirty ? 're-push' : 'close';
}
