/**
 * The app is client-rendered and has no server at runtime.
 *
 * The library binary serves the static directory; the desktop shell is a
 * webview pointed at that same directory (spec 3.3). So this is a description
 * of the deployment rather than a concession to make the static adapter work.
 *
 * `ssr = false` has to be declared HERE, on the page options, and not in
 * `kit` in vite.config.ts -- SvelteKit reads it from +layout.ts and silently
 * ignores it in the config. With it left on and no `prerender = true`,
 * adapter-static prerenders zero pages and writes ONLY `200.html`: there is
 * no index.html, so `/` is a directory listing and the app never boots. The
 * build still reports "Wrote site to build" and prints nothing wrong.
 *
 * `prerender = true` is set as well because the fallback page still has to be
 * generated for the deep links that spec 5.16 requires: a shared view URL
 * like /?q=rating%3E4 must be served by the app rather than 404ing.
 */
export const ssr = false;
export const prerender = true;
export const trailingSlash = 'never';
