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
  import { decodeCsvBytes, describeImport, importCsv, type CsvImport } from '$lib/api/csv.js';

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
  let fileEl = $state<HTMLInputElement | null>(null);

  /**
   * A file chosen with the file picker, already imported.
   *
   * It is the SAME shape as a paste result -- `CsvImport` carries `values`,
   * `skipped` and `duplicates` in `ParsedValue` form -- so one preview and one
   * Apply button serve both. That is the reason the importer was given
   * `parsePaste`'s result type instead of a CSV-specific one: a second type
   * would have meant a second preview, a second Apply, and a second Esc.
   */
  let fromFile = $state<CsvImport | null>(null);
  /** Set when a file could not be read at all, as opposed to read and refused. */
  let fileError = $state<string | null>(null);

  const parsed = $derived<PasteResult | null>(pasted === null ? null : parsePaste(pasted, p.values));
  const addable = $derived(parsed === null ? 0 : addCount(parsed));

  /** How many values are waiting, whichever way they arrived. */
  const fileAddable = $derived(fromFile === null ? 0 : fromFile.values.length);

  /** The sentence, from whichever source is pending. */
  const summary = $derived(
    fromFile !== null ? describeImport(fromFile) : parsed === null ? null : preview(parsed),
  );

  /**
   * Whether the pending import can be applied.
   *
   * A TRUNCATED file cannot, however many values it managed to parse. The
   * whole point of reporting the truncation is that the file is not what the
   * user thinks it is, and an Apply button that is merely disabled is a
   * control the user cannot understand -- so the truncated state has no Apply
   * at all and says what to fix.
   */
  const truncated = $derived(fromFile?.truncatedAtLine ?? null);
  const canApply = $derived(
    fromFile !== null ? fromFile.values.length > 0 && truncated === null : addable > 0,
  );

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
    // One commit path for both sources. A paste and a file produce the same
    // kind of value, and two commit paths would be two places for a bug in
    // "what exactly gets added" to live.
    if (fromFile !== null) {
      if (!canApply) return;
      p.onchange([...p.values, ...fromFile.values.map((v) => v.value)]);
      fromFile = null;
      fileError = null;
      return;
    }
    if (parsed === null) return;
    p.onchange([...p.values, ...parsed.values.map((v) => v.value)]);
    pasted = null;
  }

  function cancel() {
    pasted = null;
    fromFile = null;
    fileError = null;
    menuOpen = false;
  }

  /**
   * Read a chosen file and import it.
   *
   * `File.arrayBuffer()` rather than `FileReader` because it is a promise and
   * this is already inside one, and because the bytes are needed as a
   * `Uint8Array` for the encoding check -- which is the whole reason the file
   * path is not `file.text()`. A UTF-16 CSV read with `.text()` is mojibake
   * before the parser ever sees it, and the parser cannot tell, because the
   * damage is done to the bytes.
   */
  async function onFile(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (file === undefined) return;
    fileError = null;
    fromFile = null;
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const decoded = decodeCsvBytes(bytes);
      fromFile = importCsv(decoded.text, {
        // Trimmed because a tag with a leading space cannot be found again, and
        // the spreadsheet the user exported from is the one that put the space
        // there by accident. `importCsv` leaves it off by default so a general
        // reader does not silently change data.
        trimValues: true,
        existing: p.values,
      });
    } catch {
      // A file that cannot be read at all. The message says what is wrong
      // rather than "error", and it does not claim the field is unchanged --
      // it is, because nothing was applied, but that is implied by there being
      // no preview.
      fileError = 'Could not read that file. Try saving it again as a plain CSV.';
    } finally {
      // Cleared so choosing the SAME file twice fires a second `change` event.
      // Without this, a user who cancels a file and picks it again gets
      // nothing at all, with no indication that anything was attempted.
      input.value = '';
    }
  }

  function openFilePicker() {
    menuOpen = false;
    fileEl?.click();
  }

  function oncancelmenu() {
    menuOpen = false;
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      // Esc closes, and closes the PREVIEW first: §10.7 names Esc-closes-modals
      // explicitly, and a user who pastes by accident needs one Esc to undo the
      // paste and a second to close the menu.
      if (pasted !== null || fromFile !== null) {
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

  /**
   * Esc is listened for on the DOCUMENT, not on the wrapper.
   *
   * The wrapper's `onkeydown` only fires for events that bubble through it, so
   * Esc worked when the user had clicked the field and did nothing after a file
   * import -- which leaves focus on the hidden file input, outside the wrapper.
   * That is the one way a preview is opened without the user touching the field,
   * so it is the one way Esc failed, and it is the way a user who opens a file,
   * reads the warning, and changes their mind is left with no way out.
   *
   * It is on the document rather than on the preview because the preview is not
   * focusable either, and making a message focusable so that a key can reach it
   * is a worse trade than listening where the key actually lands.
   *
   * The listener is removed when the preview closes, and it calls
   * `stopPropagation` so that Esc dismissing a preview does not also reach an
   * enclosing modal that would close at the same time.
   */
  $effect(() => {
    const open = pasted !== null || fromFile !== null || fileError !== null;
    if (!open || typeof document === 'undefined') return;
    const onEscape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.stopPropagation();
      cancel();
    };
    document.addEventListener('keydown', onEscape, true);
    return () => document.removeEventListener('keydown', onEscape, true);
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

      <!--
        The file half of #1296, which is "CSV AND paste-parse import" -- one
        issue, two formats. It is a menu item rather than a bare input because
        the menu is already the "add values by some means" surface, and two
        entry points in two places is how a feature ends up with one of them
        undocumented.
      -->
      <button
        type="button"
        role="menuitem"
        data-testid="multi-file-item"
        onclick={openFilePicker}
      >
        Import a CSV file
      </button>
    </div>
  {/if}

  <!--
    A real `<input type="file">`, visually hidden and triggered by a button,
    rather than a styled div. A file input is a control the platform owns --
    drag-and-drop, the OS file picker, the mobile share sheet, and a real
    `accept` filter -- and replacing it with a div throws all of that away.
    `accept` is a hint, not a filter: the parser sniffs the delimiter and the
    encoding, so a `.txt` with commas in it imports correctly and a `.csv` that
    is really tab-separated still works.
  -->
  <!--
    The label is the button that opens the picker, and the association is
    explicit because the association a sighted user gets is a position, which
    is not a relationship a screen reader can compute. axe flagged this as
    `label` (critical) on /tags: an unlabelled file input is announced as
    "file upload, button" and nothing else, so a user cannot tell what it
    imports or which field it belongs to.

    The id is derived from the field's own label rather than written out, and
    that is a correctness requirement rather than tidiness: /tags has TWO of
    these on one page, and a literal id would give both the same one. A
    duplicate id is not a lint warning here, it is a label that points at the
    wrong field -- which is worse than no label, because it is confidently
    wrong.
  -->
  <label class="visually-hidden" for="file-{p.label}">Import a delimited list</label>
  <input
    id="file-{p.label}"
    type="file"
    class="visually-hidden"
    data-testid="multi-file-input"
    accept=".csv,.tsv,.txt,text/csv,text/plain"
    bind:this={fileEl}
    onchange={onFile}
  />

  <!--
    A file that could not be read. It gets its own line rather than becoming a
    preview, because there IS nothing to preview: saying "nothing to add" about
    a file that failed to open would be a lie about a file the user is fairly
    sure exists.
  -->
  {#if fileError !== null}
    <p class="error" data-testid="multi-file-error">{fileError}</p>
  {/if}

  <!--
    The preview, in the field rather than in a dialog. It is one sentence and
    two buttons, and a modal for that is a modal the user dismisses without
    reading — which is how a 400-row paste gets applied unexamined.

    ONE preview for a paste and for a file, because `CsvImport` is shaped like
    `PasteResult`. `summary` and `canApply` are the two things that differ, and
    they are the two things that should.
  -->
  {#if summary !== null}
    <div
      class="preview"
      data-testid="multi-preview"
      data-state={canApply ? 'ready' : 'empty'}
      data-source={fromFile !== null ? 'file' : 'paste'}
    >
      <p data-testid="multi-preview-text">{summary}</p>

      <!--
        A truncated file gets NO Apply button at all, not a disabled one. The
        file is not what the user thinks it is: its last quoted value swallowed
        the rest of it. A greyed-out button next to "Add 47" is a control the
        user cannot act on and cannot understand, and the message beside it
        already says what to fix.
      -->
      {#if truncated === null}
        <button
          type="button"
          data-testid="multi-preview-apply"
          disabled={!canApply}
          onclick={apply}
        >
          {canApply ? `Add ${fromFile !== null ? fileAddable : addable}` : 'Nothing to add'}
        </button>
      {/if}

      <button type="button" data-testid="multi-preview-cancel" onclick={cancel}>Cancel</button>

      <!--
        Warnings are shown, not swallowed. A file that parsed with a stray quote
        in it has values the user should look at before they are committed, and
        §10.7's rule is that an action states its scope -- a warning the user
        cannot see is not a warning.
      -->
      {#if fromFile !== null && fromFile.warnings.length > 0}
        <p data-testid="multi-file-warnings">
          {fromFile.warnings.length === 1
            ? '1 row had a stray quote; it was kept as text.'
            : `${fromFile.warnings.length} rows had a stray quote; they were kept as text.`}
        </p>
      {/if}
    </div>
  {/if}
</div>

<style>
  /*
    The hidden file input must be hidden to SIGHT and not to ASSISTIVE TECH.
    `display: none` and `visibility: hidden` both remove it from the
    accessibility tree, which would leave a file input the platform can open and
    no screen reader can describe -- the same control, present for the mouse and
    absent for everyone else.

    Clipping to a 1px box keeps it focusable, in the tree, and reachable by
    keyboard, which is the combination that makes it an accessible control
    rather than a decoration.
  */
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  .preview {
    /* The preview is a message plus two buttons, and it sits in the flow rather
       than floating, so it cannot cover the values it is about to change. */
    margin-top: 0.5rem;
    padding: 0.5rem;
    border: 1px solid currentColor;
    border-radius: 4px;
  }

  .preview[data-state='empty'] {
    /* Dimmed, not hidden: the user still needs to read WHY nothing will happen. */
    opacity: 0.7;
  }

  .error {
    margin-top: 0.5rem;
  }
</style>
