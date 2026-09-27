<!--
  A multi-value field with right-click paste. Spec 10.10, plan T-P5-006 item 11;
  #7139.

  # The problem

  §9.4 is about tag organization, and its edit boxes take many values. Copying a
  list of tags out of a spreadsheet and pasting it into one of those boxes did
  nothing, because the box was a single `<input>` and a paste is a single
  string. #7139 is that gap.

  # The decisions, and where they live

  All of them are in `$lib/api/paste.js` and tested there at their boundaries:
  what separates values, whether a separator inside quotes is data, what counts
  as a duplicate, and the sentence that tells the user what is about to happen.

  This file holds three things only a component can do:

   1. **the right-click menu**, because `contextmenu` has no pure equivalent;
   2. **the preview before the commit**, because it must be dismissible and the
      paste must not land until the user says so -- a paste into a 400-tag
      field is not obviously reversible by hand;
   3. **the keyboard**, because Esc must cancel and the menu must trap focus.

  # Why the menu has items rather than a bare "paste"

  A right-click menu with one item is a worse paste than a click on a button,
  and `navigator.clipboard.readText()` needs a user gesture plus a permission
  that can be denied. The `paste` event carries the text already, so the menu
  offers Paste when the browser will give it to us and says so plainly when it
  will not. Offering a menu item that always fails is worse than offering none.

  # What it does not do

  It does not persist anything. `onchange` hands the new list to the parent and
  the parent's draft is what gets saved, so this component has no store and no
  idea what a tag is.
-->
<script lang="ts">
  import { addCount, parsePaste, preview, type PasteResult } from '$lib/api/paste.js';

  interface Props {
    /** The field's label, for the aria name and the menu heading. */
    label: string;
    /** What the field currently holds. */
    values: readonly string[];
    onchange: (values: readonly string[]) => void;
  }

  const p = $props<Props>();

  /**
   * The clipboard text, once the browser has handed it over.
   *
   * `null` until a paste event has fired, and `null` means "nothing to paste" --
   * not "the clipboard is empty". A right-click on a field the user has never
   * focused has no clipboard text, and inventing an empty paste would open a
   * dialog saying "nothing to paste" about text the user cannot see.
   */
  let pasted = $state<string | null>(null);
  let menuOpen = $state(false);
  let menuEl = $state<HTMLElement | null>(null);
  let fieldEl = $state<HTMLElement | null>(null);

  const parsed = $derived<PasteResult | null>(pasted === null ? null : parsePaste(pasted, p.values));
  const addable = $derived(parsed === null ? 0 : addCount(parsed));

  function openMenu(event: MouseEvent) {
    // The browser's own menu is suppressed because it contains exactly one
    // item this field offers better. A right-click menu beside the system's is
    // two menus for one action.
    event.preventDefault();
    menuOpen = true;
  }

  /**
   * Ask the browser to paste into the field.
   *
   * `document.execCommand('paste')` is deprecated and only works from a user
   * gesture, which this is -- a click on a menu item. It is used here because
   * the alternative, `navigator.clipboard.readText()`, is an async permission
   * prompt that can be denied, and a denied prompt leaves the user staring at a
   * menu that closed and nothing pasted.
   *
   * When the browser refuses (Firefox without an extension, a headless run), the
   * field is focused so the user's own Ctrl-V works, and the menu closes. That
   * is the honest failure: the path is still one keystroke away, and the item
   * did not claim a success it did not have.
   */
  function requestPaste() {
    menuOpen = false;
    fieldEl?.focus();
    const ok =
      typeof document !== 'undefined' &&
      typeof document.execCommand === 'function' &&
      document.execCommand('paste');
    if (!ok) {
      // Nothing to do but leave the field focused and ready.
      // The comment is the whole behaviour: there is no error to show because
      // nothing failed that the user can act on -- the next Ctrl-V works.
    }
  }

  function onPaste(event: ClipboardEvent) {
    const text = event.clipboardData?.getData('text/plain');
    if (text === undefined || text === '') return;
    event.preventDefault();
    pasted = text;
    menuOpen = false;
  }

  function apply() {
    if (parsed === null) return;
    p.onchange([...p.values, ...parsed.values.map((v) => v.value)]);
    pasted = null;
  }

  function cancel() {
    pasted = null;
    menuOpen = false;
  }

  function oncancelmenu() {
    menuOpen = false;
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      // Esc closes, and closes the PREVIEW first: §10.7 names Esc-closes-modals
      // explicitly, and a user who pastes by accident needs one Esc to undo the
      // paste and a second to close the menu.
      if (pasted !== null) {
        event.stopPropagation();
        cancel();
        return;
      }
      menuOpen = false;
    }
  }

  /** Move focus into the menu when it opens, so a keyboard user is not stranded. */
  $effect(() => {
    if (menuOpen && menuEl !== null) {
      const first = menuEl.querySelector<HTMLElement>('button');
      first?.focus();
    }
  });
