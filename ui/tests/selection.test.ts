/*
  The selection model. Spec 10.6/10.7, plan T-P5-006.

  # The rules that are wrong in non-obvious ways

  Four of these tests exist because the obvious implementation is wrong:

    - deselecting one row of a select-all must survive (the XOR, not a union);
    - a select-all with no count from the server must not report the loaded
      count (it must report 'unknown');
    - a select-all does not survive a change of result;
    - a saved edit is not an unsaved edit.

  Each is a data-loss bug wearing a friendly label, and each is one line away
  from being right.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  clear,
  emptySelection,
  isSelected,
  isSelectAll,
  isUnsaved,
  prune,
  selectAll,
  selectedCount,
  setPicked,
  toggle,
  type Selection
} from '../src/lib/api/selection.js';

const ids = (...xs: string[]) => xs;
const set = (...xs: string[]) => new Set(xs);

describe('identity, not position', () => {
  test('a selection is a set of ids, and paging does not change it', () => {
    // The ordinary bug: select three rows, page, and the third selected row is
    // now row 0 of the new page. Storing ids is the whole fix, and this test
    // states what must not happen rather than what must.
    const sel = setPicked(ids('obj-1', 'obj-2', 'obj-3'));
    const nextPage = set('obj-4', 'obj-5', 'obj-6'); // the new loaded page

    for (const id of ['obj-1', 'obj-2', 'obj-3']) {
      assert.equal(isSelected(sel, id, () => false), true, `${id} survives paging`);
    }
    for (const id of nextPage) {
      assert.equal(isSelected(sel, id, () => false), false, `${id} was never selected`);
    }
  });

  test('an empty selection is a real value, not a special case', () => {
    const sel = emptySelection();
    assert.equal(isSelectAll(sel), false);
    assert.equal(isSelected(sel, 'obj-1', () => false), false);
    assert.equal(selectedCount(sel, ['obj-1', 'obj-2']), 0);
    assert.deepEqual(clear(), {});
  });
});

describe('select-all is a mode, not an operation', () => {
  test('select-all holds a query rather than a set of ids', () => {
    // A Set<id> cannot express "all 4,000": enumerating them is a page of state
    // the size of the library, and a select-all that is secretly only the
    // loaded page is data loss.
    const sel = selectAll({ filter: 'beach' });
    assert.equal(isSelectAll(sel), true);
    assert.equal(sel.query?.filter, 'beach');
  });

  test('a select-all covers what the query matches, by the caller\'s answer', () => {
    const sel = selectAll({ filter: 'beach' });
    // The server decides what matches, not this module. So the seam is the
    // predicate, and a selection that disagreed with the query would be a
    // bulk edit against a different set than the user saw.
    const matches = (id: string) => id === 'obj-1' || id === 'obj-9';
    assert.equal(isSelected(sel, 'obj-1', matches), true);
    assert.equal(isSelected(sel, 'obj-9', matches), true);
    assert.equal(isSelected(sel, 'obj-2', matches), false);
  });
});

describe('deselecting inside a select-all', () => {
  test('one row turned off, and it stays off', () => {
    // The XOR. A union of picked and excluded is the obvious implementation and
    // it is wrong: it makes a deselection a no-op that the next page load
    // silently undoes.
    let sel: Selection = selectAll({ filter: 'beach' });
    const matches = () => true;

    assert.equal(isSelected(sel, 'obj-1', matches), true);
    sel = toggle(sel, 'obj-1');
    assert.equal(isSelected(sel, 'obj-1', matches), false, 'turned off');

    // And it is still off after a page load re-renders from the same value.
    const reloaded: Selection = sel;
    assert.equal(isSelected(reloaded, 'obj-1', matches), false);
  });

  test('turning it back on removes the exclusion rather than adding a pick', () => {
    let sel = toggle(selectAll({ filter: 'beach' }), 'obj-1');
    sel = toggle(sel, 'obj-1');
    assert.equal(isSelected(sel, 'obj-1', () => true), true);
    assert.equal(sel.excluded?.size, 0, 'no exclusion is left behind to resurface');
  });

  test('exclusion beats a match, always', () => {
    const sel = toggle(selectAll({ filter: 'beach' }), 'obj-1');
    // Even a predicate that says yes: the user's last word was "not this one".
    assert.equal(isSelected(sel, 'obj-1', () => true), false);
  });

  test('a hand-picked id outside the query is not in the selection', () => {
    // The union bug, at the only input where it differs. A `query` selection is
    // exactly what the query matches: a `picked` id the query does not match is
    // *not* selected, or a select-all would quietly include rows the user never
    // saw -- and never saw the count for either.
    //
    // This is the case that distinguishes the query branch from falling through
    // to the picked check, which is the shape a "just union them" fix takes.
    const sel: Selection = { query: { filter: 'beach' }, picked: set('obj-x') };
    assert.equal(isSelected(sel, 'obj-x', () => false), false);
    // And it stays out even though the same id is a legitimate pick with no
    // query at all, so this is about the mode, not the id.
    assert.equal(isSelected({ picked: set('obj-x') }, 'obj-x', () => false), true);
  });

  test('an exclusion does not select a row the query does not match', () => {
    // Excluding something that was never included is a no-op, not a pick. A
    // union would make `excluded` into a second `picked`.
    const sel: Selection = { query: { filter: 'beach' }, excluded: set('obj-x') };
    assert.equal(isSelected(sel, 'obj-x', () => false), false);
  });
});

describe('counting', () => {
  test('a hand-picked set counts exactly, against the loaded rows', () => {
    const sel = setPicked(ids('a', 'c'));
    assert.equal(selectedCount(sel, ['a', 'b', 'c']), 2);
  });

  test("a select-all with no count from the server says 'unknown'", () => {
    // The bug this branch exists to prevent: reporting the loaded count as a
    // total, so a user bulk-deletes 40 of 4,000 because the UI said 40.
    const sel = selectAll({ filter: 'beach' });
    assert.equal(selectedCount(sel, ['a', 'b', 'c']), 'unknown');
  });

  test("a select-all uses the server's count, minus the exclusions", () => {
    let sel = selectAll({ filter: 'beach' });
    assert.equal(selectedCount(sel, ['a', 'b', 'c'], 4000), 4000);

    sel = toggle(sel, 'a');
    sel = toggle(sel, 'b');
    assert.equal(selectedCount(sel, ['a', 'b', 'c'], 4000), 3998);
  });

  test('a server count is ignored once there is no select-all', () => {
    // The server answered about a select-all that has since been cleared. Its
    // number now describes something the user is not selecting.
    const sel = setPicked(ids('a'));
    assert.equal(selectedCount(sel, ['a', 'b', 'c'], 4000), 1);
  });

  test('excluding everything from a select-all is zero, not negative', () => {
    const sel: Selection = { query: { filter: 'x' }, excluded: set('a', 'b', 'c') };
    assert.equal(selectedCount(sel, ['a', 'b', 'c'], 2), 0);
  });
});

describe('a change of result', () => {
  test('a select-all does not survive, and is not reinterpreted', () => {
    // The rows it named were described by the old query. "Everything" under a
    // new filter is a selection the user never made.
    const { selection, dropped } = prune(selectAll({ filter: 'old' }), set('a', 'b'));
    assert.equal(isSelectAll(selection), false);
    assert.deepEqual(selection.picked, undefined);
    assert.equal(dropped, 0);
  });

  test('picked ids still present are kept, and the ones that vanished are counted', () => {
    const sel = setPicked(ids('a', 'b', 'c', 'd'));
    const { selection, dropped } = prune(sel, set('a', 'c', 'e'));
    assert.deepEqual([...(selection.picked ?? [])].sort(), ['a', 'c']);
    assert.equal(dropped, 2, 'b and d were not in the new result');
  });

  test('nothing dropped means the selection is returned unchanged', () => {
    // Identity, so a `$derived` on it does not re-render the world on every
    // page load.
    const sel = setPicked(ids('a', 'b'));
    const { selection, dropped } = prune(sel, set('a', 'b', 'c'));
    assert.equal(selection, sel);
    assert.equal(dropped, 0);
  });

  test('a selection with nothing picked prunes to nothing and reports zero', () => {
    const { selection, dropped } = prune(emptySelection(), set('a'));
    assert.equal(dropped, 0);
    assert.equal(isSelected(selection, 'a', () => false), false);
  });
});

describe('the dirty rule', () => {
  test('a saved edit is not an unsaved edit', () => {
    // The narrowness is the point. A guard that cannot tell blocks a user who
    // has already saved.
    assert.equal(isUnsaved(0, false), false);
  });

  test('a pending edit is unsaved', () => {
    assert.equal(isUnsaved(1, false), true);
  });

  test('a failed save is unsaved even with nothing pending', () => {
    // The case a counter cannot represent: the user hit save, it failed, and
    // there is no pending edit left to count. Losing their work here is the
    // worst case in the whole rule.
    assert.equal(isUnsaved(0, true), true);
  });

  test('nothing pending, nothing failed, nothing to warn about', () => {
    assert.equal(isUnsaved(0, false), false);
  });
});

describe('transitions', () => {
  test('toggling without a query writes to picked, both ways', () => {
    let sel = toggle(emptySelection(), 'a');
    assert.equal(isSelected(sel, 'a', () => false), true);
    sel = toggle(sel, 'a');
    assert.equal(isSelected(sel, 'a', () => false), false);
    assert.equal(sel.picked?.size, 0);
  });

  test('setPicked is exactly the ids given, and nothing else', () => {
    // Pinned on the whole value, not on `.picked`. `setPicked` is how a
    // selection is replaced wholesale, and a leftover `query` or `excluded`
    // from a previous mode would be invisible in a `.picked`-only assertion
    // while silently changing what every later `isSelected` returns.
    assert.deepEqual(setPicked(ids('a', 'b')), { picked: set('a', 'b') });
  });

  test('setPicked called twice yields the second set, not the union', () => {
    // A "merge" here would be invisible in any single call: the function takes
    // no selection, so the only way to merge is to read one from somewhere.
    // Asserting the sequence is what makes that implementation wrong.
    const first = setPicked(ids('a', 'b'));
    const second = setPicked(ids('c'));
    assert.deepEqual([...(first.picked ?? [])].sort(), ['a', 'b']);
    assert.deepEqual([...(second.picked ?? [])], ['c']);
    assert.equal(second.query, undefined);
  });

  test('selecting all discards a hand-picked set', () => {
    // Otherwise the two modes accumulate and a later deselect cannot be
    // reasoned about from the value alone.
    const sel = selectAll({ filter: 'x' });
    assert.equal(sel.picked, undefined);
  });

  test('a transition does not mutate the selection it was given', () => {
    // `Selection` is a value; the components hold it in `$state` and a
    // mutation would not notify anything.
    const sel = setPicked(ids('a'));
    toggle(sel, 'b');
    assert.deepEqual([...(sel.picked ?? [])], ['a']);
  });
});
