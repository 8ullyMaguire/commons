/*
  Tests for the bulk result model. Spec 10.7, plan T-P5-006.

  # What is being tested here

  Not arithmetic. Every function in `bulk.ts` is a few lines of comparison; the
  tests exist for the *boundaries*, where the interesting decisions are:

   - `applied === 0` splits into two states that must never both read as
     "nothing happened" (§ the module docs: 'blocked' versus 'empty');
   - a count that disagrees with the request is the case `prune` reports, and
     classifying it as 'done' hides a lost row;
   - the singular/plural split in every user-facing string, because "Tagged 1
     objects" is the kind of thing that ships.

  A test that asserts `classify({applied: 3, ...})` is 'done' is a test of the
  implementation. The tests below are each about a decision, and each would
  still fail if the function were rewritten to be more or less clever.
 */

import { strict as assert } from 'node:assert';
import { describe, it } from 'node:test';

import {
  IDLE,
  bulkTarget,
  canApply,
  classify,
  resultMessage,
  scopeMessage,
  type BulkOutcome
} from '../src/lib/api/bulk.js';
import { selectAll, setPicked, type Selection } from '../src/lib/api/selection.js';

/** An outcome, with only the fields a test cares about. */
const out = (
  applied: number,
  skipped_invisible = 0,
  requested = applied
): BulkOutcome => ({ applied, skipped_invisible, requested });

describe('classify: applied and hidden together', () => {
  it('is done when everything the caller named was written', () => {
    assert.equal(classify(out(3)).state, 'done');
  });

  it('is partial, not done, when some were hidden', () => {
    // The distinction the whole module exists for: a real write happened, and
    // something the user selected was not touched. Collapsing these into 'done'
    // tells a user their selection was fully applied when it was not.
    assert.equal(classify(out(3, 2, 5)).state, 'partial');
  });

  it('is partial with one hidden as well as with many', () => {
    // A fixture with one of each thing cannot test a threshold. If `skipped >
    // 1` were the rule, this would pass as 'done' and the bug would ship.
    assert.equal(classify(out(4, 1, 5)).state, 'partial');
  });
});

describe('classify: nothing was written', () => {
  it('is blocked when everything named was invisible, not empty', () => {
    // The most valuable assertion in this file. Both have applied === 0, and a
    // UI that collapses them reports a permissions problem as an empty library.
    assert.equal(classify(out(0, 3, 3)).state, 'blocked');
  });

  it('is empty when nothing matched and nothing was hidden', () => {
    assert.equal(classify(out(0, 0, 0)).state, 'empty');
  });

  it('distinguishes the two by the hidden count alone', () => {
    // requested > 0 with no hidden row means the ids are simply not there any
    // more -- a stale selection, which is a different message again.
    assert.equal(classify(out(0, 0, 5)).state, 'empty');
  });
});

describe('classify: normalises what it is given', () => {
  it('never reports a negative count', () => {
    // A server that underflows, or a subtraction done twice on the client,
    // must not render as "-3 objects".
    const r = classify({ applied: -1, skipped_invisible: -2, requested: -3 });
    assert.equal(r.applied, 0);
    assert.equal(r.skippedInvisible, 0);
    assert.equal(r.requested, 0);
  });

  it('keeps the numbers it was given for a normal outcome', () => {
    assert.deepEqual(classify(out(3, 2, 5)), {
      state: 'partial',
      applied: 3,
      skippedInvisible: 2,
      requested: 5
    });
  });
});

describe('resultMessage: what the user is told', () => {
  it('says nothing before anything has happened', () => {
    assert.equal(resultMessage(IDLE), null);
  });

  it('says nothing for an empty result, because the scope line already did', () => {
    assert.equal(resultMessage(classify(out(0, 0, 0))), null);
  });

  it('reports a partial result as a result, not a failure', () => {
    const msg = resultMessage(classify(out(3, 2, 5)));
    assert.ok(msg, 'a partial write must say something');
    assert.match(msg!, /Tagged 3 objects/);
    assert.match(msg!, /not visible to you/);
    // "left alone" rather than "failed": nothing failed.
    assert.match(msg!, /left alone/);
    assert.doesNotMatch(msg!, /fail|error|could not/i);
  });

  it('explains a blocked result, because silence would be a lie', () => {
    const msg = resultMessage(classify(out(0, 3, 3)));
    assert.ok(msg, 'a fully-blocked write must explain itself');
    assert.match(msg!, /not visible to you/);
    assert.match(msg!, /nothing was changed/);
  });
});

