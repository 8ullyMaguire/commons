<!--
  Create from subpage. Spec 10.10, plan T-P5-006 item 10; #3694, #1017, #3122.

  A real route, not a flag on the browse page, for the reason `list` is one: a
  subpage's create action is linkable — "create these" is a thing a user may
  want to send someone, and it has to survive a reload.

  # What this route does NOT do, stated plainly

  It does not parse CSV, and it does not walk a subpage's segments. Both are
  separate tickets with their own specs: §7.1's importer owns column order,
  delimiter and encoding, and the segment walker owns §5.18's rules about a
  clip's segment list. This route takes the rows that the owner of those
  problems hands it — `?rows=` — and does the one thing that is this ticket's:
  create what is missing, and say honestly what it skipped.

  # The one number this page exists for

  `?rows=a&rows=b&rows=c` is a user pasting three titles. Two of them are
  probably already in their library. The button therefore says `Create 1`, not
  `Create 3`, and the scope line says why. §10.10's rule is that an action
  states its scope before the user confirms; a create that reports
  "created 0 rows" after the fact has told the user nothing they could act on.
-->
<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import CreateFromSubpage from '$lib/components/CreateFromSubpage.svelte';
  import { IDLE, classify, type CreateResult } from '$lib/api/create.js';
  import { createAllMissing } from '$lib/api/client.js';

  /**
   * The rows, read from the URL.
   *
   * Repeated `?rows=` rather than one joined string, because a title containing
   * `,` or `|` is a title, and a separator a user can type into their own data
   * is a separator that eventually splits a row in the wrong place. `URLSearchParams`
   * gives one value per occurrence and does the decoding, so a row with a space
   * or a `&` in it survives.
   */
  const rows = $derived(page.url.searchParams.getAll('rows'));

  /**
   * How many of these are already in the library, or `undefined` while the
   * answer is unknown.
   *
   * `undefined` is not zero, and the component treats it as unknown: a button
   * that says "Create 40" because nobody has asked the server yet is a promise
   * the server may not keep, which is the same rule the bulk modal follows for
   * its scope line.
   */
  let distinctIds = $state<number | undefined>(undefined);
  let result = $state<CreateResult>(IDLE);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function confirm() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      const { createAllMissing: outcome } = await createAllMissing(rows);
      // `classify`, not a literal 'done': a run with refused rows is `partial`
      // and one with nothing created and nothing refused is `done`, and the
      // difference is the whole point of the three counts. Hardcoding it here
      // rendered a refusal as a clean success -- caught by the e2e, invisible
      // to the unit test, which only exercises the module.
      result = { state: classify(outcome), outcome };
      distinctIds = outcome.created + outcome.existing;
    } catch (e) {
      // A thrown error is a failure, not an outcome. Rendering a failed import
      // as "0 created" would tell a user their paste was empty when in fact
      // the request never completed.
      error = e instanceof Error ? e.message : 'create failed';
      result = { state: 'failed', error };
    } finally {
      busy = false;
    }
  }

  function cancel() {
    void goto('/list');
  }
</script>

<svelte:head><title>Create from subpage</title></svelte:head>

<CreateFromSubpage
  {result}
  drafts={rows}
  {distinctIds}
  {error}
  {busy}
  oncreate={confirm}
  oncancel={cancel}
/>
