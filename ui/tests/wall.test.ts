/*
  Tests for the wall: group-by and auto-scroll. Spec 10.4, plan T-P5-006
  item 14; #6544, #6955.

  # What is being tested here

  The two decisions, at their boundaries.

  #6544 is grouping. Grouping looks like a `Map` and is not: the boundaries
  have to respect the query's ORDER, a row with no value for the key has to go
  somewhere specific (nowhere), and a descending date query must not have its
  months re-sorted ascending on the way to the screen.

  #6955 is auto-scroll, and the whole of it is one question asked wrong: "is the
  user at the bottom?" The failure is a user who scrolled up to read something
  and gets dragged back down as the next page arrives. The slack, the unpin
  stickiness and the re-pin condition are all that question, and each has a test
  that fails when it is deleted.

  # The one that cost a rewrite

  `groupExtents` reserves space for groups that have not loaded. The first
  version reserved the MEDIAN group height, and a test caught that a wall which
  SHRINKS as rows arrive is unusable: the content under the user's cursor moves.
  A pending group is therefore at least `max(placeholder, known + pending)`, and
  the test asserts the wall never shrinks as a group fills -- in that direction
  specifically, because a test asserting only "it is tall enough" passes on a
  wall that teleports.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  AUTOSCROLL_SLACK,
  autoScroll,
  columnCount,
  groupExtents,
  groupHeight,
  groupLabel,
  groupRows,
  groupValue,
  initialScrollTop,
  medianGroupHeight,
  monthOf,
  pendingPerGroup,
  yearOf,
  type WallMetrics,
} from '../src/lib/api/wall.js';
import { makeRow } from './helpers/row.js';

/** 4 columns, 100px tiles, 8px gaps, a 28px header. */
const M: WallMetrics = { tile: 100, gap: 8, headerHeight: 28, columns: 4 };

// ---------------------------------------------------------------------------

describe('monthOf and yearOf', () => {
  // The reason these exist as string operations. `new Date('2024-06-01')` is
  // midnight UTC, and `getMonth()` in any timezone west of Greenwich returns
  // May. A wall that files every item on the first of a month under the
  // previous month is invisible to a test run in UTC, which is why the parse
  // is a regex and the tests below are timezone-independent by construction.
  test('reads the month off the string, so no timezone can shift it', () => {
    assert.equal(monthOf('2024-06-01'), '2024-06');
    assert.equal(monthOf('2024-06-30T23:59:59Z'), '2024-06');
    assert.equal(monthOf('2024-12-31'), '2024-12');
    assert.equal(monthOf('2024-01-01'), '2024-01');
  });

  test('a date with no month is not a month', () => {
    assert.equal(monthOf('2024'), null);
    assert.equal(monthOf('June 2024'), null);
    assert.equal(monthOf(''), null);
    assert.equal(monthOf(null), null);
    assert.equal(monthOf(undefined), null);
  });

  test('yearOf agrees with monthOf on the year', () => {
    assert.equal(yearOf('2024-06-01'), '2024');
    // A four-digit year alone IS enough for a year grouping, even though it is
    // not enough for a month. Dropping it would lose a real group.
    assert.equal(yearOf('2024'), '2024');
    assert.equal(yearOf('24-06-01'), null, 'not a four-digit year');
  });
});

// ---------------------------------------------------------------------------

describe('groupValue', () => {
  test('"none" has no group value at all', () => {
    // Not '' and not a placeholder: there is no section to put it in, and
    // inventing one puts a header above every item in the library.
    assert.equal(groupValue(makeRow(), 'none'), null);
  });

  test('reads the grouping fields off the row', () => {
    const r = makeRow({ date: '2024-06-02', producer: 'Studio X', folder: 'A/B' });
    assert.equal(groupValue(r, 'month'), '2024-06');
    assert.equal(groupValue(r, 'year'), '2024');
    assert.equal(groupValue(r, 'studio'), 'Studio X');
    assert.equal(groupValue(r, 'folder'), 'A/B');
  });

  test('a row with no value is null, not a placeholder string', () => {
    // The distinction is load-bearing: `null` is what makes `groupRows` DROP the
    // row. A row that becomes "Unknown" is a section a user has to click
    // through to find the three rows they wanted.
    const r = makeRow();
    assert.equal(groupValue(r, 'studio'), null);
    assert.equal(groupValue(r, 'folder'), null);
    assert.equal(groupValue(r, 'performer'), null, 'an empty list is not a value');
    assert.equal(groupValue(r, 'tag'), null);
  });

  test('organized and rating group by their value as a string', () => {
    assert.equal(groupValue(makeRow({ organized: 'yes' }), 'organized'), 'yes');
    assert.equal(groupValue(makeRow({ organized: null }), 'organized'), null);
    assert.equal(groupValue(makeRow({ rating: 4 }), 'rating'), '4');
    assert.equal(groupValue(makeRow({ rating: null }), 'rating'), null);
  });
});

