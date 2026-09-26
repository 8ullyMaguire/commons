<!--
  The command palette. T-P5-006 item 5. Spec 10.7 (#2542); plan §T-P5-006
  item 5.

  # It filters the registry, and that is the whole point

  Not a second list. If the palette had its own array of commands it would
  drift from the keyboard map within a release -- a command bound to a key and
  missing from the palette is the bug #2542 is really about, and it can only
  happen if the two lists are written separately. Here there is one registry, so
  the impossible thing is impossible.

  # Why the highlighted row is tracked by id

  By index it would be wrong the moment the list re-filters, which is on every
  keystroke. The user types one more character, the row under the cursor changes
  to something else, and Enter runs a command they never highlighted. By id, the
  highlight follows the command or is dropped -- never silently transferred to a
  different command.

  # Why Escape is handled here and not by the global resolver

  The global resolver is a *scope* system, and this is not a scope: the palette
  is modal over whatever opened it, including another modal. If the palette
  pushed a scope, closing it would have to pop exactly the right frame, and a
  palette opened from inside a frame that was itself a palette is the case where
  that goes wrong. Native `<dialog>` plus `oncancel` already gives correct
  Escape semantics for a modal, and using them means the palette needs no
  participation in the scope stack at all.
-->
<script lang="ts">
  import { commands, paletteRows, moveHighlight } from '$lib/api/commands-ui.js';
  import type { Command } from '$lib/api/commands.js';

  interface Props {
    open: boolean;
    onclose: () => void;
    /** How many objects are selected, for the commands that need some. */
    selected?: number;
    /**
     * How a chosen command is carried out.
     *
     * A prop, not an exported setter holding a `$state` function. The registry
     * holds ids and titles, not components, so *something* has to know how to
     * perform each command -- and a callback is that something, passed in.
     */
    onrun?: (c: Command) => void;
  }

  const { open, onclose, selected = 0, onrun }: Props = $props();

  let dialog = $state<HTMLDialogElement | null>(null);
  let query = $state('');
  /** The highlighted command's id, or `null` for none. */
  let highlighted = $state<string | null>(null);

  let rows = $derived(paletteRows(commands, query, selected));
  /** A disabled row is still shown, and is not selectable. */
  let runnable = $derived(rows.filter((r) => !r.disabled));
  let highlightedRow = $derived(rows.find((r) => r.command.id === highlighted) ?? null);

  // The highlight follows the list. Kept as an id for the reason in the header;
  // when the highlighted command is filtered out, the first runnable row takes
  // over, so the user is never left with a cursor on nothing after typing.
  $effect(() => {
    if (rows.length === 0) {
      highlighted = null;
      return;
    }
    if (highlighted === null || !rows.some((r) => r.command.id === highlighted)) {
      highlighted = (runnable[0] ?? rows[0]).command.id;
    }
  });

  // The open effect reads BOTH `open` and `dialog`, and both are load-bearing.
  //
  // `dialog` is populated by `bind:this`, which happens *after* the first effect
  // run. An effect that depends only on `open` therefore runs while `dialog` is
  // still `null`, calls `dialog?.showModal()` on nothing, and never runs again
  // -- so the dialog exists in the DOM and is never opened. The `?.` is what
  // makes it silent: a `dialog.showModal()` without the guard would have thrown
  // and been visible immediately.
  //
  // This is the same class of bug as the unsaved guard's: a `$state` written by
  // something other than the reactive graph, read in an effect that does not
  // list it as a dependency. Reading `dialog` here is what registers it.
  $effect(() => {
    const el = dialog;
    console.log('DIAG15 palette effect open=' + open + ' el=' + (el ? 'yes' : 'null'));
    if (open) {
      // A fresh open starts from a blank query. Carrying the last query over
      // means reopening lands the user in a filtered list they did not ask for,
      // with the cursor on whatever happens to be first.
      query = '';
      highlighted = null;
      el?.showModal();
    } else if (el?.open) {
      el.close();
    }
  });

  /**
   * Move the highlight by `delta` within the *runnable* rows.
   *
   * Runnable and not rows, so the arrow keys skip disabled ones. A disabled row
   * is a command that needs a selection and has none; landing the cursor on it
   * and having Enter do nothing is the worst of both, because the user cannot
   * tell whether the palette is broken or they are missing a selection.
   */
  function move(delta: number) {
    const i = runnable.findIndex((r) => r.command.id === highlighted);
    // A highlight on a disabled row has no index in `runnable`; -1 plus a
    // downward step lands on the first row, which is what the user means.
    const next = moveHighlight(i < 0 ? (delta > 0 ? -1 : 0) : i, delta, runnable.length);
    highlighted = runnable[next]?.command.id ?? null;
  }

  /**
   * The browser closed the dialog, so the state must agree.
   *
   * Native `<dialog>` handles Escape itself -- `cancel` then `close`, with no
   * code from us. But `open` is a prop owned by the layout, and it is still
   * `true` at that moment, so the effect above sees `open === true` and calls
   * `showModal()` again: the palette shuts and springs straight back open, and
   * from the outside Escape does nothing at all.
   *
   * A native dismissal is a change to the same fact the prop describes, and it
   * has to be pushed back in. `cancel` rather than `close` because `close` also
   * fires on our own `close()` call, which would be a redundant second write.
   */
  function oncancel(e: Event) {
    e.preventDefault();
    onclose();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      move(1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      move(-1);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      invoke();
    }
  }

  function invoke() {
    // Disabled rows do not run. The click path and the keyboard path both go
    // through here, because a palette where the row is visibly disabled but
    // Enter still runs it is worse than no palette.
    if (highlightedRow && !highlightedRow.disabled) {
      onclose();
      onrun?.(highlightedRow.command);
    }
  }
</script>

<dialog
  bind:this={dialog}
  class="palette"
  data-testid="palette"
  onkeydown={onkeydown}
  oncancel={oncancel}
>
  <input
    data-testid="palette-input"
    class="query"
    type="text"
    placeholder="Type a command"
    aria-label="Command palette"
    aria-controls="palette-list"
    bind:value={query}
  />
  <ul id="palette-list" data-testid="palette-list" role="listbox" aria-label="Commands">
    {#each rows as row (row.command.id)}
      <li
        role="option"
        aria-selected={row.command.id === highlighted}
        aria-disabled={row.disabled}
        data-testid="palette-row"
        data-id={row.command.id}
        data-disabled={row.disabled}
        class:highlighted={row.command.id === highlighted}
        class:disabled={row.disabled}
      >
        <button
          type="button"
          disabled={row.disabled}
          onclick={() => {
            highlighted = row.command.id;
            invoke();
          }}
        >
          <span class="title">{row.command.title}</span>
          {#if row.command.detail}
            <span class="detail">{row.command.detail}</span>
          {/if}
          <!--
            A command with no binding shows an em dash rather than an empty
            space: the user is being told "this exists, and it is not on your
            keyboard", which is the honest reading and is what makes the palette
            a complete shortcut map.
          -->
          <kbd data-testid="palette-binding">{row.binding === '' ? '—' : row.binding}</kbd>
        </button>
      </li>
    {:else}
      <li data-testid="palette-empty" class="empty">No matching command</li>
    {/each}
  </ul>
</dialog>

<style>
  .palette {
    width: min(34rem, 92vw);
    padding: 0;
    border: 1px solid var(--edge, #444);
    border-radius: 8px;
    background: var(--surface, #1a1a1a);
    color: inherit;
  }
  .query {
    width: 100%;
    box-sizing: border-box;
    padding: 0.75rem;
    border: 0;
    border-bottom: 1px solid var(--edge, #444);
    background: transparent;
    color: inherit;
    font: inherit;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0.25rem;
    max-height: 24rem;
    overflow: auto;
  }
  button {
    display: flex;
    gap: 0.75rem;
    align-items: baseline;
    width: 100%;
    padding: 0.4rem 0.5rem;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .highlighted button {
    background: var(--edge, #333);
  }
  .disabled button,
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .title {
    flex: 1;
  }
  .detail {
    opacity: 0.6;
    font-size: 0.85em;
  }
  kbd {
    font: inherit;
    font-size: 0.8em;
    opacity: 0.7;
  }
  .empty {
    padding: 0.75rem 0.5rem;
    opacity: 0.6;
  }
</style>
