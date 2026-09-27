<!--
  The wall: a grid in labelled sections, with auto-scroll. Spec 10.4, plan
  T-P5-006 item 14; #6544, #6955.

  # Why this does not reuse VirtualGrid

  `VirtualGrid` fixes the row height, which is what lets it compute the scroll
  height of a 100,000 item library from the count alone. A grouped wall has a
  VARIABLE row height: a header, then however many rows the group happens to
  need. So the scrolling is this component's own, and `wall.ts` owns the
  geometry. The reasoning, including why the scrollbar is approximate while
  groups are loading, is in that file's header and is not repeated here.

  # What is in this component and what is not

  Only DOM: the scroll container, the listener, the section elements. Every
  decision -- where the boundaries fall, how tall a pending group reserves,
  whether the wall should follow the bottom -- is a function in `wall.ts` with
  its own tests. That split is why the mutation script can reach the rules at
  all; nothing in a `.svelte` file is mutatable.

  # The scroll listener

  On this element, not on `window`, and the same reason as `VirtualGrid`: a
  window listener fires for unrelated scrolling and recomputes the window on
  every one of them, and on a trackpad that is every frame.
-->
<script lang="ts">
  import type { ObjectRow } from '$lib/api/client.js';
  import { KeysetStore } from '$lib/api/keyset.js';
  import type { GridQuery } from '$lib/api/keyset.js';
  import { tileShape, POSTER_RATIO } from '$lib/api/media-view.js';
  import {
    autoScroll,
    columnCount,
    groupExtents,
    groupLabel,
    groupRows,
    initialScrollTop,
    pendingPerGroup,
    type GroupKey,
    type WallMetrics
  } from '$lib/api/wall.js';

  interface Props {
    /** The query, from the URL. Changing it resets the wall. */
    query: GridQuery;
    /** How to group. `none` is a plain grid with no headers. */
    groupBy?: GroupKey;
    /** Injectable store, for tests. */
    store?: KeysetStore;
  }

  const p: Props = $props();

  const own = p.store ?? new KeysetStore();

  /**
   * The store, mirrored.
   *
   * `KeysetStore` is a plain class with a `get state()`, and a getter over
   * `$state` is NOT a tracked dependency: reading `own.state` in a `$derived`
   * registers nothing, so the derived computes once at mount and never again.
   * The wall then renders an empty list forever while the page looks finished,
   * and the build is perfectly happy. `ImageGrid` and the list route both
   * mirror the store the same way, and reassign after every load.
   */
  let state = $state(own.state);

  /** Tile width, which sets the column count and therefore the row height. */
  const TILE = 160;
  const GAP = 8;
  const HEADER = 28;

  let viewport = $state({ width: 0, height: 0 });
  let scrollTop = $state(0);
  let pinned = $state(false);

  const columns = $derived(columnCount(viewport.width || TILE * 4, TILE, GAP));
  const metrics = $derived<WallMetrics>({ tile: TILE, gap: GAP, headerHeight: HEADER, columns });

  const rows = $derived(state.rows);
  const groups = $derived(groupRows(rows, p.groupBy ?? 'none'));
  const label = $derived(groupLabel(p.groupBy ?? 'none'));

  /**
   * How many rows are still unloaded in each group.
   *
   * `total` is the query's count, so the wall can reserve space for rows it has
   * not seen. This is why a grouped wall's scrollbar is approximate while it
   * loads, and `exact` below is the flag that says so rather than pretending.
   */
  const pending = $derived(
    pendingPerGroup(groups, groups.map((g) => g.indices.length), state.totalCount ?? rows.length)
  );
  const extents = $derived(groupExtents(groups, pending, TILE * POSTER_RATIO, metrics));
  const tileHeight = $derived(TILE * POSTER_RATIO);

  /**
   * Load one page and mirror the result.
   *
   * The mirror is the load-bearing half. `own.state` is a getter over `#state`,
   * which is not reactive state at all, so nothing re-renders when it changes.
   * Assigning the snapshot into `$state` is what makes the wall grow; without
   * it the first page renders and every page after it is fetched and discarded.
   */
  async function loadMore(): Promise<void> {
    await own.loadMore();
    state = own.state;
  }

  $effect(() => {
    // Read the query so the effect re-runs when it changes, then reset and
    // load. `reset` drops the cursor and bumps the generation, so a slow page
    // from the previous filter cannot land in the new one.
    void p.query;
    scrollTop = initialScrollTop();
    pinned = false;
    own.reset(p.query);
    state = own.state;
    void loadMore();
  });

  /**
   * Follow the bottom while pinned.
   *
   * Runs on every change to the extents, which is what makes it follow: the
   * scroll height grows as rows arrive, and this is the only thing that moves
   * the viewport to the new bottom. The decision is `autoScroll`'s; this is the
   * call and the assignment.
   */
  $effect(() => {
    const state = { scrollTop, scrollHeight: extents.totalHeight, clientHeight: viewport.height, pinned };
    const r = autoScroll(state);
    pinned = r.pinned;
    if (r.scrollTop !== null && r.scrollTop !== scrollTop) {
      scrollTop = r.scrollTop;
      viewportEl?.scrollTo({ top: r.scrollTop });
    }
  });

  let viewportEl = $state<HTMLElement | null>(null);

  function onscroll(): void {
    const el = viewportEl;
    if (el === null) return;
    scrollTop = el.scrollTop;
    // Fetch the next page before the user reaches the end, not at it: at the end
    // means they see the bottom of the content and then watch it not move.
    if (el.scrollHeight - el.scrollTop - el.clientHeight < el.clientHeight) {
      void loadMore();
    }
  }

  /** The top of each group, so the scroll maths is not done in the template. */
  const offsets = $derived.by(() => {
    const out: number[] = [];
    let at = 0;
    for (const h of extents.groupHeights) {
      out.push(at);
      at += h + metrics.gap;
    }
    return out;
  });