// ---------------------------------------------------------------------------

describe('groupRows', () => {
  const rows = [
    makeRow({ id: 'a', date: '2024-06-01', producer: 'X' }),
    makeRow({ id: 'b', date: '2024-06-20', producer: 'Y' }),
    makeRow({ id: 'c', date: '2024-07-01', producer: 'X' }),
    makeRow({ id: 'd', date: '2024-05-01', producer: null }),
  ];

  test('groups by value, keeping the source order inside each group', () => {
    const groups = groupRows(rows, 'studio');
    assert.deepEqual(
      groups.map((g) => [g.value, g.indices]),
      [
        ['X', [0, 2]],
        ['Y', [1]],
      ]
    );
  });

  // The first claim in the header: a descending query must not be re-sorted on
  // the way to the screen. The wall is a view of the query, not a second
  // opinion about it.
  test('groups appear in first-seen order, not sorted', () => {
    const descending = groupRows(rows, 'month');
    assert.deepEqual(
      descending.map((g) => g.value),
      ['2024-06', '2024-07', '2024-05']
    );
  });

  test('a row with no value for the key is dropped, not filed under "Unknown"', () => {
    // Row `d` has no producer. If it were filed, every library would have a
    // section the user has to open to discover is mostly nothing.
    const groups = groupRows(rows, 'studio');
    assert.equal(
      groups.flatMap((g) => g.indices).includes(3),
      false,
      'the valueless row must not appear in any group'
    );
  });

  test('an empty string is not a group either', () => {
    // A blank producer from a half-filled form is not a producer called "".
    const g = groupRows([makeRow({ producer: '' })], 'studio');
    assert.deepEqual(g, []);
  });

  test('"none" gives one group holding everything, and no header', () => {
    const groups = groupRows(rows, 'none');
    assert.equal(groups.length, 1);
    assert.deepEqual(groups[0]!.indices, [0, 1, 2, 3]);
    assert.equal(groupLabel('none'), null, 'and the label says render no header');
  });

  test('"none" on an empty list is no groups, not one empty group', () => {
    // One empty group renders a header and a body with nothing in it, which is
    // a visible artefact for an empty result.
    assert.deepEqual(groupRows([], 'none'), []);
  });

  test('an empty list is no groups', () => {
    assert.deepEqual(groupRows([], 'month'), []);
  });

  test('every group carries its own index', () => {
    const groups = groupRows(rows, 'studio');
    assert.deepEqual(
      groups.map((g) => g.index),
      [0, 1]
    );
  });
});

// ---------------------------------------------------------------------------

describe('pendingPerGroup', () => {
  test('nothing pending when the loaded rows account for the whole result', () => {
    const groups = groupRows([makeRow({ producer: 'X' }), makeRow({ producer: 'Y' })], 'studio');
    assert.deepEqual(pendingPerGroup(groups, [1, 1], 2), [0, 0]);
  });

  // Spreading the shortfall evenly would make the FIRST group -- usually the
  // newest and the biggest -- reserve a share it does not need, so the
  // scrollbar is wrong at the top, which is where the user starts.
  test('the shortfall is shared out in proportion to what each group has', () => {
    const groups = groupRows([makeRow({ producer: 'X' }), makeRow({ producer: 'Y' })], 'studio');
    // 3 known, 10 total, 7 short. X has 1, Y has 2, so 7/3 and 14/3.
    assert.deepEqual(pendingPerGroup(groups, [1, 2], 10), [2, 5]);
  });

  test('with nothing loaded the shortfall is shared evenly', () => {
    const groups = [
      { value: 'a', indices: [], pending: 0, index: 0 },
      { value: 'b', indices: [], pending: 0, index: 1 },
    ];
    assert.deepEqual(pendingPerGroup(groups, [0, 0], 10), [5, 5]);
  });
});

