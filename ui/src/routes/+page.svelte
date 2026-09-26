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
  import VirtualGrid from '$lib/components/VirtualGrid.svelte';
  import { viewFromLocation, viewToHref, type ViewState } from '$lib/api/view.js';

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
    <option value="date">Date</option>
    <option value="title">Title</option>
    <option value="rating">Rating</option>
    <option value="added">Date added</option>
  </select>
  <button onclick={() => apply({ direction: view.direction === 'DESC' ? 'ASC' : 'DESC' })}>
    {view.direction === 'DESC' ? '↓' : '↑'}
  </button>
</div>

<div class="grid-host">
  <VirtualGrid query={view} density={view.density} />
</div>

<style>
  .grid-host {
    height: 100%;
    min-height: 0;
  }
</style>