describe('resultMessage: singular and plural', () => {
  it('uses the singular for exactly one', () => {
    assert.equal(resultMessage(classify(out(1))), 'Tagged 1 object.');
    assert.match(resultMessage(classify(out(1, 1, 2)))!, /Tagged 1 object\./);
  });

  it('does not say "1 objects" or "2 object"', () => {
    // A fixture with only large numbers never reaches either branch.
    assert.doesNotMatch(resultMessage(classify(out(1)))!, /objects/);
    assert.doesNotMatch(resultMessage(classify(out(2)))!, /2 object\./);
    assert.doesNotMatch(resultMessage(classify(out(2)))!, /2 objects is/);
  });

  it('agrees with itself in the blocked case', () => {
    assert.match(resultMessage(classify(out(0, 1, 1)))!, /The one object/);
    assert.match(resultMessage(classify(out(0, 2, 2)))!, /All 2 objects/);
  });
});

describe('canApply: the button follows the server, not the client', () => {
  it('is enabled only when the write would reach something', () => {
    assert.equal(canApply(classify(out(1))), true);
    assert.equal(canApply(classify(out(3, 2, 5))), true);
    assert.equal(canApply(classify(out(0, 3, 3))), false);
    assert.equal(canApply(classify(out(0, 0, 0))), false);
  });

  it('is disabled while the count is still unknown', () => {
    // Optimistic enabling followed by 'nothing matched' is a dead end; a
    // moment of a disabled button is not.
    assert.equal(canApply(IDLE), false);
  });
});

describe('scopeMessage: what the user is told before writing', () => {
  it('names a select-all as a search rather than a count of loaded rows', () => {
    const sel = selectAll({ term: 'beach' } as never);
    const msg = scopeMessage(sel, classify(out(4000)), 'unknown');
    assert.match(msg, /Everything matching this search/);
    // "about", because the client has no honest count for a select-all.
    assert.match(msg, /about 4000 objects/);
  });

  it('says currently nothing for a select-all that matches nothing', () => {
    const sel = selectAll({ term: 'zzz' } as never);
    assert.match(scopeMessage(sel, classify(out(0)), 'unknown'), /currently nothing/);
  });

  it('reports a hand-picked count', () => {
    const sel = setPicked(['a', 'b', 'c']);
    assert.equal(scopeMessage(sel, classify(out(3)), 3), '3 objects selected.');
    assert.equal(scopeMessage(sel, classify(out(1)), 1), '1 object selected.');
  });

  it('says so when nothing is selected', () => {
    assert.equal(scopeMessage({}, classify(out(0)), 0), 'No objects selected.');
  });

  it('falls back to the server count when the client has none', () => {
    // A hand-picked set always has a client count, so 'unknown' here means the
    // caller's page state is gone; the server's number is the only one left.
    const sel = setPicked(['a', 'b', 'c', 'd']);
    assert.match(scopeMessage(sel, classify(out(4)), 'unknown'), /4 objects selected/);
  });
});

describe('bulkTarget: the two shapes stay two shapes', () => {
  it('sends ids for a hand-picked set and no query', () => {
    const req = bulkTarget(setPicked(['a', 'b']), 'F');
    assert.deepEqual(req, { kind: 'IDS', ids: ['a', 'b'], excluded: [] });
    // `kind`, not an inferred shape: "neither given" and "both given" are then
    // states the server's schema rejects rather than states it must notice.
    assert.equal((req as { query?: unknown }).query, undefined);
  });

  it('sends the query and the exclusions for a select-all', () => {
    const sel: Selection = {
      ...selectAll({ term: 'beach' } as never),
      excluded: new Set(['x'])
    };
    const req = bulkTarget(sel, 'tag-1', 'bulk-edit');
    assert.equal('ids' in req, false, 'a select-all must not be degraded to ids');
    assert.deepEqual(req.excluded, ['x']);
  });

  it('always sends an exclusions array, even when empty', () => {
    // An absent field and an empty one are the same thing to the server, but
    // sending it always means the wire shape does not vary with the selection's
    // internal state.
    const req = bulkTarget(selectAll({} as never), 'tag-1', 'bulk-edit');
    assert.deepEqual(req.excluded, []);
  });

  it('sends an empty id list rather than omitting it', () => {
    // The client-side twin of `1 = 0` in bulk.rs: "nothing selected" has to
    // arrive as "nothing", never as an absent field the server might read as
    // "everything".
    const req = bulkTarget({}, 'tag-1', 'bulk-edit');
    assert.deepEqual(req.ids, []);
    assert.equal('query' in req, false);
  });
});
