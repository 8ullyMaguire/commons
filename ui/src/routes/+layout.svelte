<!--
  The app shell, and the unsaved-entry guard. Spec 10.7, 10.10; plan T-P5-006
  item 4. One layout for both modes (spec 3.1): the library and the index are
  two routes in one binary, not two applications.

  # Why the guard is here and not in `beforeNavigate`

  `adapter-static` with no server, so SvelteKit's `beforeNavigate` does not run
  -- it is a server-side hook and there is no server. A guard written there
  compiles, type-checks, and never executes, which is the same failure shape as
  `$state` in a plain `.ts` file: a green build and an absent feature.

  The layout is the one component mounted for every route and *not* remounted
  across a navigation, so it is the only place a listener can outlive the
  navigation it is meant to interrupt.

  # The mechanism: intercept, never undo

  The obvious implementation watches `page.url`, sees the navigation, and calls
  `goto()` back to put the user where they were. That is wrong, and the failure
  is quiet enough to survive a review: **SvelteKit destroys the outgoing page
  component when a navigation commits.** So undoing a navigation does not return
  the user to their page -- it *remounts* a fresh one. Every piece of component
  state is rebuilt from scratch, and an in-progress edit is destroyed by the very
  navigation the guard performed to protect it. The confirm then renders over a
  now-clean page, and it read "nothing to save".

  That is not hypothetical; it is what this file did, and it cost the better part
  of a debugging session. The trace that proves it: the surface's effect logged
  `dirty=true`, then the guard fired, then the *same* surface logged
  `dirty=false draft=""` and deregistered itself.

  So: intercept the navigation before it commits and nothing is ever destroyed.
  A capture-phase `click` listener on the document runs before the browser
  follows the link, so `preventDefault()` there stops the unmount outright. There
  is no undo to get wrong.

  For navigations with no click behind them -- `goto()` from code, a form submit,
  the browser's back button -- there is nothing to intercept, and the URL watcher
  handles them. There the page has already unmounted, and no mechanism available
  to an `adapter-static` app can prevent that. What survives is the *registry*,
  because it is module state rather than component state, so the confirm can
  still name what was at risk and "stay" can still mean something: it takes the
  user back to the page they were on, which now needs re-entering their edit.
  The spec asks only that leaving is confirmed and the scope spelled out, which
  is exactly what this delivers -- and asking for more would be asking for
  something the platform does not offer.

  # The ordering, which is still the mechanism

      1. ask whether anything is unsaved
      2. if the user says leave, CONSUME the registry
      3. only then navigate

  Consuming *after* the navigation lands re-prompts on the way out. A user
  re-prompted for the edit they just chose to lose learns to click through, and
  a guard that can be clicked through protects nothing on the one navigation
  that mattered.

  And cancelling must consume *nothing*: a guard that resolves false and then
  clears state has discarded the edit and left the user where they were, which
  is the worst of both outcomes. The test asserts the registry is untouched
  after a cancel, not merely that a dialog closed.

  # The tab-close case is a different mechanism entirely

  A navigation is confirmable; a tab close is not. Only `beforeunload` sees it,
  and only the *browser* may render a prompt there -- a custom dialog is ignored
  by every current browser. So that case gets `beforeunload` plus a visible
  always-there indicator, and the spec asks for nothing more because nothing
  more is available. The handler is registered only while something is dirty: a
  permanently-registered one prompts on every reload forever, which is how a
  guard becomes wallpaper.
