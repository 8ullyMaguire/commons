<!--
  Folder view: the pane on the left, the wall on the right. Spec 9.5, 5.14;
  plan T-P5-006 item 16; #1586, #1723.

  # Why this is a real route rather than a component on the grid page

  Because a folder is a place, and a place is a URL. A user who wants to send
  someone "the stuff I filed under Untagged" needs a link that opens that folder
  and not the grid they happened to be on. Every other state on this page --
  the open folder, the group-by, the filter -- is in the query string, and
  `goto` with `replaceState` keeps the back button from becoming a history of
  half-finished navigation.

  # The one thing this page has to get right

  Opening a folder REPLACES the filter rather than narrowing it (§5.14: a folder
  holds no objects; it *is* a query). So the URL carries the folder id and the
  wall reads the folder's filter — the folder's own filter, not the URL's `q`
  intersected with it. A folder whose filter is `tagged x` shows everything
  tagged x even if the user arrived with `?q=cat` in hand, and the pane says
  which filter is running so that is visible rather than surprising.
-->
<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import FolderPane from '$lib/components/FolderPane.svelte';
  import Wall from '$lib/components/Wall.svelte';
  import { parseGroupKey } from '$lib/api/wall.js';
  import { viewFromLocation, type ViewState } from '$lib/api/view.js';
  import {
    ROOT_NAV,
    openFolder,
    toggleExpanded,
    type Folder,
    type FolderNav
  } from '$lib/api/folder-tree.js';

  /**
   * The folders.
   *
   * A literal, because the GraphQL server is T-P6-007 and `Folders` is not in
   * the transport yet. What is NOT literal is the shape and the tree logic: the
   * fixture includes an orphan and a parent cycle on purpose, so the paths that
   * report broken data are exercised in the browser rather than only in the
   * unit tests. When the query lands, this array is the only thing that changes.
   */
  const FOLDERS: readonly Folder[] = [
    { id: 'f-inbox', name: 'Inbox', filter: 'organized = false', parentId: null, position: 0, icon: '📥', notify: true },
    { id: 'f-untagged', name: 'Untagged', filter: 'tag_count = 0', parentId: 'f-inbox', position: 0, icon: null, notify: false },
    { id: 'f-by-studio', name: 'By studio', filter: 'all', parentId: null, position: 1, icon: null, notify: false },
    { id: 'f-studio-a', name: 'Studio A', filter: 'producer = "A"', parentId: 'f-by-studio', position: 0, icon: null, notify: false },
    { id: 'f-studio-b', name: 'Studio B', filter: 'producer = "B"', parentId: 'f-by-studio', position: 1, icon: null, notify: false },
    // An orphan: its parent was deleted. Reported, not hidden.
    { id: 'f-orphan', name: 'Lost parent', filter: 'rating >= 4', parentId: 'f-deleted', position: 9, icon: null, notify: false },
    // A cycle: two folders each other's parent. Reachable only by dragging a
    // folder into its own descendant, and migration 0018 is supposed to refuse
    // it -- so this is exactly the data the view must survive.
    { id: 'f-loop-1', name: 'Loop one', filter: 'all', parentId: 'f-loop-2', position: 8, icon: null, notify: false },
    { id: 'f-loop-2', name: 'Loop two', filter: 'all', parentId: 'f-loop-1', position: 9, icon: null, notify: false }
  ];

  /** The open folder, from the URL. `null` is the tree root. */
  const openId = $derived<string | null>(page.url.searchParams.get('folder'));

  const groupBy = $derived(parseGroupKey(page.url.searchParams.get('group')));

  /**
   * The nav, from the URL.
   *
   * `expanded` is a repeated parameter, so the pane's state is linkable too --
   * which is what lets the e2e set up a deep tree without clicking through it.
   * Opening a folder expands its ancestors, so this is usually one parameter
   * rather than a list.
   */
  const nav = $derived<FolderNav>({
    open: openId,
    expanded: page.url.searchParams.getAll('expanded')
  });

  const current = $derived(FOLDERS.find((f) => f.id === openId) ?? null);

  /**
   * The query the wall runs.
   *
   * The folder's filter REPLACES the URL's `q`. This is the §5.14 semantics --
   * a folder holds no objects, it is a query, and membership is recomputed on
   * every open -- and it is the part a user gets wrong if it is left implicit.
   */
  const query = $derived.by((): ViewState => {
    const base = viewFromLocation(page.url);
    if (current === null) return base;
    return { ...base, filter: current.filter };
  });

  function setFolder(id: string | null): void {
    const url = new URL(page.url);
    url.searchParams.delete('folder');
    // A new folder is a new place: the old expansion trail is not meaningful.
    url.searchParams.delete('expanded');
    if (id !== null) {
      url.searchParams.set('folder', id);
      for (const anc of openFolder(ROOT_NAV, id, FOLDERS).expanded) {
        url.searchParams.append('expanded', anc);
      }
    }
    void goto(`${url.pathname}${url.search}`, { replaceState: true, noScroll: true, keepFocus: true });
  }

  function onToggle(id: string): void {
    const url = new URL(page.url);
    url.searchParams.delete('expanded');
    for (const e of toggleExpanded(nav, id).expanded) url.searchParams.append('expanded', e);
    void goto(`${url.pathname}${url.search}`, { replaceState: true, noScroll: true, keepFocus: true });
  }
</script>

<svelte:head><title>Folder view</title></svelte:head>

<div class="split">
  <FolderPane
    folders={FOLDERS}
    {nav}
    onopen={setFolder}
    ontoggle={onToggle}
  />

  <div class="content">
    <nav aria-label="Group by">
      {#each ['none', 'month', 'studio', 'performer'] as k (k)}
        <a
          href="?{page.url.searchParams.toString() ? `${page.url.search}&` : ''}group={k}"
          aria-current={k === groupBy ? 'page' : undefined}
        >
          {k}
        </a>
      {/each}
    </nav>
    <Wall {query} {groupBy} />
  </div>
</div>

<style>
  .split {
    display: flex;
    height: 100%;
    min-height: 0;
  }
  .content {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  nav {
    display: flex;
    gap: 10px;
    padding: 6px 8px;
    font-size: 12px;
  }
</style>
