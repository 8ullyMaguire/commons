<!--
  The ignore-list settings row. Spec 10.10, plan T-P5-006 item 13; #2318, #2399.

  # Why this is a component and not a function

  Three things only a component can do, and all three are about the user
  understanding the setting:

   1. **the inverted mode**, because "never overwrite this field" and "only
      ever write these two fields" are the same widget with a toggle, and a
      toggle has no pure equivalent;
   2. **the summary line**, which must re-derive as boxes are ticked and can
      only be built from the live scope — `describeScope` does the wording;
   3. **the warning about an entry this build does not recognise**, which is a
      per-entry annotation rather than a list-level fact.

  # The one design decision worth stating

  There are TWO lists — one for what a scrape may WRITE, one for what a SEARCH
  may look at — and they are separate props. Conflating them produces the worst
  of both: a user who excludes a field from the search silently stops getting
  its value, with nothing on screen saying why. §10.7's rule is that an action
  states its scope, and these are two different scopes.

  # What it does not do

  It does not persist. `onchange` hands the two scopes to the parent, and the
  GraphQL server is T-P6-007, so there is nowhere to save them yet. The two
  `scopeToQuery` outputs are exposed as the URLs a parent would navigate to, and
  that is the whole of the integration for now.
-->
<script lang="ts">
  import {
    describeScope,
    isValidFieldName,
    scopeFrom,
    scopeToQuery,
    unknownEntries,
    type Scope,
  } from '$lib/api/ignore-list.js';
  import { FIELDS_BY_SUBJECT, type SubjectName } from '$lib/api/tagger-fields.js';

  interface Props {
    /** Which subject's fields this row is for. */
    subject: SubjectName;
    /** What a scrape may WRITE, excluding these. */
    applyScope: Scope;
    onapplychange: (scope: Scope) => void;
    /** What a SEARCH may look at, excluding these. */
    searchScope: Scope;
    onsearchchange: (scope: Scope) => void;
    /** The fields that exist, for an inverted scope's summary. Defaults to all. */
    allFields?: readonly string[];
  }

  const p = $props<Props>();

  const fields = $derived<readonly string[]>(
    p.allFields ?? FIELDS_BY_SUBJECT[p.subject],
  );

  /** The two summaries, kept beside their controls rather than in one place. */
  const applySummary = $derived(describeScope(p.applyScope, fields));
  const searchSummary = $derived(describeScope(p.searchScope, fields));

  /** Entries on each list that match no field this build knows. */
  const applyUnknown = $derived(unknownEntries(p.applyScope));
  const searchUnknown = $derived(unknownEntries(p.searchScope));

  /**
   * Toggle one field on one list.
   *
   * The `ignoreAll` flag is preserved rather than reset: a user who unchecks
   * two fields from an inverted list is editing the KEEP list, and resetting the
   * flag would silently flip the meaning of the other entries rather than the
   * one they touched.
   */
  function toggle(
    scope: Scope,
    field: string,
    onChange: (next: Scope) => void,
  ): void {
    // Membership in the list, in both directions. Under an inverted scope the
    // list holds the fields that are KEPT, and the label above it says "only
    // the ticked fields are used" — so a tick must mean "used", which is
    // membership. Using `isIgnored` here would make every box untick itself the
    // moment the inversion went on, and the user would be looking at an
    // allow-list in which nothing is allowed.
    const has = scope.fields.includes(field);
    const next = has ? scope.fields.filter((f) => f !== field) : [...scope.fields, field];
    onChange(scopeFrom(next, scope.ignoreAll));
  }

  function setInverted(scope: Scope, onChange: (next: Scope) => void): void {
    // Turning the inversion ON carries the list over, which makes it a KEEP
    // list. That is almost never what a user means by the label — they mean it
    // relative to what is already ticked — so the summary beside it changes
    // visibly and the user sees the consequence before saving.
    //
    // Turning it OFF with nothing kept means "ignore nothing", which is the
    // honest reading of "I did not mean to invert this": a keep-list of zero
    // entries would silently mean "write nothing", and that is a different
    // action from the one the user is performing.
    const next = scope.ignoreAll ? scopeFrom([], false) : scopeFrom(scope.fields, true);
    onChange(next);
  }
