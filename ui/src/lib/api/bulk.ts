/*
  The bulk result, as the UI has to present it. Spec 10.7, plan T-P5-006.

  # Why the client keeps its own model of the server's count

  `Store::bulk_apply_tag` returns a `BulkOutcome` with three numbers, and the
  obvious client is to format that struct directly. That loses the one thing the
  numbers mean *together*: the difference between "you tagged 12 things" and
  "you tagged 12 of the 15 you selected, and the other 3 are not yours to
  see."

  Those are different sentences and they need different UI, but `BulkOutcome` is
  the same struct either way. So the client's job is not to re-count -- it must
  not, a client count is a guess with a progress bar -- but to turn the server's
  three numbers into a *state* the view can switch on.

  # The states, and the one that is not a success

  Four states, from the three numbers:

    applied > 0, nothing hidden      -> 'done'
    applied > 0, something hidden    -> 'partial'
    applied == 0, all hidden         -> 'blocked'   <-- not 'empty'
    applied == 0, nothing hidden     -> 'empty'

  The split that earns its keep is 'blocked' versus 'empty'. Both have
  `applied === 0` and both render as "nothing happened", which is how a
  permission problem gets reported to a user as an empty library. The user who
  selected fifteen unverified objects has been told nothing, and the message
  they need is "those are not visible to you", not "no objects matched".

  # Why 'partial' is not a warning

  A partial result is the *correct* result of a correct write. The three hidden
  objects were never the caller's to write; the server did exactly what it was
  asked and within its own rights. Rendering it as a warning teaches users that
  bulk edit is unreliable, which is the opposite of what the data layer just
  ensured. It is worth a neutral, informational line, and no more.
 */

import type { BulkTarget } from './client.js';
import type { Selection } from './selection.js';

/** What the server reported, mirroring Rust's `BulkOutcome`. */
export interface BulkOutcome {
  readonly applied: number;
  readonly skipped_invisible: number;
  readonly requested: number;
}

/** How a bulk write ended, from the server's three numbers. */
export type BulkState =
  | 'done'
  | 'partial'
  | 'blocked'
  | 'empty'
  | 'idle';

/** A `BulkState` plus the counts that produced it. */
export interface BulkResult {
  readonly state: BulkState;
  readonly applied: number;
  readonly skippedInvisible: number;
  readonly requested: number;
}

/** The result of a modal that has not been applied yet. */
export const IDLE: BulkResult = {
  state: 'idle',
  applied: 0,
  skippedInvisible: 0,
  requested: 0,
};

/**
 * Classify the server's answer.
 *
 * Takes the outcome rather than the numbers, so a caller cannot pass them in
 * the wrong order -- `applied` and `requested` are both `usize` and a swap
 * compiles, and the swap turns 'done' into 'blocked'.
 */
export function classify(outcome: BulkOutcome): BulkResult {
  const applied = Math.max(0, outcome.applied);
  const skipped = Math.max(0, outcome.skipped_invisible);
  const requested = Math.max(0, outcome.requested);

  let state: BulkState;
  if (applied > 0) {
    // Hidden rows alongside a real write is a *result*, not a warning: the
    // server refused only what the caller had no right to touch.
    state = skipped > 0 ? 'partial' : 'done';
  } else if (skipped > 0) {
    // Everything the caller named was invisible to it. A permissions shape, and
    // the one that must not be reported as an empty result.
    state = 'blocked';
  } else {
    state = 'empty';
  }

  return { state, applied, skippedInvisible: skipped, requested };
}

/**
 * The line to show under the confirm button.
 *
 * One function rather than a switch in the component, because these strings are
 * the part of this feature a user reads, and a string written in a `.svelte`
 * file is a string no test can assert on. Each says what happened, not what the
 * user should feel about it.
 *
 * `null` for 'idle' and 'empty': an empty result needs no message, because the
 * modal's own scope line already told the user what they were about to touch and
 * a second line restating "none of it" is noise. 'blocked' is the exception --
 * it is the one state where silence would be a lie.
 */
