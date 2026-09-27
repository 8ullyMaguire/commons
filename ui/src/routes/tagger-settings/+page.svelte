<!--
  Tagger settings, per-field ignore lists. Spec 10.10, plan T-P5-006 item 13;
  #2318, #2399.

  A real route for the reason `create-from-subpage` and `tags` are: the two
  scopes are the state, and state in a component variable is lost on reload,
  cannot be linked, and does not answer the back button. A user who wants to
  tell someone "this is how I configure the tagger" needs a URL that says so.

  Both scopes round-trip through the URL — one repeated parameter each, never a
  joined string — so a configured tagger is a link, and so the e2e has a way to
  set up a state that would otherwise need a click-through.

  It does not persist. The GraphQL server is T-P6-007.
-->
<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import IgnoreListSettings from '$lib/components/IgnoreListSettings.svelte';
  import { NO_IGNORES, ignoreAllExcept, scopeFrom, type Scope } from '$lib/api/ignore-list.js';
  import { SUBJECTS, type SubjectName } from '$lib/api/tagger-fields.js';

  const subject = $derived<SubjectName>(
    (page.url.searchParams.get('subject') as SubjectName | null) ?? 'object',
  );

  /**
   * Read one scope out of the URL.
   *
   * `invert=1` marks the allow-list form. Reading it as a flag rather than
   * inferring it from an empty list is what keeps "ignore everything" and
   * "ignore nothing" distinguishable in a URL — the same distinction
   * `scopeToQuery` makes on the way out.
   */
  function readScope(key: 'ignore' | 'search'): Scope {
    const invert = page.url.searchParams.get(`${key}-invert`) === '1';
    const entries = page.url.searchParams.getAll(key);
    return entries.length === 0 && !invert ? NO_IGNORES : scopeFrom(entries, invert);
  }

  let applyScope = $state<Scope>(readScope('ignore'));
  let searchScope = $state<Scope>(readScope('search'));

  /**
   * Write a scope back into the URL.
   *
   * `goto` with `replaceState`, not a bare `history.replaceState`. SvelteKit
   * owns the router's idea of the current URL, and writing history directly
   * leaves the two disagreeing: the address bar changes and `page.url` does
   * not, so a reload of the URL the test just read comes back with the old
   * state. That is not a cosmetic disagreement -- it means the link the user
   * copies is not the state they are looking at.
   *
   * `replaceState: true` so a checkbox does not push a history entry per tick.
   * The back button should leave the settings, not walk back through the user's
   * last four clicks.
   */
  async function writeScope(key: 'ignore' | 'search', scope: Scope): Promise<void> {
    const url = new URL(page.url);
    url.searchParams.delete(key);
    url.searchParams.delete(`${key}-invert`);
    for (const f of scope.fields) url.searchParams.append(key, f);
    if (scope.ignoreAll) url.searchParams.set(`${key}-invert`, '1');
    await goto(`${url.pathname}${url.search}`, { replaceState: true, noScroll: true, keepFocus: true });
  }

  function onapplychange(scope: Scope): void {
    applyScope = scope;
    writeScope('ignore', scope);
  }

  function onsearchchange(scope: Scope): void {
    searchScope = scope;
    writeScope('search', scope);
  }

  /** The allow-list preset, so the inverted mode is reachable without the toggle. */
  function useAllowList(): void {
    onapplychange(ignoreAllExcept(['title', 'date']));
  }
</script>

<svelte:head><title>Tagger settings</title></svelte:head>

<nav>
  {#each SUBJECTS as s (s)}
    <a href="?subject={s}" aria-current={s === subject ? 'page' : undefined}>{s}</a>
  {/each}
</nav>

<IgnoreListSettings
  {subject}
  {applyScope}
  {onapplychange}
  {searchScope}
  {onsearchchange}
/>

<button type="button" data-testid="ignore-allow-list-preset" onclick={useAllowList}>
  Only ever use title and date
</button>

<!--
  The rendered scopes, so a test can read the state without reverse-engineering
  it from which checkboxes are ticked. The URL is the real contract; this is the
  same information in a form a test can assert on directly.
-->
<p data-testid="ignore-state">
  apply={applyScope.ignoreAll ? 'invert' : 'list'}:{applyScope.fields.join('|')} search=
  {searchScope.ignoreAll ? 'invert' : 'list'}:{searchScope.fields.join('|')}
</p>
