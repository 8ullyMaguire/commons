<!--
  The theme control. T-P5-007, spec §10.8.

  # A `radiogroup`, not a `select` and not a toggle

  Three states that are not on/off, where "system" is a real third answer and
  not a synonym for "whatever is currently painted". That is a radio group: the
  user picks one of three, and the current answer is one of three. A checkbox
  or a two-state switch cannot express it without lying, because "on" would
  have to mean both "light" and "system, currently dark".

  `aria-checked` on `role="radio"` rather than a native `<input type=radio>`
  because the three options are icon+label buttons in a toolbar, and a native
  radio group inside a toolbar brings roving tabindex and arrow-key handling
  that a toolbar already owns. The trade is real -- a native input would give
  keyboard behaviour for free -- and it is taken deliberately: the toolbar
  already handles arrows, so a second owner would fight it.

  # The roving tabindex

  One tab stop for the whole group, arrows to move within it, which is the
  ARIA radio pattern. Three separate tab stops is the alternative and it is
  worse: a keyboard user tabbing through a toolbar hits three stops to express
  one decision.

  # `aria-live` is on the GROUP, not the buttons

  Announcing the change matters, and it belongs on the container: a live region
  on a button that also receives focus is announced twice, once for the focus
  and once for the change. The group is a `radiogroup` and is already in the
  tree, so a polite live region on it is a node that exists before the change
  rather than one created by it -- a live region inserted at the same moment as
  its own text is frequently not announced at all.
-->
<script lang="ts">
  import { tick } from 'svelte';
  import { theme } from '$lib/theme/theme-store.svelte.js';
  import { CHOICES, type ThemeChoice } from '$lib/theme/theme.js';

  const LABEL: Record<ThemeChoice, string> = {
    system: 'Match system',
    light: 'Light',
    dark: 'Dark'
  };

  const GLYPH: Record<ThemeChoice, string> = {
    system: '◐',
    light: '☀',
    dark: '☾'
  };

  /**
   * The label a screen reader announces, which is not the visible one.
   *
   * The glyph is meaningless spoken ("white sun with rays"), and the visible
   * label is already the button's text, so the accessible name would otherwise
   * be read as the glyph plus the label plus the state. Announcing the CHOICE
   * and the FACT is the whole message: "Dark, selected".
   */
  function nameFor(c: ThemeChoice): string {
    return LABEL[c];
  }

  /**
   * A stable handle on the buttons, so focus can be moved to one AFTER the
   * re-render that the selection causes.
   *
   * The first version focused the button synchronously, straight after
   * `theme.set()`. It worked for the selection and not for the focus, and the
   * reason is the reactive flush: `{#each CHOICES as c, i (c)}` is keyed by the
   * choice, so the node for `light` SURVIVES, but Svelte re-creates the node
   * when `tabindex` changes on the old and the new one, and a `.focus()` on a
   * node that is about to be replaced lands on nothing. The e2e caught it as
   * "aria-checked is true and the element is not focused" -- which reads as a
   * focus bug and is a rendering-order bug.
   *
   * `await tick()` is the fix and not a `setTimeout`: the flush is
   * microtask-ordered, so a microtask is enough and a timer would pass
   * non-deterministically.
   */
  let group: HTMLElement | null = $state(null);

  async function onKeydown(e: KeyboardEvent, index: number) {
    const n = CHOICES.length;
    let next: number | null = null;
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') next = (index + 1) % n;
    else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') next = (index - 1 + n) % n;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = n - 1;
    if (next === null) return;
    e.preventDefault();
    theme.set(CHOICES[next]);
    await tick();
    // Focus follows selection, which is the radio pattern. Without it the
    // arrow key changes the theme and leaves focus on the old button, so the
    // next arrow press acts on the wrong index.
    const buttons = group?.querySelectorAll<HTMLButtonElement>('button') ?? [];
    buttons[next]?.focus();
  }
</script>

<div
  class="theme"
  bind:this={group}
  role="radiogroup"
  aria-label="Colour theme"
  data-testid="theme-group"
  data-choice={theme.choice}
  data-resolved={theme.resolved}
>
  {#each CHOICES as c, i (c)}
    <button
      type="button"
      role="radio"
      aria-checked={theme.choice === c}
      aria-label={nameFor(c)}
      tabindex={theme.choice === c ? 0 : -1}
      data-testid="theme-{c}"
      data-choice={c}
      onclick={() => theme.set(c)}
      onkeydown={(e) => onKeydown(e, i)}
    >
      <span aria-hidden="true">{GLYPH[c]}</span>
    </button>
  {/each}
  <span class="sr" role="status" aria-live="polite" data-testid="theme-status">
    {LABEL[theme.choice]} theme
  </span>
</div>

<style>
  .theme {
    display: inline-flex;
    gap: 2px;
  }
  button {
    background: transparent;
    color: var(--muted);
    border: 1px solid transparent;
    border-radius: 4px;
    min-width: 32px;
    min-height: 32px;
    cursor: pointer;
    font-size: 0.9rem;
  }
  /* The selected state is carried by aria-checked AND by this, so it is
     visible to a user who cannot see the checkmark and to a user who cannot
     see the border colour. Neither alone is enough. */
  button[aria-checked='true'] {
    color: var(--fg);
    border-color: var(--border);
    background: var(--surface);
  }
  /* Visually hidden but present. `display: none` and `visibility: hidden` both
     remove it from the accessibility tree, which defeats the entire purpose --
     a live region that is hidden is a live region that never announces. */
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