</script>

<!--
  `onpaste` on the field itself, not only on the menu. A user who already has
  the field focused and presses Ctrl-V expects it to work, and #7139 is about
  the right-click path only because that was the gap -- not because the
  keyboard path was worth removing.
-->
<div class="multi" data-testid="multi-value" onkeydown={onkeydown}>
  <span class="label" id="multi-label">{p.label}</span>

  <ul data-testid="multi-values" aria-labelledby="multi-label">
    {#each p.values as value (value)}
      <li>{value}</li>
    {/each}
  </ul>

  <div
    class="field"
    role="textbox"
    tabindex="0"
    aria-labelledby="multi-label"
    data-testid="multi-field"
    bind:this={fieldEl}
    oncontextmenu={openMenu}
    onpaste={onPaste}
  >
    <!--
      A real, focusable button rather than a div with a click handler, for the
      reason the whole accessibility section of §10.8 exists: a mouse-only
      affordance is a feature a keyboard user does not have.
    -->
    <button
      type="button"
      data-testid="multi-menu-button"
      aria-haspopup="menu"
      aria-expanded={menuOpen}
      onclick={() => (menuOpen = !menuOpen)}
    >
      Add values
    </button>
  </div>

  {#if menuOpen}
    <div
      class="menu"
      role="menu"
      data-testid="multi-menu"
      bind:this={menuEl}
      oncontextmenu={oncancelmenu}
    >
      <!--
        The one item, and it works by asking the browser for the clipboard
        rather than by reading it: `navigator.clipboard.readText()` needs a
        permission that can be denied, and a denied read is a menu item that
        does nothing. The `paste` event carries the text with no permission at
        all, so the item dispatches a real one on the field and lets the
        browser's own handler fill it in.

        That is why this is a button and not `oncontextmenu -> paste`: a
        synthetic `ClipboardEvent` with no clipboard data is indistinguishable
        from an empty clipboard, so the field's handler treats it as nothing and
        the menu silently does nothing -- which is #7139 again, in a new place.
      -->
      <button
        type="button"
        role="menuitem"
        data-testid="multi-paste-item"
        onclick={requestPaste}
      >
        Paste values
      </button>
    </div>
  {/if}

  <!--
    The preview, in the field rather than in a dialog. It is one sentence and
    two buttons, and a modal for that is a modal the user dismisses without
    reading — which is how a 400-row paste gets applied unexamined.
  -->
  {#if parsed !== null}
    <div class="preview" data-testid="multi-preview" data-state={addable > 0 ? 'ready' : 'empty'}>
      <p data-testid="multi-preview-text">{preview(parsed)}</p>
      <button
        type="button"
        data-testid="multi-preview-apply"
        disabled={addable === 0}
        onclick={apply}
      >
        {addable === 0 ? 'Nothing to add' : `Add ${addable}`}
      </button>
      <button type="button" data-testid="multi-preview-cancel" onclick={cancel}>Cancel</button>
    </div>
  {/if}
</div>
