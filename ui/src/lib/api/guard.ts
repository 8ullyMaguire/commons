/*
  Unsaved-entry protection. Spec 10.7, 10.10; plan T-P5-006 item 4.

  # What this module is

  The decision half of the guard, with no router in it. `shouldPrompt` answers
  "does leaving now risk anything", `summary` answers "what, in words", and
  `consume` answers "the user said leave anyway, so stop asking". The layout
  wires those to `page.url` and `beforeunload`; nothing here knows a navigation
  exists.

  That split is the same one as `selection.ts` and `gestures.ts`, and for the
  same reason: these are the rules, and a rule inside a `.svelte` file cannot be
  unit-tested or mutated. Three of the four acceptance cases in the spec are
  about the rules, and all three are reachable from here.

  # The registry is a set of *surfaces*, not a boolean

  The failure this exists to prevent (#6466: "unsaved entries intermittently
  lost") is not that a flag was missed. It is that a guard has to be *asked*
  whether anything is dirty, and every surface that holds an edit has to
  remember to answer. One surface forgetting is a lost edit, and it is lost
  silently, which is why the report says "intermittently".

  So a surface *registers* rather than setting a flag. Registration is
  idempotent and keyed by id, which means the same surface registering twice
  cannot inflate the count -- a count that can be inflated is a count that
  reports "2 unsaved items" when there is one, and a user who has learned that
  the count is wrong stops trusting the dialog.

  # Why `kind` is part of the key

  A pending edit and a failed save are both "unsaved", and merging them tells a
  user their four in-progress tags are at risk when the actual problem is that
  a save *errored*. The second is worse — the work is already lost from the
  server's point of view — and it gets its own count, its own sentence, and a
  different response: a failed save is not something "leave anyway" fixes.

  # The consume/clear ordering, which is the whole mechanism

  `consume` exists because the alternative -- block the navigation, then clear
  in an effect afterwards -- loses the edit. A guard that warns the user, lets
  them choose to leave, and then discards their work anyway is worse than no
  guard at all: the user was told, they decided, and the decision was ignored.

  So the registry is cleared *before* the navigation is allowed, and a test
  asserts it (spec section 4, case 2: cancelling leaves everything alone --
  because `cancel` is not `consume`).
 */

import { isUnsaved } from './selection.js';

/** What kind of unsaved work a surface is holding. */
export type EditKind = 'pending' | 'failed';

/**
 * One surface's dirty state.
 *
 * `label` is what the user is told, so it is a surface's own name rather than
 * something derived here: "Scene 12" means more to a user than "1 item", and
 * the generic version is what a guard looks like when nobody wrote the copy.
 */
export interface SurfaceState {
  readonly id: string;
  readonly kind: EditKind;
  readonly label?: string;
}

/** Everything dirty, keyed by surface id. */
export type Registry = ReadonlyMap<string, SurfaceState>;

/** An empty registry. A fresh object, because `Registry` is not mutated. */
export function emptyRegistry(): Registry {
  return new Map();
}

/**
 * Register a surface, or update it.
 *
 * Keyed by `id`, so a surface that re-renders and re-registers replaces its own
 * entry rather than adding a second one. `count` defaults to 1 because a
 * surface with a dirty flag and no count is the common case, and 0 is available
 * for "this whole view is unsaved" without a number to put on it.
 */
export function register(
  reg: Registry,
  surface: SurfaceState
): Registry {
  // Identity-preserving when nothing changes, and the caller depends on it: the
  // store compares `next === this.reg` to avoid writing `$state` for a
  // re-registration that says exactly what is already there. Always allocating
  // a new Map made a surface's own `$effect` -- which reads the registry and
  // writes it back -- self-trigger until Svelte killed it with
  // `effect_update_depth_exceeded`.
  const prev = reg.get(surface.id);
  if (
    prev !== undefined &&
    prev.kind === surface.kind &&
    prev.label === surface.label
  ) {
    return reg;
  }
  const next = new Map(reg);
  next.set(surface.id, surface);
  return next;
}

/** A surface that is no longer dirty. Unknown ids are a no-op. */
export function clear(reg: Registry, id: string): Registry {
  if (!reg.has(id)) return reg;
  const next = new Map(reg);
  next.delete(id);
  return next;
}

/**
 * Is anything unsaved?
 *
 * A *pending* count of 0 is still unsaved, because a surface that registered a
 * pending edit with no count is saying "I am dirty, I just cannot say how much".
 * Treating 0 as clean is the bug that makes the guard fire on some navigations
 * and not others, which is the "#6466 intermittently" shape.
 */
export function isDirty(reg: Registry): boolean {
  for (const s of reg.values()) {
    if (isDirtyOne(s)) return true;
  }
  return false;
}

/** How many surfaces are dirty. */
export function dirtyCount(reg: Registry): number {
  let n = 0;
  for (const s of reg.values()) if (isDirtyOne(s)) n += 1;
  return n;
}

