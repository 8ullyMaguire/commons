<!--
  A surface that holds an unsaved edit. Spec 10.7, 10.10; plan T-P5-006 item 4.

  # Why this component exists at all

  Not to demo a guard -- the guard is in `+layout.svelte` and needs *something*
  dirty to protect, or the Playwright spec has to reach into module state from
  the test, which is the same coupling in a worse place.

  It is also the shape every real surface takes, and that shape is the point:
  a surface does not set a flag, it **registers**. Registration is idempotent by
  id, so a re-render cannot inflate the count, and it is cleared by an explicit
  save or revert. The alternative -- a boolean a surface flips -- is what makes
  this class of bug intermittent: one surface forgets to clear it on save, and
  the user gets a phantom prompt forever with nothing to save.

  # Why the register effect keys on the dirty flag and not on a deep watch

  An effect that read the draft's contents would re-register on every keystroke,
  replacing the registry entry hundreds of times and re-running every derived
  guard value with it. The *fact* of being dirty is the dependency; what is
  dirty is the surface's own business and lives in its own state.
-->
<script lang="ts">
  import { onDestroy } from 'svelte';
  import { guard } from '$lib/api/guard-store.svelte.js';

  interface Props {
    /** The object being edited, for the surface id and the label. */
    objectId: string;
    /** The object's title, so the prompt can name it. */
    title: string;
  }

  const { objectId, title }: Props = $props();

  let draft = $state('');
  let saved = $state<string | null>(null);
  let failing = $state(false);

  /**
   * Is there anything to lose?
   *
   * `isUnsaved(pendingEdits, hasFailedSave)`, not a boolean of my own: a
   * counter is wrong twice -- it does not fall back to zero when the last
   * pending edit is saved, and it cannot represent "edited, saved, edited
   * again" without a second piece of state to subtract the save from.
   */
  const dirty = $derived(draft !== (saved ?? '') || failing);

  // Register on the *fact* of being dirty. `failing` is listed separately
  // because a failed save is a different kind of risk and the prompt names it
  // differently: the work is already not persisted, so telling the user only
  // that they have "unsaved changes" gets them to leave believing it is safe.
  const id = `edit:${objectId}`;

  $effect(() => {
    if (dirty) {
      guard.add({ id, kind: failing ? 'failed' : 'pending', label: title });
    } else {
      guard.drop(id);
    }
  });

  // Deregister on destroy, which is not optional and is not the same thing as
  // the effect above. The effect only re-runs when `dirty` changes; unmounting
  // is not a change to `dirty`, so without this the entry simply persists after
  // the component is gone.
  //
  // The symptom was a library page that opened with "Unsaved changes" in the
  // toolbar, forever, with no editor on the page and nothing to save. A
  // registration that outlives its owner is a guard protecting a ghost, and
  // because it never clears, every future navigation prompts about it too.
  //
  // `onDestroy` rather than the cleanup half of the `$effect`: an effect
  // cleanup would re-run whenever `dirty` flips, and the deregistration is
  // wanted exactly once -- at the end.
  //
  // The guard snapshots the registry when a navigation starts, so a surface
  // that unmounts mid-navigation is still named in the confirm -- the
  // deregistration is about not outliving the *page*, and the snapshot is taken
  // before the page goes anywhere.
  onDestroy(() => guard.drop(id));

  function save() {
    // A real save would round-trip; here it can be told to fail, because
    // "the save failed" is a state the guard exists to handle and a demo that
    // cannot reach it is a demo that hides half the feature.
    if (failing) return;
    saved = draft;
  }
</script>

<section data-testid="edit-surface">
  <label for="edit-title">Title</label>
  <input id="edit-title" data-testid="edit-input" bind:value={draft} />

  <button data-testid="edit-save" onclick={save}>Save</button>
  <button data-testid="edit-fail" onclick={() => (failing = !failing)}>
    {failing ? 'Clear failed save' : 'Simulate failed save'}
  </button>
  <span data-testid="edit-dirty">{dirty ? 'dirty' : 'clean'}</span>
</section>

<style>
  section {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    padding: 0.5rem;
  }
  span {
    font-size: 0.85rem;
    opacity: 0.7;
  }
</style>
