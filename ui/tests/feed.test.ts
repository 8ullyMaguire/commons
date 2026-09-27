/**
 * The feed's thresholds, tested at the boundary rather than near it.
 *
 * T-P5-006 item 9, spec §10.6. A drag of 29% and a drag of 45% both "test the
 * threshold" in the sense that they produce different results, and neither can
 * tell you whether 30% is right. The tests below sit ON the number: exactly at,
 * one pixel under, one pixel over.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  COMMIT_THRESHOLD,
  PREFETCH_AHEAD,
  classifyCommit,
  isPlaying,
  nextFocus,
  preloadAttr,
  preloadFor,
} from '../src/lib/api/feed.js';

/** A viewport height in pixels, so 1% of it is 10px and the arithmetic reads. */
const H = 1000;

test('commits at exactly the threshold', () => {
  // Exactly COMMIT_THRESHOLD, not "near" it. A rule of `fraction >=
  // threshold` and a rule of `fraction > threshold` differ only here, and that
  // difference is one whole gesture for a user on a phone.
  assert.deepEqual(classifyCommit(-COMMIT_THRESHOLD * H, H), { kind: 'commit', axis: 'up' });
});

test('springs back one pixel short of the threshold', () => {
  assert.deepEqual(classifyCommit(-(COMMIT_THRESHOLD * H - 1), H), { kind: 'spring-back' });
});

test('commits one pixel over the threshold', () => {
  assert.deepEqual(classifyCommit(-(COMMIT_THRESHOLD * H + 1), H), { kind: 'commit', axis: 'up' });
});

test('treats a tap as no movement at all', () => {
  // 0 is the interesting zero, not an arbitrary one: it is what a click
  // without movement produces, and a feed that navigates on a click opens
  // items the user only meant to select.
  assert.deepEqual(classifyCommit(0, H), { kind: 'spring-back' });
});

test('a downward drag moves back, not forward', () => {
  // The inversion. dy > 0 is the finger moving down, which reveals the
  // PREVIOUS item -- the same way a list scrolls up. Getting this backwards
  // produces a feed that scrolls opposite to the finger, which users report as
  // "the swipe is inverted" and nobody as a sign error.
  assert.deepEqual(classifyCommit(COMMIT_THRESHOLD * H + 1, H), {
    kind: 'commit',
    axis: 'down',
  });
});

test('a fast flick that barely moved still springs back', () => {
  // Speed is deliberately not an argument. A 5%-of-screen flick is a tap that
  // went wrong, and honouring it is how a feed advances on every
  // scroll-adjacent gesture.
  assert.deepEqual(classifyCommit(-50, H), { kind: 'spring-back' });
});

test('a viewport that has no height yet never commits', () => {
  // The first frame, and any layout where the element is display:none. A zero
  // viewport with a division in it makes the threshold 0.3 *pixels*, so any
  // drag of any size commits and the feed jumps on first paint.
  for (const h of [0, -1, Number.NaN]) {
    assert.deepEqual(classifyCommit(-500, h), { kind: 'spring-back' }, `height ${h}`);
  }
});

test('moves one item in each direction', () => {
  assert.equal(nextFocus(3, 1, 10), 4);
  assert.equal(nextFocus(3, -1, 10), 2);
});

test('clamps at the last item rather than wrapping', () => {
  // The feed/carousel distinction. A wrapping feed has to define what "the end"
  // means and forces the position to be history rather than an index.
  assert.equal(nextFocus(9, 1, 10), 9);
  assert.equal(nextFocus(0, -1, 10), 0);
});

test('is 0 for an empty feed, not -1', () => {
  // -1 would make every caller's bounds check one wrong, and an index into
  // nothing is still nothing.
  assert.equal(nextFocus(0, 1, 0), 0);
  assert.equal(nextFocus(5, -1, -3), 0);
});

test('pulls an out-of-range current back toward the valid range', () => {
  // A library that shrank under a saved position. Clamping after the add
  // would move 50 further from a feed of 10.
  assert.equal(nextFocus(50, 1, 10), 9);
  assert.equal(nextFocus(-5, -1, 10), 0);
});

test('handles a delta of more than one', () => {
  assert.equal(nextFocus(0, 5, 10), 5);
  assert.equal(nextFocus(9, -5, 10), 4);
});

test('preloads exactly the next two, and nothing else', () => {
  assert.deepEqual(preloadFor(0, 10), [1, 2]);
  assert.deepEqual(preloadFor(5, 10), [6, 7]);
});

test('preloads nothing at or before the focus', () => {
  // The item behind the focus has been seen; preloading it spends data on
  // bytes the user already has.
  for (const focus of [0, 1, 5, 9]) {
    for (const i of preloadFor(focus, 10)) {
      assert.ok(i > focus, `focus ${focus} preloaded ${i}`);
    }
  }
});

test('preloads nothing beyond the prefetch distance', () => {
  for (const focus of [0, 1, 5, 9]) {
    for (const i of preloadFor(focus, 10)) {
      assert.ok(i <= focus + PREFETCH_AHEAD, `focus ${focus} preloaded ${i}`);
    }
  }
});

test('preload stops at the end of the feed', () => {
  assert.deepEqual(preloadFor(8, 10), [9]);
  assert.deepEqual(preloadFor(9, 10), []);
  assert.deepEqual(preloadFor(0, 2), [1]);
  assert.deepEqual(preloadFor(0, 1), []);
  assert.deepEqual(preloadFor(0, 0), []);
});

test('exactly one index plays', () => {
  const playing = [0, 1, 2, 3, 4].filter((i) => isPlaying(i, 2));
  assert.deepEqual(playing, [2]);
});

test('nothing plays when the focus is out of range', () => {
  // A focus index past the end of a feed that just shrank. If this returned
  // true for the last item, the last item would start playing on a feed nobody
  // is looking at.
  assert.equal(isPlaying(9, 50), false);
});

test('preloadAttr is auto for the focused item and the two ahead', () => {
  assert.equal(preloadAttr(3, 3, 10), 'auto');
  assert.equal(preloadAttr(4, 3, 10), 'auto');
  assert.equal(preloadAttr(5, 3, 10), 'auto');
});

test('preloadAttr stops one item past the prefetch distance', () => {
  // The boundary: PREFETCH_AHEAD items ahead preloads, one more does not.
  assert.equal(preloadAttr(3 + PREFETCH_AHEAD, 3, 10), 'auto');
  assert.equal(preloadAttr(3 + PREFETCH_AHEAD + 1, 3, 10), 'none');
});

test('preloadAttr is none for everything behind the focus', () => {
  assert.equal(preloadAttr(0, 3, 10), 'none');
  assert.equal(preloadAttr(2, 3, 10), 'none');
});
