/*
  Tests for the unsaved guard's rules. Spec 10.7, 10.10; plan T-P5-006 item 4.

  # The four cases the spec names, and which of them a naive guard fails

  1. Prompt on navigation with a dirty registry. The easy one.
  2. Cancelling changes *nothing* -- the edit is still pending, the registry is
     untouched, the URL has not moved. The naive guard resolves false and then
     clears state, which discards the edit and leaves the user where they
     were: the worst of both outcomes.
  3. Do NOT prompt when nothing is unsaved. A guard that always fires is a guard
     users learn to dismiss unread, and then it protects nothing on the one
     navigation that mattered.
  4. A failed save blocks with nothing pending. The case a counter cannot
     represent at all.

  Plus the two that only exist because the registry is a set of surfaces:
  double-registration, and prompt determinism.
 */

import { strict as assert } from 'node:assert';
import { describe, it } from 'node:test';

import {
  clear,
  consume,
  decide,
  dirtyCount,
  emptyRegistry,
  isDirty,
  register,
  shouldPrompt,
  split,
  summary,
  type Registry
} from '../src/lib/api/guard.js';

const pending = (id: string) => ({ id, kind: 'pending' }) as const;
const failed = (id: string) => ({ id, kind: 'failed' }) as const;

/** A registry with the given surfaces in it. */
const reg = (...surfaces: Parameters<typeof register>[1][]): Registry =>
  surfaces.reduce((r, s) => register(r, s), emptyRegistry());

describe('the clean case', () => {
  it('is not dirty', () => {
    assert.equal(isDirty(emptyRegistry()), false);
  });

  it('does not prompt on navigation', () => {
    // Acceptance case 3. A guard that fires on every navigation is a guard the
    // user learns to dismiss without reading, and then it protects nothing on
    // the navigation that mattered.
    assert.equal(decide(emptyRegistry(), 'navigation'), 'proceed');
  });

  it('has nothing to say', () => {
    // `null`, not "nothing is at risk": the layout does not prompt at all when
    // this is null, so a caller rendering it unconditionally would show a user
    // who is merely navigating a message about risk that does not exist.
    assert.equal(summary(emptyRegistry()), null);
  });

  it('is not dirty once a surface is cleared', () => {
    const r = clear(reg(pending('a')), 'a');
    assert.equal(isDirty(r), false);
  });

  it('ignores a clear for a surface it never had', () => {
    // Returns the same reference, so a no-op clear cannot churn a derived value
    // in the layout and re-run the guard for nothing.
    const r = reg(pending('a'));
    assert.equal(clear(r, 'nope'), r);
  });
});

describe('the dirty case', () => {
  it('prompts on navigation', () => {
    // Acceptance case 1 -- the plan's own floor.
    assert.equal(decide(reg(pending('a')), 'navigation'), 'confirm');
  });

  it('prompts on unload too', () => {
    // A tab close is not catchable by a router at all, and `beforeunload` is
    // the only thing that sees it.
    assert.equal(decide(reg(pending('a')), 'unload'), 'confirm');
  });

  it('counts one surface as one', () => {
    assert.equal(dirtyCount(reg(pending('a'))), 1);
    assert.equal(dirtyCount(reg(pending('a'), pending('b'))), 2);
  });

  it('treats a surface with no label and no count as fully dirty', () => {
    // The "#6466 intermittently" shape. An earlier version carried a
    // per-surface `count` and read 0 as clean, so a view-level "this is dirty"
    // flag came back clean and the guard fired on some navigations and not
    // others. The field is gone; this asserts the rule that replaced it -- a
    // surface that registered at all is dirty, and the prompt carries its id.
    const r = register(emptyRegistry(), { id: 'flag', kind: 'pending' });
    assert.equal(isDirty(r), true);
    assert.equal(decide(r, 'navigation'), 'confirm');
    assert.equal(split(r).surfaces[0].id, 'flag');
  });
});

describe('registration is idempotent', () => {
  it('does not inflate the count when a surface re-registers', () => {
    // Keyed by id, so a re-render cannot turn one dirty surface into two. A
    // count that can be inflated reports "2 unsaved changes" when there is one,
    // and a user who has learned the count is wrong stops trusting the dialog.
    const once = reg(pending('a'));
    const twice = register(once, pending('a'));
    assert.equal(dirtyCount(twice), 1);
  });

  it('replaces the state, so a save clears a pending edit', () => {
    // The surface's own save is what clears it; the registry holds no count to
    // subtract.
    const dirty = reg(pending('a'));
    const saved = clear(dirty, 'a');
    assert.equal(isDirty(saved), false);
  });

  it('lets a kind change in place', () => {
    // A surface that was pending and then failed to save is one surface, not
    // two.
    const r = register(reg(pending('a')), failed('a'));
    assert.equal(dirtyCount(r), 1);
    assert.equal(split(r).failed, 1);
  });
});

