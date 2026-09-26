<svelte:head>
  <title>Commons — Index</title>
</svelte:head>

<script lang="ts">
  import EditSurface from '$lib/components/EditSurface.svelte';
</script>

<h1>Index mode</h1>
<p>
  The public index (spec 3.1). It is bound to all interfaces and federates
  claims, but it is the same binary as the library with a different store
  behind it. This route exists so the two modes are real navigations rather
  than a mode flag; the index surfaces come in Phase 3.
</p>

<!--
  The unsaved-edit surface, mounted here so the guard in `+layout.svelte` has
  something to protect. It belongs on this route rather than the library
  because the library is a browse surface: a browse surface does not hold
  edits, and mounting an editor on it would imply otherwise. The two toolbar
  links are a real navigation, which is exactly the move the guard has to
  intercept.
-->
<EditSurface objectId="index-demo" title="Index demo object" />