// ---------------------------------------------------------------------------

describe('geometry', () => {
  test('the column count is what fits, and never zero', () => {
    // A viewport narrower than one tile gives one column, not zero: zero
    // columns divides by zero in every row-height calculation downstream.
    assert.equal(columnCount(400, 100, 8), 3);
    assert.equal(columnCount(10, 100, 8), 1);
    assert.equal(columnCount(0, 100, 8), 1);
    assert.equal(columnCount(400, 0, 8), 1, 'a zero tile width is not a layout');
  });

  test('a group is a header plus whole rows plus the gaps between them', () => {
    // 5 items, 4 columns -> 2 rows -> 2 gaps, not 1: the gap after the last
    // row belongs to the NEXT group, not to this one.
    assert.equal(groupHeight(5, 100, M), 28 + 2 * 100 + 1 * 8);
    // Exactly one row has no internal gap.
    assert.equal(groupHeight(4, 100, M), 28 + 100);
    assert.equal(groupHeight(8, 100, M), 28 + 2 * 100 + 8);
  });

  test('an empty group is a header with no rows', () => {
    assert.equal(groupHeight(0, 100, M), 28);
  });

  // Median, not mean. One group of 5,000 items must not make every other
  // section reserve 5,000 items' worth of blank space -- with the mean, a
  // library with one enormous group is mostly blank.
  test('the placeholder is the median, so one huge group cannot inflate the rest', () => {
    const heights = [10, 10, 10, 10_000];
    assert.equal(medianGroupHeight(heights, 100, M), 10);
    assert.notEqual(medianGroupHeight(heights, 100, M), (10 + 10 + 10 + 10_000) / 4);
  });

  test('an even number of known heights takes the middle pair', () => {
    // The average of the two middle values -- both middle values, not the lower
    // one and not the mean of all four.
    assert.equal(medianGroupHeight([10, 20, 30, 40], 100, M), 25);
  });

  test('with nothing known the placeholder is one group, not zero', () => {
    // A zero-height wall is a wall the user cannot drag. This is the state a
    // freshly-opened wall is in, so it is the state that matters.
    assert.equal(medianGroupHeight([], 100, M), groupHeight(1, 100, M));
    assert.ok(medianGroupHeight([], 100, M) > 0);
  });
});

// ---------------------------------------------------------------------------

describe('groupExtents', () => {
  const groups = groupRows(
    [makeRow({ producer: 'X' }), makeRow({ producer: 'X' }), makeRow({ producer: 'Y' })],
    'studio'
  );

  test('an all-loaded wall is exact', () => {
    const e = groupExtents(groups, [0, 0], 100, M);
    assert.equal(e.exact, true);
    assert.equal(e.pendingGroups, 0);
    assert.deepEqual(e.groupHeights, [groupHeight(2, 100, M), groupHeight(1, 100, M)]);
  });

  test('the total includes the gap BETWEEN groups but not after the last', () => {
    // An extra trailing gap is a scroll range the user can drag into that
    // contains nothing, which reads as a rendering bug.
    const e = groupExtents(groups, [0, 0], 100, M);
    const expected = groupHeight(2, 100, M) + groupHeight(1, 100, M) + M.gap;
    assert.equal(e.totalHeight, expected);
  });

  test('a pending group is reported as pending', () => {
    const e = groupExtents(groups, [0, 3], 100, M);
    assert.equal(e.exact, false, 'an approximate scrollbar must not be presented as exact');
    assert.equal(e.pendingGroups, 1);
  });

  // The one that cost a rewrite. A wall that SHRINKS as rows arrive moves
  // everything under the user's cursor.
  test('a group does not shrink as it loads', () => {
    const before = groupExtents(groups, [0, 40], 100, M);
    const after = groupExtents(groups, [0, 0], 100, M);
    assert.ok(
      before.groupHeights[1]! >= after.groupHeights[1]!,
      `a pending group must reserve at least what it will need: ${before.groupHeights[1]} < ${after.groupHeights[1]}`
    );
    assert.ok(before.totalHeight >= after.totalHeight);
  });

  test('a pending group reserves room for the rows that are coming', () => {
    // 40 more rows in a 4-column wall is 10 more rows, so the reservation has to
    // be a real row count and not a token amount of blank space.
    const e = groupExtents(groups, [0, 40], 100, M);
    assert.ok(e.groupHeights[1]! >= groupHeight(41, 100, M));
  });

  test('a wall of no groups has no height', () => {
    const e = groupExtents([], [], 100, M);
    assert.equal(e.totalHeight, 0);
    assert.equal(e.exact, true);
  });
});

