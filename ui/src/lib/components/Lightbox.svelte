<!--
  The full-screen viewer. Spec 10.2 (C55), stash#7147/#7148/#7149/#7154.

  # This file contains no thresholds

  Every decision about what a gesture means lives in `$lib/api/gestures.ts`,
  which is pure and unit-tested at each boundary. This component attaches
  listeners, keeps a pan offset, and renders. That split is the reason the
  three bugs are fixed at all: the fix for each is a number, and a number
  inline in a component is a number nobody can test.

  # Why the three bugs are three bugs

  #7147 -- a fast drag releases and jumps to the next image. Cause: the release
  is read as a flick. Fixed by requiring both slop and duration.

  #7148 -- a trackpad's two-finger scroll changes image. Cause: on a trackpad
  the wheel IS the pan gesture, so wheel must never navigate while there is
  anything to pan. No drag logic can fix this; it is a different device.

  #7149 -- the same at a zoom level where the pan range is small. One rule
  covers both: navigate only when the image is not pannable.

  # The pan range is what makes "pannable" answerable

  A fitted image has offset 0 and no room to move, so `pannable` is false and
  the wheel is free to mean "next". A zoomed image has room, so the wheel pans.
  Nothing else decides it -- not the zoom level as a number, not the device.
  Asking "is there anywhere left to go" is the whole of #7148 and #7149, and it
  is why the wheel handler is three lines.

  # #7154: back, and the unsaved modal

  The browser cannot know a modal is open, so this pushes a history entry on
  open and closes on `popstate`. If the modal is dirty it *re-pushes* instead,
  which is a deliberate loop: the entry goes back, the modal stays, the edits
  stay. The alternative is silently losing an edit, and §10.7's rule is that
  destructive actions confirm -- losing one on a back press is the worst
  version of that. The loop is visible to the user (back appears to do nothing)
  rather than invisible (work disappears), and that is the right way round.

  # data-index, not a store, is what the tests read

  The tests assert on `data-index` because the complaint is what the user sees.
  A test that reads the Svelte state is a test of the state, and a lightbox
  whose state is right while the DOM is stale is exactly the bug being fixed.
-->
<script lang="ts">
  import { classifyDrag, classifyPopstate, classifyWheel, type PointerSample } from '$lib/api/gestures.js';
  import type { ObjectRow } from '$lib/api/client.js';

  interface Props {
    /** The rows the lightbox is a view over. */
    rows: readonly ObjectRow[];
    /** Index of the current row. */
    index: number;
    /** Called with the new index. The grid owns the list; this owns the pan. */
    onindexchange?: (index: number) => void;
    /** Called on close (Escape, the close button, or back). */
    onclose?: () => void;
    /**
     * Are there unsaved edits? While true, back re-pushes instead of closing
     * (#7154). The component does not know what "unsaved" means -- a per-image
     * edit form owns that -- so it is told.
     */
    dirty?: boolean;
  }

  let { rows, index, onindexchange, onclose, dirty = false }: Props = $props();

  /** Pan offset in CSS pixels. The only state this component owns. */
  let offsetX = $state(0);
  let offsetY = $state(0);

  /**
   * Where the current press started, or null when no button is down.
   *
   * Doubles as the "is a drag in progress" flag the wheel handler needs. It
   * cannot be a separate boolean: two pieces of state that both mean "the
   * button is down" can disagree, and when they do the wheel acts on a gesture
   * the user is not performing -- which is the accept criterion for T-P5-005
   * failing. One field, one meaning.
   */
  let pressStart: PointerSample | null = null;

  const current = $derived(rows[index]);

  /**
   * Is there anywhere left to pan?
   *
   * "Left" rather than "is it zoomed": a zoomed image scrolled back to centre
   * has nowhere to go either, and a wheel that navigates there is the same
   * surprise as on a fitted image.
   */
  const pannable = $derived(offsetX !== 0 || offsetY !== 0 || zoomed);

  /** Zoom. At 1 the image is fitted, and panning does nothing. */
  let zoomed = $state(false);

  function clamp(v: number): number {
    if (!zoomed) return 0;
    return v;
  }

  function go(next: number) {
    const bounded = Math.max(0, Math.min(rows.length - 1, next));
    if (bounded !== index) {
      onindexchange?.(bounded);
    }
    // A new image starts centred. Carrying the pan across is how a user ends up
    // looking at a corner of the next image and concluding it is broken.
    offsetX = 0;
    offsetY = 0;
  }

  function onpointerdown(e: PointerEvent) {
    pressStart = { x: e.clientX, y: e.clientY, t: e.timeStamp };
  }

  function onpointerup(e: PointerEvent) {
    if (!pressStart) return;
    // The decision is two pure calls. The end point is measured from the press
    // directly, which is the fix for #7147: a pointermove lost in the race
    // before pointerup cannot make a 400px drag look like a 4px one.
    const out = classifyDrag(pressStart, { x: e.clientX, y: e.clientY, t: e.timeStamp });
    pressStart = null;
    if (out.kind === 'pan') {
      offsetX = clamp(offsetX + out.dx);
      offsetY = clamp(offsetY + out.dy);
    } else if (out.kind === 'flick') {
      go(index + 1);
    }
  }

  function onwheel(e: WheelEvent) {
    // `preventDefault` because a wheel over a lightbox must not scroll the page
    // behind it. A trackpad generates a continuous stream of these, and the
    // default action would fight the pan.
    e.preventDefault();
    const outcome = classifyWheel(e.deltaY, pannable, pressStart !== null);
    if (outcome === 'ignore') {
      // A drag owns the input. The event is still swallowed -- otherwise the
      // page behind scrolls -- but it changes nothing about the image.
      return;
    }
    if (outcome === 'navigate') {
      go(index + (e.deltaY > 0 ? 1 : -1));
    } else if (zoomed) {
      offsetX = clamp(offsetX - e.deltaX);
      offsetY = clamp(offsetY - e.deltaY);
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      onclose?.();
    } else if (e.key === 'ArrowRight') {
      go(index + 1);
    } else if (e.key === 'ArrowLeft') {
      go(index - 1);
    }
  }

  function onpopstate() {
    if (classifyPopstate(dirty) === 're-push') {
      // Put the entry back. The modal and its edits both stay.
      history.pushState({ lightbox: index }, '');
    } else {
      onclose?.();
    }
  }

  $effect(() => {
    // Opened: push an entry so back closes rather than leaving the page.
    history.pushState({ lightbox: index }, '');
    const onKey = (e: KeyboardEvent) => onkeydown(e);
    const onPop = () => onpopstate();
    window.addEventListener('keydown', onKey);
    window.addEventListener('popstate', onPop);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('popstate', onPop);
    };
  });
