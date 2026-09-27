<!-- The vertical feed route (§10.6, #3859).

     Same rule as the main browse surface and for the same reason: every piece
     of state is in the URL and every change goes back to the URL (§5.16,
     §15.10). A focus index held in a component variable is lost on reload,
     cannot be linked, and does not answer the back button -- and the focus of a
     feed is the one piece of state a user is most likely to want to send
     someone ("this one, at 2:14").

     So the route reads `?focus=` and writes it back with `replaceState`, and
     `VerticalFeed` stays a pure function of a prop. -->
<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import VerticalFeed from '$lib/components/VerticalFeed.svelte';
  import { viewFromLocation, type ViewState } from '$lib/api/view.js';
  import { KeysetStore } from '$lib/api/keyset.js';
  import { nextFocus } from '$lib/api/feed.js';

  const view = $derived<ViewState>(viewFromLocation(page.url));

  /**
   * The focus index, read from the URL.
   *
   * `viewFromLocation` does not know about `focus` -- it is feed state, not
   * grid state, and putting it there would make a grid's URL carry a parameter
   * that does nothing on a grid. Parsed here instead, and parsed strictly for a
   * reason: a hand-edited `?focus=abc` must not become NaN and then an
   * invisible item. `Number('')` is 0, so an empty value is the start of the
   * feed; `parseInt('12abc')` is 12, so a typo'd link would silently open item
   * 12. The regex is the strict version of both.
   */
  const focus = $derived<number>(parseFocus(page.url.searchParams.get('focus')));

  function parseFocus(raw: string | null): number {
    if (raw === null) return 0;
    if (!/^\d+$/.test(raw)) return 0;
    const n = Number(raw);
    return Number.isSafeInteger(n) ? n : 0;
  }

  /**
   * How many items the feed holds.
   *
   * A WINDOW, and the number is the point of the whole exercise. §4.2 says a
   * virtualized view holds no more than a window of rows, and the feed is where
   * that is easiest to get wrong: it looks like a list, it iterates a prop, and
   * handing it 100,000 rows renders 100,000 slides with no error anywhere.
   * `WINDOW` is generous for a feed -- a phone holds a few seconds of video in
   * memory, not a library -- and the e2e spec asserts the rendered count is
   * bounded.
   */
  const WINDOW = 12;

  /**
   * The rows, loaded through the same keyset store the grid uses.
   *
   * Reusing `KeysetStore` rather than fetching here is the same argument as
   * reusing `classifyDrag`: it already knows about generations (so a slow page
   * from a previous filter cannot land in the new one), about a page in
   * flight, and about the cursor. A second loader in this route would be a
   * second set of those three bugs.
   *
   * `reset` on every query change, keyed on the *encoded* query rather than
   * the object: `view` is a fresh object on every `$derived` evaluation, so
   * comparing it by identity would reset the store on every render and the feed
   * would never load a second page.
   */
  const store = new KeysetStore({ pageSize: WINDOW });
  const queryKey = $derived(JSON.stringify(view));

  // The store is a plain class, not a Svelte store, so nothing re-renders when
  // it changes. `VirtualGrid` solves this by copying `store.state` into `$state`
  // and reassigning it after every load, and this route does the same rather
  // than subscribing -- subscribing properly would mean the store being a Svelte
  // store, which is what makes it testable without Svelte at all.
  let grid = $state(store.state);
  let loadedFor = $state<string | null>(null);
  $effect(() => {
    const key = queryKey;
    if (loadedFor === key) return;
    loadedFor = key;
    store.reset({ filter: view.filter, sort: view.sort, direction: view.direction });
    grid = store.state;
    void store.loadMore().then(() => {
      grid = store.state;
    });
  });

  const rows = $derived(grid.rows);

  /** The window, sliced to the focus, with the focus at its head. */
  const windowed = $derived.by(() => {
    const f = nextFocus(focus, 0, rows.length);
    // PREFETCH behind as well as ahead: a drag DOWN has to have the previous
    // item already loaded or it springs back against nothing.
    const start = Math.max(0, Math.min(f - 1, rows.length - WINDOW));
    return rows.slice(start, start + WINDOW);
  });

  /** The focus, rebased onto the window rather than onto the whole list. */
  const focusInWindow = $derived.by(() => {
    const f = nextFocus(focus, 0, rows.length);
    const start = Math.max(0, Math.min(f - 1, rows.length - WINDOW));
    return Math.min(Math.max(f - start, 0), Math.max(windowed.length - 1, 0));
  });

  function setFocus(next: number) {
    // Clamp against the WHOLE list, not the window: the window is a slice, and
    // clamping to it would make "next" stop at the edge of what happens to be
    // loaded, which is the lightbox bug `ImageGrid`'s own comment describes.
    const clamped = nextFocus(next, 0, rows.length);
    const u = new URL(page.url);
    if (clamped === 0) u.searchParams.delete('focus');
    else u.searchParams.set('focus', String(clamped));
    // `replaceState`: swiping through forty items should not push forty history
    // entries, and the back button on a feed should leave the feed rather than
    // walk backwards through it one item at a time.
    goto(u.pathname + u.search, { replaceState: true, keepFocus: true, noScroll: true });
    // Past the loaded window, ask for more. A no-op when a page is in flight or
    // the list is finished, so a fast swipe through a short list does not fire
    // forty requests.
    if (clamped >= grid.rows.length - 2) {
      void store.loadMore().then(() => {
        grid = store.state;
      });
    }
  }
</script>

<svelte:head>
  <title>Commons — Feed</title>
</svelte:head>

<VerticalFeed items={windowed} focusIndex={focusInWindow} onfocuschange={setFocus} />
