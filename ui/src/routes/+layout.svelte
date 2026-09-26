<!-- The app shell. One layout for both modes (spec 3.1): the library and the
     index are two routes in one binary, not two applications. -->
<script lang="ts">
  import '../app.css';
  import { page } from '$app/state';

  let { children } = $props();
</script>

<div class="app">
  <nav class="toolbar">
    <strong>Commons</strong>
    <!-- The two modes. A plain link, so it is a real navigation and the
         back button works, rather than a client-side state toggle. -->
    <a href="/" aria-current={page.url.pathname === '/' ? 'page' : undefined}>Library</a>
    <a
      href="/index-mode"
      aria-current={page.url.pathname === '/index-mode' ? 'page' : undefined}>Index</a
    >
    <span class="spacer"></span>
  </nav>
  <main>
    {@render children()}
  </main>
</div>

<style>
  .spacer {
    flex: 1;
  }
  main {
    min-height: 0;
    overflow: hidden;
  }
</style>
