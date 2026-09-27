/**
 * Focus: where it goes when a dialog opens, where it cannot go while one is
 * open, and where it comes back to when it closes.
 *
 * T-P5-007, spec §10.8 (focus management). This is the file; the axe scan
 * cannot check any of it, because axe knows nothing about what happened before
 * the dialog opened or what happens after it closes.
 *
 * # The three questions, and why each one has a different answer
 *
 * **Where does focus go when a dialog opens?** To the first thing inside it.
 * Not the dialog itself, even though `role="dialog"` is often given
 * `tabindex="-1"` for exactly this purpose: a focus ring drawn around an empty
 * box is a ring around nothing, and a screen reader announcing "dialog" with
 * no content selected is a user who has to guess where to start. The
 * exception is a dialog whose first control is destructive or a close button
 * -- then the dialog itself, so a stray Enter does not destroy anything.
 *
 * **Where can focus NOT go?** Outside, while the dialog is open. Without a trap,
 * Tab from the last control walks into the page behind, and the page behind is
 * visible. A user who cannot see that it is behind has no way to know they are
 * no longer in the dialog. The trap is the whole point of a modal, and this is
 * the piece a `tabindex="-1"` on the container does NOT give you.
 *
 * **Where does focus come back to?** The element that opened the dialog, and
 * only if it is still there. This is the one that gets skipped, and skipping it
 * is the single most common dialog defect: the user closes the dialog and is
 * dumped at the top of the document, having to Tab back to where they were.
 * "Only if it is still there" is the part that is usually wrong too -- a dialog
 * opened from a list row that the dialog itself deleted has no element to
 * return to, and focusing a detached node silently sends focus to `<body>`.
 *
 * # Why this is pure
 *
 * Every function here takes the elements it needs and returns what should
 * become focused. No `document`, no globals, no `querySelector`. That is what
 * makes "Tab from the last control wraps to the first" a table-driven test
 * rather than a browser, and it is why a change to this file cannot break a
 * page that uses it -- the only integration is the return value.
 */

/**
 * The slice of an element this module needs.
 *
 * `localName` rather than a synthetic attribute: the first version of this
 * read a `data-fake-tag` so the tests could pass a plain object, and that is a
 * test-shaped hole in a production interface -- it invites a caller to invent
 * the attribute and get a silently wrong answer. `localName` is on every
 * element in every browser.
 */
export interface Focusable {
  readonly localName: string;
  /** `tabindex` as a string, if it has one. */
  getAttribute(name: string): string | null;
  /** `disabled`, for the elements that have it. */
  readonly disabled?: boolean;
}

/**
 * Is this element in the tab order?
 *
 * The rules, in the order the browser applies them:
 *
 *  - `disabled` is out. A disabled button is not focusable and not reachable
 *    with a click either, so a trap that includes one strands the user on it.
 *  - `tabindex="-1"` is out of the TAB order but still programmatically
 *    focusable. That distinction is the whole reason the dialog container is
 *    allowed to be a focus target while not being a tab stop.
 *  - `tabindex` >= 0 is in, ahead of everything with no tabindex.
 *  - With no tabindex: only the natively focusable elements. `a` only counts
 *    with an `href` -- an anchor without one is not a link and not focusable,
 *    which is a real trap for a dialog built from a list of plain `<a>` tags.
 *
 * `inert` and `disabled` on an ancestor are deliberately NOT checked here. The
 * caller passes the elements it has already resolved, and resolving "is this
 * reachable" through a whole ancestor chain is the DOM's job -- doing it here
 * would mean re-implementing focusability, and a partial re-implementation is
 * worse than none: it is confidently wrong about the cases nobody tested.
 */
export function isTabbable(el: Focusable): boolean {
  if (el.disabled) return false;

  const tabindex = el.getAttribute('tabindex');
  if (tabindex !== null) {
    const n = Number(tabindex);
    // A non-numeric tabindex is invalid and the browser ignores the attribute
    // entirely, so the element falls back to being natively focusable. Treating
    // it as excluded is the conservative reading and matches what a user
    // experiences in the browsers that do this.
    if (Number.isNaN(n)) return false;
    return n >= 0;
  }

  const tag = el.localName.toLowerCase();
  const nativelyFocusable = ['button', 'input', 'select', 'textarea'].includes(tag);
  if (nativelyFocusable) return true;
  if (tag === 'a') return el.getAttribute('href') !== null;
  // `contenteditable` is focusable but has no attribute that says so
  // consistently, so it is the caller's job to mark it.
  return false;
}

/**
 * The tab stops inside a container, in DOM order.
 *
 * `DOM order` and not `tabindex order`: a positive `tabindex` reorders the
 * whole document, and honouring it here would make the trap disagree with what
 * the browser actually does when the user tabs out. The browser's order is the
 * only order that is correct.
 */
export function tabbables<T extends Focusable>(items: readonly T[]): T[] {
  return items.filter(isTabbable);
}

/**
 * Where `Tab` goes from `current`, or `null` to let the browser handle it.
 *
 * `null` means "do nothing": the caller returns without calling
 * `preventDefault`, and the browser moves focus itself. Used when the current
 * element is not one of ours, which happens when focus is outside the dialog
 * entirely -- a click on the backdrop, or focus arriving from the browser's
 * own chrome.
 *
 * The wrap is the part that matters. A trap that stops at the end is not a
 * trap, it is a dead end: the user presses Tab and nothing happens, which they
 * experience as the page having frozen.
 */
export function nextFocusable<T extends Focusable>(
  stops: readonly T[],
  current: T,
  shift: boolean
): T | null {
  if (stops.length === 0) return null;
  const i = stops.indexOf(current);
  if (i < 0) return null;
  const n = stops.length;
  return shift ? stops[(i - 1 + n) % n] : stops[(i + 1) % n];
}

/**
 * The element to focus when a dialog opens.
 *
 * `first` unless it is absent, in which case the dialog itself -- a dialog
 * whose controls are all `tabindex="-1"` is unreachable otherwise, and an
 * empty dialog with no focusable content still has to be dismissible.
 */
export function initialFocus<T extends Focusable, D extends Focusable>(
  stops: readonly T[],
  dialog: D
): T | D {
  return (stops[0] as T | undefined) ?? dialog;
}

/**
 * Whether focus may be restored to `opener`, and to what.
 *
 * `null` when it may not. The two reasons, both of which happen in this app:
 * the opener was a row in a list the dialog just deleted, or the opener was
 * inside a panel the dialog just closed. Focusing a detached element throws in
 * some engines and silently resets to `<body>` in others, and "focus went to
 * the top of the page" is the bug this whole file exists to prevent -- so the
 * check is made here rather than left to the caller's `if (opener)`.
 */
export function restoreTarget<T extends Focusable>(
  opener: T | null | undefined,
  isConnected: (el: T) => boolean
): T | null {
  if (!opener) return null;
  return isConnected(opener) ? opener : null;
}
