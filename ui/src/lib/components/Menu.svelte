<!--
  A menu that opens next to its button and stays inside the window.

  T-P5-007, spec 10.8, stash#4667. The positioning is `use:popover`; the
  behaviour below it is the part that makes it a menu rather than a div:

  - Escape closes it and returns focus to the button, which is the whole point
    of a menu being keyboard-operable. A menu that closes and strands focus on
    <body> is worse than one that does not open.
  - Arrow keys move between items, Home/End jump to the ends, and typing a
    letter jumps to the next item starting with it. This is the WAI-ARIA
    menu pattern, and without it the menu is a list of links that a keyboard
    user has to Tab through -- which for a nine-item menu is nine Tab presses
    to get past it.
  - The menu is `role="menu"` with `role="menuitem"` children, and the button
    is `aria-haspopup`/`aria-expanded`. Without the roles a screen reader
    announces it as a list, and the user has no idea Tab is not the way out.
-->

<script lang="ts">
  import { popover } from '$lib/theme/viewportPopover.js';

  interface Props {
    /** The label on the trigger. Also the accessible name of the menu. */
    label: string;
    items: { id: string; label: string; onselect: (id: string) => void }[];
  }

  let { label, items }: Props = $props();

  let button: HTMLButtonElement | undefined = $state();
  let menu: HTMLDivElement | undefined = $state();
  let open = $state(false);
  /** Index of the highlighted item, or -1 for "nothing is highlighted". */
  let highlighted = $state(-1);

  function show(from: 'first' | 'last' = 'first') {
    open = true;
    highlighted = from === 'last' ? items.length - 1 : 0;
    // Wait a frame: the menu has to be in the DOM and MEASURED before
    // `use:popover` can place it, and focusing an element that is about to be
    // moved is the same failure as focusing one that has already moved.
    //
    // The frame matters for more than position. Until focus is inside the menu
    // a keypress goes to the TRIGGER, whose handler opens rather than moves --
    // so without this, the first ArrowDown after opening does nothing, which
    // is exactly what the e2e caught.
    // Two frames, not one, and the reason is specific: the menu is rendered by
    // the `{#if}` on THIS tick, and the action measures the node in the frame
    // after that. Focusing in the first frame can land on a node that the
    // placement pass is about to move, and a focus on a node the browser then
    // relocates is a focus the browser drops -- silently, back to the trigger.
    //
    // A silent focus loss is the hardest kind of bug to see, because
    // everything is still on screen and every assertion about VISIBILITY
    // passes. Only asserting on `document.highlightedElement` catches it.
    requestAnimationFrame(() => requestAnimationFrame(() => focusItem(highlighted)));
  }

  function hide(refocus = true) {
    open = false;
    highlighted = -1;
    if (refocus) button?.focus();
  }

  function focusItem(i: number) {
    if (i < 0 || i >= items.length) return;
    highlighted = i;
    const el = menu?.querySelectorAll<HTMLElement>('[role="menuitem"]')[i];
    el?.focus();
  }

  function move(delta: number) {
    const n = items.length;
    if (n === 0) return;
    // Wraps. A menu that stops at the ends needs a Shift+Tab to leave, which
    // is a different gesture than the ArrowDown the user just used.
    focusItem((highlighted + delta + n) % n);
  }

  function onkeydown(e: KeyboardEvent) {
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        move(1);
        return;
      case 'ArrowUp':
        e.preventDefault();
        move(-1);
        return;
      case 'Home':
        e.preventDefault();
        focusItem(0);
        return;
      case 'End':
        e.preventDefault();
        focusItem(items.length - 1);
        return;
      case 'Tab':
        // A menu is not a tab stop. Tabbing out closes it, and lets the browser
        // do what Tab means. Swallowing Tab would trap the keyboard user.
        hide(false);
        return;
      default:
        break;
    }
    // Typeahead: a single printable character jumps to the next item starting
    // with it, wrapping. Held down it repeats, which is what every native menu
    // does and what users' fingers already know.
    if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
      const ch = e.key.toLowerCase();
      const from = highlighted + 1;
      const order = [...items.slice(from), ...items.slice(0, from)];
      const hit = order.findIndex((it) => it.label.toLowerCase().startsWith(ch));
      if (hit >= 0) {
        e.preventDefault();
        focusItem((from + hit) % items.length);
      }
    }
  }

  function onbuttonkeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      show('first');
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      show('last');
    }
  }

  /** A click anywhere else closes it. On `window`, so it catches a click that
   *  misses every element, which is most of them. */
  function onwindowpointerdown(e: PointerEvent) {
    if (!open) return;
    const t = e.target as Node;
    if (menu?.contains(t) || button?.contains(t)) return;
    hide(false);
  }
