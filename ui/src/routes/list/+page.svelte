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
  import BulkEditModal from '$lib/components/BulkEditModal.svelte';
  import { KeysetStore } from '$lib/api/keyset.js';
  import { viewFromLocation, type ViewState } from '$lib/api/view.js';
  import { selectedCount, type Selection } from '$lib/api/selection.js';
  import { bulkTarget, type BulkOutcome } from '$lib/api/bulk.js';
  import { bulkApplyTag, fetchTags, query } from '$lib/api/client.js';

  const view = $derived<ViewState>(viewFromLocation(page.url));

  /**
   * One store for the page's lifetime.
   *
   * A `KeysetStore` is a plain class, not a Svelte store, so it is created once
   * and not per render. Creating it in the template would reset the loaded pages
   * on every re-render and the table would never fill.
   */
  const store = new KeysetStore();

  // --- the bulk edit (T-P5-006 item 3) ---------------------------------------
  //
  // The selection is mirrored here rather than read out of the table, because a
  // parent cannot reach into a child's `$state`. The table already reports every
  // change through `onchange`, so the mirror costs one assignment and needs no
  // second source of truth to drift from.
  let selection = $state<Selection>({});
  let modalOpen = $state(false);
  let busy = $state(false);
  let outcome = $state<BulkOutcome | undefined>(undefined);
  let bulkError = $state<string | null>(null);
  let tags = $state<{ id: string; name: string }[]>([]);

  // The tags are the *write target* of a bulk edit, so they are loaded with the
  // page rather than fetched when the modal opens. A modal that opens empty and
  // fills in a moment later is a modal whose confirm button flickers from
  // disabled to enabled while the user is reading the scope line.
  $effect(() => {
    let cancelled = false;
    fetchTags()
      .then((r) => {
        if (!cancelled) tags = r.tags;
      })
      .catch(() => {
        /* An empty tag list leaves the modal with nothing to choose, which is a
           visible dead end rather than a silent failure. */
      });
    return () => {
      cancelled = true;
    };
  });

  /**
   * The write, and what it reports.
   *
   * The server's three numbers are stored raw and classified by the modal, so
   * the client never computes a count of its own -- see `bulk.ts`. On a failure
   * the previous outcome is *cleared* rather than left in place: a modal still
   * showing "Tagged 12 objects" above a red error is reporting two
   * contradictory things about one action.
   *
   * Through `bulkApplyTag` and not a local `fetch`, because
   * `tests/invariants.test.ts` asserts the transport is the only module that
   * names one. That test caught this route's first version.
   */
  async function applyBulkTag(tagId: string) {
    busy = true;
    bulkError = null;
    try {
      const res = await bulkApplyTag(tagId, bulkTarget(selection, view.filter ?? ''));
      outcome = {
        applied: res.bulkApplyTag.applied,
        skipped_invisible: res.bulkApplyTag.skippedInvisible,
        requested: res.bulkApplyTag.requested
      };
      selection = {};
      await refresh();
    } catch (e) {
      outcome = undefined;
      bulkError = e instanceof Error ? e.message : 'the write could not be sent';
    } finally {
      busy = false;
    }
  }

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

  /** The loaded ids, derived once: the guard, the button and the modal all read them. */
  const loadedIds = $derived(state.rows.map((r) => r.id));

  function openBulk() {
    // Nothing selected, nothing to open. A modal that opens on an empty
    // selection and reports "no objects selected" is a dialog asking the user
    // to confirm a write with no scope.
    //
    // The count is over the *real* loaded ids, not an empty list. Reading it
    // with `[]` counts zero hand-picked rows -- the guard then refuses to open
    // a modal the button is visibly enabled for, which is the worst version of
    // this bug: the UI says yes and the handler says no, with nothing on screen
    // to explain the difference.
    if (selectedCount(selection, loadedIds, state.totalCount) === 0) return;
    outcome = undefined;
    bulkError = null;
    modalOpen = true;
  }

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
    onchange={(next) => (selection = next)}
  />

  <button
    data-testid="open-bulk"
    onclick={openBulk}
    disabled={selectedCount(selection, loadedIds, state.totalCount) === 0}
  >
    Bulk edit
  </button>

  <BulkEditModal
    open={modalOpen}
    {selection}
    {loadedIds}
    serverCount={state.totalCount}
    {tags}
    {outcome}
    error={bulkError}
    {busy}
    onapply={applyBulkTag}
    oncancel={() => (modalOpen = false)}
  />
</div>

<style>
  .host {
    height: 100%;
    min-height: 0;
  }
</style>
