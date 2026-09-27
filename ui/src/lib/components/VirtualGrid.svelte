<!--
  The virtualized grid. Every browse surface in Phase 5 is built on this
  (spec 4.2, 10.4).

  # Fixed row height is not a simplification, it is the design

  A variable-height virtualizer needs measured heights, which means a layout
  pass before the first paint, which means a blank frame and a scrollbar that
  grows as you scroll. A fixed row height means the scroll height of a 100,000
  item library is computable from the count alone, so the scrollbar is right on
  the first frame and the user can drag it to the end immediately. Tiles are a
  fixed size in a responsive column count, so the ROW height is fixed once the
  column count is known, and that is the only thing that varies.

  # What is in the DOM

  Only the rows in the viewport, plus `OVERSCAN` rows above and below. At a
  1080p viewport with 240px rows that is about 5 visible rows, so the rendered
  set is roughly 15-20 rows for any library size. The Playwright test asserts
  the rendered count stays under a bound with 5,000 items loaded, which is the
  assertion that keeps this honest: without it, a regression that renders every
  row still "works" and only fails at 100k where nobody has a library.

  # Where the scroll listener is attached

  On the scroll container, not on `window`. The grid is a bounded viewport
  inside a page; a window listener fires for unrelated scrolling and recomputes
  the window on every one, and on a trackpad that is every frame.
-->

