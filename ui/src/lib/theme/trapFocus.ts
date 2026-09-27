/**
 * A focus trap, as a Svelte action. T-P5-007, spec §10.8.

  # Why this is an action and not a component

  The behaviour is a property of a CONTAINER, not a thing with content. The
  Lightbox, the command palette, the context menu and the confirm dialog all
  need it, and none of them should have to implement it or remember to. A
  wrapper component would have to be the outermost thing in each of them, which
  puts a layout element in charge of content it knows nothing about.

  `use:trapFocus` also has the property that it cannot be forgotten: forgetting
  `tabindex="-1"` is one forgotten attribute, and forgetting an entire wrapper
  component is a different kind of mistake.

  # What it does, in order

  1. Records the element that had focus, so it can be restored.
  2. Moves focus to the first tab stop inside, or to the container itself.
  3. Traps Tab and Shift+Tab between the stops, wrapping at both ends.
  4. Pulls focus back if it escapes -- a click on the backdrop, or browser
     chrome. Without this the trap is a Tab handler, and a mouse user can walk
     straight out of it.
  5. Restores focus on destroy, if the opener is still connected.

  Step 4 is the one that is usually missing, and the one that matters for a
  mouse or touch user: a trap that only intercepts the keyboard is not a trap
  for the person clicking on the page behind.

  # Why `focusin` is on the document and not on the container

  A `focusin` on the container never fires for focus that has already left it.
  Listening on the document is the only way to notice the escape. It is
  registered in the CAPTURE phase so it runs before anything else can move
  focus again -- a bubble-phase handler is too late when another listener has
 * already redirected focus, which is what happens with a focus-guarding
 * library the dialog does not know about.
 */
import { initialFocus, nextFocusable, restoreTarget, tabbables } from './focus.js';

/**
 * Is this escape allowed?
 *
 * Defaults to "no" -- a backdrop click and the answer is come back. A dialog
 * that deliberately lets focus go (a non-modal popover) passes a predicate.
 */
export interface TrapOptions {
  allowEscape?: (el: Element) => boolean;
  /** Skip moving focus in, for a dialog that manages its own. */
  autoFocus?: boolean;
}

