/**
 * The two create actions, and the reporting the data layer's `CreateOutcome`
 * makes possible.
 *
 * Plan T-P5-006 item 10; spec `docs/spec/t-p5-006-create.md` §10.10.
 *
 * # Why this is a pure module and not part of the component
 *
 * `create-all-missing` has one number that decides the whole UX — how many of
 * the rows the user pasted were *already in their library* — and that number
 * comes back from the server. Everything else about the action is arithmetic on
 * it. Putting the arithmetic in a `.svelte` file would make it untestable
 * without a DOM, and the arithmetic is where the interesting cases live.
 *
 * # The claim this module exists to keep
 *
 * "You pasted 40 rows and 35 were already there" and "You created 40 objects"
 * are different sentences, and a UI that renders one number has silently chosen
 * between them. §10.10's action exists *because* "already there" is a common
 * outcome, so collapsing it into the success count tells the user their import
 * did nothing — when in fact it did 5 things.
 *
 * So the two counts are kept apart all the way to the rendered string, and the
 * string itself is asserted on. A component that shows a single "40" is a
 * claim, and the test is what keeps it honest.
 */

/** The three counts `CreateOutcome` reports, plus the ids it derived. */
export interface CreateOutcome {
  /** Rows that did not exist and were written. */
  readonly created: number;
  /** Drafts whose id was already in the library. Not an error. */
  readonly existing: number;
  /**
   * Drafts the caller was not allowed to create.
   *
   * Always 0 in a local single-user build. Present because the field is on the
   * wire, and a client that drops a field the server sends will eventually
   * render `undefined` as a count.
   */
  readonly refused: number;
}

/** The four states a create can be in once it has been asked for. */
export type CreateState = 'idle' | 'running' | 'done' | 'partial' | 'blocked' | 'failed';

/** One create request and everything the UI needs to render it. */
export interface CreateResult {
  readonly state: CreateState;
  readonly outcome?: CreateOutcome;
  /** Set only when `state === 'failed'`. */
  readonly error?: string;
}

/** Nothing asked for yet. */
export const IDLE: CreateResult = Object.freeze({ state: 'idle' });

/** No outcome yet, so every count reads as unknown rather than as zero. */
export function running(previous?: CreateResult): CreateResult {
  // The previous outcome is deliberately DROPPED. Keeping it would let the
  // modal keep showing the last run's counts under a spinner, and a user who
  // has just pressed the button twice reads that as one run having done
  // everything. The counts belong to a finished run.
  void previous;
  return { state: 'running' };
}

/**
 * Classify a server result.
 *
 * The split that earns its cost is `blocked` against `partial`. Both have
 * `created === 0` in one direction or the other, both would render as "nothing
 * happened" if collapsed, and collapsing them tells a user whose rows were all
 * refused that their paste was empty. `partial` is not a failure either: a
 * create that refused only what the caller had no right to write is a correct
 * write, and rendering it as a warning teaches users that import is unreliable.
 *
 * - `done` — at least one created, nothing refused.
 * - `partial` — at least one created AND at least one refused.
 * - `blocked` — nothing created and at least one refused.
 * - `failed` — an error, not an outcome.
 *
 * A result with nothing created, nothing refused and something already there is
 * `done`, not `blocked`: the action succeeded at the only thing it was asked to
 * do, which was not duplicate anything.
 */
export function classify(outcome: CreateOutcome): CreateState {
  if (outcome.refused > 0) {
    return outcome.created > 0 ? 'partial' : 'blocked';
  }
  return 'done';
}

/**
 * The one-line summary of a finished run.
 *
 * Returns `''` for `running` and `failed` — those render a spinner and an error
 * respectively, and a summary line next to either is a second, competing
 * statement about the same run.
 */
export function summary(result: CreateResult): string {
  if (result.state === 'idle' || result.state === 'running' || result.state === 'failed') {
    return '';
  }
  const { created, existing, refused } = result.outcome ?? { created: 0, existing: 0, refused: 0 };

  if (result.state === 'blocked') {
    return refused === 1 ? '1 row was refused.' : `${refused} rows were refused.`;
  }
  if (refused > 0) {
    // The refused count is not dropped here. A partial run that renders only
    // its successes tells the user everything went well.
    return `${plural(created, 'row')} created, ${plural(refused, 'row')} refused.`;
  }
  if (existing === 0) {
    return `${plural(created, 'row')} created.`;
  }
  if (created === 0) {
    // The all-duplicates case, and the one most worth naming. "Created 0" reads
    // as a failure; "40 rows were already in your library" is the truth and is
    // the whole reason the action reports `existing` separately.
    return existing === 1
      ? 'That row was already in your library.'
      : `${existing} rows were already in your library.`;
  }
  return (
    `${plural(created, 'row')} created, ` +
    `${existing === 1 ? '1 was' : `${existing} were`} already in your library.`
  );
}

/** A second line naming what the run will not do, or `''` when there is nothing to say. */
export function detail(result: CreateResult): string {
  if (result.state === 'blocked') {
    return 'Nothing was written. You do not have permission to create these.';
  }
  if (result.state === 'partial') {
    const { refused } = result.outcome ?? { refused: 0 };
    return `${plural(refused, 'row')} were not created because this library already has them.`;
  }
  const { existing } = result.outcome ?? { existing: 0 };
  if (result.state === 'done' && existing > 0) {
    return 'Existing items were left untouched, so running this again changes nothing.';
  }
  return '';
}

/**
 * Whether the confirm button may be pressed.
 *
 * False while running — a second press would start a second import against the
 * same selection — and false with nothing to create, because a button that can
 * be pressed to do nothing is how a user learns to stop reading buttons.
 */
export function canApply(result: CreateResult, draftCount: number): boolean {
  if (result.state === 'running') return false;
  return draftCount > 0;
}

/** The redundant rows in a pasted list, as the number a user cares about. */
export function redundantCount(draftCount: number, distinctIds: number): number {
  if (distinctIds > draftCount) return 0;
  return draftCount - distinctIds;
}

/**
 * What the confirm button says, before the press.
 *
 * "Create 40" when 40 are new, "Create 5" when 35 already exist. The button
 * names the number that will be *created*, because that is the one the click
 * causes — a button reading "Process 40" when 35 will be skipped is the kind of
 * small overstatement that makes people stop believing a UI's numbers.
 */
export function applyLabel(draftCount: number, distinctIds: number): string {
  const willCreate = Math.min(draftCount, distinctIds);
  return willCreate === 1 ? 'Create 1' : `Create ${willCreate}`;
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}