// ---------------------------------------------------------------------------

describe('autoScroll', () => {
  const base = { scrollHeight: 1000, clientHeight: 400, pinned: true };

  test('a wall opens at the top, not the bottom', () => {
    // A wall that opens at the bottom is a wall whose top the user cannot find.
    // Auto-scroll pins only after they have scrolled down once.
    assert.equal(initialScrollTop(), 0);
  });

  test('stays pinned while more items arrive', () => {
    // The whole feature: the user is at the bottom, rows arrive, and the view
    // follows so they keep seeing new items without pressing anything.
    const r = autoScroll({ ...base, scrollTop: 600 });
    assert.equal(r.pinned, true);
    assert.equal(r.scrollTop, 600, 'the new bottom, which is 1000 - 400');
    assert.equal(r.reason, 'still-pinned');
  });

  // The behaviour that makes or breaks an infinite feed. A user who scrolled up
  // to read something is not asking to be dragged back down.
  test('stops following as soon as the user scrolls up', () => {
    const r = autoScroll({ ...base, scrollTop: 300 });
    assert.equal(r.pinned, false);
    assert.equal(r.scrollTop, null, 'and the position is left alone');
    assert.equal(r.reason, 'user-scrolled-up');
  });

  // A strict `=== bottom` would unpin on the last pixel of every trackpad flick.
  test('tolerates a few pixels of momentum below the bottom', () => {
    const atBottom = base.scrollHeight - base.clientHeight;
    const r = autoScroll({ ...base, scrollTop: atBottom - AUTOSCROLL_SLACK + 1 });
    assert.equal(r.pinned, true, 'one pixel of overscroll is not a user scrolling up');
  });

  test('does not tolerate a scroll that is genuinely above the bottom', () => {
    const atBottom = base.scrollHeight - base.clientHeight;
    const r = autoScroll({ ...base, scrollTop: atBottom - AUTOSCROLL_SLACK - 1 });
    assert.equal(r.pinned, false);
  });

  // Without the stickiness, one frame of momentum that lands "close enough"
  // re-pins mid-flick and yanks the view down while the finger is still moving.
  test('once unpinned, stays unpinned even back near the bottom', () => {
    const atBottom = base.scrollHeight - base.clientHeight;
    const r = autoScroll({ ...base, pinned: false, scrollTop: atBottom - 10 });
    assert.equal(r.pinned, false);
    assert.equal(r.scrollTop, null, 'a flick that ends near the bottom is not a re-pin');
    assert.equal(r.reason, 'unpinned');
  });

  test('re-pins when the user returns all the way to the bottom', () => {
    const atBottom = base.scrollHeight - base.clientHeight;
    const r = autoScroll({ ...base, pinned: false, scrollTop: atBottom });
    assert.equal(r.pinned, true);
    assert.equal(r.scrollTop, atBottom);
  });

  test('a wall shorter than its viewport has nowhere to follow, and does not divide by zero', () => {
    // scrollHeight < clientHeight: maxScroll is 0, and 0 >= 0 - slack is true,
    // so it pins at 0. A negative scrollTop here would be a real bug and the
    // browser would silently clamp it.
    const r = autoScroll({ scrollTop: 0, scrollHeight: 200, clientHeight: 400, pinned: true });
    assert.equal(r.pinned, true);
    assert.equal(r.scrollTop, 0);
    assert.ok(r.scrollTop! >= 0);
  });
});
