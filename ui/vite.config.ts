import { sveltekit } from '@sveltejs/kit/vite';
import adapter from '@sveltejs/adapter-static';
import { defineConfig } from 'vite';

/**
 * The SvelteKit config lives here, inside vite.config.ts, and there is no
 * svelte.config.js. That works ONLY because `sveltekit()` is called WITH the
 * config object below.
 *
 * This is worth writing down because the natural version does not work. With
 * a bare `plugins: [sveltekit()]`, SvelteKit falls through to
 * `load_svelte_config()`, finds no svelte.config.js, and silently uses its
 * defaults — which means NO ADAPTER, and a build warning that scrolls past:
 *
 *   No Svelte config file found in ... - using SvelteKit's default
 *   configuration without an adapter.
 *
 * The build still succeeds and still produces output, so the failure mode is
 * a missing static export rather than a build error. Passing the config to
 * sveltekit() makes SvelteKit skip load_svelte_config() entirely, and it warns
 * if a svelte.config.js also exists.
 */
import type { KitConfig } from '@sveltejs/kit';

const kit: KitConfig = {
  /**
   * adapter-static because the desktop shell is a webview pointed at local
   * files (spec 3.3) and library mode can serve the same directory. One build
   * artifact that works in both places is the point; there is no SSR process
   * to run in the shell.
   *
   * The `ssr: false` that makes the SPA build go through is NOT here. It lives
   * in src/routes/+layout.ts, because SvelteKit reads it from the page
   * options and ignores it in this object. Getting that wrong produces a build
   * that SUCCEEDS and ships nothing usable: with `ssr` left on and no
   * `prerender = true`, adapter-static prerenders zero pages and writes only
   * `200.html`. There is no `index.html`, so `/` is a directory listing and
   * the app never boots, and the build reports "Wrote site to build" while
   * printing nothing wrong at all.
   *
   * The routes below exist for the same reason. `prerender.entries` cannot be
   * inferred by crawling a client-rendered app: the crawler starts at `/`, and
   * `/` contains no link to `/index-mode` because the nav is part of the client
   * bundle rather than the prerendered HTML.
   */
  prerender: {
    entries: ['*']
  },
  adapter: adapter({
    pages: 'build',
    assets: 'build',
    // A SPA fallback. The grid is a client-side view, so a deep link like
    // /?q=rating%3E4 must be served by the app rather than 404ing. With
    // `strict: true` a missing file is an error rather than a silent
    // fallback, which is what makes that fallback trustworthy.
    fallback: '200.html',
    precompress: false,
    strict: true
  }),
  alias: {
    $lib: 'src/lib'
  },
  typescript: {
    // This hook receives the generated tsconfig, NOT a Vite config. The first
    // version typed the parameter as a Vite `UserConfig` and set `checkJs` on
    // it, which TypeScript rejected -- correctly, because `checkJs` is a
    // tsconfig option and this was setting a property on the wrong object
    // entirely. It was doing nothing before.
    config(cfg: Record<string, unknown>) {
      // The generated config includes the app's own JS. There is no
      // hand-written JS in this package -- every source file is TypeScript or
      // Svelte -- so leaving checkJs on only adds the config files themselves
      // to the check.
      cfg.checkJs = false;
      cfg.noUnusedLocals = true;
      return cfg;
    }
  }
};

export default defineConfig({
  plugins: [sveltekit(kit)],
  server: {
    /**
     * The dev proxy is the only place the UI learns where the backend is. In
     * production the same `/graphql` path is served by the backend itself, so
     * there is exactly one URL shape and no environment-specific fetch code —
     * which is what makes "no second private API" (spec 3.3) hold by
     * construction rather than by review.
     */
    proxy: {
      '/graphql': {
        target: 'http://127.0.0.1:9999',
        changeOrigin: false
      }
    }
  }
});
