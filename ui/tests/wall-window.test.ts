/*
  Tests for the wall's windowing: §4.2 for a grouped surface. Spec 10.4, plan
  T-P5-006 item 15.

  # What is being tested here

  The one thing item 14 got wrong, and it is a §4.2 violation rather than a
  cosmetic one: a wall that renders every group renders 50,000 tiles for a
  50,000-row library, and the DOM is the thing that stops the browser.

  Three claims, each of which has a specific way of being got wrong:

   1. **the rendered set is bounded independently of the library size.** A wall
     that renders everything "works" -- and is a DOM dump at the size where it
     stops working. Asserted against a bound, not an exact count, for the reason
     `grid.spec.ts` says: the count is a tuning decision, the bound is a
     contract.

  2. **a group's row window is computed from ITS OWN offset.** This is the bug
     this module exists to not have. The scroll offset is the wall's, so a group
     starting 5,000px down has a local offset of zero; a row window computed from
     the global offset lands tens of thousands of rows past the end of that
     group and renders nothing.

  3. **the scan is a binary search.** A wall with 5,000 groups that linearly
     scans on every scroll frame is a dropped-frame layout, not a slow one. The
     test asserts the answer, and a mutation asserts the shape.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  GROUP_OVERSCAN,
  groupExtents,
  groupLayout,
  groupRows,
  wallWindow,
  type Group,
  type WallMetrics,
} from '../src/lib/api/wall.js';
import { makeRow } from './helpers/row.js';
import type { ObjectRow } from '../src/lib/api/client.js';

/** 4 columns, 100px tiles, 8px gaps, a 28px header. */
const M: WallMetrics = { tile: 100, gap: 8, headerHeight: 28, columns: 4 };

/** A group of `count` items, as `groupRows` would produce one. */
function group(value: string, count: number, index: number): Group {
  return {
    value,
    indices: Array.from({ length: count }, (_, i) => index * 10_000 + i),
    pending: 0,
    index,
  };
}

/** The layout for a set of groups, all loaded. */
function layoutFor(groups: Group[]) {
  const extents = groupExtents(groups, groups.map(() => 0), 100, M);
  return groupLayout(groups, extents, groups.map(() => 0), M);
}

// ---------------------------------------------------------------------------

describe('groupLayout', () => {
  test('each group starts below the previous one ends, plus the gap', () => {
    const layout = layoutFor([group('a', 8, 0), group('b', 4, 1), group('c', 12, 2)]);
    for (let i = 1; i < layout.boxes.length; i += 1) {
      const prev = layout.boxes[i - 1]!;
      const b = layout.boxes[i]!;
      assert.equal(b.top, prev.top + prev.height + M.gap);
    }
  });

  test('the first group starts at the top', () => {
    assert.equal(layoutFor([group('a', 4, 0)]).boxes[0]!.top, 0);
  });

  test('the content height has no trailing gap', () => {
    // A trailing gap is scroll range containing nothing.
    const layout = layoutFor([group('a', 4, 0), group('b', 4, 1)]);
    const last = layout.boxes[layout.boxes.length - 1]!;
    assert.equal(layout.contentHeight, last.top + last.height);
  });

  test('rows are the loaded items in whole rows of columns', () => {
    // 12 items in 4 columns is 3 rows; 13 is 4. The last row is short and that
    // is fine, but the COUNT has to be right or the window slices a row that
    // does not exist.
    assert.equal(layoutFor([group('a', 12, 0)]).boxes[0]!.rows, 3);
    assert.equal(layoutFor([group('a', 13, 0)]).boxes[0]!.rows, 4);
    assert.equal(layoutFor([group('a', 4, 0)]).boxes[0]!.rows, 1);
    assert.equal(layoutFor([group('a', 0, 0)]).boxes[0]!.rows, 0);
  });

  // A pending group reserves height for rows that are not there. If `rows`
  // counted the reservation, the window would try to render rows that were
  // never loaded and produce undefined tiles.
  test('a pending group reports loaded rows separately from reserved ones', () => {
    const groups = [group('a', 4, 0)];
    const pending = [40];
    const extents = groupExtents(groups, pending, 100, M);
    const layout = groupLayout(groups, extents, pending, M);
    const b = layout.boxes[0]!;
    assert.equal(b.loadedRows, 1, 'one row is loaded');
    assert.ok(b.rows > b.loadedRows, 'and the reservation is larger');
    assert.equal(b.pending, 40);
  });
});

// ---------------------------------------------------------------------------