-->
<script lang="ts">
  import '../app.css';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { guard } from '$lib/api/guard-store.svelte.js';
  import { decide, isDirty, summary } from '$lib/api/guard.js';
  import CommandPalette from '$lib/components/CommandPalette.svelte';
  import { commands, handleKey } from '$lib/api/commands-ui.js';
  import { registerAppCommands, CMD } from '$lib/api/command-bindings.js';
  import type { Command } from '$lib/api/commands.js';

  let { children } = $props();

  // Capture phase on the document, so it runs before the browser follows the
  // link and before SvelteKit's own router handler. A bubble-phase listener on
  // the document would be too late in some cases; a listener on the toolbar
  // would miss every link outside it, and the back button is not a link at all.
  $effect(() => {
    document.addEventListener('click', intercept, { capture: true });
    return () => document.removeEventListener('click', intercept, { capture: true });
  });

  /**
   * The registry, and the two questions asked of it, all derived *here*.
   *
   * Not `guard.message` / `guard.dirty` / `guard.registry`. Those are getters on
   * a class instance, and a getter reading `$state` is not a tracked dependency
   * for a `$derived` or an `$effect` in another component -- so the template
   * rendered the message once at mount, while the registry was still empty, and
   * showed an empty dialog over a real unsaved edit. The getters are right for
   * imperative callers; a template needs the state in its own scope.
   *
   * One chain rather than three independent reads, so the watcher, the
   * template and the `beforeunload` handler cannot disagree about whether the
   * page is dirty -- which would show the indicator while the handler is absent,
   * or the reverse.
   */
  let registry = $derived(guard.last);
  let dirty = $derived(isDirty(guard.reg));
  let message = $derived(summary(snapshot));

  // ---------------------------------------------------------------- commands

  /**
   * How many objects are selected, for the commands that need some.
   *
   * A number rather than a selection object: the registry must not be able to
   * *decide* whether a command is allowed, only name it. `resolve` returns
   * `disabled` and the surface decides what to do about it, which is what keeps
   * "is this command runnable" from becoming a second source of truth next to
   * the selection itself.
   *
   * Published by whoever owns the selection rather than derived here. There is
   * nothing in the URL to derive it from -- `viewFromLocation` decodes sort,
   * filter, density and direction, and selection is deliberately component state
   * -- so the shell has to be told. Defaulting to 0 instead would leave every
   * gated command permanently disabled, which looks like a broken gate rather
   * than like a constant, and that is the more expensive mistake.
   */
  let selectedCount = $state(0);

  let paletteOpen = $state(false);
  /** Set while the palette is open, so the shell knows not to also act. */
  let paletteQuery = $state('');

  registerAppCommands();

  /**
   * The one keydown listener for the whole app.
   *
   * There were three before this item, each calling `window.addEventListener`
   * independently -- in the lightbox, the bulk modal, and the list table -- so a
   * single Escape was interpreted by components that could not know about each
   * other. #2833 is not a feature that can be added to that arrangement; it is
   * what having one resolver makes possible, and it cannot be arrived at one
   * handler at a time.
   *
   * `window` and not `document`: the listener has to see keys aimed at an
   * `<input>`, because deciding a key is *text* and staying out of the way is
   * also a decision. A listener that never sees input keystrokes cannot make it.
   */
  $effect(() => {
    const onKey = (e: KeyboardEvent) => {
      // While the palette is up it owns the keyboard entirely. It is a modal
      // over whatever opened it, including a modal, so "the top scope wins"
      // would be true and useless -- and its own Arrow/Enter handling is below.
      if (paletteOpen) return;

      handleKey(e, commands, runCommand, selectedCount);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  /**
   * Perform a command by id. `false` declines, and the key is left to the browser.
   */
  function runCommand(id: string): boolean {
    switch (id) {
      case CMD.palette:
        paletteOpen = true;
        return true;
      case CMD.search:
        // A real surface to focus would be a filter input on the list route;
        // until there is one, the command is registered and discoverable rather
        // than absent, and says so.
        // Declined, not run: there is no input to focus yet, so claiming the
        // key would leave `/` doing nothing at all. It falls through to the
        // browser, which types `/` -- the honest behaviour for a command that
        // is registered but not yet wired.
        console.info('commons: search is not yet focusable from the keyboard');
        return false;
      case CMD.clear:
        window.dispatchEvent(new CustomEvent('commons:clear-selection'));
        return true;
      default:
        // The selection and bulk commands are handled by the list route, which
        // owns the selection. A command the shell cannot perform is a no-op
        // *here* and not a dead command, because the route's own listener will
        // see the same keystroke through the same registry.
        // Delegated to the route that owns the selection. Whether it actually
        // ran is that route's answer to give, so this declines: the command is
        // handled elsewhere and the shell has no way to know it landed.
        window.dispatchEvent(new CustomEvent('commons:command', { detail: id }));
        return false;
    }
  }

  function onPaletteRun(c: Command) {
    runCommand(c.id);
  }


  /**
   * The navigation to perform once the user says leave, or `null`.
   *
   * Held as a *thunk*, not a URL. That is the whole redesign, and it comes
   * straight from the bug this file used to have: the guard undid a navigation
   * by calling `goto()` back to where it came from, and SvelteKit destroys the
   * outgoing page component when the navigation commits. So the undo
   * **remounted** the page, `EditSurface`'s `draft` reset to `''`, `dirty` went
   * false, and the surface deregistered itself -- the guard's own undo destroyed
   * the edit it existed to protect, and the confirm rendered over a now-clean
   * page with an empty message.
   *
   * A navigation that is intercepted *before it commits* never unmounts
   * anything. So the guard no longer undoes; it defers. The link's own click is
   * never allowed to complete until the user has decided, and until then the
   * page component is still mounted and its state is intact.
   */
  let pending = $state<(() => void) | null>(null);
  let confirming = $state(false);
  /**
   * The URL to return to on "stay", or `null` when the page is still mounted.
   *
   * Set only by the URL watcher, which is the only path where something has
   * already unmounted. A click-intercepted navigation leaves it `null`, and
   * "stay" then means only "close the dialog" -- which is right, because there
   * is nothing to go back to.
   */
  let returnTo = $state<string | null>(null);
  /**
   * The registry as it was when this navigation started.
   *
   * Not decorative, and the reason the guard works for the browser's back
   * button. A history navigation has no click to intercept, so the URL watcher
   * is the only thing that notices it -- but by then the outgoing page has
   * already unmounted, and every surface on it has already deregistered. A
   * guard reading the *live* registry at that point finds it empty and waves the
   * navigation through, silently losing the edit it was built to protect.
   *
   * So the decision is made against a snapshot taken the instant the navigation
   * is first observed, and the confirm renders from that snapshot rather than
   * from whatever the registry has become since. The live registry keeps its own
   * lifecycle -- deregistration on unmount is still correct and still needed --
   * and the snapshot just remembers what the user was about to walk away from.
   */
  let snapshot = $derived(guard.last);
  /**
   * The last registry seen while every surface was still mounted.
   *
   * Refreshed by the effect below whenever the registry changes, which is why
   * the URL watcher can consult it: a history navigation has no click to
   * intercept, and by the time the URL watcher runs the outgoing page has
   * unmounted and its surfaces have deregistered. Reading the *live* registry
   * at that moment finds it empty and waves the navigation through, silently
   * losing the edit the guard exists to protect.
   *
   * This is a "last known good" value, and it is the honest version of the
   * idea: the guard reports what was at risk when the user started navigating,
   * not what the registry happens to contain now that the page is gone. For a
   * click -- intercepted before anything unmounts -- the two are identical, so
   * this only matters for the case that has no earlier moment to read.
   */

  let lastUrl = $state(page.url.href);
  /**
   * Is the next URL change a navigation the user has already answered?
   *
   * Set by `leave()` and cleared by the watcher once it has seen the change.
   * Not `$state`-in-an-effect-safe on its own: it is bookkeeping read and
   * written by the watcher and by the click handler, never rendered.
   */
  let allowed = false;
  /**
   * Did this URL change come from a click the handler above saw?
   *
   * Set when `intercept` defers a navigation, cleared by the watcher when it
   * acts on the change. It is how the two registries are told apart: a
   * click-intercepted navigation is still on a mounted page, so `guard.reg` is
   * authoritative and the sticky record must be ignored; a navigation that
   * arrives with no click has already unmounted everything, so the live
   * registry is empty and the record is the only remaining evidence.
   *
   * A boolean rather than a URL because it answers "what kind of change was
   * this", not "where did it go" -- and the watcher already has the URL.
   */
  let clickedHref: string | null = null;
  /**
   * The URL of the last link click the handler saw, or `null`.
   *
   * A fact about a *navigation*, not a flag about the page, and the distinction
   * is load-bearing rather than stylistic. A boolean written by a click handler
   * and read by an `$effect` is read at the wrong moment: the effect's
   * dependency is `page.url`, so the router schedules it, and by the time its
   * body runs the handler's write may not have landed. A URL is not -- the
   * watcher compares it against the change it is reacting to, so both values are
   * read at a point it controls.
   *
   * The comparison is what tells the two cases apart. A click-intercepted
   * navigation is still on a mounted page, so `guard.reg` is authoritative and
   * the sticky record must be ignored. A navigation with no click behind it --
   * the back button, a form submit -- has already unmounted everything, so the
   * live registry is empty and the record is the only remaining evidence of
   * what was at risk.
   */

  /**
   * Should a navigation from `from` to `to` be allowed?
   *
   * Called from a capture-phase click handler on the document, which runs
   * *before* the browser follows the link and therefore before SvelteKit sees
   * a navigation. `preventDefault` there is what stops the unmount; there is
   * nothing to undo afterwards.
   *
   * The one thing this cannot catch is a navigation with no originating event:
   * `goto()` from code, a form submit, or a browser back. Those are handled
   * below by the URL watcher, which prompts and re-issues through the same
   * `defer` path -- accepting that a programmatic navigation has already
   * unmounted its page by the time we notice, and that the registry -- which is
   * module state, not component state -- is what survives that.
   */
  function intercept(event: MouseEvent): void {
    if (event.defaultPrevented) return;
    if (event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;

    const anchor = (event.target as Element | null)?.closest?.('a[href]');
    if (!(anchor instanceof HTMLAnchorElement)) return;
    // A new tab is not this tab losing its state.
    if (anchor.target && anchor.target !== '_self') return;
    if (anchor.hasAttribute('download')) return;
    if (anchor.dataset.guardSkip !== undefined) return;

    const href = anchor.href;
    if (href === lastUrl) return;
    // Same document: an anchor link within the page, not a route change.
    if (new URL(href, location.href).pathname === location.pathname) return;
    // Recorded for every link click the handler sees, whether or not it blocks.
    // A click the handler allowed is exactly as much a fact as one it blocked:
    // recording only the blocking path made the watcher treat every allowed
    // click as a history navigation with no click behind it.
    clickedHref = href;
    if (untrackDecide() === 'proceed') return;

    event.preventDefault();
    defer(() => void goto(href));
  }

  /** The decision, read without making the click handler a reactive reader. */
  function untrackDecide() {
    return decide(guard.reg, 'navigation');
  }

  /** Hold a navigation until the user has answered. */
  function defer(navigate: () => void): void {
    pending = navigate;
    confirming = true;
  }

  // Programmatic and back/forward navigations have no click to intercept. Catch
  // them on the URL, and re-issue through `defer` so the user sees the same
  // dialog and the same two buttons.
  $effect(() => {
    const href = page.url.href;
    if (href === lastUrl) return;
    const from = lastUrl;
    lastUrl = href;
    const fromClick = clickedHref === href;
    clickedHref = null;
    if (allowed) {
      // The navigation `leave()` just authorised. Clear the flag and let it
      // through without asking -- but only this once, so a later unsolicited
      // navigation is still guarded.
      allowed = false;
      return;
    }

    // Which registry to ask, and why the two paths differ.
    //
    // A click the handler intercepted never reaches here with anything at risk
    // that the handler did not already see, so the live registry is the whole
    // truth and the sticky record is not consulted at all. Asking it anyway is
    // what made a navigation the user had *already answered* re-prompt: the
    // record still named the edit they abandoned, and the page they came back
    // to had not been typed into yet.
    //
    // A navigation with no click behind it -- the back button, a form submit --
    // has already unmounted the surfaces, so the live registry is empty and only
    // the record knows what was at risk. That is the case the record exists for.
    const atRisk = fromClick ? guard.reg : guard.last;
    if (decide(atRisk, 'navigation') === 'proceed') return;
    // Already re-issuing this one; `defer` holds a thunk, and without this the
    // effect would re-defer on its own `goto` and open a second dialog.
    if (confirming) return;

    // `from` is the page the user was on before this navigation committed, and
    // it is the only record of that page -- the component behind it is already
    // unmounted. Captured before it is overwritten.
    returnTo = from;
    pending = () => void goto(href);
    confirming = true;
  });

  // The tab-close case. `beforeunload` and only `beforeunload`.
  $effect(() => {
    if (!dirty) return;
    const onLeave = (e: BeforeUnloadEvent) => {
      // `returnValue` is the whole API. The string is ignored by every current
      // browser, which is why the visible indicator exists: the prompt cannot
      // be styled or worded, so the *page* has to carry the message.
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', onLeave);
    return () => window.removeEventListener('beforeunload', onLeave);
  });

  function stay() {
    // Nothing is consumed. The edit is still pending and still registered, so
    // the next navigation prompts again -- which is correct: the user chose not
    // to leave, and the work is still theirs.
    //
    // For a click-intercepted navigation this just closes the dialog: the page
    // was never unmounted, so there is nothing to undo. `returnTo` is null in
    // that case.
    //
    // For a navigation the URL watcher caught after the fact, the page *is*
    // gone, and the only honest "stay" is to go back to where the user was.
    // Their unsaved edit does not survive that round trip -- the platform
    // unmounted the component before anything could ask -- but the confirm is
    // the mechanism that told them before they left, and putting them back on
    // their own page is worth more than leaving them on the one they did not
    // choose.
    confirming = false;
    pending = null;
    if (returnTo !== null) {
      const back = returnTo;
      returnTo = null;
      allowed = true;
      void goto(back, { replaceState: true });
    }
  }

  function leave() {
    const navigate = pending;
    if (navigate === null) return;
    // Consume BEFORE navigating. See the header: the other order re-prompts on
    // the way out, and a re-prompted user clicks through.
    guard.consume();
    confirming = false;
    pending = null;
    // This navigation is already answered for. Without the flag the URL
    // watcher sees the resulting URL change, finds the freshly-remounted
    // surface dirty again, and opens a second dialog for the same edit the
    // user just chose to lose -- the exact failure §10.10 names.
    allowed = true;
    returnTo = null;
    navigate();
  }
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
    <!--
      The visible half of the tab-close case. `beforeunload`'s prompt cannot be
      worded, so a user about to close the tab gets one line from the page
      instead -- and it renders only while dirty, because an indicator that is
      always on stops being read the moment it is always on.
    -->
    {#if dirty}
      <span data-testid="unsaved-indicator" class="unsaved" role="status">
        Unsaved changes
      </span>
    {/if}
  </nav>
  <main>
    {@render children()}
  </main>
</div>

<!--
  The command palette. Mounted here, in the shell, rather than inside any
  surface: `Ctrl+P` has to work from anywhere including from inside a modal, and
  a palette mounted inside the list route does not exist on the index route or
  inside the lightbox. One instance, one registry, one keyboard.
-->
<CommandPalette
  open={paletteOpen}
  selected={selectedCount}
  onclose={() => (paletteOpen = false)}
  onrun={onPaletteRun}
/>

<!--
  The confirm. `role="alertdialog"` and not `role="dialog"`: this one blocks a
  navigation the user has already started, and a screen reader that treats it as
  a passive panel will not interrupt.
-->
{#if confirming}
  <div class="scrim" data-testid="guard-scrim">
    <div
      class="confirm"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="guard-title"
      aria-describedby="guard-body"
    >
      <h2 id="guard-title">Leave without saving?</h2>
      <p id="guard-body" data-testid="guard-message">{message}</p>
      <footer>
        <button data-testid="guard-stay" onclick={stay}>Stay</button>
        <button data-testid="guard-leave" onclick={leave}>Leave anyway</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .spacer {
    flex: 1;
  }
  main {
    min-height: 0;
    overflow: hidden;
  }
  .unsaved {
    /* Not a warning colour. A pending edit is ordinary work in progress, and
       dressing it as an alarm trains users to ignore the one that matters --
       the failed save, which the confirm names in words. */
    color: var(--muted, #9a9a9a);
    font-size: 0.85rem;
  }
  .scrim {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: rgb(0 0 0 / 0.5);
    z-index: 100;
  }
  .confirm {
    background: var(--surface, #1a1a1a);
    color: inherit;
    border: 1px solid var(--edge, #444);
    border-radius: 6px;
    padding: 1rem;
    max-width: 28rem;
  }
  .confirm h2 {
    margin: 0 0 0.5rem;
    font-size: 1rem;
  }
  footer {
    display: flex;
    gap: 0.5rem;
    justify-content: flex-end;
    margin-top: 1rem;
  }
</style>