</script>

<div class="wall" bind:this={viewportEl} bind:clientWidth={viewport.width} bind:clientHeight={viewport.height} onscroll={onscroll} data-testid="wall" data-exact={extents.exact} data-pinned={pinned} data-groups={groups.length} data-total={extents.totalHeight}>
  <!--
    The spacer is what makes the scrollbar real. The sections below are
    absolutely positioned inside it at the offsets the pure geometry computed,
    so the rendered height and the scroll height are the same number -- there
    is no separate "content height" that can disagree with the scroll range.
  -->
  <div class="spacer" style:height="{extents.totalHeight}px" data-testid="wall-spacer"></div>

  <div class="sections" style:height="{extents.totalHeight}px">
    {#each groups as g (g.value + ':' + g.index)}
      <section
        class="group"
        data-testid="wall-group"
        data-value={g.value}
        data-pending={pending[g.index]}
        style:top="{offsets[g.index]}px"
        style:height="{extents.groupHeights[g.index]}px"
      >
        {#if label !== null}
          <h2 data-testid="wall-group-label">{label}: {g.value}</h2>
        {/if}
        <div class="items">
          {#each g.indices as i (rows[i]!.id)}
            {@const r = rows[i]!}
            {@const shape = tileShape(r, POSTER_RATIO)}
            <a
              class="tile"
              href="/object/{r.id}"
              data-testid="wall-tile"
              data-kind={shape.kind}
              style:width="{TILE}px"
              style:height="{tileHeight}px"
            >
              {#if r.coverPath !== null}
                <img src="/media/{r.coverPath}" alt="" loading="lazy" width={TILE} height={tileHeight} />
              {/if}
              <span class="title">{r.title ?? r.id}</span>
            </a>
          {/each}
        </div>
      </section>
    {/each}
  </div>
</div>

<style>
  .wall {
    position: relative;
    overflow-y: auto;
    height: 100%;
  }
  .spacer {
    width: 1px;
  }
  .sections {
    position: absolute;
    inset: 0;
  }
  .group {
    position: absolute;
    left: 0;
    right: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  h2 {
    font: 600 13px/28px system-ui, sans-serif;
    margin: 0;
    height: 28px;
    color: var(--muted, #888);
  }
  .items {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .tile {
    position: relative;
    overflow: hidden;
    background: #1a1a1a;
    display: block;
  }
  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .title {
    position: absolute;
    inset: auto 0 0 0;
    font: 11px/1.6 system-ui, sans-serif;
    padding: 2px 4px;
    background: #000a;
    color: #fff;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
