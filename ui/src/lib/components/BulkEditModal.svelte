<!--
  The bulk-edit modal. Spec 10.7; plan T-P5-006 item 3.

  # The rule this component exists to enforce

  §10.7: a destructive action states its scope, and a bulk write names how many
  rows it is about to touch *before* the user confirms. Not after, not as a
  toast. Before.

  The reason is that the number the user can see is not the number the write
  will use. A select-all over 4,000 objects with three deselected has a client
  count of `unknown` (`selectedCount` refuses to guess), and a hand-picked set
  of twelve can be down to nine by the time the server counts. So the modal asks
  the server, and it renders `canApply` — which is false while the answer is
  unknown. A confirm button that is optimistically enabled and then reports
  'nothing matched' is a dead end with a progress bar.

  # Why the outcome has four states and not two

  The server returns three numbers and the component turns them into one of
  `done` / `partial` / `blocked` / `empty`. The split that earns its cost is
  `blocked` against `empty`: both have `applied === 0`, both would render as
  "nothing happened", and collapsing them tells a user whose selection is all
  unverified that their library is empty. `partial` is likewise not a failure —
  the server refused only what the caller had no right to touch, which is the
  correct behaviour of a correct write, and rendering it as a warning teaches
  users that bulk edit is unreliable.

  All of that logic is in `$lib/api/bulk.js` and tested there. This file holds
  layout, focus, and the keyboard.

  # What it does not do

  It does not compute a count, not even to display one, and it does not decide
  membership. Both belong to the server (`bulk.rs`) and to `selection.js`
  respectively. A count computed here is a number a user trusts about their
  library, and a client count is a guess.
