<!--
  The wall route. Spec 10.4, plan T-P5-006 item 14; #6544, #6955.

  A real route for the same reason the other Phase 5 surfaces are: the grouping
  is state, and state in a component variable is lost on reload, cannot be
  linked, and does not answer the back button. A user who wants to show someone
  "here is everything from Studio X, newest first" needs a URL that says so.

  The group key is one query parameter, and an unrecognised one falls back to
  `none` rather than rendering an empty wall -- a stale bookmark should show the
  grid, not an error.
-->
<script lang="ts">
  import { page } from '$app/state';
  import Wall from '$lib/components/Wall.svelte';
  import { GROUP_KEYS, parseGroupKey } from '$lib/api/wall.js';
  import { viewFromLocation } from '$lib/api/view.js';

  // `parseGroupKey` rather than an inline check, so the fallback rule lives
  // with the key list instead of being restated -- and is tested.
  const groupBy = $derived(parseGroupKey(page.url.searchParams.get('group')));

  // The same view state every other surface reads, so a filter or a sort
  // carried over from the grid applies here too.
  const query = $derived(viewFromLocation(page.url));
</script>

<svelte:head><title>Wall</title></svelte:head>

<nav aria-label="Group by">
  {#each GROUP_KEYS as k (k)}
    <a href="?group={k}" aria-current={k === groupBy ? 'page' : undefined}>{k}</a>
  {/each}
</nav>

<Wall {query} {groupBy} />