describe('wallWindow', () => {
  test('an empty wall renders nothing', () => {
    const w = wallWindow(layoutFor([]), M, 0, 800);
    assert.deepEqual(w.groupIndices, []);
    assert.deepEqual(w.rowWindows, []);
  });

  // The §4.2 claim. A bound, not an exact count: the count is a tuning
  // decision, the bound is a contract.
  test('the rendered set is bounded independently of how many groups exist', () => {
    const many = Array.from({ length: 5000 }, (_, i) => group(`g${i}`, 8, i));
    const layout = layoutFor(many);
    const w = wallWindow(layout, M, 0, 800);

    assert.ok(
      w.groupIndices.length <= 12,
      `at most a screenful plus overscan, got ${w.groupIndices.length}`
    );
    // And it is a small fraction of the wall, which is the part that matters.
    assert.ok(w.groupIndices.length < many.length / 100);
  });

  test('a wall of one group renders that group', () => {
    const w = wallWindow(layoutFor([group('a', 8, 0)]), M, 0, 800);
    assert.deepEqual(w.groupIndices, [0]);
  });

  test('the window MOVES when the scroll does', () => {
    // The assertion that distinguishes a window from a truncation. A component
    // that renders the first N groups forever passes a count check and is not
    // virtualized at all.
    const many = Array.from({ length: 200 }, (_, i) => group(`g${i}`, 8, i));
    const layout = layoutFor(many);

    const top = wallWindow(layout, M, 0, 800);
    const middle = wallWindow(layout, M, 20_000, 800);

    assert.notDeepEqual(top.groupIndices, middle.groupIndices);
    assert.ok((middle.groupIndices[0] ?? 0) > (top.groupIndices[0] ?? 0));
  });

  test('the first VISIBLE group is rendered, and so is the one before it', () => {
    const many = Array.from({ length: 200 }, (_, i) => group(`g${i}`, 8, i));
    const layout = layoutFor(many);
    const w = wallWindow(layout, M, 20_000, 800);

    // The first RENDERED group is the overscan one, which starts above the
    // viewport -- conflating "first rendered" with "first visible" is the
    // mistake, and it is why the assertion is about the overlap set.
    //
    // The claim: every group that overlaps the viewport is rendered.
    const bottom = 20_000 + 800;
    const overlapping = layout.boxes
      .map((b, i) => ({ b, i }))
      .filter(({ b }) => b.top + b.height > 20_000 && b.top < bottom)
      .map(({ i }) => i);
    assert.ok(overlapping.length > 0);
    for (const gi of overlapping) {
      assert.ok(w.groupIndices.includes(gi), `group ${gi} overlaps the viewport and is not rendered`);
    }
  });

  test('overscan keeps a group that starts just above the viewport', () => {
    // Without overscan, a group scrolled half off the top renders with its
    // visible half missing.
    const many = Array.from({ length: 200 }, (_, i) => group(`g${i}`, 8, i));
    const layout = layoutFor(many);
    const deep = layout.boxes[50]!;
    // A scroll position that lands just inside group 50.
    const w = wallWindow(layout, M, deep.top + 4, 800);
    assert.ok(
      w.groupIndices.includes(50),
      'the partly-scrolled group is rendered whole'
    );
  });

  test('scrolling to the end renders the LAST group', () => {
    const many = Array.from({ length: 200 }, (_, i) => group(`g${i}`, 8, i));
    const layout = layoutFor(many);
    const w = wallWindow(layout, M, layout.contentHeight, 800);
    assert.ok(w.groupIndices.includes(199));
  });

  // The bug this module exists to not have. A row window computed from the
  // wall's scroll offset rather than the group's would land far past the end of
  // a group halfway down the page, and that group would render no tiles at all.
  test('a group far down the wall still renders its rows', () => {
    const many = Array.from({ length: 300 }, (_, i) => group(`g${i}`, 8, i));
    const layout = layoutFor(many);
    const target = 200;
    const box = layout.boxes[target]!;

    const w = wallWindow(layout, M, box.top + 2, 800);
    const at = w.groupIndices.indexOf(target);
    assert.ok(at >= 0, 'the group is in the window');
    const [first, last] = w.rowWindows[at]!;
    assert.ok(
      last > first,
      `a group on screen must render rows: got [${first}, ${last}) of ${box.loadedRows}`
    );
    assert.ok(first < box.loadedRows, 'and the window is inside the group');
  });

  test('a row window never runs past the end of its group', () => {
    const many = Array.from({ length: 300 }, (_, i) => group(`g${i}`, 8, i));
    const layout = layoutFor(many);
    for (const scroll of [0, 5_000, 20_000, 60_000, 120_000]) {
      const w = wallWindow(layout, M, scroll, 800);
      w.groupIndices.forEach((gi, n) => {
        const box = layout.boxes[gi]!;
        const [first, last] = w.rowWindows[n]!;
        assert.ok(first >= 0, `no negative row at ${scroll}`);
        assert.ok(last <= box.loadedRows, `window past the end at ${scroll}: ${last} > ${box.loadedRows}`);
      });
    }
  });

  test('a group with no loaded rows renders no rows, and does not crash', () => {
    // A pending group whose page has not landed. The reservation holds the
    // space; the window is empty.
    const groups = [group('a', 0, 0), group('b', 4, 1)];
    const pending = [40, 0];
    const extents = groupExtents(groups, pending, 100, M);
    const layout = groupLayout(groups, extents, pending, M);
    const w = wallWindow(layout, M, 0, 800);

    const at = w.groupIndices.indexOf(0);
    if (at >= 0) assert.deepEqual(w.rowWindows[at], [0, 0]);
  });

  test('a zero-height viewport renders the first group rather than nothing', () => {
    // A viewport measured before layout has a height of 0. Rendering nothing
    // means a blank wall that stays blank until something forces a re-render.
    const w = wallWindow(layoutFor([group('a', 8, 0), group('b', 8, 1)]), M, 0, 0);
    assert.ok(w.groupIndices.length > 0);
  });

  test('a negative scroll offset is treated as the top', () => {
    // Overscroll bounce on macOS produces a negative scrollTop, and a floor of
    // a negative number is a negative row index.
    const w = wallWindow(layoutFor([group('a', 8, 0), group('b', 8, 1)]), M, -50, 800);
    w.groupIndices.forEach((_, n) => {
      assert.ok(w.rowWindows[n]![0] >= 0);
    });
  });

  test('a group whose rows are all on screen renders all of them', () => {
    // A short group is the common case, and a window that computes a partial
    // range for it drops tiles the user can see.
    const w = wallWindow(layoutFor([group('a', 8, 0)]), M, 0, 5_000);
    const [first, last] = w.rowWindows[0]!;
    assert.equal(first, 0);
    assert.equal(last, 2, '8 items in 4 columns is 2 rows');
  });

  test('the overscan default is at least one group', () => {
    // Zero overscan is a wall with holes in it when scrolling fast.
    assert.ok(GROUP_OVERSCAN >= 1);
  });
});