export function trapFocus(node: HTMLElement, options: TrapOptions = {}) {
  const { allowEscape, autoFocus = true } = options;

  const opener = document.activeElement as HTMLElement | null;
  let released = false;

  /**
   * A CSS selector that identifies `el`, or null.
   *
   * In order of how much they actually identify a thing.
   *
   * An `id` is unique by definition -- that is what it is for.
   *
   * Then a unique attribute value. `data-testid` is NOT unique: `grid-tile` is
   * on every tile in the grid, so `[data-testid="grid-tile"]` resolves to the
   * FIRST tile, and restoring focus there is worse than not restoring at all,
   * because it lands on the wrong row and looks correct. So a testid is only
   * usable when it is actually unique on the page, and the check is the whole
   * point of the function.
   *
   * Then the href, for a link: `?id=obj-3` is as identifying as the markup
   * gets, and two links to the same href are the same destination by
   * definition, so duplicates are not a false match here.
   *
   * Null when nothing identifies the element, which is a real answer and not a
   * failure: the caller then falls back to the node itself.
   */
  const anchorKey = (el: HTMLElement): string | null => {
    if (el.id) return `#${CSS.escape(el.id)}`;
    for (const attr of ['data-testid', 'data-id', 'data-lightbox-index']) {
      const value = el.getAttribute(attr);
      if (!value) continue;
      const sel = `[${attr}="${CSS.escape(value)}"]`;
      if (document.querySelectorAll(sel).length === 1) return sel;
    }
    if (el instanceof HTMLAnchorElement) {
      const href = el.getAttribute('href');
      if (href) return `a[href="${CSS.escape(href)}"]`;
    }
    return null;
  };

  /** The stops, re-read each time: the dialog's contents change. */
  const stops = (): HTMLElement[] =>
    tabbables(Array.from(node.querySelectorAll<HTMLElement>('*')));

  if (autoFocus) {
    queueMicrotask(() => {
      if (released) return;
      // Re-read rather than computing the target before the microtask: a
      // dialog whose children render in a child effect has none at action
      // time, and focusing a node that is about to be replaced is the same
      // failure as focusing one that has already been detached.
      const now = stops();
      (now.length > 0 ? now[0] : node).focus();
    });
  }

  /** Tab and Shift+Tab, wrapped at both ends. */
  function onKeydown(e: KeyboardEvent) {
    if (e.key !== 'Tab') return;
    const list = stops();
    if (list.length === 0) {
      // With no stops, letting the event through moves focus to <body> -- out
      // of the dialog, with the page behind it still visible. Holding Tab on
      // the container is the lesser evil, and the user can Tab away again once
      // the dialog has content.
      e.preventDefault();
      node.focus();
      return;
    }
    const from = document.activeElement as HTMLElement;
    const next = nextFocusable(list, from, e.shiftKey);
    e.preventDefault();
    if (next === null) {
      // Focus is not on a stop -- it is on the container, or it arrived from
      // outside. Send it to the first (or last) stop, which is what a browser
      // does when focus is nowhere in a dialog.
      (e.shiftKey ? list[list.length - 1] : list[0]).focus();
      return;
    }
    next.focus();
  }

  /** Focus that left without a Tab key. */
  function onFocusIn(e: FocusEvent) {
    if (released) return;
    const into = e.target as Element | null;
    if (into === null) return;
    if (node === into || node.contains(into)) return;
    if (allowEscape?.(into)) return;
    const list = stops();
    (list.length > 0 ? list[0] : node).focus();
  }

  document.addEventListener('keydown', onKeydown, true);
  document.addEventListener('focusin', onFocusIn, true);

  return {
    destroy() {
      released = true;
      document.removeEventListener('keydown', onKeydown, true);
      document.removeEventListener('focusin', onFocusIn, true);
      // Only if the opener is still connected. A dialog opened from a row it
      // deleted has no element to return to, and focusing a detached node
      // sends focus to <body> -- which is the exact "focus jumped to the top
      // of the page" complaint the restore exists to prevent.
      const back = restoreTarget(opener, (el) => el.isConnected);
      if (!back) return;
      // AFTER the DOM settles. This grid is VIRTUALIZED: closing the lightbox
      // re-renders it, the row window is recomputed, and the tile the user
      // clicked can be RECYCLED -- the original node detached and a new one
      // taking its place. Restoring synchronously focuses the old node, which
      // is already out of the document, and focus lands on <body>.
      //
      // It passes most of the time, which is what makes it a flake rather than
      // a failure: the recycle only happens when the recomputed window moves.
      // So the restore waits a microtask, by which time the new tile exists,
      // and falls back to the opener's position in the list when the node
      // itself is gone for good (a deleted row).
      // Waiting for the DOM is not enough here and cost three runs to find
      // out. The grid is VIRTUALIZED: closing the lightbox re-renders it, and
      // the tile can be RECYCLED -- the original node detached, a new one
      // taking its place. A microtask is often before the swap, so it focuses
      // the old node and focus lands on <body>. It is intermittent because the
      // recycle only happens when the recomputed window moves.
      //
      // So the anchor is not the node, it is a DESCRIPTION of the node, and
      // the restore re-finds it. A stable key beats a live reference: it
      // survives the swap by construction.
      //
      // `rAF` rather than a longer timeout, because the swap is a render and
      // the next frame is after it. A timeout would be a guess about how long
      // rendering takes, and a guess that is either too slow to be noticed or
      // too fast to be reliable.
      const key = anchorKey(back);
      requestAnimationFrame(() => {
        const target = key ? document.querySelector<HTMLElement>(key) : back;
        if (target?.isConnected) {
          target.focus();
          return;
        }
        // Nothing to go back to -- the row was deleted, or the page moved on.
        // Focusing <body> is the correct outcome here, and the alternative
        // (throwing, or focusing a detached node) is worse. The previous
        // version focused "some element somewhere", which put focus on an
        // unrelated control and looked like it had worked.
      });
    }
  };
}