-->
<script lang="ts">
  import { commands } from '$lib/api/commands-ui.js';
  import { NO_UNDO, canUndo, describe, type UndoState } from '$lib/api/undo.js';
  import {
    IDLE,
    canApply,
    classify,
    resultMessage,
    scopeMessage,
    type BulkOutcome,
    type BulkResult
  } from '$lib/api/bulk.js';
  import { isSelectAll, selectedCount, type Selection } from '$lib/api/selection.js';

  interface Props {
    open: boolean;
    selection: Selection;
    /** The ids currently loaded, for the hand-picked count. */
    loadedIds: readonly string[];
    /**
     * The server's count for a select-all, or `undefined` while it is in
     * flight. `undefined` is not "zero" and the modal treats it as unknown.
     */
    serverCount?: number;
    /** The tags a user may pick from. */
    tags: readonly { id: string; name: string }[];
    /** The outcome of the last applied write, or `undefined` before one. */
    outcome?: BulkOutcome;
    /**
     * The last write's error, or `null`.
     *
     * A prop rather than local state because the request belongs to the parent --
     * it owns the client, the tag list and the selection -- so the failure is
     * something the parent learns and this component is told. A local `let
     * error` would be write-only, which is the shape a stub takes: the branch
     * renders, the markup is tested, and no code can ever reach it.
     */
    error?: string | null;
    /** In flight: the button is disabled and the scope line is provisional. */
    busy?: boolean;
    /**
     * The last write's undo offer, or `NO_UNDO` before one.
     *
     * A prop, not local state, for the reason `error` is: the undo is a
     * round-trip the parent owns, and this component renders what it is told.
     */
    undo?: UndoState;
    onapply: (tagId: string) => void;
    /** Called when the user presses Undo. */
    onundo?: () => void;
    oncancel: () => void;
  }

  /**
   * The props object, kept.
   *
   * Destructured for readability *and* read as `p.x` wherever a value has to be
   * tracked inside an `$effect`. A destructured prop is a plain local: reading
   * it does not register a dependency, so an effect that closes over `open`
   * runs once at mount and never again. That is not a subtle lint, it is a modal
   * that never opens, and it is the reason this file holds the object.
   */
  const p: Props = $props();

  // Defaults applied to locals, because a default *is* a fallback read and the
  // `??` form below reads the same way at every use site.
  const selection = $derived(p.selection);
  const loadedIds = $derived(p.loadedIds);
  const serverCount = $derived(p.serverCount);
  const tags = $derived(p.tags);
  const outcome = $derived(p.outcome);
  const error = $derived(p.error ?? null);
  const busy = $derived(p.busy ?? false);
  const onapply = $derived(p.onapply);
  const undo = $derived(p.undo ?? NO_UNDO);
  const oncancel = $derived(p.oncancel);

  let chosen = $state<string>('');
  let dialog = $state<HTMLDialogElement | null>(null);

  // The <dialog> is opened with showModal(), not with an `open` attribute.
  // A plain `open` attribute gives a non-modal box with no backdrop, no Escape
  // handling and no focus trap, and `::backdrop` never renders -- so the modal
  // is a div that looks like a dialog and behaves like one that is not. The
  // native method is the only one that gives all four.
  //
  // Bidirectional on purpose: `onclose` fires when Escape or the native close
  // runs, and cancelling has to be able to close it too, or the user's only way
  // out is the button.
  //
  // `open` is read from the props object rather than from a destructured local.
  // Destructuring `{ open }` in the `let { ... } = $props()` line gives a plain
  // local, and a plain local read inside `$effect` is not a tracked dependency --
  // so the effect ran exactly once, at mount, and never again. The dialog was
  // wired correctly to a prop that could not change, and the symptom was a
  // modal that never opened no matter what the parent did. The props *object*
  // stays reactive, so `p.open` is the tracked read.

  /**
   * Claim the keyboard while this modal is open.
   *
   * `register(..., true)` pushes a frame for the scope if it is not already on
   * the stack, and `popScope(id)` removes it -- so the frame cannot outlive the
   * modal, because both happen in the same effect that opens and closes the
   * dialog. `popScope` refuses to pop a frame that is not on top, so if two
   * modals ever overlap this one's cleanup is a no-op rather than a corruption of
   * the stack.
   *
   * The `Escape` entry is a *declining* no-op: it wins the key, then returns
   * `false` so the browser still gets it. That is the whole trick. The top frame
   * has to win, or the app scope's `select.clear` runs behind an open modal --
   * but winning and then preventing is worse, because the native `<dialog>`
   * dismissal is the thing that should actually happen and a swallowed Escape is
   * a modal that will not close.
   *
   * Declining is not the same as not binding. A frame that binds nothing lets
   * `resolve` walk outward to the app frame and find `select.clear` there.
   */
  $effect(() => {
    commands.register(
      {
        id: 'bulk.escape',
        title: 'Close the bulk editor',
        detail: 'Dismiss this without writing',
        keywords: ['cancel', 'dismiss', 'close'],
        scope: 'bulk-edit',
        binding: { key: 'Escape', code: 'Escape', mods: [] },
        run: () => {
          // Decline. The dialog's own `oncancel` closes it, via the browser's
          // native Escape; this command's only job is to keep the app scope's
          // `select.clear` from running first.
          return false;
        }
      },
      true
    );
    return () => {
      commands.drop('bulk.escape');
      commands.popScope('bulk-edit');
    };
  });

  $effect(() => {
    const el = dialog;
    if (!el) return;
    if (p.open && !el.open) el.showModal();
    else if (!p.open && el.open) el.close();
  });

  // The result, from the server's three numbers. `undefined` until a write has
  // been applied, and then the modal is reporting rather than asking.
  //
  // Pre-write, there is no outcome yet, and `IDLE` is the state that means
  // "not asked". `canApply(IDLE)` is false -- so a button gated on the outcome
  // alone can never be pressed, because pressing it is what produces the
  // outcome. The gate has to be the *scope*, which is known before any write.
  let result = $derived<BulkResult>(outcome ? classify(outcome) : IDLE);

  // What the user believes they selected. `selectedCount` returns 'unknown' for
  // a select-all with no server count, and the scope line is written to say
  // "about" rather than to paper over it.
  let believed = $derived(selectedCount(selection, loadedIds, serverCount));

  /**
   * May the user press Apply?
   *
   * Two gates, and they are different questions:
   *
   *  - before a write: is there a scope? The server's count says yes, and the
   *    client believes too, so a hand-picked set opens an enabled button. A
   *    select-all whose count has not arrived is `'unknown'` and stays
   *    disabled, because confirming against a guess is the thing §10.7 is about.
   *  - after a write: did it reach anything? A 'blocked' or 'empty' result must
   *    not be re-pressable, or the user retries a write the server has already
   *    refused on the same grounds.
   */
  let applicable = $derived(
    outcome ? canApply(result) : believed !== 'unknown' && believed > 0
  );

  let scope = $derived(scopeMessage(selection, result, believed));
  let message = $derived(resultMessage(result));

  // A fresh open starts with nothing chosen. The error is *not* cleared here:
  // it is the parent's, and a modal that reopened still showing the previous
  // failure would be reporting an error the user is not currently causing. The
  // parent clears it by dropping the prop, which it must do anyway to clear the
  // outcome.
  $effect(() => {
    if (p.open) chosen = '';
  });

  function confirm() {
    if (!applicable || busy || chosen === '') return;
    onapply(chosen);
  }

  function onkeydown(e: KeyboardEvent) {
    // Escape is deliberately NOT handled here. `showModal()` gives the dialog a
    // native Escape that fires `close`, and this component's `onclose` is
    // already `oncancel`. A keydown handler doing it as well calls oncancel
    // twice, and the second call runs against a modal the parent has already
    // torn down.
    //
    // Enter confirms, because the modal has exactly one action. Guarded on
    // `applicable` and on a tag being chosen, so a stray Enter cannot fire a
    // write the user never saw scoped, and skipped when the select has focus
    // so choosing a tag with the keyboard does not also submit it.
    if (e.key === 'Enter' && (e.target as HTMLElement)?.tagName !== 'SELECT') {
      e.preventDefault();
      confirm();
    }
  }
