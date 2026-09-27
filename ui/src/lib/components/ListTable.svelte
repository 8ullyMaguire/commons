<!--
  The list view, and the first surface that shows a selection. Spec 10.4 (C49),
  10.6 (C53); plan T-P5-006 items 1-2.

  # Why this is a table and not the grid

  Bulk editing needs a row that says which fields exist and which are empty. A
  tile has a cover and a title and no room for "rating: 4, organized: yes,
  duration: 12m04s", so a bulk modal that opens over tiles is working from data
  the user cannot see. The table is where "select these twelve and rate them" is
  possible.

  # What this file owns, and what it does not

  It owns layout, focus, the keyboard, and the anchor for a shift-click. It owns
  no selection rule: every question of the form "is this row selected", "how
  many are selected", and "extend to here" is a call into
  `$lib/api/selection.js`. A table that kept its own `selectedIds` would put the
  rules back where they cannot be mutated or unit-tested.

  # The three behaviours that have their own test

    - a row checkbox reflects the *mode*: under a select-all every box is checked
      even for rows not yet loaded, and the count reads "all" rather than a
      number the user would then trust as a total.
    - shift-click extends a range over the *loaded* rows and lands as ids, so a
      page load does not move it. The span takes its first row's state, via
      `setSelected` -- a `reduce` of `toggle` would deselect the rows in the
      span that were already selected, the opposite of extending a selection.
    - a change of result prunes the selection and *reports* what it dropped.

  # Why the select-all control is a button and not a checkbox

  A header checkbox that means "all of them" and a row checkbox that means "this
  one" are different controls wearing the same widget. The header one is
  tri-state over the whole query, and its indeterminate state is a real case:
  some loaded rows selected, others not. A button cannot be read as "toggle me"
  by anything expecting a checkbox's two-state contract, which is the ambiguity
  worth designing away.
