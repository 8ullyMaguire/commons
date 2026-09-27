<!--
  Create from subpage. Spec 10.10, plan T-P5-006 item 10; #3694.

  # Why this is a route and not two rows in the bulk modal

  The bulk modal is *selection*-driven: it renders a scope line, and the scope
  is the set of ids the user has picked. The two create actions have no
  selection by construction — `create-from-subpage` starts from one item's
  linked subpages, and `create-all-missing` starts from a pasted list that is
  not in the library yet. Forcing them into the modal would mean either
  inventing a fake selection or teaching the modal about a second, different
  notion of scope, and the modal's whole reason for existing is that its scope
  line is a promise about exactly what will be written.

  So this is its own route, and the rule it follows is the same one the
  vertical feed follows: **every piece of state is in the URL**. The pasted
  rows are state, and §5.16 is not an exception for text a user typed.

  # What it does NOT do

  It does not parse CSV. §7.1's importer is its own ticket, with its own
  questions (column order, delimiter, encoding); this route takes rows that are
  already structured, and the parsing is a separate concern with a separate
  spec. What it does do is render the one number that decides the action --
  how many of these rows are already in your library -- because a user about to
  press a button that creates 5 objects out of 40 pasted rows is entitled to
  know that before pressing it, not after.
-->
<script lang="ts">
  import {
    IDLE,
    applyLabel,
    canApply,
    detail,
    redundantCount,
    summary,
    type CreateResult
  } from '$lib/api/create.js';

  interface Props {
    /**
     * The create result, owned by the page that owns the client.
     *
     * A prop rather than local state for the reason `BulkEditModal`'s `error` is
     * one: the request is a round-trip the parent makes, and a local `let`
     * would be write-only — the branch renders, the markup is tested, and no
     * code can ever reach it.
     */
    result?: CreateResult;
    /** The rows the user pasted, already structured. */
    drafts: readonly string[];
    /** Ids already in the library, as the server reports them. */
    distinctIds?: number;
    error?: string | null;
    busy?: boolean;
    oncreate: () => void;
    oncancel: () => void;
  }

  const p = $props<Props>();

  // Read as `p.x` wherever a value is tracked in a `$derived`, because a
  // destructured prop is a plain local and reading it registers no dependency.
  const result = $derived(p.result ?? IDLE);
  const draftCount = $derived(p.drafts.length);
  const distinctIds = $derived(p.distinctIds ?? draftCount);

  const applicable = $derived(canApply(result, draftCount));
  const label = $derived(applyLabel(draftCount, distinctIds));
  const message = $derived(summary(result));
  const submessage = $derived(detail(result));
  const redundant = $derived(redundantCount(draftCount, distinctIds));

  function confirm() {
    p.oncreate();
  }
</script>

<dialog data-testid="create-from-subpage" open>
  <h2>Create from subpage</h2>

  <!--
    The scope line, and the reason this dialog exists at all. §10.10: a
    destructive action states its scope before the user confirms. This one is not
    destructive, so the promise it makes is the other direction -- exactly how
    many rows it will ADD, and exactly how many it will skip.
  -->
  <p data-testid="create-scope">
    {#if draftCount === 0}
      Nothing to create.
    {:else}
      {draftCount} {draftCount === 1 ? 'row' : 'rows'} from this subpage
      {#if redundant > 0}
        &mdash; {redundant} already in your library
      {/if}
    {/if}
  </p>

  {#if p.error}
    <p data-testid="create-error" role="alert">{p.error}</p>
  {/if}

  <!--
    In the dialog, not a toast, for the reason `BulkEditModal` states: this is a
    modal, everything outside it is inert, and a toast shown while it is open is
    unclickable.
  -->
  {#if message}
    <p data-testid="create-result" data-state={result.state} class:warn={result.state === 'blocked'}>
      {message}
    </p>
    {#if submessage}
      <p data-testid="create-detail">{submessage}</p>
    {/if}
  {/if}

  <footer>
    <button data-testid="create-cancel" onclick={() => p.oncancel()}>Cancel</button>
    <button
      data-testid="create-apply"
      disabled={!applicable || p.busy}
      onclick={confirm}
    >
      {p.busy ? 'Creating…' : label}
    </button>
  </footer>
</dialog>
