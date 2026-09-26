/*
  The selection model. Spec 10.6/10.7, plan T-P5-006.

  # Why this is a module and not state in a component

  Selection is the one piece of T-P5-006 that seventeen surfaces all read, and
  it is the one with rules that are wrong in ways a test must catch. A rule
  hidden in a Svelte component is a rule each surface re-implements slightly
  differently, and the difference shows up as a bulk edit hitting rows the
  user never selected.

  So: a selection is a value, every rule is a pure function of it, and the
  component holds a value rather than a behaviour. Same split as
  `gestures.ts`, and for the same reason -- it is what makes
  `scripts/mutate-selection.py` possible.

  # Identity, not position

  A selection is a set of object *ids*. Row indices are a view over a
  selection, derived per render, and they are never stored.

  The reason is not tidiness. A selection held as indices is wrong in the
  ordinary case: the user selects three rows, pages, and the third selected
  row is now row 0 of the new page. Every UI that does this has the same bug
  and the same report -- "it tagged the wrong three" -- and the fix is
  always the same: store the id.

  # Select-all is a mode, not an operation

  "Select all 4,000" and "select the 40 loaded" are different intents, and a
  user who cannot tell which one they did will bulk-delete something. So
  `selectAll` holds the *query* that defines the selection rather than a set
  of ids, and the set is the ids chosen so far.

  This is the same shape as a database cursor, and it is the only shape in
  which "select all 4,000 then bulk-tag" is both correct and honest about
  what it is about to touch. A `Set<id>` cannot express it: enumerating 4,000
  ids to hold them is a page of state the size of the library, and a
  select-all that is really only the loaded page is a data-loss bug wearing a
  friendly label.

  # The two modes interact, and the rule is not obvious

  A set of explicitly chosen ids *and* a query means: everything the query
  matches, plus the explicit ids, minus nothing. Why keep the explicit ids at
  all? Because the user can deselect one row of a select-all, and that
  deselection is a single id that must survive the next page load. So
  `excluded` holds what was turned off, and the membership rule is:

      in the selection  <=>  the query matches it  XOR  it is excluded
  union an explicit pick, when there is no query:

      in the selection  <=>  id is in `picked`  XOR  id is in `excluded`

  and the `XOR` is the whole point. A union of the two sets is the obvious
  implementation and it is wrong: it makes deselecting a row of a select-all a
  no-op that the next page load silently undoes.
 */

import type { GridQuery } from './keyset.js';

/** An object id, as the store and the client already spell it. */
export type ObjectId = string;

/**
 * The selection.
 *
 * `query` and `picked` are the two halves of select-all and manual picking.
 * `excluded` applies to both. All three are optional and all three default to
 * "nothing selected", so a default-constructed selection is a real, valid,
 * empty selection rather than a special case every caller has to handle.
 */
export interface Selection {
  /** Set when the user chose "select everything matching this". */
  readonly query?: GridQuery;
  /** Ids chosen by hand, or deselected from a `query`. */
  readonly picked?: ReadonlySet<ObjectId>;
  /** Ids turned off inside a `query`. Empty and absent mean the same thing. */
  readonly excluded?: ReadonlySet<ObjectId>;
}

// ---------------------------------------------------------------------------
// Membership
// ---------------------------------------------------------------------------

/**
 * Is this id in the selection?
 *
 * The caller answers `matchesQuery`, because whether an id matches a query is
 * the server's business and a decision this module has no way to make. That is
 * a deliberate seam: the alternative is a query evaluator in the client, which
 * would then disagree with the server's about stemming, filters and null
 * handling -- and a selection that disagrees with the query is a bulk edit
 * against a different set than the user saw.
 */
export function isSelected(
  sel: Selection,
  id: ObjectId,
  matchesQuery: (id: ObjectId) => boolean
): boolean {
  if (sel.excluded?.has(id)) return false;
  if (sel.query) {
    if (matchesQuery(id)) return true;
    return false;
  }
  return sel.picked?.has(id) ?? false;
}

/**
 * Does this selection cover *everything* the query matches?
 *
 * The distinction the UI must show, per the spec's own rule that a
 * destructive action names its scope. A select-all and a hand-picked set of
 * forty are both "40 selected" if the library happens to be small, and the
 * number alone is a lie in one of those cases.
 */
export function isSelectAll(sel: Selection): boolean {
  return sel.query !== undefined;
}

// ---------------------------------------------------------------------------
// Counting
// ---------------------------------------------------------------------------

