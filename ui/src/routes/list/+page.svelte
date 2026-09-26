<!--
  The list view. Spec 10.4 (C49), 10.6 (C53); plan T-P5-006 item 2.

  A real route, not a prop on the browse page, for the same reason
  `index-mode` is a route: a view mode the user can arrive at by URL is a view
  mode that can be linked to, reloaded, and put in the back button. A `?view=list`
  on one route would make the list a mode of the grid, and the two have
  different loading and different selection lifecycles.

  The view is still the same value the browse page uses, read from the URL the
  same way, so a link to `/list?sort=title` reproduces the list exactly.
-->
<script lang="ts">
  import { page } from '$app/state';
  import ListTable from '$lib/components/ListTable.svelte';
  import { KeysetStore } from '$lib/api/keyset.js';
  import { viewFromLocation, type ViewState } from '$lib/api/view.js';

  const view = $derived<ViewState>(viewFromLocation(page.url));

  /**
   * One store for the page's lifetime.
   *
   * A `KeysetStore` is a plain class, not a Svelte store, so it is created once
   * and not per render. Creating it in the template would reset the loaded pages
   * on every re-render and the table would never fill.
   */
  const store = new KeysetStore();

  /**
   * The store's state, mirrored into Svelte's.
   *
   * This is `VirtualGrid`'s pattern, and it is here for the same reason it is
   * there: the store has no Svelte knowledge, so nothing re-renders when it
   * changes. Reading `store.state` straight from the template compiles and
   * builds and renders an empty table forever, because the value is read once
   * and never again. The mirror is the whole mechanism.
   */
  // svelte-ignore state_referenced_locally
  let state = $state(store.state);

  async function refresh() {
    await store.loadMore();
    state = store.state;
  }

  // A view change is a different result set: reset rather than append. The
  // comparison is on the serialized query, because the router hands back a new
  // object on every navigation even when nothing changed.
  // svelte-ignore state_referenced_locally
  let lastKey = JSON.stringify(view);
  $effect(() => {
    const key = JSON.stringify(view);
    if (key !== lastKey) {
      lastKey = key;
      store.reset(view);
      state = store.state;
      refresh();
    }
  });

  // First load. Guarded on `started`, not on `rows.length === 0`: an empty
  // result leaves `rows` empty and `started` false, so a rows-length guard
  // refetches forever on an empty library. `VirtualGrid` documents the same
  // trap in full.
  $effect(() => {
    const s = state;
    if (!s.started && !s.loading && !s.error) {
      refresh();
    }
  });
</script>

<svelte:head>
  <title>Commons — List</title>
</svelte:head>

<div class="host">
  <!--
    `matchesQuery` answers "does the current query match this id?", and it is a
    prop because the answer is the server's to give.

    On this page every loaded row *is* in the current result -- the store loaded
    them by this query -- so `() => true` is correct here and only here. It
    would be wrong on a page mixing sources, and it is the reason the seam
    exists rather than a constant inside the table: the table cannot know, and a
    caller that guesses is guessing about which rows a bulk edit will touch.

    The loaded rows being a subset of the query is also why the header control
    is honest about the difference: a select-all covers the server's count,
    which is larger than `rows.length` on any library big enough to page.
  -->
  <ListTable
    rows={state.rows}
    totalCount={state.totalCount}
    density={view.density}
    matchesQuery={() => true}
  />
</div>

<style>
  .host {
    height: 100%;
    min-height: 0;
  }
</style>