</script>

<dialog
  bind:this={dialog}
  data-testid="bulk-modal"
  aria-label="Bulk edit"
  onclose={oncancel}
  onkeydown={onkeydown}
>
  <h2>Apply a tag</h2>

  <!-- The scope line. Above the control, not below it, and present before the
       write: §10.7 asks the action to name its scope, and a scope shown after
       the confirmation is a receipt, not a warning. -->
  <p data-testid="bulk-scope">{scope}</p>

  <label for="bulk-tag">Tag</label>
  <select id="bulk-tag" data-testid="bulk-tag-select" bind:value={chosen}>
    <option value="" disabled>Choose a tag…</option>
    {#each tags as tag (tag.id)}
      <option value={tag.id}>{tag.name}</option>
    {/each}
  </select>

  <!-- In the dialog, not a toast: the user is mid-action and a toast that
       outlives the modal reports a failure against a window they have closed. -->
  {#if error}
    <p data-testid="bulk-error" role="alert">{error}</p>
  {/if}

  <!-- After a write, and only then. `resultMessage` is null for 'idle' and
       'empty', so this block collapses to nothing in the asking state. -->
  {#if message}
    <p
      data-testid="bulk-result"
      data-state={result.state}
      class:warn={result.state === 'blocked'}
    >
      {message}
    </p>
  {/if}

  <!--
    The undo offer, IN the dialog rather than in a floating toast.

    A toast was the first design and it cannot work: this dialog is
    `showModal()`, which makes everything outside it inert, so a toast shown
    while the modal is open is unclickable. Playwright reported it as
    "bulk-modal intercepts pointer events", which is the browser saying the
    interaction is gone -- a user would call it a frozen page. And the error
    line above already states the principle: a toast that outlives a window the
    user has closed reports a failure against a dialog they are no longer in.

    The state and the wording come from `$lib/api/undo.js`; this is the same
    split as the rest of the file. Only `offerable` renders the button, and
    there is no undo operation in the client yet, so `onundo` reports a failure
    and the button does not come back -- see `undoLastWrite` in the page.
  -->
  {#if canUndo(undo)}
    <p data-testid="bulk-undo-line" class="undo-line">
      {describe(undo)}
      <button type="button" data-testid="bulk-undo" onclick={() => p.onundo?.()}>
        Undo
      </button>
    </p>
  {:else if undo.state === 'failed' || undo.state === 'superseded' || undo.state === 'spent'}
    <p data-testid="bulk-undo-line" class="undo-line undo-line-fail">
      {describe(undo)}
    </p>
  {/if}

  <footer>
    <button data-testid="bulk-cancel" onclick={oncancel}>Cancel</button>
    <button
      data-testid="bulk-apply"
      disabled={!applicable || busy || chosen === ''}
      onclick={confirm}
    >
      {busy ? 'Applying…' : 'Apply'}
    </button>
  </footer>
</dialog>

<style>
  dialog {
    border: 1px solid var(--edge, #444);
    border-radius: 6px;
    background: var(--surface, #1a1a1a);
    color: inherit;
    padding: 1rem;
    min-width: 22rem;
  }
  h2 {
    margin: 0 0 0.5rem;
    font-size: 1rem;
  }
  p {
    margin: 0 0 0.75rem;
  }
  /* A 'blocked' result is informational, not an error: the server did the
     right thing. Coloured as a warning because the user asked for something
     they cannot have, but not as a failure. */
  .warn {
    color: var(--warn, #d8a657);
  }
  [data-testid='bulk-error'] {
    color: var(--error, #d9534f);
  }
  .undo-line {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin: 0.25rem 0 0;
  }
  /* Only a failure is coloured. A superseded undo is the system having moved
     on, and colouring it would teach users that undo is unreliable. */
  .undo-line-fail {
    color: var(--warn, #d8a657);
  }
  footer {
    display: flex;
    gap: 0.5rem;
    justify-content: flex-end;
    margin-top: 1rem;
  }
</style>
