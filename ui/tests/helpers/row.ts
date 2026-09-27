/*
  One row factory for every UI test. 

  # Why this exists

  Four test files each declared their own `function row(...)`, and when the wall
  added four grouping fields to `ObjectRow` (#6544), every one of them had to
  change. That is the wrong number of files to touch for a field the wall does
  not use, and the failure mode is quiet: a helper that omits a field and is
  cast, or typed loosely, keeps compiling long after the shape moved.

  So a row is built in ONE place, and it is total: every field of `ObjectRow`,
  with the null case spelled out. A new field on the row breaks this factory and
  nothing else, which is a compile error pointing at the one file that should
  change.

  `over` is `Partial` so a test can vary one field without naming the other
  fifteen, and the spread goes LAST so a test can never accidentally set a field
  the factory does not know about.
 */

import type { ObjectRow } from '../../src/lib/api/client.js';

/** A complete row, with the neutral value for every field. */
export function makeRow(over: Partial<ObjectRow> = {}): ObjectRow {
  return {
    id: 'o1',
    kind: 'Scene',
    title: 'Item 1',
    date: null,
    rating: null,
    organized: null,
    coverPath: null,
    width: 800,
    height: 1200,
    durationMs: null,
    // The grouping fields (#6544). Empty, not null: a row that belongs to no
    // studio is a real row, and `groupRows` drops it rather than filing it
    // under a section called "Unknown".
    producer: null,
    performers: [],
    tags: [],
    folder: null,
    ...over
  };
}

/**
 * `count` rows with distinct ids, spread over the given dates.
 *
 * For the wall, where the grouping is the point: one row per month is the
 * smallest set that has more than one group, and a single group cannot tell a
 * grouping bug from no grouping at all.
 */
export function makeRows(count: number, over: Partial<ObjectRow> = {}): ObjectRow[] {
  return Array.from({ length: count }, (_, i) => makeRow({ id: `obj-${i}`, ...over }));
}
