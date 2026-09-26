<!--
  The image browse surface, and the lightbox's host. Spec 9.6 (C51), 10.2 (C55).

  # Why this composes VirtualGrid instead of being a grid

  Spec 4.2 and 10.4 say virtualization runs through every list view. A second
  grid is a second implementation of the part that is hard -- fixed row height,
  a window computed from the count, an overscan -- and `VirtualGrid` documents
  its own choices at length precisely because they are not obvious. So this
  component renders one `VirtualGrid` and adds the image-specific part: the
  selection that opens the lightbox.

  # The lightbox is a view over this selection, not a second navigation model

  Every bug in T-P5-005 is a symptom of the grid and the lightbox disagreeing
  about which image is current. So there is exactly one index, it lives here,
  and the lightbox is handed it plus a change callback. The lightbox owns a pan
  offset and nothing else.

  # How a tile click reaches this component

  A delegated listener on a wrapper, not a callback threaded through
  `VirtualGrid`. `VirtualGrid` has no other use for one, so a prop would make a
  component that knows about the lightbox for the lightbox's sake. The wrapper
  reads `data-lightbox-index` off the clicked tile, which is the same index the
  lightbox pages by -- one number, defined in one place.

  # Why the lightbox is given the library and not the visible window

  A virtualized grid holds a window, and handing that window to the lightbox
  would make "next image" stop at the edge of the viewport -- which is the
  complaint, not the feature. So the lightbox pages over `rows`, the loaded
  list. Paging past the loaded prefix is T-P5-006's work; `Lightbox` clamps its
  index to the rows it was given, so the worst case here is that "next" stops,
  never that it shows the wrong image.
-->
<script lang="ts">
  import VirtualGrid from '$lib/components/VirtualGrid.svelte';
  import Lightbox from '$lib/components/Lightbox.svelte';
  import { KeysetStore, type GridQuery } from '$lib/api/keyset.js';

  interface Props {
    query: GridQuery;
    density?: number;
    store?: KeysetStore;
    /**
     * Are there unsaved edits? While true, back does not close (spec #7154).
     */
    dirty?: boolean;
  }

  let { query, density = 240, store = new KeysetStore(), dirty = false }: Props = $props();

  /**
   * The loaded rows, which the lightbox pages over.
   *
   * A virtualized grid holds a window, and handing that window to the lightbox
   * would make "next image" stop at the edge of the viewport -- which is the
   * complaint, not the feature. So the lightbox gets the loaded list instead.
   *
   * `KeysetStore` is a plain class with a getter rather than a Svelte store, so
   * this `$derived` does not re-run when a page lands; the grid re-renders and
   * passes the same store down, and the lightbox reads whatever is loaded at
   * the moment it is opened. That is sufficient and it is the safe direction: a
   * lightbox opened on a tile is by construction on a *loaded* tile, and its
   * index is resolved against the list at that instant. Paging past the loaded
   * prefix is T-P5-006's work, and `Lightbox` clamps to the rows it was given,
   * so the worst case is that "next" stops rather than showing a wrong image.
   */
  const rows = $derived(store.state.rows);

  /** Index of the image the lightbox is showing, or null when it is closed. */
  let open = $state<number | null>(null);
</script>

<div
  class="lightbox-host"
  role="presentation"
  onclick={(e) => {
    const tile = (e.target as HTMLElement).closest<HTMLElement>('[data-lightbox-index]');
    if (tile) open = Number(tile.dataset.lightboxIndex);
  }}
>
  <VirtualGrid {query} {density} {store} />
</div>

{#if open !== null && rows.length > 0}
  <Lightbox
    {rows}
    index={open}
    {dirty}
    onindexchange={(i) => (open = i)}
    onclose={() => (open = null)}
  />
{/if}

<style>
  .lightbox-host {
    height: 100%;
    min-height: 0;
  }
</style>
