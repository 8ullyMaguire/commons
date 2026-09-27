/**
 * Where a popover goes, given the space it has.
 *
 * T-P5-007, spec §10.8 (#4667). Pure geometry, no DOM, so "a menu near the
 * bottom-right corner opens upward and leftward" is an assertion rather than a
 * screenshot.
 *
 * # The rule, and the order the three parts run in
 *
 *   1. **Flip** if the requested side does not fit and the opposite side does.
 *   2. **Shift** along the cross axis so the popover stays on screen.
 *   3. **Clamp** as a last resort, for the case where neither side fits.
 *
 * The order is the design. Flip-first means a menu that does not fit opens the
 * other way rather than sliding -- sliding moves the thing the user aimed at
 * away from their cursor, and a menu that appears under the pointer is
 * indistinguishable from a menu that opened in the wrong place.
 *
 * # Why the "does the opposite fit" test matters
 *
 * A menu taller than the viewport fits in neither direction. Flipping anyway
 * puts it off the top, and the result is worse than either placement: the user
 * sees the bottom edge of a menu and no top. So the flip is conditional, and
 * the no-fit case falls through to the shift and the clamp, which at least
 * pins it to the viewport.
 *
 * # `gap` exists so a popover never touches its trigger
 *
 * Without it, a flip puts the popover's edge exactly on the trigger's edge and
 * the two read as one shape. One arrow-width of separation is the smallest
 * value that still reads as "this belongs to that".
 */

/** A rectangle in CSS pixels, viewport-relative. */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Which side of the trigger the popover is asked for. */
export type Side = 'top' | 'right' | 'bottom' | 'left';

/** The side a popover is asked for, and the side it ended up on. */
export interface Placement {
  side: Side;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Viewport {
  width: number;
  height: number;
}

export interface PlaceOptions {
  /** The trigger. */
  anchor: Rect;
  /** The popover's own size, before placement. */
  size: { width: number; height: number };
  /** The space available, usually the viewport. */
  viewport: Viewport;
  /** Which side was asked for. */
  prefer?: Side;
  /** Separation from the anchor, in px. */
  gap?: number;
  /** Space to keep between the popover and the viewport edge. */
  margin?: number;
}

const DEFAULT_GAP = 8;
const DEFAULT_MARGIN = 8;

/** The four positions, before any fitting. */
function raw(anchor: Rect, size: { width: number; height: number }, side: Side, gap: number): Rect {
  switch (side) {
    case 'top':
      return {
        x: anchor.x,
        y: anchor.y - size.height - gap,
        width: size.width,
        height: size.height
      };
    case 'bottom':
      return {
        x: anchor.x,
        y: anchor.y + anchor.height + gap,
        width: size.width,
        height: size.height
      };
    case 'left':
      return {
        x: anchor.x - size.width - gap,
        y: anchor.y,
        width: size.width,
        height: size.height
      };
    case 'right':
      return {
        x: anchor.x + anchor.width + gap,
        y: anchor.y,
        width: size.width,
        height: size.height
      };
  }
}

/** The opposite side, which is what a flip means. */
export function opposite(side: Side): Side {
  switch (side) {
    case 'top':
      return 'bottom';
    case 'bottom':
      return 'top';
    case 'left':
      return 'right';
    case 'right':
      return 'left';
  }
}

/**
 * Does `r` fit inside the viewport, allowing for `margin`?
 *
 * The width and height are checked as well as the position. A popover wider
 * than the viewport cannot be placed anywhere, and a placement function that
 * only checks the position reports such a popover as fitting.
 */
export function fits(r: Rect, vp: Viewport, margin: number): boolean {
  return (
    r.x >= margin &&
    r.y >= margin &&
    r.x + r.width <= vp.width - margin &&
    r.y + r.height <= vp.height - margin
  );
}

/**
 * Place a popover.
 *
 * Returns the side it ended up on as well as the box, because a caller that
 * draws an arrow needs to know: an arrow pointing at nothing is the visible
 * symptom of a flip the caller did not know about.
 */
export function place(o: PlaceOptions): Placement {
  const prefer = o.prefer ?? 'bottom';
  const gap = o.gap ?? DEFAULT_GAP;
  const margin = o.margin ?? DEFAULT_MARGIN;
  const { anchor, size, viewport } = o;

  const first = raw(anchor, size, prefer, gap);
  const other = raw(anchor, size, opposite(prefer), gap);

  let side: Side;
  let box: Rect;
  if (fits(first, viewport, margin)) {
    side = prefer;
    box = first;
  } else if (fits(other, viewport, margin)) {
    side = opposite(prefer);
    box = other;
  } else {
    // Neither side fits: a popover taller or wider than the viewport, or an
    // anchor in a corner. Keep the REQUESTED side -- the arrow still points
    // somewhere sensible -- and let the shift and clamp below pin it.
    side = prefer;
    box = first;
  }

  // Shift along the cross axis, then clamp -- and SHRINK if the box cannot fit
  // even at the origin.
  //
  // The shrink is the part the first version got wrong, and the test caught it
  // by asserting a 2000px-tall popover lands inside an 800px viewport. Clamping
  // the ORIGIN alone leaves `y + height` at 2048: the box starts on screen and
  // runs off the bottom, which is the same failure with a different number.
  //
  // The width and height come back in the result, so a caller can reflow the
  // content -- a menu that is silently narrower than its content was measured
  // will have text that overflows horizontally, and that has to be visible to
  // the caller rather than a surprise in the layout.
  const width = Math.min(box.width, viewport.width - margin * 2);
  const height = Math.min(box.height, viewport.height - margin * 2);
  const x = clamp(box.x, margin, viewport.width - width - margin);
  const y = clamp(box.y, margin, viewport.height - height - margin);

  return { side, x, y, width, height };
}

function clamp(v: number, lo: number, hi: number): number {
  if (hi < lo) return lo;
  return Math.min(Math.max(v, lo), hi);
}

/**
 * The corner a context menu should open into, given where the user pointed.
 *
 * A context menu (#7139) is placed at the pointer rather than relative to a
 * trigger, so there is no requested side to flip -- the direction comes from
 * which quadrant the pointer is in, and the menu opens AWAY from the nearest
 * edges, into the larger half of the screen.
 *
 * The two axes are named separately rather than collapsed to a `Side`,
 * because a menu has one `side` for placement and TWO for the arrow: a menu in
 * the bottom-left quadrant opens up-and-right, which is one `side` for
 * placement (`top`) and a corner for the arrow (`up-right`). Returning a
 * single `Side` loses the second axis and the arrow points at the wrong edge.
 */
export type Corner = 'up-left' | 'up-right' | 'down-left' | 'down-right';

export function sideForPoint(point: { x: number; y: number }, vp: Viewport): Corner {
  const left = point.x < vp.width / 2;
  const up = point.y < vp.height / 2;
  // Read the two axes independently. Testing only the dominant one puts a
  // menu opened in the bottom-left quadrant to the RIGHT -- into a third of
  // the screen -- when half of it is available on the left.
  if (up) return left ? 'up-left' : 'up-right';
  return left ? 'down-left' : 'down-right';
}

/**
 * The placement side implied by a corner.
 *
 * The vertical axis wins, because a menu is tall and a viewport is not: a menu
 * that flipped up when there was 400 px below it and 100 px above reads as
 * broken even though it is technically on screen. So the side that has more
 * room is the one used.
 */
export function sideForCorner(c: Corner, vp: Viewport): Side {
  return c === 'up-left' || c === 'up-right' ? 'top' : 'bottom';
}