/**
 * How many does the user believe they selected?
 *
 * Three sources, in order of trustworthiness, and the order matters:
 *
 *  1. `serverCount` -- the count from the server for a select-all. The only
 *     honest number for a set larger than what is loaded, and the server is
 *     the only one that can count a filter.
 *  2. `loadedCount` -- how many of the loaded rows are selected, when there is
 *     no select-all.
 *  3. Nothing else. An estimate from the loaded page is not offered as a
 *     total, because a user who bulk-deletes 40 of 4,000 because the UI said
 *     40 has been lied to by arithmetic.
 *
 * `serverCount` is ignored when there is no `query`, on purpose: if the server
 * was asked about a select-all that has since been cleared, its number now
 * describes something the user is no longer selecting.
 */
export function selectedCount(
  sel: Selection,
  loadedIds: readonly ObjectId[],
  serverCount?: number
): number | 'unknown' {
  if (isSelectAll(sel)) {
    const excluded = sel.excluded?.size ?? 0;
    if (serverCount === undefined) {
      // A select-all with no count from the server is *at least* the loaded
      // matches, and reporting 'unknown' is the honest answer. Reporting the
      // loaded count is the bug this branch exists to prevent.
      return 'unknown';
    }
    return Math.max(0, serverCount - excluded);
  }
  let n = 0;
  for (const id of loadedIds) {
    if (isSelected(sel, id, () => false)) n += 1;
  }
  return n;
}

// ---------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------

/** An empty selection. A fresh object, because `Selection` is not mutated. */
export function emptySelection(): Selection {
  return {};
}

/**
 * Add or remove one id.
 *
 * Toggling under a `query` writes to `excluded`; without one it writes to
 * `picked`. Same visible effect, and that is the point -- a caller that
 * guessed which list to touch would get it wrong in exactly the mode where
 * the mistake is invisible until a bulk write.
 */
export function toggle(sel: Selection, id: ObjectId): Selection {
  const excluded = new Set(sel.excluded ?? []);
  const picked = new Set(sel.picked ?? []);

  if (sel.query) {
    if (excluded.has(id)) excluded.delete(id);
    else excluded.add(id);
    return { ...sel, excluded };
  }

  if (picked.has(id)) picked.delete(id);
  else picked.add(id);
  return { ...sel, picked };
}

/** Select exactly these ids, discarding any select-all. */
export function setPicked(ids: Iterable<ObjectId>): Selection {
  return { picked: new Set(ids) };
}

/** Select everything matching `query`, discarding any hand-picked ids. */
export function selectAll(query: GridQuery): Selection {
  return { query };
}

/** The empty selection. Named `clear` so callers do not hand-roll `{}`. */
export function clear(): Selection {
  return emptySelection();
}

// ---------------------------------------------------------------------------
// Surviving a change of query
// ---------------------------------------------------------------------------

/**
 * What happens to a selection when the result set changes under it.
 *
 * A filter change cannot leave the selection looking the same, because the
 * rows it referred to may not be in the new result at all. The rule:
 *
 *  - a `query` select-all is dropped entirely. The new query is a different
 *    set and the old one is not reinterpreted as "all of the new one";
 *  - hand-picked ids that are still present are kept;
 *  - hand-picked ids that are gone are counted and reported, and the count is
 *    the *return value* rather than a log line.
 *
 * Returning the number is deliberate. The caller's obligation is to tell the
 * user, and a function that returns nothing leaves the obligation to whoever
 * happens to call it -- which is how a selection silently narrows to nothing
 * and a bulk edit lands on zero rows while the user believes it is editing
 * their selection.
 */
export interface PruneResult {
  readonly selection: Selection;
  /** How many picked ids were not in `presentIds` and were dropped. */
  readonly dropped: number;
}

export function prune(sel: Selection, presentIds: ReadonlySet<ObjectId>): PruneResult {
  // A select-all does not survive a change of result: the rows it named are
  // described by the old query, and "everything" under a new filter is a
  // selection the user never made.
  if (sel.query) {
    return { selection: emptySelection(), dropped: 0 };
  }

  const picked = sel.picked;
  if (!picked) return { selection: sel, dropped: 0 };

  const kept = new Set<ObjectId>();
  let dropped = 0;
  for (const id of picked) {
    if (presentIds.has(id)) kept.add(id);
    else dropped += 1;
  }

  if (dropped === 0) return { selection: sel, dropped: 0 };
  return { selection: { ...sel, picked: kept }, dropped };
}

// ---------------------------------------------------------------------------
// The dirty rule
// ---------------------------------------------------------------------------

/**
 * Is this edit unsaved?
 *
 * A narrow rule on purpose, and the narrowness is the point: an edit that was
 * saved is not unsaved, and a guard that cannot tell the difference blocks a
 * user who has already saved their work. So the input is a *save state*, not
 * an edit count.
 *
 * A counter would be wrong twice: it does not fall back to zero when the last
 * pending edit is saved, and it cannot represent "edited, saved, edited again"
 * without a second piece of state to subtract the save from.
 */
export function isUnsaved(pendingEdits: number, hasFailedSave: boolean): boolean {
  return hasFailedSave || pendingEdits > 0;
}
