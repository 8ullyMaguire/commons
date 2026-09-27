<!--
  The owner's share links: make one, see them all, turn one off.
  Spec §9.5 (#5612), plan T-P5-007 part 2B.

  # The one thing this page must get right

  It must never show a "copy link" button for a link from the list. The server
  stores only a hash of the token (migration 0022), so there is nothing to copy
  for any link this page can see — a button there would be a control that cannot
  work, and a user would learn that by clicking it.

  So the URL is shown ONCE, in the moment it is created, with a sentence saying
  why it cannot be shown again. That sentence is the feature's whole design
  stated to the user: a link you cannot re-read is a link that cannot leak from
  your screen, and the price is that a lost link is reissued rather than
  recovered.

  # Why the list is not filtered to live links

  "Where is the link I sent on Tuesday" is a question about a link that no
  longer works, and the state is what tells the owner whether to reissue it
  (expired) or to ask the recipient what they saw (turned off). The server sends
  `state` and this page renders it rather than deriving it, because the rule is
  "revoked outranks expired" and a client that implements that differently shows
  an owner the wrong state on a link they deliberately killed.

  # Why the access count is shown but not the log

  The count answers "did this get used", which is the question an owner has
  ninety seconds after sharing. The full log -- every attempt, granted or not --
  is the question they have when something looks wrong, and it is a different
  screen. This page links to nothing for it yet rather than rendering a log
  inline behind an expander nobody opens.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import {
    createShare,
    listShares,
    revokeShare,
    stateLabel,
    type CreatedShare,
    type ShareListItem
  } from '$lib/api/share.js';

  /**
   * The link just created, shown once.
   *
   * `$state`, not a URL, and deliberately: putting a token in the URL would put
   * it in the browser history, in a bookmark, and in anything that reads the
   * address bar. A capability that persists in history is not the thing the
   * hashing was for.
   */
  let created = $state<CreatedShare | null>(null);
  let items = $state<ShareListItem[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);

  // Kept in state rather than a form element, because the target is chosen from
  // the page a user is on and this route is reachable on its own too.
  let targetId = $state('');
  let scope = $state<'view' | 'view_download'>('view');
  let hours = $state(24);
  let password = $state('');

  async function refresh() {
    try {
      items = await listShares();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(refresh);

  async function oncreate(event: SubmitEvent) {
    event.preventDefault();
    error = null;
    busy = true;
    try {
      created = await createShare({
        targetKind: 'object',
        targetId: targetId.trim(),
        scope,
        expiresInHours: hours,
        ...(password ? { password } : {})
      });
      // The form is cleared so a second submit does not silently create a
      // second link for the same target, which the owner then has to revoke.
      targetId = '';
      password = '';
      await refresh();
    } catch (e) {
      // The server's 422 message names the field, and it is shown verbatim.
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function onrevoke(id: string) {
    error = null;
    busy = true;
    try {
      await revokeShare(id);
      await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>Share links</title></svelte:head>

<main data-testid="share-admin">
  <h1>Share links</h1>

  <!--
    `role="alert"` on the error for the same reason as the recipient page: on a
    form, the message IS the feedback, and a visible-only message is not
    feedback for everyone.
  -->
  {#if error}
    <p data-testid="share-admin-error" role="alert">{error}</p>
  {/if}

  {#if created}
    <!--
      The one time the URL is ever shown. `readonly` on the input so a stray
      click cannot select half of it, and `autocomplete="off"` so a browser
      does not offer to remember a capability.
    -->
    <section data-testid="share-created" aria-labelledby="created-h">
      <h2 id="created-h">Your link</h2>
      <p>
        This is the only time this link will be shown. It cannot be looked up
        again — only turned off and replaced.
      </p>
      <input
        data-testid="share-created-url"
        readonly
        autocomplete="off"
        aria-label="The share link"
        value={created.url}
      />
      <p>
        Expires <time datetime={created.expires_at}>{created.expires_at}</time>
      </p>
      <button type="button" data-testid="share-created-dismiss" onclick={() => (created = null)}>
        Done
      </button>
    </section>
  {/if}

  <section aria-labelledby="new-h">
    <h2 id="new-h">Make a link</h2>
    <form data-testid="share-form" onsubmit={oncreate}>
      <label for="share-target">Object id</label>
      <input id="share-target" data-testid="share-target" bind:value={targetId} required />

      <label for="share-scope">Permission</label>
      <!--
        A radio group rather than a select, for the same reason `ThemeControl`
        is one: a select hides the other options, and "what does this allow"
        is the question a person is answering when they open a share dialog.
      -->
      <fieldset id="share-scope" data-testid="share-scope">
        <legend>Permission</legend>
        <label>
          <input type="radio" bind:group={scope} value="view" />
          View only
        </label>
        <label>
          <input type="radio" bind:group={scope} value="view_download" />
          View and download
        </label>
      </fieldset>

      <label for="share-hours">Expires in (hours)</label>
      <input
        id="share-hours"
        data-testid="share-hours"
        type="number"
        min="1"
        max="2160"
        bind:value={hours}
      />

      <label for="share-password">Password (optional)</label>
      <input
        id="share-password"
        data-testid="share-password"
        type="password"
        autocomplete="new-password"
        bind:value={password}
      />

      <button type="submit" data-testid="share-submit" disabled={busy}>
        {busy ? 'Working…' : 'Make link'}
      </button>
    </form>
  </section>

  <section aria-labelledby="list-h">
    <h2 id="list-h">Your links</h2>
    <table data-testid="share-list">
      <thead>
        <tr>
          <th scope="col">Target</th>
          <th scope="col">Permission</th>
          <th scope="col">Expires</th>
          <th scope="col">Uses</th>
          <th scope="col">State</th>
          <th scope="col"><span class="visually-hidden">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each items as item (item.id)}
          <tr data-testid="share-row" data-share-id={item.id}>
            <td>{item.target_id}</td>
            <td>{item.scope === 'view' ? 'View' : 'View and download'}</td>
            <td>
              {#if item.expires_at}
                <time datetime={item.expires_at}>{item.expires_at}</time>
              {:else}
                —
              {/if}
            </td>
            <td data-testid="share-uses">{item.access_count}</td>
            <td data-testid="share-state">{stateLabel(item.state)}</td>
            <td>
              <!--
                Offered on a dead link too, and idempotent server-side, because
                an owner who cannot see a control for a link they want to be
                certain about will not trust the state column beside it.
              -->
              <button
                type="button"
                data-testid="share-revoke"
                disabled={busy}
                onclick={() => onrevoke(item.id)}
              >
                Turn off
              </button>
            </td>
          </tr>
        {:else}
          <tr>
            <td colspan="6" data-testid="share-empty">No links yet.</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>
</main>

<style>
  main {
    max-width: 52rem;
    margin: 0 auto;
    padding: 2rem 1rem;
  }
  h1 {
    font-size: 1.25rem;
  }
  h2 {
    font-size: 1rem;
    margin-top: 2rem;
  }
  section {
    margin-bottom: 2rem;
  }
  form {
    display: grid;
    gap: 0.4rem;
    max-width: 28rem;
  }
  fieldset {
    border: 1px solid var(--border);
    /* The legend is the group label, so the visible border is what makes the
       radio pair read as one control rather than two. */
  }
  fieldset label {
    display: inline-flex;
    gap: 0.35rem;
    align-items: center;
    margin-right: 1rem;
  }
  /* 44px is the §10.8 hit-target floor. */
  button,
  input:not([type='radio']) {
    min-height: 44px;
  }
  [data-testid='share-created'] {
    border: 1px solid var(--border);
    padding: 1rem;
  }
  [data-testid='share-created-url'] {
    width: 100%;
    font-family: ui-monospace, monospace;
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: 0.5rem 0.4rem;
    border-bottom: 1px solid var(--border);
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
