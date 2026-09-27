<!--
  The recipient's page: someone opens a link you sent them.
  Spec §9.5 (#5612), plan T-P5-007 part 2B.

  # What this page has to get right, and it is not the happy path

  The happy path is one fetch and a link to the object. Everything else is a
  sentence, and `share.ts` holds those sentences rather than this file, because
  the words a recipient reads are a *contract* with the server's reasons: a
  reason added on the server must not produce a blank page here.

  Four states, and the order they are decided in matters:

    1. the link did not open — expired, revoked, malformed, or wrong password;
    2. it opened and names a target — show what was shared;
    3. it opened and is an object — offer the player;
    4. it opened and is a smart collection — offer the list.

  A refusal is NOT an error page. The recipient did nothing wrong; time passed,
  or the sender changed their mind. So this renders a sentence and a way to get
  help (who to ask), never a stack trace and never a bare "not found" — see
  `share.ts` for why "not found" is the wrong thing to tell someone holding an
  expired link.

  # Why there is no password form

  The server has no unlock route, deliberately: a session cookie carrying an
  unlock would mean two routes trusting it, one of them handing out file bytes.
  So a `password_required` refusal is terminal here too, with a sentence that
  says to ask the sender. A field on this page could not be honoured, and a
  field that cannot be honoured is worse than no field.

  # Why the token is read from the path and not stored

  The token is the capability. Keeping it in sessionStorage or a cookie would
  make it outlive the tab, and a capability that survives the user closing the
  thing they opened it in is not time-limited in any sense a user would
  recognise. The URL is the only copy, which is also what makes "copy the
  address bar" the intended way to pass a link on.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { openShare, type ShareResult } from '$lib/api/share.js';

  const token = $derived(page.params.token ?? '');

  let result = $state<ShareResult | null>(null);
  /** Set only when the fetch itself failed — not when the link was refused. */
  let transportError = $state<string | null>(null);

  onMount(async () => {
    // The token can change without the component being recreated, because
    // `/s/[token]` is the same route with a different parameter. A one-shot
    // onMount would show the first link's answer for every link after it.
    $effect(() => {
      const current = token;
      let cancelled = false;
      result = null;
      transportError = null;
      openShare(current)
        .then((r) => {
          if (!cancelled) result = r;
        })
        .catch((e: unknown) => {
          if (!cancelled) transportError = e instanceof Error ? e.message : String(e);
        });
      return () => {
        cancelled = true;
      };
    });
  });
</script>

<svelte:head><title>Shared with you</title></svelte:head>

<main data-testid="share-page">
  <h1>Shared with you</h1>

  <!--
    Three separate "nothing to show yet" states rather than one. A page that
    renders nothing while a fetch is in flight is indistinguishable from a page
    that rendered nothing because the link is dead, and a user watching a blank
    screen cannot tell which they are looking at.
  -->
  {#if transportError}
    <p data-testid="share-transport-error" role="alert">
      Could not reach the server. The link may still work — try again in a moment.
    </p>
    <p class="detail">{transportError}</p>
  {:else if result === null}
    <p data-testid="share-loading">Checking this link…</p>
  {:else if result.kind === 'refused'}
    <!--
      `role="alert"` so a screen reader announces it. This page is, for a
      refused link, the entire message -- and a message that is only visible is
      a message a screen-reader user does not get.
    -->
    <p data-testid="share-refused" role="alert">{result.message}</p>
  {:else}
    <p data-testid="share-opened">
      {#if result.targetKind === 'object'}
        Someone shared a file with you.
      {:else}
        Someone shared a collection with you.
      {/if}
    </p>
    <p>
      <a data-testid="share-target" href={`/list?id=${encodeURIComponent(result.targetId)}`}>
        Open it
      </a>
    </p>
    {#if result.canDownload}
      <!--
        Only shown when the server said so. A download control on a `view` link
        produces a 403 the recipient cannot explain, which reads as "this link
        is broken" rather than "this link does not allow that".
      -->
      <a data-testid="share-download" href={`/media/${encodeURIComponent(result.targetId)}`}>
        Download
      </a>
    {/if}
  {/if}
</main>

<style>
  main {
    max-width: 40rem;
    margin: 0 auto;
    padding: 2rem 1rem;
  }
  h1 {
    font-size: 1.25rem;
    margin: 0 0 1rem;
  }
  [role='alert'] {
    /* The refusal is the page's whole content in the dead case, so it is given
       the weight of a heading rather than being a grey line among grey lines. */
    font-size: 1.05rem;
    font-weight: 600;
  }
  .detail {
    color: var(--muted);
    font-size: 0.85rem;
  }
  a {
    color: var(--accent);
  }
</style>