-->
<script lang="ts">
  import {
    isSelectAll,
    isSelected,
    selectedCount,
    selectAll,
    setSelected,
    toggle,
    type ObjectId,
    type Selection
  } from '$lib/api/selection.js';
  import { SelectionController } from '$lib/api/selection-controller.svelte.js';
  import type { ObjectRow } from '$lib/api/client.js';

  interface Props {
    rows: readonly ObjectRow[];
    density?: number;
    /** The server's count for the current filter, when it is known. */
    totalCount?: number;
    /**
     * A predicate answering "does the current query match this id?".
     *
     * Defaulting to "matches nothing" is deliberate. A select-all with no
     * predicate is not a select-all of anything, and defaulting to "matches
     * everything" would make a component that forgot to pass the prop select
     * the whole library.
     */
    matchesQuery?: (id: ObjectId) => boolean;
    /** Called when the selection changes, for a parent that needs to act. */
    onchange?: (selection: Selection) => void;
  }

  let { rows, density = 240, totalCount, matchesQuery = () => false, onchange }: Props =
    $props();

  const ctl = new SelectionController();
  const selection = $derived(ctl.selection);

  /**
   * The anchor for a shift-click range. Null until the first plain click.
   *
   * Local to this component and deliberately not part of the selection value: it
   * is a cursor, not a choice, and putting it in the value would force
   * `setPicked` and `prune` to preserve a cursor they know nothing about.
   */
  let anchor: ObjectId | null = null;

  /**
   * How many are selected, for display.
   *
   * `selectedCount` returns `'unknown'` for a select-all with no server count.
   * Rendering that as a number is the bug the string exists to prevent, so it
   * is rendered as a word.
   */
  const count = $derived(selectedCount(selection, rows.map((r) => r.id), totalCount));

  const display = $derived(
    count === 'unknown' ? `all${totalCount ? ` ${totalCount}` : ''}` : String(count)
  );

  const allLoadedSelected = $derived(
    rows.length > 0 &&
    (isSelectAll(selection) || rows.every((r) => isSelected(selection, r.id, matchesQuery)))
  );

  const someLoadedSelected = $derived(
    !allLoadedSelected && rows.some((r) => isSelected(selection, r.id, matchesQuery))
  );

  function commit(next: Selection) {
    ctl.set(next);
    onchange?.(next);
  }

  function ontoggle(id: ObjectId) {
    commit(toggle(selection, id));
  }

  /**
   * Extend the selection from the anchor to `to`.
   *
   * Returns nothing, but sets `rangeHandled` when it applied a change, so the
   * checkbox's own `onchange` knows not to toggle again. A range that did not
   * apply anything -- no anchor, or an anchor that is no longer loaded -- is
   * reported the other way, and the change handler does the toggle.
   */
  function onrange(to: ObjectId) {
    if (anchor === null) {
      ontoggle(to);
      anchor = to;
      rangeHandled = false;
      return;
    }
    const a = rows.findIndex((r) => r.id === anchor);
    const b = rows.findIndex((r) => r.id === to);
    if (a < 0 || b < 0) {
      // The anchor is no longer loaded, so the range is undefined. Toggle the
      // one row rather than guessing a range across a page boundary, and let
      // the checkbox's own change stand rather than doing it twice.
      rangeHandled = true;
      ontoggle(to);
      anchor = to;
      return;
    }
    const [lo, hi] = a < b ? [a, b] : [b, a];
    const span = rows.slice(lo, hi + 1);
    const makeSelected = !isSelected(selection, span[0].id, matchesQuery);
    rangeHandled = true;
    commit(
      setSelected(
        selection,
        span.map((r) => r.id),
        makeSelected
      )
    );
    anchor = to;
  }

  /**
   * Set when a gesture has already applied the change and the checkbox's own
   * `onchange` must not toggle again.
   *
   * A flag rather than a parameter because the two are separate Svelte props:
   * the click handler decides *which* gesture this is, and the change handler
   * needs to know whether it has been acted on. Reading `event.shiftKey` again
   * in `onchange` does not work, because `change` carries no modifier state at
   * all.
   */
  let rangeHandled = false;

  function onselectall() {
    anchor = null;
    commit(isSelectAll(selection) ? {} : selectAll({}));
  }

  /**
   * A change of *result* prunes the selection.
   *
   * Exposed as a function the parent calls, rather than derived from `rows`:
   * rows change when a page lands, and an `$effect` over them would clear a
   * selection on every page load. Paging is not a change of filter, and the two
   * must not share a trigger. See `selection-controller.ts`.
   */
  export function onresultchange() {
    ctl.pruneTo(new Set(rows.map((r) => r.id)));
  }
</script>

<div
  class="list"
  data-testid="list-table"
  data-selected={count === 'unknown' ? 'all' : String(count)}
  data-dropped={ctl.dropped}