<script lang="ts">
  import type { ObjectRow } from '$lib/api/client.js';
  import { KeysetStore } from '$lib/api/keyset.js';
  import type { GridQuery } from '$lib/api/keyset.js';
  import type { ViewState } from '$lib/api/view.js';
  import { tileShape, POSTER_RATIO } from '$lib/api/media-view.js';

  interface Props {
    /** The query, from the URL. Changing it resets the list. */
    query: GridQuery;
    /** Tile width in CSS pixels. Row height is derived from the aspect ratio. */
    density?: number;
    /** Injectable store, for tests. */
    store?: KeysetStore;
    /** Columns. 0 means "derive from the container width". */
    columns?: number;
    /**
     * The declared row ratio: what a row is shaped by, and what a row whose
     * media is unknown falls back to.
     *
     * A prop and not a per-row computation, and that is the whole trick. A
     * *proportional* grid -- every tile its own aspect, nothing cropped -- makes
     * the row height a function of the row's contents, and a virtualizer's
     * window is a function of the row height, so the window would depend on rows
     * it has not loaded. The symptom is not a crash: it is a grid that scrolls
     * to the wrong place.
     *
     * So the row height stays the declared ratio applied to the tile width (see
     * `rowHeight`), and the *tile* is proportional within that box: a landscape
     * image is drawn at its own width/height and centred, with the row's spare
     * pixels above and below. Nothing is cropped, the scroll math is unchanged,
     * and the cost is a little vertical slack in a row of mixed media -- which
     * is what every photo browser does and is the honest rendering.
     */
    aspect?: number;
  }

  let {
    query,
    density = 240,
    store = new KeysetStore(),
    columns = 0,
    aspect = POSTER_RATIO
  }: Props = $props();

  /** How many rows to render above and below the viewport. */
  const OVERSCAN = 3;

  /**
   * Tile aspect, WIDTH : HEIGHT -- 2:3, the portrait poster shape most
   * libraries are full of.
   *
   * Note the direction. This is width divided by height, so a tile `w` pixels
   * wide is `w / ASPECT` pixels tall. The first version of this file used
   * `density * ASPECT` for the row height, which multiplies a width by a
   * width/height ratio and so produces a height that is 2.25x too small for
   * a 2:3 tile. Every row overflowed its box, and Chromium would not settle on
   * a layout -- the page locked its main thread on any result short enough to
   * fit on one screen, which is every result under about 500 items, and the
   * five tests that used 5,000 items all passed.
   */
  const GAP = 8;

  /**
   * Height reserved for a tile's title, under the image.
   *
   * The row height is derived from the tile WIDTH, which fixes the image
   * height, but a tile is the image AND its caption. Leaving the caption out
   * made every row about 19px too short, so titles were clipped by the row
   * below -- visually wrong, and invisible to every test that only measured
   * the image. Measured from the rendered caption at the default density
   * rather than guessed; it is a single line of text at the tile's font size.
   */
  const CAPTION = 20;

  let viewport: HTMLDivElement | undefined = $state();
  // Seeded from the prop, then owned by the ResizeObserver. The initial value
  // is the prop's, which is correct -- `columns` is 0 for "decide from the
  // measured width" and a real number when the caller pins it.
  // svelte-ignore state_referenced_locally
  let cols = $state(columns);
  let scrollTop = $state(0);
  let viewportHeight = $state(0);

  // The store is not a Svelte store, so this is the component's copy of the
  // last snapshot it read, refreshed after every load. The initial value is
  // the store's starting state, which is the empty list -- correct, because
  // nothing has been requested yet.
  // svelte-ignore state_referenced_locally
  let view = $state(store.state);

  // The store is a plain object with no Svelte knowledge, so the component
  // pulls from it after each load. Subscribing properly would mean the store
  // being a Svelte store, which would make it untestable without Svelte.
  async function refresh() {
    await store.loadMore();
    view = store.state;
  }

  /**
   * Retry after an error. It has to go through `refresh`, not call the store
   * directly: the store is not a Svelte store, so nothing re-renders when it
   * changes. The first version called `store.retry()` from the template and
   * then assigned `view` in a `.then()`, which works, but only just -- the
   * same two lines appear in three places and the third one is missing the
   * assignment. One function, one job.
   */
  async function retry() {
    await store.retry();
    view = store.state;
  }

  // A query change (the URL changed) starts a new list. The comparison is on
  // the query object, not the identity, because the router hands back a new
  // object on every navigation even when nothing changed.
  // Deliberately NOT $view. This is a snapshot of the query as of the last
  // reset, and the effect below compares against it to decide whether the
  // query changed. Making it reactive would make the effect re-run on its own
  // write and reset the list in a loop. Svelte's `state_referenced_locally`
  // warning is correct about the reference and wrong about the intent, so the
  // intent is spelled out here.
  // svelte-ignore state_referenced_locally
  let lastKey = JSON.stringify(query);
  $effect(() => {
    const key = JSON.stringify(query);
    if (key !== lastKey) {
      lastKey = key;
      store.reset(query);
      view = store.state;
      refresh();
    }
  });

  // First load.
  //
  // The guard has to include `atEnd` and `loaded`, not just rows/loading/error.
  // An earlier version tested only `rows.length === 0 && !loading && !error`,
  // which is true again immediately after a SUCCESSFUL LOAD THAT RETURNED
  // NOTHING -- so the effect fired again, fetched again, got nothing again,
  // and the grid spun on `/graphql` forever. It looks like a slow network and
  // is a request loop; the empty library is the case that triggers it, which is
  // why it survives contact with any real dataset.
  //
  // `started` is the honest "has a load been attempted" flag, and it is what
  // `loaded` cannot be: an empty result leaves `loaded` at 0, which looks
  // identical to a list that has not started.
  $effect(() => {
    const s = view;
    if (!s.started && !s.loading && !s.error) {
      refresh();
    }
  });

  // Measure. A ResizeObserver rather than a window resize listener, because the
  // grid's width changes when a sidebar collapses, not only when the window
  // does.
  $effect(() => {
    if (!viewport) return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width ?? 0;
      if (w > 0) cols = columns > 0 ? columns : Math.max(1, Math.floor((w + GAP) / (density + GAP)));
      // Height is read but NOT assigned to the reactive `viewportHeight` from
      // inside this callback. Doing so is a resize loop: `visibleCount`
      // derives from `viewportHeight`, so writing it re-renders the window,
      // which re-lays-out the spacer, which resizes the viewport, which fires
      // this observer again. The first version of this component did exactly
      // that and locked the main thread on any non-empty result.
      //
      // The height is picked up by the measure effect below, which runs after
      // Svelte has flushed the DOM and so is outside the observer's own call
      // stack.
    });
    ro.observe(viewport);
    return () => ro.disconnect();
  });

  /**
   * Measure the viewport height, but never from inside the ResizeObserver.
   *
   * The first version of this component assigned `viewportHeight` from the
   * observer's callback. That is a resize feedback loop, and it locked the
   * main thread: `visibleCount` derives from `viewportHeight`, so writing it
   * re-renders the window, which re-lays-out the spacer, which resizes the
   * viewport, which fires the observer again. With a non-empty result the
   * page stopped responding entirely.
   *
   * The rAF version was also wrong, in a quieter way: an $effect that returns
   * `cancelAnimationFrame` gets its cleanup run before the frame in some
   * paths, so the measurement silently never happened and the grid rendered
   * zero rows. The measurement is done synchronously instead. Svelte flushes
   * effects after the DOM is updated, so `clientHeight` here is the height of
   * the viewport that was just laid out with the current spacer.
   */
  $effect(() => {
    density;
    columns;
    // `view.rows.length` is a dependency on purpose. The viewport has a
    // height from the first frame, but the WINDOW inside it is sized from
    // `viewportHeight`, so a viewport measured before the first rows land
    // renders zero rows -- and the empty state shows even though the query
    // succeeded. Re-measuring on each load is what makes the retry-after-error
    // and empty-then-refetch paths render.
    view.rows.length;
    if (!viewport) return;
    const h = viewport.clientHeight;
    if (h > 0) viewportHeight = h;
  });

  function onScroll() {
    if (!viewport) return;
    scrollTop = viewport.scrollTop;

    // Two things must not happen here, and both of them cost an afternoon.
    //
    // Do not write `viewportHeight`. `visibleCount` derives from it, so writing
    // it re-renders the window, which re-lays out the spacer, which resizes
    // the scroll area, which fires `scroll` again. That is a direct re-entrant
    // loop. The height is measured by the effect above instead, which only
    // runs when something that can change it actually changes.
    //
    // Do not fetch when the list fits on screen. `remaining < viewportHeight`
    // is trivially true -- remaining is negative -- whenever the content is
    // shorter than the viewport, which is every result under about 500 items.
    // `refresh()` then reassigns `view`, the re-render re-applies the spacer
    // style, and the browser re-fires `scroll`: an infinite render loop with no
    // network traffic at all. It looks like a hang and a hung main thread, and
    // the request counter never moves, so it does not look like a fetch bug.
    //
    // The condition is therefore "are there more rows to fetch", not "is there
    // less than a screen left of pixels". The store still refuses a second
    // in-flight fetch, so calling this on every scroll is cheap.
    if (view.atEnd || view.loading) return;
    const remaining = viewport.scrollHeight - scrollTop - viewport.clientHeight;
    if (remaining < viewport.clientHeight) refresh();
  }

  const rowHeight = $derived(Math.round(density / aspect) + CAPTION + GAP);

  const total = $derived(view.totalCount ?? view.rows.length);
  const totalRows = $derived(Math.ceil(total / Math.max(1, cols)));
  const totalHeight = $derived(totalRows * rowHeight);

  // The first visible row, and the window. Computed from scrollTop and the
  // row height alone — no measurement of the children, which is what keeps this
  // O(1) per frame rather than O(rendered).
  const firstRow = $derived(Math.max(0, Math.floor(scrollTop / rowHeight) - OVERSCAN));
  const visibleCount = $derived(
    Math.ceil(viewportHeight / rowHeight) + OVERSCAN * 2
  );
  const lastRow = $derived(Math.min(totalRows, firstRow + visibleCount));

  const windowed = $derived(
    Array.from({ length: Math.max(0, lastRow - firstRow) }, (_, i) => {
      const rowIndex = firstRow + i;
      const start = rowIndex * cols;
      const items: (ObjectRow | undefined)[] = [];
      for (let c = 0; c < cols; c++) {
        items.push(view.rows[start + c]);
      }
      return { rowIndex, top: rowIndex * rowHeight, items };
    })
  );

  /** A stable key. The id, never the array index: an index key makes an
   * appended row re-render every row after it, which is the whole point of
   * appending. */
  function keyFor(r: ObjectRow | undefined, i: number): string {
    return r ? r.id : `empty-${i}`;
  }