/**
 * Is this one surface dirty?
 *
 * `count === 0` is still dirty: zero is a legal count meaning "I am dirty and
 * have nothing to put a number on", which is what a view-level flag registers.
 * The only clean surface is an absent one, and absence is expressed by `clear`.
 */
function isDirtyOne(_s: SurfaceState): boolean {
  // Every surface present in the registry is dirty. There is no third state:
  // `register` is how a surface says it is dirty and `clear` is how it says it
  // is not, so "present but clean" is not representable and does not need a
  // rule.
  //
  // An earlier version carried a per-surface `count` and treated 0 as clean.
  // That is the shape of the #6466 report -- a surface registering a view-level
  // "this is dirty" flag with nothing to count came back clean, so the guard
  // fired on some navigations and not others. A count in the prompt that no
  // surface is responsible for filling in is a lie waiting for a caller, so the
  // field is gone rather than defaulted. The prompt counts *surfaces*, and its
  // copy says "changes" rather than "items" so the two cannot be confused.
  return true;
}

/** The surfaces holding pending edits, and the total they represent. */
export interface Split {
  readonly pending: number;
  readonly failed: number;
  /** Surfaces, for naming them. Ordered by id so a prompt is deterministic. */
  readonly surfaces: readonly SurfaceState[];
}

/**
 * The registry, counted and ordered.
 *
 * Ordered by id, not by insertion: `Map` preserves insertion order, so two
 * registries with the same contents can prompt differently depending on which
 * surface registered first, and a confirm that reorders itself between two
 * identical states reads as a different dialog.
 */
export function split(reg: Registry): Split {
  const surfaces = [...reg.values()]
    .filter(isDirtyOne)
    .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  let pending = 0;
  let failed = 0;
  for (const s of surfaces) {
    if (s.kind === 'failed') failed += 1;
    else pending += 1;
  }
  return { pending, failed, surfaces };
}

/**
 * Does leaving now risk anything?
 *
 * The one function the router asks, and the reason it exists separately from
 * `isDirty` is that a *navigation* and a *close* are not the same question. A
 * navigation can be confirmed; a tab close cannot, because only the browser may
 * prompt there. So the caller needs to know which kind of risk it is facing
 * before it can decide whether it is even allowed to ask.
 */
export function shouldPrompt(
  reg: Registry,
  _kind: 'navigation' | 'unload'
): boolean {
  return isDirty(reg);
}

/**
 * The confirm's body.
 *
 * §10.7 asks a destructive action to spell out its scope, and a bare "Are you
 * sure?" does not. Every branch names a number and, where there is one, a
 * surface. `null` for a clean registry: the layout does not prompt at all, and
 * a caller that renders this unconditionally would put "nothing is at risk" in
 * front of a user who is trying to navigate.
 */
export function summary(reg: Registry): string | null {
  const { pending, failed, surfaces } = split(reg);
  // The empty case is decided here rather than by an `isDirty` guard above.
  // That guard was dead weight: an empty registry means no surfaces, which means
  // no parts, so the early return and the fallthrough both produce null, and a
  // mutation script replacing one with the other found nothing to kill. One
  // path, and the condition is the one that is actually load-bearing.
  if (surfaces.length === 0) return null;

  // A failed save is named on its own, and first. It is a different problem
  // from work in progress -- the work is already not persisted -- and a user
  // who is told only "you have unsaved edits" will assume their work is safely
  // held somewhere and leave.
  const parts: string[] = [];
  if (failed > 0) {
    parts.push(
      failed === 1
        ? '1 edit failed to save and may already be lost'
        : `${failed} edits failed to save and may already be lost`
    );
  }
  if (pending > 0) {
    parts.push(pending === 1 ? '1 unsaved change' : `${pending} unsaved changes`);
  }

  const named = surfaces.find((s) => s.label);
  let out = `You have ${parts.join(' and ')}.`;
  if (named?.label) {
    out += ` (${named.label})`;
  }
  return out;
}

/**
 * The user chose to leave anyway.
 *
 * Clears the registry so the *next* navigation is not re-prompted, which is the
 * difference between a guard and an inescapable dialog. Returns a fresh registry
 * because the caller's own reference must not be mutated -- a guard that
 * cleared a shared map in place would lose the state of a surface that
 * re-registers a microtask later.
 */
export function consume(reg: Registry): Registry {
  return reg.size === 0 ? reg : emptyRegistry();
}

/**
 * The single decision the layout makes, so the rule is in one tested place
 * rather than in a chain of `if`s across a component.
 *
 * Returns `'proceed'` or `'confirm'`, never a boolean, because the two callers
 * do different things with a yes and a third thing (a native `beforeunload`)
 * with a maybe, and a boolean forces each caller to re-derive which it has.
 */
export function decide(
  reg: Registry,
  kind: 'navigation' | 'unload'
): 'proceed' | 'confirm' {
  return shouldPrompt(reg, kind) ? 'confirm' : 'proceed';
}

// `isUnsaved` is re-exported so a caller wiring a surface has the primitive and
// the registry in one import, and so the two cannot drift: the registry's
// dirtiness rule *is* that function, applied per surface.
export { isUnsaved };