export function resultMessage(res: BulkResult): string | null {
  switch (res.state) {
    case 'idle':
      return null;
    case 'empty':
      return null;
    case 'done':
      return res.applied === 1 ? 'Tagged 1 object.' : `Tagged ${res.applied} objects.`;
    case 'partial':
      return (
        res.applied === 1
          ? `Tagged 1 object. ${res.skippedInvisible} of your selection is not visible to you and was left alone.`
          : `Tagged ${res.applied} objects. ${res.skippedInvisible} of your selection ` +
            `is not visible to you and ${res.skippedInvisible === 1 ? 'was' : 'were'} left alone.`
      );
    case 'blocked':
      return (
        res.skippedInvisible === 1
          ? 'The one object you selected is not visible to you, so nothing was changed.'
          : `All ${res.skippedInvisible} objects you selected are not visible to you, so nothing was changed.`
      );
  }
}

/**
 * Is the confirm button worth enabling?
 *
 * Deliberately *not* "the selection is non-empty". The question is whether this
 * *write* has anything to do, and a select-all over an empty result has nothing
 * to do however many ids the user believes they picked. A button that is
 * enabled and then reports 'empty' is a dead end for the user, so the scope
 * count -- the server's -- is what gates it.
 *
 * An 'idle' result means the count has not come back yet, so it also reads as
 * disabled. Enabling it optimistically and then refusing is worse than a moment
 * of a disabled button.
 */
export function canApply(res: BulkResult): boolean {
  return res.state === 'done' || res.state === 'partial';
}

/**
 * The line naming the scope, shown *before* the write.
 *
 * Separate from `resultMessage` because it is the pre-write statement of what
 * is about to happen and the post-write statement of what did, and a single
 * string trying to be both is either a lie before the write or a non-sequitur
 * after it.
 *
 * `selected` is what the *client* believes, and it is deliberately not the only
 * number here: for a select-all the client has no honest count (see
 * `selectedCount` in `selection.ts`, which returns 'unknown' rather than
 * guessing), so the server's `applied` is the only real total and the string
 * says so with "about" rather than pretending.
 */
export function scopeMessage(
  sel: Selection,
  res: BulkResult,
  selected: number | 'unknown'
): string {
  const all = sel.query !== undefined;
  const n = res.applied;

  if (all) {
    if (n === 0) return 'Everything matching this search (currently nothing).';
    return `Everything matching this search — about ${n} object${n === 1 ? '' : 's'}.`;
  }

  const picked = selected === 'unknown' ? res.applied : selected;
  if (picked === 0) return 'No objects selected.';
  if (picked === 1) return '1 object selected.';
  return `${picked} objects selected.`;
}

/**
 * The target for the current selection, as the server's schema takes it.
 *
 * A function rather than a literal at each call site because the two shapes are
 * the ones that get confused: sending `{ kind: 'IDS' }` for a select-all is the
 * bug that turns "tag everything matching" into "tag the 40 I loaded", and a
 * select-all that silently degrades to the loaded page is a data-loss bug
 * wearing a friendly label.
 *
 * `kind` is named on both branches rather than inferred, so "neither given" and
 * "both given" are states the server's schema rejects rather than states it
 * has to notice at runtime.
 *
 * The `excluded` list travels with the target rather than being applied
 * client-side for the same reason the ids do: the server has to be the one
 * deciding membership, or the client and the server disagree about the set a
 * bulk edit will touch.
 *
 * The filter travels as a serialized string, the same encoding `PageInput.filter`
 * uses, so a select-all over the current view covers exactly the rows on screen.
 */
export function bulkTarget(sel: Selection, filterJson: string): BulkTarget {
  const excluded = [...(sel.excluded ?? [])];
  if (sel.query) {
    return { kind: 'QUERY', query: filterJson, excluded };
  }
  return { kind: 'IDS', ids: [...(sel.picked ?? [])], excluded };
}
