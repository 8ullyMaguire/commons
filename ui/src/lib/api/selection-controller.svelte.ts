/*
  The selection as a Svelte-facing controller. Spec 10.6/10.7, plan T-P5-006.

  # Why the `.svelte.ts` extension

  Runes (`$state`) only compile in files Svelte's compiler owns, which means a
  `.svelte.ts` suffix. A plain `.ts` file with `$state` in it fails at runtime
  with "$state is not defined" -- a page error, not a build error, because the
  Vite plugin only processes files it recognises as Svelte modules. The build
  succeeds and the route renders nothing, which is the exact shape of the
  "build succeeded and shipped nothing" failure this repo has hit before.

  # Why a controller and not logic in the component

  `selection.ts` is pure: it takes a value and returns a value, which is what
  makes it mutable and testable. A Svelte component needs the other half -- a
  reactive value plus a way for a parent to observe changes -- and putting that
  half inside a `.svelte` file means the file cannot be unit-tested at all.

  So the controller is a small class in a `.ts` file, holding the value in
  Svelte's `$state` runes via `$state` from `svelte`. It is the only place
  where "a selection changed" and "a re-render should happen" are wired
  together, and it is the only place that knows about either.

  # Why `onresultchange` is explicit rather than derived

  A prune belongs to a change of *result*, not to a change of rows. Rows change
  when a page lands; a selection made on page 1 must survive paging to page 2.
  An `$effect` over `rows` conflates the two and clears the selection on every
  page load, which is the single most annoying bug this model could have.

  So the caller says when the result changed. `pruneTo` is the only entry
  point that shrinks a selection, and it is not reachable from a row update.
 */

import { emptySelection, prune, type ObjectId, type Selection } from './selection.js';

export class SelectionController {
  #sel = $state<Selection>(emptySelection());

  /** Ids to report as dropped by the most recent prune, for display. */
  #dropped = $state(0);

  get selection(): Selection {
    return this.#sel;
  }

  get dropped(): number {
    return this.#dropped;
  }

  /** Replace the value. Used by transitions that are not toggles. */
  set(next: Selection): void {
    this.#sel = next;
  }

  /**
   * The result set changed. Prune, and keep the dropped count so the caller can
   * show it.
   *
   * The count is state rather than a return value because the caller is a
   * template, and a template cannot hold a value across the re-render that
   * follows. Returning it would work exactly once and then lose it.
   */
  pruneTo(presentIds: ReadonlySet<ObjectId>): void {
    const { selection, dropped } = prune(this.#sel, presentIds);
    this.#sel = selection;
    this.#dropped = dropped;
  }

  /** Acknowledge the notice, so a second prune of zero does not re-show it. */
  clearDropped(): void {
    this.#dropped = 0;
  }
}
