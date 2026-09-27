<!--
  The Media tab: scenes and images in one list. Spec 5.17, 10.4, 10.5; plan
  T-P5-006 item 17; #1030, #7068, #1508.

  # What "unified" costs, stated up front

  The model is already unified — §5.3, "Image is an Object, so it votes like
  one" — so the whole server side of this tab is one facet: `kind IN ('scene',
  'image')`, which `CmpOp::In` already compiles and which composes with every
  other filter. There is no join and no second identity.

  What is NOT free is the presentation, and this page is where the two kinds stop
  being interchangeable: a scene is playable and an image is not, a scene with an
  unmeasured duration is still playable, and a row of a kind this build cannot
  place has to be SAID rather than dropped.

  # The tab replaces the URL's filter, and that is a real limitation

  `view.ts` is explicit: the filter is "one opaque, versioned, base64url-encoded
  string... it is the thing the server parses", and no UI code parses it. So a
  `kind` restriction cannot be ANDed onto a user's `?q=cat` without decoding it,
  which is the thing the design forbids.

  The result: opening this tab with a filter in the URL shows scenes and images
  matching nothing in particular, and `?q=` is not honoured. That is a genuine
  gap, not a shortcut, and it is the first thing to fix when the filter codec
  moves to the client (spec 5.16 wants the whole AST round-tripping through the
  URL). What this page does instead of hiding it: `mediaFilter` replaces, and
  `looksLikeFilter` decides whether a hand-edited `q` is worth carrying, so the
  behaviour is at least knowable rather than accidental.

  # The audit line is the load-bearing part

  `object.kind` is unconstrained `TEXT` in `0001_core.sql` and an enum in
  `commons-core`, and nothing at the boundary keeps them in sync. So an unknown
  kind is an ordinary event. A grid that drops those rows looks correct and
  silently loses content — the one failure a user cannot diagnose from the screen
  in front of them — so the page reports how many rows it could not place, and
  names the kind, which is enough for someone to file a bug with.
-->
<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import Wall from '$lib/components/Wall.svelte';
  import { parseGroupKey, type GroupKey } from '$lib/api/wall.js';
  import { auditMedia, MEDIA_KINDS, mediaFilter, type MediaAudit } from '$lib/api/media.js';
  import { viewFromLocation, type ViewState } from '$lib/api/view.js';
  import type { ObjectRow } from '$lib/api/client.js';

  const groupBy = $derived(parseGroupKey(page.url.searchParams.get('group')));

  /**
   * The view, with the media facet forced on.
   *
   * `filter` is replaced rather than ANDed, for the reason in the header. The
   * result is spread over `viewFromLocation` rather than rebuilt field by field,
   * so a new field on `ViewState` — density, mode, sort — reaches this tab
   * without anyone remembering to add it here.
   */
  const query = $derived.by((): ViewState => {
    const base = viewFromLocation(page.url);
    return { ...base, filter: mediaFilter() };
  });

  /**
   * What the last page could not place.
   *
   * `$state` and not view state, and deliberately: an audit is a *report about*
   * the rows that arrived, not part of the query. Putting it in the URL would
   * make a transient condition shareable, so a link sent to someone would carry
   * "12 rows of kind hologram" as though it were a property of the library.
   */
  let audit = $state<MediaAudit | null>(null);

  function onRows(rows: readonly ObjectRow[]): void {
    audit = auditMedia(rows);
  }

  function setGroup(g: GroupKey): void {
    const url = new URL(page.url);
    url.searchParams.delete('group');
    if (g !== 'none') url.searchParams.set('group', g);
    void goto(`${url.pathname}${url.search}`, { replaceState: true, noScroll: true, keepFocus: true });
  }
</script>

<svelte:head><title>Media</title></svelte:head>

<nav aria-label="Group by" data-testid="media-group">
  <!--
    No 'kind' group. `GROUP_KEYS` in `wall.ts` is the list of *data* fields, and
    `kind` is not one of them: adding it there would make every wall offer a
    grouping that is a no-op on the other tabs, and "Kind" appearing on a page
    where every row is already a scene is a control that does nothing.
  -->
  <button type="button" onclick={() => setGroup('none')} aria-current={groupBy === 'none' ? 'true' : 'false'}>None</button>
  <button type="button" onclick={() => setGroup('month')} aria-current={groupBy === 'month' ? 'true' : 'false'}>Month</button>
  <button type="button" onclick={() => setGroup('year')} aria-current={groupBy === 'year' ? 'true' : 'false'}>Year</button>
</nav>

<!--
  What the tab is. §5.17 asks for scenes and images, and saying so on screen is
  what makes the ABSENCE of a gallery legible: a user who wonders where their
  galleries went reads the scope rather than inferring it from an empty grid.
-->
<p data-testid="media-scope">Scenes and images</p>

<!--
  `Wall`, not `VirtualGrid`. §10.4 asks for group-by with collapsible sections
  and auto-scroll, and `Wall` is the component that has both — `VirtualGrid` has
  neither, so choosing it here would quietly ship a Media tab without the
  grouping the same spec line requires of every view mode.
-->
<Wall {query} {groupBy} onrows={onRows} />

<!--
  A row of a kind this build cannot place. The count and the spelling, because
  "some items are missing" is unactionable and `hologram` is a bug report.
-->
{#if audit !== null && audit.unknownKinds.length > 0}
  <p role="status" data-testid="media-unknown">
    {audit.excluded} row{audit.excluded === 1 ? '' : 's'} of a kind this build does not
    recognise: {audit.unknownKinds.join(', ')}.
  </p>
{/if}

<style>
  nav {
    display: flex;
    gap: 8px;
    padding: 6px 8px;
    font-size: 12px;
  }
  nav button[aria-current='true'] {
    font-weight: 600;
  }
  [data-testid='media-unknown'] {
    padding: 6px 8px;
    font-size: 12px;
    opacity: 0.8;
  }
</style>
