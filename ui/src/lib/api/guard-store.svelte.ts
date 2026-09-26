/*
  The reactive registry behind the unsaved guard.

  Spec 10.7, 10.10; plan T-P5-006 item 4. The rules are in `guard.ts`; this is
  the one piece that has to be reactive, and it is `.svelte.ts` because
  `$state` in a plain `.ts` builds and ships nothing -- the compiler only
  processes files it recognises as Svelte modules, so the failure is at runtime
  with a green build.

  # Why a module-level singleton, and not a context

  A context would be tidier and it would also be wrong here. The registry has to
  outlive the route: an unsaved edit on `/list` must still be known when the
  user navigates to `/`, because that navigation is exactly what the guard is
  for. A context set in a layout *is* that lifetime, but a context read from a
  component that mounted under a *different* provider is a silent `undefined` --
  and "no guard" is a failure that looks like a feature working.

  A module singleton has one obvious failure instead: two registries in one
  process. That cannot happen in a browser tab, and a test that needs an empty
  one calls `reset()`.

  # `consume` clears here too, not only in the pure module

  The pure `consume` returns a fresh empty map. This class applies it to its own
  `$state`, which is what makes "leave anyway" stop the *next* prompt. The
  ordering that matters is in the layout: consume, then navigate. Consuming after
  the navigation lands is a guard that re-prompts on the way out, and a user who
  is re-prompted for the same edit they already chose to lose learns to click
  through, which is the guard's whole failure mode.
 */

import { isDirty, register, clear, consume, emptyRegistry, summary, type Registry, type SurfaceState } from './guard.js';

class GuardStore {
  /**
   * The registry, as reactive state.
   *
   * A public `$state` field and not a getter over a private one, and that is
   * the whole reason this class is usable from another component: a getter
   * reading private `$state` is not a *tracked dependency* for a `$derived` or
   * an `$effect` elsewhere, so `guard.message` in a template rendered once at
   * mount and showed an empty dialog over a real unsaved edit. Read the field
   * and the caller's dependency graph includes the state itself.
   */
  reg = $state<Registry>(emptyRegistry());

  /**
   * The last registry value, retained for the guard to consult.
   *
   * Retained here, in the store, rather than in the layout as a derived or an
   * effect, for a timing reason that cost a long debugging session to find: an
   * effect in the layout that snapshots `guard.reg` runs *after* a navigation
   * has already unmounted the surfaces that were in it. For a history
   * navigation there is no earlier hook at all -- `beforeunload` does not fire
   * for same-document navigations, and the back button is not a click -- so an
   * effect is structurally too late and always snapshots the empty registry.
   *
   * The store is different: `add` and `drop` run *synchronously*, inside the
   * surface's own effect, while that surface is still mounted. So the retained
   * value is always the state as of the last registration, and a navigation
   * that arrives later reads something true.
   *
   * Written by `add` and by nothing else. Specifically NOT by `drop`, and that
   * omission is the mechanism rather than an oversight: a surface that unmounts
   * has been navigated away from, so its deregistration is a *consequence* of
   * the navigation and must not also erase the record of what the user was about
   * to lose. `drop` writing here is what made the back button find an empty
   * registry and wave the edit through -- the guard destroying its own evidence
   * one step before reading it.
   *
   * `consume()` clears `reg` but not `last` either, for the same reason: the
   * user abandoning an edit does not erase that they had one.
   */
  last = $state<Registry>(emptyRegistry());

  get dirty(): boolean {
    return isDirty(this.reg);
  }

  /** The confirm's body, or `null` when there is nothing to say. */
  get message(): string | null {
    return summary(this.reg);
  }

  /**
   * A surface says it is dirty. Idempotent per id.
   *
   * The equality guard on the assignment is the load-bearing part. `register`
   * always returns a fresh Map, so an unconditional `this.reg = ...` writes
   * `$state` on every call -- and the caller, `EditSurface`, calls this from an
   * `$effect` that *reads* the registry. That is a self-triggering loop:
   * effect reads `dirty` -> writes `reg` -> effect re-runs -> writes `reg`
   * again, until Svelte kills it with `effect_update_depth_exceeded` after a
   * thousand rounds. The symptom was an empty confirm dialog over a genuinely
   * unsaved edit: the loop starves the render effect, so the derived `message`
   * never settles.
   *
   * Comparing by identity is the correct test here rather than a workaround.
   * `register` is idempotent per id by construction, so the only way the new
   * map differs in content is a real change -- and if it does differ, `===`
   * fails anyway because it is a new object. What this buys is that a *no-op*
   * re-registration does not notify anyone.
   */
  add(surface: SurfaceState): void {
    const next = register(this.reg, surface);
    if (next === this.reg) return;
    this.reg = next;
    this.last = next;
  }

  /** A surface says it is clean -- its save succeeded, or it was reverted. */
  drop(id: string): void {
    const next = clear(this.reg, id);
    if (next === this.reg) return;
    this.reg = next;
  }

  /**
   * The user chose to leave anyway.
   *
   * Returns nothing: the caller has already decided to navigate, and giving it
   * a registry back would invite it to re-prompt with a stale copy.
   */
  consume(): void {
    this.reg = consume(this.reg);
    // The one caller entitled to erase the record, because it is the one that
    // means "yes, lose it".
    //
    // `last` is deliberately sticky against `drop` -- a surface unmounting is a
    // consequence of a navigation, and clearing the record there would make the
    // guard destroy its own evidence one step before reading it. But a user who
    // chose to leave and then came back must not be asked again about the edit
    // they already abandoned, or the guard is an inescapable dialog. So
    // `consume` clears both, and it is the only thing that does.
    this.last = emptyRegistry();
  }

  /** For tests. Not reachable from a component. */
  reset(): void {
    this.reg = emptyRegistry();
  }
}

/**
 * The one registry.
 *
 * Created at module scope, not per component: a second instance would be a
 * second answer to "is anything unsaved", and whichever one the layout was not
 * holding would be the one nothing registered into.
 */
export const guard = new GuardStore();