>
  <div class="head">
    <button
      class="head-select"
      data-testid="select-all"
      data-state={allLoadedSelected ? 'all' : someLoadedSelected ? 'some' : 'none'}
      aria-pressed={allLoadedSelected}
      onclick={onselectall}
    >
      {allLoadedSelected ? '✓' : someLoadedSelected ? '–' : ''}
      <span class="visually-hidden">Select everything</span>
    </button>
    <span>Title</span>
    <span>Kind</span>
    <span>Date</span>
    <span>Rating</span>
  </div>

  {#if ctl.dropped > 0}
    <p class="note" data-testid="prune-note" role="status">
      {ctl.dropped} selected item{ctl.dropped === 1 ? '' : 's'} no longer match the filter
    </p>
  {/if}

  {#each rows as row (row.id)}
    <div
      class="row"
      data-testid="list-row"
      data-id={row.id}
      data-selected={isSelected(selection, row.id, matchesQuery)}
      style:min-height="{Math.round(density * 0.4)}px"
    >
      <!--
        A one-way `checked` attribute does not survive a re-render in Svelte:
        the DOM property is what a user clicking the box changes, and the
        attribute is only re-applied when the *expression* changes. A click
        that changes the expression re-applies it, and a click that does not --
        or any re-render for another reason -- leaves the box showing the
        user's click rather than the selection. The `onchange` is what makes
        the expression change, so the two have to be paired.
      -->
      <!--
        The range and the checkbox's own toggle are BOTH allowed to act, and
        that is the design rather than a race that happens to work.

        Two earlier versions each got this wrong in opposite directions:

        1. `preventDefault` in the click handler, with `onchange` left to run.
           It does run -- `preventDefault` on a click stops the native toggle,
           not the event -- so the clicked row was toggled off *before* the
           range was computed, and since that row is usually the anchor, the
           range decided from a selection that had just lost its first row. A
           shift-click that looked like a range and cleared the selection.

        2. A `suppressChange` flag, so only the range acted. The model was then
           right and the *box* was wrong: `preventDefault` had stopped the
           native toggle, and a one-way `checked` attribute is only re-applied
           when its expression changes for that input. The row the range ended
           on kept the browser's old value -- model said selected, box said
           unchecked, and every bulk action reads the model. A user could not
           select the row they were looking at.

        Letting the change through agrees with the range by construction: the
        range has already decided this row's state, and the checkbox's own
        toggle arrives at the same answer. `onrange` returns whether the change
        was a range so `onchange` can skip the redundant toggle when the range
        did *not* include this row -- which happens when the anchor is off
        screen and the span is undefined.
      -->
      <input
        type="checkbox"
        data-testid="row-select"
        checked={isSelected(selection, row.id, matchesQuery)}
        onclick={(e) => {
          if (e.shiftKey) {
            onrange(row.id);
          } else {
            anchor = row.id;
          }
        }}
        onchange={() => {
          // Only a plain click reaches this as a *toggle*. A shift-click has
          // already been applied by `onrange`, which sets this flag.
          if (rangeHandled) {
            rangeHandled = false;
            return;
          }
          ontoggle(row.id);
        }}
        onkeydown={(e) => {
          if (e.shiftKey) {
            // Keyboard ranges too. A keypress produces no click and so no
            // change, and without this the range would be a mouse-only feature.
            e.preventDefault();
            rangeHandled = true;
            onrange(row.id);
          }
        }}
        aria-label={`Select ${row.title ?? row.id}`}
      />
      <span class="title">{row.title ?? 'Untitled'}</span>
      <span class="kind">{row.kind}</span>
      <span class="date">{row.date ?? ''}</span>
      <span class="rating">{row.rating ?? ''}</span>
    </div>
  {/each}

  <p class="count" data-testid="selection-count">{display} selected</p>
</div>

<style>
  .list {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: auto;
  }
  .head,
  .row {
    display: grid;
    grid-template-columns: 2.5rem 1fr 6rem 8rem 4rem;
    align-items: center;
    gap: 0.5rem;
    padding: 0 0.5rem;
  }
  .head {
    position: sticky;
    top: 0;
    /* #111 was a hard-coded dark. With the text now on `var(--muted)`, which
       is theme-aware, the two disagree: light-theme text on a dark band is
       2.85:1 and fails AA, which the axe scan found. `var(--surface)` is the
       token for "one step off the page", and it is what this row always
       meant. */
    background: var(--surface);
    color: var(--muted);
    font-size: 0.8rem;
    border-bottom: 1px solid var(--border);
    z-index: 1;
  }
  .row {
    border-bottom: 1px solid var(--border);
  }
  .row[data-selected='true'] {
    background: var(--surface);
  }
  .head-select {
    width: 1.1rem;
    height: 1.1rem;
    background: none;
    border: 1px solid var(--border);
    color: var(--fg);
    cursor: pointer;
  }
  .head-select[data-state='some'] {
    border-color: var(--accent);
  }
  .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .kind,
  .date,
  .rating {
    color: var(--muted);
    font-size: 0.85rem;
  }
  .count {
    padding: 0.5rem;
    /* Was #888, which is 3.5:1 on the dark surface -- below AA for text this
       size, and it was the only axe violation the scan found on /list. The
       token is 7.4:1. The comment is here because "grey" is where every
       component's colours came from and the replacement is not obvious. */
    color: var(--muted);
    font-size: 0.85rem;
  }
  .note {
    padding: 0.5rem;
    color: #fc6;
    background: #2a2410;
    font-size: 0.85rem;
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
</style>
