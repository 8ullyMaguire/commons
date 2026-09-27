<!--
  The folder pane: the tree, the breadcrumbs, and the search box. Spec 9.5 and
  5.14, plan T-P5-006 item 16; #1586, #1723.

  # A folder holds no objects, and this renders none

  `crates/commons-store/src/folders.rs` is done, and its header is the design:
  a folder is a name, a filter and a position. Membership is recomputed on every
  open, so there is no membership here to render and nothing to invalidate when a
  filter changes. This component's whole job is a tree, a trail, and turning a
  selection into a filter.

  # The counts are deliberately absent

  §5.14 says membership is recomputed on open, which means counting a folder is a
  query per folder. `formatCount(null)` renders a dash rather than a zero, and the
  reason is in `folder-tree.ts`: a column of zeroes before the counts land looks
  like a library that is empty, which is a worse lie than "I do not know yet".

  # Broken data is shown, not swallowed

  An orphan and a parent cycle are both reported by `buildTree` and both render
  here. A folder that silently never appears is the worst outcome a navigator
  has, because the user has no way to know it exists — so the pane says "1 folder
  has a missing parent" rather than quietly rendering four of five folders.
-->
<script lang="ts">
  import type { Folder, FolderNav, FolderNode } from '$lib/api/folder-tree.js';
  import {
    ROOT_NAV,
    breadcrumbs,
    buildTree,
    formatCount,
    isCurrentCrumb,
    isExpanded,
    openFilterLabel,
    searchFolders,
    keepAncestors
  } from '$lib/api/folder-tree.js';

  interface Props {
    /** Every folder the caller may see. */
    readonly folders: readonly Folder[];
    /** What is open. */
    readonly nav?: FolderNav;
    /** Per-folder item counts, when they are known. */
    readonly counts?: Readonly<Record<string, number>>;
    /** Open a folder. */
    readonly onopen?: (id: string | null) => void;
    /** Expand or collapse one folder without opening it. */
    readonly ontoggle?: (id: string) => void;
  }

  const p: Props = $props();

  let search = $state('');

  const nav = $derived(p.nav ?? ROOT_NAV);

  /**
   * The rows to render, as a flat list with a depth.
   *
   * A search replaces the tree with a flat list of matches PLUS their ancestors,
   * rather than filtering the tree in place. Filtering in place is where a hit
   * three levels down ends up with a collapsed, unopenable path above it, and
   * `keepAncestors` is what makes a hit navigable.
   */
  const rows = $derived.by(() => {
    const q = search.trim();
    if (q === '') {
      const out: { node: FolderNode; hidden: number }[] = [];
      const walk = (nodes: readonly FolderNode[]): void => {
        for (const n of nodes) {
          const visibleChildren = n.children.filter((c) => isExpanded(nav, n.folder.id));
          out.push({ node: n, hidden: n.children.length - visibleChildren.length });
          walk(visibleChildren);
        }
      };
      walk(tree.roots);
      return out;
    }
    const visible = new Set(keepAncestors(searchFolders(p.folders, q), p.folders).map((f) => f.id));
    return tree.all
      .filter((n) => visible.has(n.folder.id))
      .map((n) => ({ node: n, hidden: 0 }));
  });

  const trail = $derived(p.nav?.open ? breadcrumbs(p.nav.open, p.folders) : []);
  const filterLabel = $derived(openFilterLabel(nav.open, p.folders));

  /**
   * The tree, rebuilt only when the folder list changes.
   *
   * `$derived` memoises on its dependencies, so this does NOT re-run per
   * keystroke: `p.folders` is untouched by `search`. That is the whole reason
   * the tree is a `$derived` and not a call inside the `rows` computation -- the
   * rows depend on the search text, and folding the tree build into them would
   * rebuild a 500-folder tree on every character typed.
   */
  const tree = $derived(buildTree(p.folders));

  function count(id: string): number | null {
    return p.counts?.[id] ?? null;
  }

  function open(id: string): void {
    p.onopen?.(id);
  }
</script>