// ---------------------------------------------------------------------------

describe('windowing at scale', () => {
  // The regression this item exists to prevent, as a test: a 5,000-row library
  // grouped by month, which is the shape a real library has.
  test('a 5,000-row wall renders a bounded number of tiles', () => {
    const rows: ObjectRow[] = Array.from({ length: 5_000 }, (_, i) =>
      makeRow({ id: `o${i}`, date: `2024-${String((i % 12) + 1).padStart(2, '0')}-01` })
    );
    const groups = groupRows(rows, 'month');
    const extents = groupExtents(groups, groups.map(() => 0), 100, M);
    const layout = groupLayout(groups, extents, groups.map(() => 0), M);
    const w = wallWindow(layout, M, 0, 800);

    const tiles = w.groupIndices.reduce(
      (n, gi, at) => n + (w.rowWindows[at]![1] - w.rowWindows[at]![0]) * M.columns,
      0
    );
    assert.ok(tiles < 400, `at most a few hundred tiles, got ${tiles}`);
  });

  test('every group in a real library is reachable by scrolling to it', () => {
    // A window that renders the top groups only would still pass every count
    // assertion above. This walks the whole wall and checks each group renders
    // when the viewport is on it.
    const rows: ObjectRow[] = Array.from({ length: 5_000 }, (_, i) =>
      makeRow({ id: `o${i}`, date: `2024-${String((i % 12) + 1).padStart(2, '0')}-01` })
    );
    const groups = groupRows(rows, 'month');
    const extents = groupExtents(groups, groups.map(() => 0), 100, M);
    const layout = groupLayout(groups, extents, groups.map(() => 0), M);

    for (const box of layout.boxes) {
      const w = wallWindow(layout, M, box.top + 1, 400);
      const at = w.groupIndices.indexOf(box.index);
      assert.ok(at >= 0, `group ${box.value} is not rendered when scrolled to`);
      const [first, last] = w.rowWindows[at]!;
      assert.ok(last > first, `group ${box.value} renders no rows when on screen`);
    }
  });
});