</script>

<div
  class="grid-viewport"
  bind:this={viewport}
  onscroll={onScroll}
  style:height="100%"
  data-testid="grid-viewport"
>
  <!-- The spacer is what creates the scrollbar. Its height is computed from
       the total count, so the scrollbar is correct on the first frame rather
       than growing as pages load. -->
  <div class="grid-spacer" style:height="{totalHeight}px" data-testid="grid-spacer">
    <div
      class="grid-window"
      style:transform="translateY({firstRow * rowHeight}px)"
      data-testid="grid-window"
    >
      {#each windowed as row (row.rowIndex)}
        <div
          class="grid-row"
          style:height="{rowHeight}px"
          style:--tile-aspect={aspect}
          data-testid="grid-row"
        >
          {#each row.items as item, i (keyFor(item, i))}
            {#if item}
              {@const index = row.rowIndex * cols + i}
              <!--
                The tile's absolute index in the whole list, not its position in
                the rendered window. `ImageGrid` reads this to open the lightbox
                on the image that was clicked, and the lightbox pages by the same
                number -- so it is computed from `rowIndex * cols` here and
                nowhere else. A window-relative index would open the wrong image
                for every tile below the first row.
              -->
              <!--
                The tile's own shape, from the row and the declared ratio.
                Computed here rather than in a helper on the store because it is
                one row's worth of data and this is the only place it is used --
                a function called once per rendered tile, not once per row.
              -->
              {@const shape = tileShape(item, aspect)}
              <a
                class="tile"
                href="?id={item.id}"
                style:width="{density}px"
                data-testid="grid-tile"
                data-id={item.id}
                data-lightbox-index={index}
                data-aspect={shape.aspect}
                data-kind={shape.kind}
              >
                {#if item.coverPath}
                  <!--
                    `object-fit: contain` and an explicit height, NOT the
                    default `cover`. `cover` is what a square tile does, and it
                    is the thing this ticket exists to stop doing: it silently
                    crops a 16:9 thumbnail to a 2:3 box and a person scanning
                    the wall is being shown a picture of the media rather than
                    the media. `contain` letterboxes into the declared row box
                    and shows the whole frame.

                    The `max-height` is what stops a very tall strip from
                    growing its own row: the row height is fixed by `rowHeight`
                    and the image is bounded by it, which is also why the
                    scroll math above never has to see this.
                  -->
                  <img
                    src={item.coverPath}
                    alt=""
                    loading="lazy"
                    width={density}
                    style:max-height="{rowHeight - CAPTION}px"
                  />
                {:else}
                  <div class="tile-placeholder" aria-hidden="true">
                    <span>{item.kind}</span>
                  </div>
                {/if}
                <span class="tile-title">{item.title ?? 'Untitled'}</span>
              </a>
            {:else}
              <div class="tile-empty" style:width="{density}px" aria-hidden="true"></div>
            {/if}
          {/each}
        </div>
      {/each}
    </div>
  </div>

  {#if view.rows.length === 0 && !view.loading && !view.error}
    <p class="grid-empty" data-testid="grid-empty">Nothing here.</p>
  {/if}

  {#if view.error}
    <div class="grid-error" data-testid="grid-error">
      <p>{view.error}</p>
      <button onclick={retry}>Retry</button>
    </div>
  {/if}

  {#if view.loading}
    <div class="grid-loading" data-testid="grid-loading" aria-live="polite">Loading…</div>
  {/if}
</div>

<style>
  .grid-viewport {
    overflow-y: auto;
    position: relative;
    /* The scrollbar must not change the layout when it appears, or the column
       count flickers on first scroll. */
    scrollbar-gutter: stable;
  }
  .grid-spacer {
    position: relative;
    width: 100%;
  }
  .grid-window {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    /* No transition on transform: a transition makes a fast scroll lag behind
       the fingers, which reads as the app being slow. */
  }
  .grid-row {
    display: flex;
    gap: 8px;
    align-items: flex-start;
  }
  .tile {
    flex: none;
    text-decoration: none;
    color: inherit;
    display: flex;
    flex-direction: column;
  }
  /*
    * `object-fit: contain`, and the row's box as the aspect, not a hardcoded
    * 2/3.
    *
    * The hardcoded `aspect-ratio: 2 / 3; object-fit: cover` this replaces is
    * the whole complaint of #1030: a library of mixed stills and video renders
    * every tile as a portrait poster, so a 16:9 frame is cropped to a third of
    * its width and a tall scan is cropped at the top and bottom. `cover` is not
    * a rendering choice, it is a decision about what the user is allowed to
    * see, made in a stylesheet, by whoever wrote the first line.
    *
    * The aspect is now a variable the component sets per row, so a caller
    * choosing a different row shape does not have to override a rule with
    * `!important`. `contain` rather than `cover`: a media browser is for
    * choosing what to open, and a tile that has silently dropped a third of the
    * frame cannot be scanned.
    *
    * A backdrop so the letterbox reads as "here is the shape of the frame" and
    * not as "the image failed to load" -- the difference matters when the media
    * IS a tall strip in a landscape row.
  */
  .tile img,
  .tile-placeholder {
    width: 100%;
    aspect-ratio: var(--tile-aspect, 2 / 3);
    object-fit: contain;
    background: #222;
    border-radius: 4px;
  }
  .tile-placeholder {
    display: grid;
    place-items: center;
    color: #888;
    font-size: 0.8rem;
  }
  .tile-title {
    font-size: 0.8rem;
    line-height: 1.2;
    margin-top: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grid-empty,
  .grid-error,
  .grid-loading {
    position: absolute;
    top: 8px;
    left: 0;
    right: 0;
    text-align: center;
    color: #999;
  }
  .grid-error {
    color: #d66;
  }
</style>
