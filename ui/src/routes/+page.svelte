<!-- The browse surface. Every piece of state on this page comes from the URL
     and every change to it goes back to the URL (spec 5.16, 15.10).

     There is no `let filter = ''` here, and that is the point: a filter held in
     a component variable is lost on reload, cannot be linked, and does not
     respond to the back button. A virtualized view cannot hold it anywhere
     else, because it holds no more than a window of rows (spec 4.2).

     The URL is the single source of truth and the component is a function of
     it. Typing goes through `goto` with `replaceState` so each keystroke does
     not push a history entry — a search box that pushes one entry per
     character makes the back button useless, which is upstream #7142. -->
<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import ImageGrid from '$lib/components/ImageGrid.svelte';
  import { KeysetStore } from '$lib/api/keyset.js';
  import { SORTS, viewFromLocation, viewToHref, type ViewState } from '$lib/api/view.js';

  /**
   * The store the grid loads pages into.
   *
   * Owned HERE rather than defaulted inside `ImageGrid`. A prop default of
   * `new KeysetStore()` is re-evaluated whenever the parent re-renders, which
   * hands the grid a fresh EMPTY store mid-session: `rows` goes to zero, the
   * lightbox's `{#if open !== null && rows.length > 0}` goes false, and
   * re-setting `open` to the index it already had is a no-op -- so after
   * closing the lightbox once, it could never be reopened.
   *
   * That is a component-shaped bug with a page-shaped fix. The page is the
   * thing whose lifetime the store should share, and passing it explicitly
   * makes the identity a fact of the code rather than an accident of when a
   * default happens to be re-evaluated.
   */
  const store = new KeysetStore();

  // The label for each sort, in the order the options are shown. Keyed by the
  // value rather than paired with it, so an option cannot exist without a
  // label and `SORTS` cannot gain a member the dropdown does not offer.
  const SORT_LABEL: Record<(typeof SORTS)[number], string> = {
    date: 'Date',
    title: 'Title',
    rating: 'Rating',
    added: 'Date added'
  };

  // The whole view, read from the URL. `$derived` because it must recompute
  // when the URL changes, not when something local does.
  const view = $derived<ViewState>(viewFromLocation(page.url));

  let searchText = $state('');

  // Seed the box from the URL once, and again whenever the URL's filter
  // changes from outside (a shared link, the back button). Guarded on
  // inequality so a keystroke in the box is not fought by the URL.
  let lastFromUrl = $state<string | null>(null);
  $effect(() => {
    const f = view.filter ?? '';
    if (f !== lastFromUrl) {
      lastFromUrl = f;
      searchText = f;
    }
  });

  function apply(next: Partial<ViewState>, replace = true) {
    const merged: ViewState = { ...view, ...next };
    // `replaceState: true` keeps the back button meaningful. A URL that
    // changes on every keystroke is a history full of half-typed searches.
    goto(viewToHref(merged), { replaceState: replace, keepFocus: true, noScroll: true });
  }

  function onSearch(e: Event) {
    const q = (e.target as HTMLInputElement).value;
    searchText = q;
    apply({ filter: q || null });
  }
</script>

<svelte:head>
  <title>Commons</title>
</svelte:head>

<div class="toolbar">
  <input
    type="search"
    placeholder="Filter…"
    value={searchText}
    oninput={onSearch}
    data-testid="search"
    aria-label="Filter"
  />
  <select
    value={view.sort}
    onchange={(e) => apply({ sort: e.currentTarget.value })}
    data-testid="sort"
    aria-label="Sort by"
  >
    {#each SORTS as s (s)}
      <option value={s}>{SORT_LABEL[s]}</option>
    {/each}
  </select>
  <button onclick={() => apply({ direction: view.direction === 'DESC' ? 'ASC' : 'DESC' })}>
    {view.direction === 'DESC' ? '↓' : '↑'}
  </button>
</div>

<div class="grid-host">
  <ImageGrid query={view} density={view.density} {store} />
</div>

<style>
  .grid-host {
    height: 100%;
    min-height: 0;
  }
</style>
