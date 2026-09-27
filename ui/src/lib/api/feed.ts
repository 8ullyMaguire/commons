/**
 * The vertical feed's decisions, with no DOM in sight.
 *
 * T-P5-006 item 9, spec §10.6. Pure module for the same reason
 * `media-view.ts` is one: the parts with a threshold in them are the parts
 * worth testing, and a threshold you can only exercise through a browser is a
 * threshold nobody tunes.
 */

/**
 * How far a drag must travel, as a fraction of the viewport height, before
 * releasing it moves to the next item.
 *
 * 0.3 is the value TikTok-class feeds converge on and it is a guess I cannot
 * derive. The two failure modes pull opposite ways: too low and a scroll that
 * was meant to be "let me look at this one" navigates; too high and a
 * deliberate flick does nothing and the user swipes again, which is the
 * gesture people read as "this is broken". Both are worse than the other, so
 * the number is in one place with a test at, just under, and just over it,
 * rather than inline in a pointer handler where it cannot be moved without
 * reading the handler.
 *
 * The reason this is a fraction and not pixels: a phone in portrait is ~700px
 * tall and a desktop window is ~900, and a pixel threshold that feels right on
 * one is unreachable on the other. The ticket is for phones.
 */
export const COMMIT_THRESHOLD = 0.3;

/**
 * How many items ahead of the focused one are preloaded.
 *
 * 2, not 3, and the number is about requests. Preload is a request; a user
 * swiping quickly through a video feed issues one per item. At three ahead, a
 * fast swipe has queued downloads for items already three screens past, which
 * on a phone is measurable data spent on nothing. Two covers a normal swipe --
 * autoplay starts before the finger lifts.
 */
export const PREFETCH_AHEAD = 2;

/** Which way a drag or a wheel moved the feed. */
export type Axis = 'up' | 'down';

/**
 * The outcome of releasing a drag.
 *
 * `commit` moves focus by one item. `spring-back` puts it back where it was.
 * There is no third case, deliberately: a half-swiped item is not an item, and
 * a feed with a persistent "in between" state is a feed whose state has to be
 * persisted, restored, serialized, and reasoned about by every test after this
 * one.
 */
export type Commit = { kind: 'commit'; axis: Axis } | { kind: 'spring-back' };

/**
 * What a drag from `dy` pixels means on release.
 *
 * `dy` is the vertical travel of the pointer, positive downward -- the same
 * sign convention as `PointerEvent.clientY` deltas, so a caller passes
 * `end.y - start.y` and does not negate it first. Getting that wrong flips the
 * whole feed, and it is the kind of bug that reads as "the swipe is inverted"
 * rather than as a sign error.
 *
 * A drag that crosses the threshold in the direction the finger moved commits.
 * One that does not springs back, however fast it was: speed is not in the
 * argument, because a fast flick that covered 5% of the screen is a tap that
 * went wrong, and honouring it is how a feed ends up advancing on every
 * scroll-adjacent gesture.
 */
export function classifyCommit(dy: number, viewportHeight: number): Commit {
  // A zero or negative viewport is a layout that has not happened yet. `|| 1`
  // would make the threshold 0.3 *px*, so a drag of any size commits and the
  // feed jumps on first paint. Treating it as "no commit" is the conservative
  // reading and costs nothing.
  if (!(viewportHeight > 0)) return { kind: 'spring-back' };
  if (dy === 0) return { kind: 'spring-back' };

  const fraction = Math.abs(dy) / viewportHeight;
  if (fraction < COMMIT_THRESHOLD) return { kind: 'spring-back' };

  // Dragging the content UP reveals the NEXT item, the way a list scrolls
  // down. This is the inversion that makes the feed feel like the lightbox
  // rotated 90 degrees rather than like a new gesture: the content follows the
  // finger, and the item that was below arrives from below.
  return { kind: 'commit', axis: dy < 0 ? 'up' : 'down' };
}

/**
 * The focus index after moving `delta` from `current`.
 *
 * Clamps at both ends. No wrap: a feed that wraps from the last item to the
 * first is a carousel, and §10.6 says feed. The difference is not cosmetic --
 * a wrapping feed has to decide what "the end" means, and a viewer who has
 * swiped 400 items down and comes back up has to find their place, which means
 * the position is history rather than an index.
 *
 * `total` of 0 or less means an empty feed, and the answer is 0: a focus index
 * into nothing is still nothing, and returning -1 would make every caller's
 * bounds check one wrong.
 */
export function nextFocus(current: number, delta: number, total: number): number {
  if (total <= 0) return 0;
  // Clamp the CURRENT index before adding, so a current that is already out of
  // range (a library that shrank under a saved position) moves toward the
  // valid range instead of away from it.
  const base = Math.min(Math.max(current, 0), total - 1);
  return Math.min(Math.max(base + delta, 0), total - 1);
}

/**
 * Which items should be preloaded, as indices.
 *
 * Exactly `focus + 1 .. focus + PREFETCH_AHEAD`, clipped to the feed. Nothing
 * before `focus`: it has been seen. Nothing beyond: that is the number the
 * spec's "the next few" does not say and a phone's data plan does.
 */
export function preloadFor(focus: number, total: number): number[] {
  const out: number[] = [];
  for (let i = focus + 1; i <= focus + PREFETCH_AHEAD && i < total; i++) {
    out.push(i);
  }
  return out;
}

/**
 * Whether this item is the one that plays.
 *
 * One item, and only the one at the focus index. Not the focused one *and* the
 * one being preloaded, and not "the one that is visible" -- a feed one screen
 * tall with overscan has two visible items at a boundary scroll position, and
 * a rule that says "visible" plays both, which is a phone at 4% battery an
 * hour for a video nobody can see.
 */
export function isPlaying(index: number, focus: number): boolean {
  return index === focus;
}

/**
 * The `preload` attribute for a `<video>` at this index.
 *
 * `auto` for the focused item and the two ahead, `none` for everything else.
 *
 * `none` rather than `metadata` for items behind the focus: a feed that
 * swiped past item 3 and back to item 1 should not be holding item 3's bytes
 * in memory, and the browser's own preloader is the implementation — a
 * hand-rolled one would be a second HTTP client with its own cache and its own
 * bugs.
 */
export function preloadAttr(index: number, focus: number, total: number): 'auto' | 'none' {
  if (isPlaying(index, focus)) return 'auto';
  return preloadFor(focus, total).includes(index) ? 'auto' : 'none';
}