</script>

<section class="ignores" data-testid="ignore-settings" data-subject={p.subject}>
  <h2>Fields this tagger ignores</h2>

  <!--
    The two lists, rendered by the same markup through a small local component
    pattern. Svelte 5 has no `{#each}` over a pair of configs without an array,
    and an array of two config objects here would be less readable than the
    repetition it replaces.
  -->
  {#each [{ key: 'apply', title: 'A scrape may not write', scope: p.applyScope, summary: applySummary, unknown: applyUnknown, onchange: p.onapplychange }, { key: 'search', title: 'A search may not look at', scope: p.searchScope, summary: searchSummary, unknown: searchUnknown, onchange: p.onsearchchange }] as list (list.key)}
    <fieldset data-testid={`ignore-list-${list.key}`}>
      <legend>{list.title}</legend>

      <!--
        The inversion toggle. It is ABOVE the list and its label says which
        direction it inverts, because a checkbox labelled "ignore all" beside a
        list of ticked fields is ambiguous about which way round it goes.
      -->
      <label class="invert">
        <input
          type="checkbox"
          data-testid={`ignore-invert-${list.key}`}
          checked={list.scope.ignoreAll}
          onchange={() => setInverted(list.scope, list.onchange)}
        />
        {list.scope.ignoreAll
          ? 'Only the ticked fields are used'
          : 'Use every field except the ticked ones'}
      </label>

      {#each fields as field (field)}
        <label class="field">
          <input
            type="checkbox"
            data-testid={`ignore-${list.key}-${field}`}
            checked={list.scope.fields.includes(field)}
            onchange={() => toggle(list.scope, field, list.onchange)}
          />
          {field}
        </label>
      {/each}

      <!--
        The summary. §10.7: an action states its scope. A settings row that says
        "3 fields ignored" tells a user nothing they can check, and a settings
        row that says nothing at all is worse.
      -->
      <p data-testid={`ignore-summary-${list.key}`}>{list.summary}</p>

      <!--
        An entry this build does not recognise is REPORTED, never removed. A
        scraper with a field this build has never seen is a normal thing, and
        the entry is still doing its job — it is silencing that field. Removing
        it would make the setting quietly wrong.
      -->
      {#if list.unknown.length > 0}
        <p data-testid={`ignore-unknown-${list.key}`} data-state="warn">
          {list.unknown.length === 1
            ? `${list.unknown[0]} is not a field this build knows. It is still ignored.`
            : `${list.unknown.join(', ')} are not fields this build knows. They are still ignored.`}
        </p>
      {/if}

      <p class="url" data-testid={`ignore-query-${list.key}`}>
        {scopeToQuery(list.scope, fields) || '(no filter)'}
      </p>
    </fieldset>
  {/each}

  <!--
    A free-text entry, for a field this build has never heard of. Without it the
    whole feature is unusable against a scraper the vocabulary has not caught
    up with, which is the case §8.1's open field design exists to allow.
  -->
  <label class="add">
    Ignore a field by name
    <input
      type="text"
      data-testid="ignore-custom"
      placeholder="field name"
      onkeydown={(e) => {
        if (e.key !== 'Enter') return;
        const value = (e.currentTarget as HTMLInputElement).value.trim();
        if (!isValidFieldName(value)) return;
        p.onapplychange(scopeFrom([...p.applyScope.fields, value], p.applyScope.ignoreAll));
        (e.currentTarget as HTMLInputElement).value = '';
      }}
    />
  </label>
</section>

<style>
  .ignores fieldset {
    margin-bottom: 1rem;
    border: 1px solid currentColor;
    border-radius: 4px;
  }

  .invert {
    display: block;
    font-weight: 600;
    margin-bottom: 0.5rem;
  }

  .field {
    display: block;
    padding-left: 1.5rem;
  }

  .url {
    font-family: monospace;
    opacity: 0.7;
  }

  [data-state='warn'] {
    /* Not red: a warning the user cannot act on is noise, and this one is
       informational — the entry IS working. */
    font-style: italic;
  }
</style>