</script>

<!--
  Escape on the WINDOW, not on the menu. Two reasons and both are real: focus
  is on the trigger for the first frame after opening, and a menu opened with
  the mouse leaves focus on the trigger until the first item is focused. Either
  way a keydown on the menu element never sees the Escape.

  The window listener is gated on `open`, so it costs nothing while closed, and
  `stopPropagation` keeps it from also reaching a dialog behind.
-->
<svelte:window
  onpointerdown={onwindowpointerdown}
  onkeydown={(e) => {
    if (!open || e.key !== 'Escape') return;
    e.preventDefault();
    e.stopPropagation();
    hide();
  }}
/>

<div class="menu-root">
  <button
    bind:this={button}
    type="button"
    class="menu-trigger"
    aria-haspopup="menu"
    aria-expanded={open}
    data-testid="menu-trigger"
    onclick={() => (open ? hide() : show())}
    onkeydown={onbuttonkeydown}
  >
    {label}
  </button>

  {#if open}
    <div
      bind:this={menu}
      use:popover={{ anchor: button }}
      class="menu-popover"
      role="menu"
      aria-label={label}
      data-testid="menu-popover"
      onkeydown={onkeydown}
    >
      {#each items as item, i (item.id)}
        <button
          type="button"
          role="menuitem"
          class="menu-item"
          class:is-highlighted={i === highlighted}
          data-testid="menu-item"
          data-item-id={item.id}
          tabindex={i === highlighted ? 0 : -1}
          onclick={() => {
            hide();
            item.onselect(item.id);
          }}
        >
          {item.label}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  <!--
    No `:focus-visible` rule here. `app.css` has one for the whole app, drawn
    with `var(--focus)` and layered above the control so it is visible on a
    filled background. A component that draws its own ring is a second source
    of truth for the focus colour, and the axe scan checks the CONTRAST of
    whichever one wins -- so the two can drift apart and only the loser looks
    right.

    Every colour below is a token from `theme/contrast.ts`, never a literal.
    A literal here is invisible until the user switches theme: the first
    version of this file paired `var(--fg, #e8e8ea)` with a hard-coded
    `#22222a` background, which is right in dark and 1.14:1 in light, and the
    axe scan found it on seven routes. The fallback in a `var()` is the trap --
    it looks theme-aware and is a second, frozen theme.
  -->
  .menu-root {
    position: relative;
    display: inline-block;
  }

  .menu-trigger {
    /* 44px, not 32. stash#6383 / #6823: a touch target smaller than this is
       below the size every platform's guidelines call the minimum, and the
       whole point of the ticket is that the target is reachable. */
    min-height: 44px;
    min-width: 44px;
    padding: 0 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }

  .menu-popover {
    /* Deliberately minimal. Position, size and z-order come from the action,
       which is the only thing that knows the viewport. A `min-width` here is
       fine and wanted; anything that fights the action's coordinates is not. */
    z-index: 40;
    display: grid;
    min-width: 180px;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    box-shadow: 0 8px 24px rgb(0 0 0 / 45%);
  }

  .menu-item {
    min-height: 44px;
    padding: 0 10px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .menu-item:hover,
  .menu-item.is-highlighted {
    background: var(--bg);
  }
</style>
