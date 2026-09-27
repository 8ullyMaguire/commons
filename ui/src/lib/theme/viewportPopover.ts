/**
 * Popovers, as geometry plus a Svelte action. T-P5-007, spec 10.8
 * (viewport-constrained popovers, stash#4667).
 *
 * # The bug this exists to stop
 *
 * A menu positioned with CSS is positioned against ONE thing: its normal flow
 * position. Open it near the bottom of the window and it renders off-screen,
 * and the user's fix is to scroll, which is not a fix. #4667 is exactly that
 * report, on a page where the popover can be opened from anywhere.
 *
 * # Why the geometry is not in the component
 *
 * `popover.ts` is the arithmetic: given an anchor rect, a popover size and a
 * viewport, which side and corner. It is pure and it is table-tested in
 * `ui/tests/popover.test.ts`. This file is only the part that needs a browser:
 * measure, place, reposition on scroll and resize, and put the node back the
 * way it was found.
 *
 * Doing it the other way round -- measuring inside a component and testing it
 * only through Playwright -- means the interesting cases (a 1px-tall
 * viewport, an anchor larger than the viewport, four sides all overflowing)
 * are all but untestable, because a test cannot make a real window that small.
 */

import { place, type Placement, type Side } from './popover.js';

export interface PopoverOptions {
  /**
   * Which side to try first. The rest of the sides are tried in order, and the
   * one with the most room wins if the preferred one does not fit.
   */
  side?: Side;
  /**
   * The gap between anchor and popover, in px. Applied on every side, so it is
   * part of the fit arithmetic rather than a margin on one edge.
   */
  margin?: number;
  /**
   * Element to position against. Defaults to the element the action is
   * attached to, which is the common case: a button that owns a menu.
   */
  anchor?: HTMLElement;
  /**
   * Called after every placement, including the ones caused by scrolling.
   * The popover is positioned with inline styles, so a host that needs to
   * know where it ended up -- to draw an arrow, say -- has no other way.
   */
  onplace?: (placement: Placement) => void;
}

export interface PopoverHandle {
  /** Re-measure and re-place. Cheap; safe to call on any layout change. */
  update(): void;
}

/**
 * Position an element against an anchor, inside the viewport.
 *
 * Attached to the POPOVER, not the anchor: the thing that needs coordinates
 * is the thing being moved, and an action on the popover means any element can
 * be a popover without its parent knowing.
 */
export function viewportPopover(
  node: HTMLElement,
  options: PopoverOptions = {}
): PopoverHandle {
  const { side = 'bottom', margin = 8, onplace } = options;

  let anchorEl: HTMLElement | null = null;
  const resolveAnchor = (): HTMLElement | null => options.anchor ?? anchorEl;

  // Recorded so `destroy` can restore the node exactly. An action that leaves
  // inline styles behind is a bug that shows up in the next component to reuse
  // the node, a long way from here.
  const originalStyle = node.getAttribute('style');
  let last: Placement | null = null;

  function update() {
    const anchor = resolveAnchor();
    if (!anchor) return;

    // `position: fixed` and the popover's own coordinates. Fixed rather than
    // absolute because the anchor may be inside a scrolled or transformed
    // ancestor, and absolute would then be measured against the wrong box.
    // The trade is that the popover does not travel with the page -- which is
    // why it re-places on scroll instead of assuming it will.
    const a = anchor.getBoundingClientRect();
    const rect = node.getBoundingClientRect();

    // Measured AFTER the previous placement is applied, so a popover that
    // changed size because of its content is measured at its current size
    // rather than the one it had when it was empty. This is why `update` is
    // safe to call repeatedly: the first call measures, later calls correct.
    const vp = { width: window.innerWidth, height: window.innerHeight };

    const p = place({
      anchor: { x: a.left, y: a.top, width: a.width, height: a.height },
      size: { width: rect.width, height: rect.height },
      viewport: vp,
      prefer: side,
      gap: margin,
      margin
    });

    node.style.position = 'fixed';
    node.style.left = `${Math.round(p.x)}px`;
    node.style.top = `${Math.round(p.y)}px`;
    // `max-width`/`max-height` are what keep a popover from being wider than
    // the window on a narrow screen. Setting them here rather than in CSS
    // because the available space is what `place` just computed, and CSS
    // cannot read that.
    node.style.maxWidth = `${Math.max(0, vp.width - margin * 2)}px`;
    node.style.maxHeight = `${Math.max(0, vp.height - margin * 2)}px`;
    node.dataset.popoverSide = p.side;

    if (p.side !== last?.side || p.x !== last?.x || p.y !== last?.y) {
      last = p;
      onplace?.(p);
    }
  }

  // Scroll and resize both change the answer without changing the DOM, and a
  // popover that does not follow is the bug. `capture` so a scrolling
  // ANCESTOR is caught too -- a menu inside a scrolling panel must move with
  // the panel, and listening on `window` alone misses it.
  const reposition = () => update();
  window.addEventListener('scroll', reposition, true);
  window.addEventListener('resize', reposition);

  // Fonts and images settle after first paint and change the popover's size.
  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => update());
    ro.observe(node);
    update();
  } else {
    update();
  }

  return {
    update,
    destroy() {
      window.removeEventListener('scroll', reposition, true);
      window.removeEventListener('resize', reposition);
      if (originalStyle === null) node.removeAttribute('style');
      else node.setAttribute('style', originalStyle);
      delete node.dataset.popoverSide;
    }
  };
}

/** `use:popover={{ anchor }}` reads better than the long name at the call site. */
export const popover = viewportPopover;
