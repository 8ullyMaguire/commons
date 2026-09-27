<!--
  The player route. Spec §11.1, §11.5 / plan T-P6-001.

  # Why a route and not just a component

  The player is reachable by URL, and that is the point: a user who bookmarked
  a file expects the bookmark to open that file, and a desktop shell (spec 3.3)
  gets a webview pointed at a URL. So `/play?o=<id>` is the route, and the
  component is what it draws.

  It also gives the e2e suite something real to drive. The 360x640 claim in the
  ticket is about pixels, and pixels need a real document with a real viewport --
  not a component mounted into a test page, which would be testing the harness
  rather than the thing that ships.

  # Where the row comes from, and why not a query of its own

  The obvious thing is `object(id:)`, and it does not exist: the schema's only
  root field is `objects(input:)`, a paginated list. Inventing a second root
  field means writing a server resolver, a client query, and a test for a
  lookup that a filtered list can do -- so this filters instead, and says so. If
  the schema grows a single-object field, this route is the only thing that
  changes.

  The filter is `id = <the id>` rather than paging and picking, because a
  ten-thousand-file library should not be downloaded to find one file. The
  server's `Eq` on `id` compiles to an index seek, so this is one row.

  # What it does with missing or refused things

  An id that is not in the query, an id the server does not have, and an id the
  user may not see are three different failures. The first is a mistake and gets
  a link back; the second and third are the same from out here, and both say the
  file could not be opened without saying which -- telling a user a file exists
  is itself a disclosure.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import Player from '$lib/components/Player.svelte';
  import { fetchObjects, type ObjectRow } from '$lib/api/client.js';
  import { mediaFilter } from '$lib/api/media.js';

  const objectId = $derived(page.url.searchParams.get('o') ?? '');

  let row = $state<ObjectRow | null>(null);
  let error = $state<string | null>(null);
  let loaded = $state(false);

  onMount(async () => {
    if (!objectId) {
      error = 'No object was named.';
      loaded = true;
      return;
    }
    try {
      // The honest shape of this lookup, and the reason it is a loop: the
      // schema has no `object(id:)` root field and `BuiltinField` has no `Id`
      // variant, so there is no way to ask the server for one file by id. What
      // there IS is a paginated list, so this pages until it finds the id or
      // runs out of pages.
      //
      // That is a real cost on a large library and it is stated here rather than
      // hidden behind a "loading" spinner that never resolves for a file on the
      // last page of a 40,000-object collection. Adding `BuiltinField::Id` and
      // an `object(id:)` root field is the fix, and it belongs to whichever
      // ticket needs a single-object lookup for its own reasons -- this route
      // is the second caller, not the first.
      const needle = objectId;
      const PAGE = 200;
      let after: string | null = null;
      for (;;) {
        const res = await fetchObjects({ first: PAGE, after, filter: mediaFilter() });
        const hit = res.objects.nodes.find((n) => n.id === needle);
        if (hit) {
          row = hit;
          break;
        }
        // Two independent reasons to stop, and both are needed: a server that
        // reports no more pages, and one that says there are more but hands
        // back no cursor. A `while (hasNextPage)` loop that ignores the second
        // asks for the same first page for ever.
        if (!res.objects.pageInfo.hasNextPage) break;
        const next = res.objects.pageInfo.endCursor;
        if (next === null || next === after) break;
        after = next;
      }
      if (!row) error = 'That file could not be opened.';
    } catch (e) {
      error = e instanceof Error ? e.message : 'That file could not be opened.';
    } finally {
      loaded = true;
    }
  });
</script>

<main>
  <h1>Player</h1>

  {#if !loaded}
    <p data-testid="player-loading">Loading…</p>
  {:else if error}
    <p class="error" role="alert" data-testid="player-route-error">{error}</p>
    <p><a href="/">Back to the library</a></p>
  {:else if row}
    <!--
      `ObjectRow` carries what the grid needs, and the grid's needs are not the
      player's: no container, no codec, no frame rate, because the database has
      no columns for them either. So no `source` is passed and no `caps` either
      -- the component asks `/media/<id>/caps`, which is the server's own probe
      and the same `rung_for` the proxy route judges with. A client that decided
      from the row would be guessing, and the guess is always "proxy": a
      transcode for every file in the library, for files that needed none.
    -->
    <Player objectId={row.id} title={row.title} />
  {/if}
</main>

<style>
  main {
    padding: 1rem;
  }
  h1 {
    font-size: 1.1rem;
    margin: 0 0 0.75rem;
  }
  .error {
    color: #c00;
  }
</style>