<aside data-testid="folder-pane">
  <input
    type="search"
    placeholder="Filter folders…"
    bind:value={search}
    data-testid="folder-search"
    aria-label="Filter folders"
  />

  <!--
    The trail, nearest first (#1723). A crumb that does not resolve renders as a
    disabled step rather than being dropped, so the trail's length still matches
    the depth the user is at.
  -->
  {#if trail.length > 0}
    <nav aria-label="Breadcrumb" data-testid="folder-crumbs">
      <button type="button" onclick={() => open(null)} data-testid="crumb-root">All</button>
      {#each trail as c, i (i)}
        {#if i > 0}<span aria-hidden="true">/</span>{/if}
        {#if c === null}
          <span data-testid="crumb-missing" aria-disabled="true">missing</span>
        {:else}
          <button
            type="button"
            onclick={() => open(c.id)}
            aria-current={isCurrentCrumb(trail, i) ? 'page' : undefined}
            data-testid="crumb"
            data-current={isCurrentCrumb(trail, i) ? 'true' : 'false'}
          >
            {c.name}
          </button>
        {/if}
      {/each}
    </nav>
  {/if}

  <!--
    What the open folder is actually showing. A user who does not know that
    opening a folder REPLACES the filter rather than narrowing it will keep
    adding filters and end up with nothing.
  -->
  <p data-testid="folder-filter">
    {#if filterLabel === null}
      Everything
    {:else}
      {filterLabel}
    {/if}
  </p>

  <ul role="tree" data-testid="folder-tree" data-rows={rows.length}>
    {#each rows as { node, hidden } (node.folder.id)}
      {@const f = node.folder}
      <li role="treeitem" aria-level={node.depth + 1} data-testid="folder-row" data-id={f.id} data-depth={node.depth}>
        <span style:padding-left="{node.depth * 14}px" class="indent"></span>
        {#if f.icon !== null}<span aria-hidden="true">{f.icon}</span>{/if}
        <button
          type="button"
          class="name"
          onclick={() => open(f.id)}
          aria-current={nav.open === f.id ? 'page' : undefined}
          data-testid="folder-name"
        >
          {f.name}
        </button>
        {#if f.notify}
          <!--
            §5.14 smart-collection notification. On the folder AND inherited by
            its ancestors, so the dot is on the one the user has to open to find
            it.
          -->
          <span title="Notifies on new matches" data-testid="folder-notify" data-inherited={f.notify !== node.notifies ? 'false' : 'true'}>●</span>
        {/if}
        <span class="count" data-testid="folder-count">{formatCount(count(f.id))}</span>
        {#if node.children.length > 0}
          <button
            type="button"
            class="twisty"
            onclick={() => p.ontoggle?.(f.id)}
            aria-expanded={isExpanded(nav, f.id) ? 'true' : 'false'}
            data-testid="folder-twisty"
          >
            {isExpanded(nav, f.id) ? '▾' : '▸'}
          </button>
        {/if}
        {#if hidden > 0}
          <!-- A collapsed folder says how much is inside. A twisty with no count
               is a control that hides things without admitting it. -->
          <span class="hidden-count" data-testid="folder-hidden">{hidden} hidden</span>
        {/if}
      </li>
    {/each}
  </ul>

  <!--
    Broken data, said out loud. Both of these are ordinary events -- a deleted
    parent, a drag into a descendant -- and a navigator that hides them leaves
    the user with a folder they cannot find and no reason why.
  -->
  {#if tree.orphans.length > 0}
    <p role="status" data-testid="folder-orphans">
      {tree.orphans.length} folder{tree.orphans.length === 1 ? '' : 's'} ha{tree.orphans.length === 1 ? 's' : 've'} a
      missing parent.
    </p>
  {/if}
  {#if tree.cycles.length > 0}
    <p role="status" data-testid="folder-cycles">
      {tree.cycles.length} folder{tree.cycles.length === 1 ? '' : 's'} form a loop and are shown at the top.
    </p>
  {/if}
</aside>

<style>
  aside {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 200px;
    padding: 8px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .name {
    flex: 1;
    text-align: left;
    background: none;
    border: 0;
    color: inherit;
    font: inherit;
    padding: 2px 0;
    cursor: pointer;
  }
  .name[aria-current='page'] {
    font-weight: 600;
  }
  .count,
  .hidden-count {
    font-size: 11px;
    opacity: 0.6;
  }
  .twisty {
    background: none;
    border: 0;
    color: inherit;
    cursor: pointer;
    width: 16px;
  }
</style>