</script>

<!--
  data-index is the assertion surface for the Playwright tests. The wheel
  handler is the one under test and a test that reads a Svelte store would be
  testing the state rather than what the user sees.
-->
<div
  class="lightbox"
  data-index={index}
  data-total={rows.length}
  role="dialog"
  aria-modal="true"
  aria-label={current?.title ?? 'Image'}
>
  <button class="close" onclick={() => onclose?.()} aria-label="Close">×</button>

  <!--
    svelte-ignore a11y_no_noninteractive_element_interactions
    A pointer target, not an interactive element: it is a surface you drag, and
    the keyboard equivalents are the arrow keys and Escape on the window.
  -->
  <div
    class="stage"
    onpointerdown={onpointerdown}
    onpointerup={onpointerup}
    onwheel={onwheel}
    style="transform: translate({offsetX}px, {offsetY}px)"
  >
    {#if current?.coverPath}
      <img src={current.coverPath} alt={current.title ?? ''} />
    {:else}
      <div class="empty">No preview</div>
    {/if}
  </div>

  <div class="nav">
    <button onclick={() => go(index - 1)} disabled={index === 0} aria-label="Previous">‹</button>
    <span class="counter">{index + 1} / {rows.length}</span>
    <button
      onclick={() => go(index + 1)}
      disabled={index >= rows.length - 1}
      aria-label="Next">›</button>
  </div>
</div>

<style>
  .lightbox {
    position: fixed;
    inset: 0;
    background: #000;
    display: grid;
    grid-template-rows: 1fr auto;
    place-items: center;
    /* Safe-area insets for notched displays, spec 10.8 (#5979). */
    padding: env(safe-area-inset-top) env(safe-area-inset-right)
      env(safe-area-inset-bottom) env(safe-area-inset-left);
  }
  .stage {
    /* `grab` rather than `grabbing` statically: the cursor is a hint and a
       stuck grabbing cursor outlives the gesture that set it. */
    cursor: grab;
    touch-action: none;
    max-width: 100%;
    max-height: 100%;
    /*
      The stage is the gesture surface, so it must stay a usable size even
      when the image inside it is not.

      Without this, the stage is exactly as big as its image, and a thumbnail
      that has not loaded -- or one that genuinely is 1x1 -- collapses it to a
      pixel. The pan gestures then have nowhere to happen: every coordinate is
      the same point, a zero-travel release is a flick, and the lightbox
      advances under a user who only tried to drag. A min box is also what
      makes the surface reachable for a very small image on a phone.

      `min()` keeps it bounded by the viewport so it cannot itself cause a
      scrollbar on a small screen.
    */
    min-width: min(100%, 40rem);
    min-height: min(100%, 30rem);
    display: grid;
    place-items: center;
  }
  .stage:active {
    cursor: grabbing;
  }
  .stage img {
    /*
      `width: 100%` against a bounded stage, so the image fills the box it was
      given instead of the box it happens to decode to. `object-fit: contain`
      then keeps the aspect ratio, which is what a viewer must do: a stretched
      preview is worse than a letterboxed one.
    */
    max-width: 100%;
    max-height: 100%;
    width: auto;
    height: auto;
    object-fit: contain;
    display: block;
    user-select: none;
    -webkit-user-drag: none;
  }
  .close {
    position: absolute;
    top: 1rem;
    right: 1rem;
    font-size: 2rem;
    line-height: 1;
    background: none;
    border: none;
    color: var(--fg);
    cursor: pointer;
  }
  .nav {
    display: flex;
    gap: 1rem;
    align-items: center;
    color: var(--fg);
    padding: 1rem;
  }
  .empty {
    color: var(--muted);
    padding: 4rem;
  }
</style>