describe('a failed save', () => {
  it('blocks with nothing pending', () => {
    // Acceptance case 4. `isUnsaved(0, true)` is the case a counter cannot
    // represent: the user hit save, it failed, and there is nothing in flight.
    const r = reg(failed('a'));
    assert.equal(split(r).pending, 0);
    assert.equal(decide(r, 'navigation'), 'confirm');
  });

  it('is named separately, and first', () => {
    // A user told only "you have unsaved changes" assumes their work is held
    // somewhere and leaves. The failed save is the one that is already gone
    // from the server's point of view.
    const msg = summary(reg(pending('a'), failed('b')))!;
    assert.match(msg, /failed to save/);
    assert.match(msg, /unsaved change/);
    assert.ok(
      msg.indexOf('failed to save') < msg.indexOf('unsaved change'),
      `the failed save must be named first: ${msg}`
    );
  });

  it('says so even when nothing is pending', () => {
    const msg = summary(reg(failed('a')))!;
    assert.match(msg, /failed to save/);
    assert.doesNotMatch(msg, /unsaved change/);
  });
});

describe('the prompt names its scope', () => {
  it('is singular for one and plural for several', () => {
    assert.match(summary(reg(pending('a')))!, /1 unsaved change\./);
    assert.match(summary(reg(pending('a'), pending('b')))!, /2 unsaved changes\./);
  });

  it('names a labelled surface', () => {
    const r = register(emptyRegistry(), {
      id: 'scene-12',
      kind: 'pending',
      label: 'Scene 12'
    });
    assert.match(summary(r)!, /Scene 12/);
  });

  it('says nothing rather than guessing when no surface is labelled', () => {
    const msg = summary(reg(pending('a'), pending('b')))!;
    assert.doesNotMatch(msg, /\(\s*\)/, `empty parenthetical: ${msg}`);
  });

  it('joins both kinds with "and"', () => {
    assert.match(summary(reg(pending('a'), failed('b')))!, / and /);
  });
});

describe('the prompt is deterministic', () => {
  it('reads the same however the surfaces registered', () => {
    // `Map` preserves insertion order, so two registries with identical
    // contents prompt differently depending on which surface mounted first --
    // and a confirm that reorders itself between identical states reads as a
    // different dialog.
    const a = reg(pending('alpha'), failed('beta'));
    const b = reg(failed('beta'), pending('alpha'));
    assert.equal(summary(a), summary(b));
    assert.deepEqual(
      split(a).surfaces.map((s) => s.id),
      split(b).surfaces.map((s) => s.id)
    );
  });
});

describe('consume: leave anyway', () => {
  it('empties the registry so the next navigation is not re-prompted', () => {
    // The difference between a guard and an inescapable dialog.
    const after = consume(reg(pending('a'), failed('b')));
    assert.equal(isDirty(after), false);
    assert.equal(decide(after, 'navigation'), 'proceed');
  });

  it('does not mutate the registry it was given', () => {
    // A guard that cleared a shared map in place would lose the state of a
    // surface that re-registers a microtask later -- and the user would lose
    // their edit with no prompt for it.
    const before = reg(pending('a'));
    const after = consume(before);
    assert.equal(isDirty(before), true, 'the original must be untouched');
    assert.notEqual(after, before);
  });
});

describe('decide', () => {
  it('never returns anything but proceed or confirm', () => {
    // A boolean would force each of the three callers to re-derive which of
    // them it has.
    for (const kind of ['navigation', 'unload'] as const) {
      assert.ok(['proceed', 'confirm'].includes(decide(emptyRegistry(), kind)));
      assert.ok(['proceed', 'confirm'].includes(decide(reg(pending('a')), kind)));
    }
  });
});

describe('shouldPrompt', () => {
  it('is the only question, and decide is its wrapper', () => {
    const r = reg(pending('a'));
    assert.equal(shouldPrompt(r, 'navigation'), decide(r, 'navigation') === 'confirm');
  });
});

// `summary` on nothing. The mutant that survived the first mutation run
// replaced the guard clause with a size check, which made `summary` answer for
// an empty registry -- and nothing asserted the answer, because every other
// test asks about a registry with something in it.
//
// Worth its own test because "nothing to say" is a real output, not an absence
// of one: the template renders the return value directly, so `null` and `''`
// are different DOM, and a caller that formatted the message would print the
// string "null" rather than staying quiet.
describe('summary with nothing at risk', () => {
  it('says nothing at all for an empty registry', () => {
    assert.equal(summary(emptyRegistry()), null);
  });

  // A surface that has been saved is one that has been *cleared*, not one
  // carrying a `clean` kind -- `register` is how a surface says it is dirty and
  // `clear` is how it says it is not, so "present but clean" is deliberately
  // not representable. This is the clean state, therefore.
  it('says nothing once the only surface has been cleared', () => {
    const r = clear(register(emptyRegistry(), pending('a')), 'a');
    assert.equal(r.size, 0);
    assert.equal(isDirty(r), false);
    assert.equal(summary(r), null);
  });
});
