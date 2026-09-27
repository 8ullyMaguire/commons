/*
  The undo offer, as the UI has to present it. Spec 10.7, plan T-P5-006 item 7.

  Companion to `bulk.ts`, and it follows the same rule: the client does not
  decide, it classifies. The server decides whether an undo is allowed
  (`Store::undo` returns one of six refusals or a count), and this turns that
  into a state the view can switch on.

  # Why the refusals are the interesting half

  The success case is one line: the toast says "Undo" and pressing it undoes.
  The refusals are where a naive implementation loses the user, because there
  are five of them and they need five different sentences.

  A refused undo is not a failure the user caused, and treating it as one is
  how "you already changed this, so undo is off" turns into an error dialog.
  The user did nothing wrong; the system changed underneath them. Every refusal
  below is a *state* the view renders, not an exception it throws.

  # The one that is a bug, not a state

  'failed' is the odd one out. It is the only state here that means the undo was
  attempted and something went wrong on our side, and it is deliberately NOT
  offered an undo button -- offering "Undo" on a toast whose undo just errored
  is how a user ends up clicking it six times.

  # Why the count comes from the server and is never re-derived here

  The toast's number is the one the server sends on the `ok` outcome, and the
  client renders it. It is not recomputed from the `BulkOutcome` the write
  returned, because those are different numbers under different conditions and
  the client's job is not to work out which one applies -- it does not have the
  per-object state that decides it.

  The one condition worth knowing, because it is the reason the server has to do
  the counting: `bulk_apply_tag`'s `ON CONFLICT DO UPDATE` *replaces* a row that
  already carried the tag, so every object the write reaches is a row it
  changes. "Added beach to 40" where 12 of them already had beach is 40 rows
  whose `source` and `confidence` were overwritten, and undoing it has to put
  all 40 back.
 */

/**
 * What the server said about one undo attempt.
 *
 * Mirrors Rust's `UndoError`, plus the count for the success. Every refusal is
 * its own value rather than a boolean, because a boolean forces the caller to
 * re-ask the server which one it was, and the answer is already here.
 */
export type UndoOutcome =
  | { readonly kind: 'ok'; readonly restored: number }
  | { readonly kind: 'expired' }
  | { readonly kind: 'already-undone' }
  | { readonly kind: 'superseded'; readonly objectId: string }
  | { readonly kind: 'not-yours' }
  | { readonly kind: 'no-such-record' }
  | { readonly kind: 'failed'; readonly reason: string };

/** How an undo toast renders. */
export type UndoState =
  /** Offerable, with a count. The only state that carries a button. */
  | { readonly state: 'offerable'; readonly restored: number }
  /** The window closed. Said once, not offered. */
  | { readonly state: 'expired'; readonly restored: number }
  /** Someone changed one of the objects. Name the object. */
  | { readonly state: 'superseded'; readonly objectId: string }
  /** Pressing it did nothing. Do not offer it again. */
  | { readonly state: 'spent'; readonly restored: number }
  /** We could not tell whether it was theirs, so we did not touch it. */
  | { readonly state: 'refused'; readonly reason: string }
  /** The undo itself failed. No button, ever. */
  | { readonly state: 'failed'; readonly reason: string };

/** The toast's resting state, before any write has happened. */
export const NO_UNDO: UndoState = { state: 'refused', reason: 'no undo yet' };

/**
 * Turn the server's answer into a state the view can switch on.
 *
 * Takes the outcome rather than the pieces, so a caller cannot put `restored`
 * where `objectId` goes -- both are strings in two of the variants and the
 * swap compiles.
 */
export function classifyUndo(outcome: UndoOutcome): UndoState {
  switch (outcome.kind) {
    case 'ok':
      return { state: 'offerable', restored: outcome.restored };
    case 'expired':
      return { state: 'expired', restored: 0 };
    case 'already-undone':
      return { state: 'spent', restored: 0 };
    case 'superseded':
      return { state: 'superseded', objectId: outcome.objectId };
    case 'not-yours':
      // Deliberately worded the same as 'no-such-record': the toast must not
      // tell a caller that a record they cannot see exists, or the toast
      // becomes an oracle for guessing record ids.
      return { state: 'refused', reason: 'not available' };
    case 'no-such-record':
      return { state: 'refused', reason: 'not available' };
    case 'failed':
      return { state: 'failed', reason: outcome.reason };
  }
}

/**
 * Whether the toast may render a button.
 *
 * One predicate, so a view cannot forget the rule. `failed` is the case that
 * matters: offering "Undo" on a toast whose undo just errored is how a user
 * clicks it six times.
 */
export function canUndo(state: UndoState): boolean {
  return state.state === 'offerable';
}

/**
 * The one line the toast shows.
 *
 * The refusals get sentences rather than a generic failure, because "undo
 * failed" tells the user nothing they can act on. A superseded undo in
 * particular names the object: "someone changed one of these" is not
 * actionable, and "object 7f3a was changed since" is.
 */
export function describe(state: UndoState): string {
  switch (state.state) {
    case 'offerable':
      return `Undo ${state.restored} change${state.restored === 1 ? '' : 's'}`;
    case 'expired':
      return 'This can no longer be undone';
    case 'superseded':
      return `Object ${state.objectId} was changed since`;
    case 'spent':
      return 'Already undone';
    case 'refused':
      return state.reason;
    case 'failed':
      return `Undo failed: ${state.reason}`;
  }
}
