<!--
  A tag editor, for the multi-value field. Spec 10.10, plan T-P5-006 item 11;
  #7139, §9.4.

  A real route for the same reason `create-from-subpage` is: the values are the
  state, and state in a component variable is lost on reload, cannot be linked,
  and does not answer the back button. A user who wants to send someone "these
  are the tags on this item" needs a URL that says so.

  It mounts the field and owns the list. It does not persist anything -- the
  GraphQL server is T-P6-007 -- and it says so rather than pretending otherwise.
-->
<script lang="ts">
  import { page } from '$app/state';
  import MultiValueField from '$lib/components/MultiValueField.svelte';

  /**
   * The values, as a repeated query parameter.
   *
   * Repeated rather than joined, for the same reason the create route does it:
   * a tag containing a comma is a tag, and a separator a user can type into
   * their own data is a separator that eventually splits a row in the wrong
   * place.
   */
  let values = $state<string[]>(page.url.searchParams.getAll('tag'));

  function onchange(next: readonly string[]) {
    values = [...next];
  }
</script>

<svelte:head><title>Tags</title></svelte:head>

<MultiValueField label="Tags" {values} {onchange} />

<p data-testid="tag-state">{values.join('|')}</p>
