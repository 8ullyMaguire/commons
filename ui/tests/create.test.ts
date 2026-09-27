/*
  Tests for the create result model. Spec 10.10, plan T-P5-006 item 10.

  # What is being tested here

  The claim the module exists to keep: "you pasted 40 rows and 35 were already
  in your library" and "you created 40 objects" are DIFFERENT sentences, and a
  UI that renders one number has silently chosen between them.

  So the tests are about the boundaries where that choice is visible:

   - the all-duplicates case, where a single success count renders as `Created 0`
     and reads as a failure;
   - `blocked` against `done`, because both have nothing created and both would
     render as "nothing happened" if collapsed;
   - `partial`, which is a correct write of the rows the caller had a right to
     write, and must not render as a warning;
   - the singular/plural split in every string, because "Created 1 rows" ships.

  A test asserting `classify({created: 3, ...}) === 'done'` is a test of the
  implementation. Each test below is about a decision, and would still fail if
  the function were rewritten to be more or less clever.
 */

import { strict as assert } from 'node:assert';
import { describe, it } from 'node:test';

import {
  IDLE,
  applyLabel,
  canApply,
  classify,
  detail,
  redundantCount,
  running,
  summary,
  type CreateOutcome,
  type CreateResult
} from '../src/lib/api/create.js';

/** An outcome, with only the fields a test cares about. */
const out = (
  created: number,
  existing = 0,
  refused = 0
): CreateOutcome => ({ created, existing, refused });

/** A finished result, so the string tests read as one line each. */
const done = (created: number, existing = 0, refused = 0): CreateResult => ({
  state: classify(out(created, existing, refused)),
  outcome: out(created, existing, refused)
});

describe('classify', () => {
  it('is done when something was created and nothing refused', () => {
    assert.equal(classify(out(5)), 'done');
  });

  it('is done, not blocked, when everything already existed', () => {
    // The case the whole `existing` count exists for. "Blocked" would tell a
    // user with a full library that they are not allowed to create anything.
    assert.equal(classify(out(0, 40)), 'done');
  });

  it('is blocked when nothing was created and something was refused', () => {
    assert.equal(classify(out(0, 0, 3)), 'blocked');
  });

  it('is partial when some were created and some refused', () => {
    // A partial run is the correct behaviour of a correct write, not a failure.
    assert.equal(classify(out(4, 0, 2)), 'partial');
  });

  it('is done when it was asked to do nothing at all', () => {
    assert.equal(classify(out(0, 0, 0)), 'done');
  });
});

describe('summary', () => {
  it('says what was created when there was nothing to skip', () => {
    assert.equal(summary(done(5)), '5 rows created.');
  });

  it('uses the singular for one row', () => {
    assert.equal(summary(done(1)), '1 row created.');
  });

  // The claim of the module, as an exact string.
  it('names the already-there count instead of rendering a success', () => {
    assert.equal(summary(done(0, 40)), '40 rows were already in your library.');
    assert.equal(summary(done(1, 40)), '1 row created, 40 were already in your library.');
  });

  it('does not say "created 0" for an all-duplicates run', () => {
    // The specific failure: a single count renders this as a failure.
    assert.ok(!summary(done(0, 12)).includes('0'));
  });

  it('keeps the refused count in a partial run', () => {
    // A partial that renders only its successes tells the user all was well.
    assert.equal(summary(done(4, 0, 2)), '4 rows created, 2 rows refused.');
  });

  it('reports a blocked run as refused, not as created', () => {
    assert.equal(summary(done(0, 0, 3)), '3 rows were refused.');
    assert.equal(summary(done(0, 0, 1)), '1 row was refused.');
  });

  it('says nothing while running, because the spinner already says it', () => {
    assert.equal(summary(running()), '');
  });

  it('says nothing on failure, so the error is the only statement', () => {
    // Two competing sentences about one run is how a user reads the wrong one.
    assert.equal(summary({ state: 'failed', error: 'boom' }), '');
  });

  it('says nothing before anything was asked', () => {
    assert.equal(summary(IDLE), '');
  });
});

describe('detail', () => {
  it('explains that a blocked run wrote nothing', () => {
    assert.match(detail(done(0, 0, 2)), /Nothing was written/);
  });

  it('tells a partial run why the rest was not created', () => {
    assert.match(detail(done(4, 0, 2)), /2 rows were not created/);
  });

  it('tells a done-with-skips run that it is safe to repeat', () => {
    // The action is idempotent, and saying so is what stops a user from
    // worrying about whether running it twice duplicates anything.
    assert.match(detail(done(1, 40)), /running this again changes nothing/);
  });

  it('has nothing to say about a clean run', () => {
    assert.equal(detail(done(5)), '');
  });
});

describe('canApply', () => {
  it('is false while running, so a second press cannot start a second import', () => {
    assert.equal(canApply(running(), 40), false);
  });

  it('is false with nothing to create', () => {
    // A button that can be pressed to do nothing is how a user stops reading
    // buttons.
    assert.equal(canApply(IDLE, 0), false);
  });

  it('is true with rows to create', () => {
    assert.equal(canApply(IDLE, 1), true);
  });

  it('is false while running even when there are rows', () => {
    // The case a `return draftCount > 0` alone would get wrong.
    assert.equal(canApply({ state: 'done', outcome: out(5) }, 5), true);
    assert.equal(canApply(running(), 5), false);
  });
});

describe('redundantCount', () => {
  it('is zero when every row is distinct', () => {
    assert.equal(redundantCount(40, 40), 0);
  });

  it('is the number of pasted duplicates', () => {
    assert.equal(redundantCount(40, 5), 35);
  });

  it('is zero rather than negative if the counts disagree', () => {
    // `distinctIds` comes from the server and a stale one can exceed the local
    // draft count. A negative "duplicates" is worse than a zero.
    assert.equal(redundantCount(5, 40), 0);
  });
});

describe('applyLabel', () => {
  // The button names what the click WILL create, not what was pasted.
  it('names what will be created, not what was pasted', () => {
    assert.equal(applyLabel(40, 5), 'Create 5');
  });

  it('names the pasted count when nothing already exists', () => {
    assert.equal(applyLabel(40, 40), 'Create 40');
  });

  it('uses the singular for one', () => {
    assert.equal(applyLabel(1, 1), 'Create 1');
  });

  it('never names more than was pasted', () => {
    assert.equal(applyLabel(5, 40), 'Create 5');
  });
});
