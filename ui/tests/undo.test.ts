import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import {
  classifyUndo,
  canUndo,
  describe as describeUndo,
  NO_UNDO,
  type UndoOutcome,
} from '../src/lib/api/undo.js';

describe('undo outcome classification', () => {
  test('a successful undo is offerable and carries the count', () => {
    const s = classifyUndo({ kind: 'ok', restored: 12 });
    assert.equal(s.state, 'offerable');
    assert.equal(canUndo(s), true);
    if (s.state === 'offerable') assert.equal(s.restored, 12);
  });

  test('an expired record is not offerable', () => {
    const s = classifyUndo({ kind: 'expired' });
    assert.equal(s.state, 'expired');
    assert.equal(canUndo(s), false);
  });

  test('a consumed record is spent, not offerable again', () => {
    const s = classifyUndo({ kind: 'already-undone' });
    assert.equal(s.state, 'spent');
    // The one that matters: a second click must be impossible, or the user
    // clicks six times.
    assert.equal(canUndo(s), false);
  });

  test('a superseded undo names the object that moved', () => {
    const s = classifyUndo({ kind: 'superseded', objectId: 'o-7f3a' });
    assert.equal(s.state, 'superseded');
    if (s.state === 'superseded') assert.equal(s.objectId, 'o-7f3a');
    assert.equal(canUndo(s), false);
  });

  test('another caller\'s record and a missing one read the same', () => {
    // Not a coincidence: a toast that distinguishes them is an oracle for
    // guessing whether a record id exists.
    const theirs = classifyUndo({ kind: 'not-yours' });
    const missing = classifyUndo({ kind: 'no-such-record' });
    assert.equal(describeUndo(theirs), describeUndo(missing));
  });

  test('a failed undo is never offerable', () => {
    const s = classifyUndo({ kind: 'failed', reason: 'connection reset' });
    assert.equal(s.state, 'failed');
    assert.equal(canUndo(s), false);
  });

  test('only offerable carries a button', () => {
    // Exhaustive over the six variants, so adding a seventh without deciding
    // this fails to compile rather than rendering a button nobody wanted.
    const outcomes: UndoOutcome[] = [
      { kind: 'ok', restored: 1 },
      { kind: 'expired' },
      { kind: 'already-undone' },
      { kind: 'superseded', objectId: 'x' },
      { kind: 'not-yours' },
      { kind: 'no-such-record' },
      { kind: 'failed', reason: 'x' },
    ];
    const withButton = outcomes.filter((o) => canUndo(classifyUndo(o)));
    assert.equal(withButton.length, 1);
    assert.equal(withButton[0].kind, 'ok');
  });
});

describe('undo toast wording', () => {
  test('the count is singular for one change and plural for many', () => {
    assert.equal(
      describeUndo(classifyUndo({ kind: 'ok', restored: 1 })),
      'Undo 1 change'
    );
    assert.equal(
      describeUndo(classifyUndo({ kind: 'ok', restored: 12 })),
      'Undo 12 changes'
    );
  });

  test('a superseded undo names the object rather than failing vaguely', () => {
    // "Undo failed" tells the user nothing they can act on; the object id is
    // what lets them go and look at it.
    const line = describeUndo(classifyUndo({ kind: 'superseded', objectId: 'o-7f3a' }));
    assert.match(line, /o-7f3a/);
    assert.doesNotMatch(line, /failed/i);
  });

  test('a failure says so and keeps the reason', () => {
    const line = describeUndo(classifyUndo({ kind: 'failed', reason: 'timeout' }));
    assert.match(line, /timeout/);
  });

  test('the resting state has no button', () => {
    assert.equal(canUndo(NO_UNDO), false);
  });
});
