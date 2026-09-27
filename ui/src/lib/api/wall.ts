/**
 * Grouping the wall, and scrolling it without a scrollbar. Spec 10.4, plan
 * T-P5-006 item 14; #6544, #6955.
 *
 * # What a wall with group-by has to decide
 *
 * A grid sorted by date, grouped into sections by month, is three separate
 * problems that look like one:
 *
 *  1. **where the group boundaries fall** — a pure function of the rows, and the
 *     only part that can be tested exactly;
 *  2. **how a group is laid out** — a group of 40 items is a 40-item grid, and a
 *     group of 3 is a 3-item grid, so group heights differ. A virtualizer that
 *     assumed a uniform row height (which is what `VirtualGrid` does, and is
 *     the right trade there) cannot lay out a variable number of groups without
 *     the group count being known, which needs the whole list;
 *  3. **auto-scroll**, which is a scroll position that has to be right on the
 *     first frame for the same reason `VirtualGrid`'s height is: a scrollbar
 *     that grows as you scroll is a scrollbar you cannot drag.
 *
 * # Why grouping breaks the fixed-row-height design, and what is done about it
 *
 * `VirtualGrid` fixes the row height so the scroll height of a 100,000 item
 * library is computable from the count alone. A grouped wall has a *variable*
 * row height: a section header, then as many rows as the group happens to need.
 *
 * Two ways out, and the second is chosen:
 *
 * - measure the groups, which means the whole list before the first paint;
 * - **make the group's row count known without the rows.** It is not: a group
 *   that is still loading has an unknown size, and that is the normal case, not
 *   an edge case.
 *
 * So the wall does NOT reuse `VirtualGrid`'s scrolling. It computes the
 * scroll height from the group boundaries it knows, and `groupExtents` is the
 * function that makes the gap explicit: a group whose size is unknown reports
 * it, and the wall renders a placeholder of the *median* group height rather
 * than guessing. The scrollbar is then approximately right, moves monotonically,
 * and becomes exact as groups load — which is the best that can be done without
 * loading everything, and is stated here rather than presented as exact.
 *
 * `medianGroupHeight` is the median rather than the mean because one group of
 * 5,000 items must not make every other section reserve 5,000 items' worth of
 * blank space.
 *
 * Everything here is pure. The component holds the scroll listener and nothing
 * else, and the boundaries are testable without a DOM.
 */

import type { ObjectRow } from '$lib/api/client.js';

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

/** How rows are grouped. The key is a field on the row. */
export type GroupKey =
  | 'none'
  | 'month'
  | 'year'
  | 'studio'
  | 'performer'
  | 'tag'
  | 'folder'
  | 'organized'
  | 'rating';

/**
 * Every grouping key, in the order the settings list shows them.
 *
 * The runtime list rather than only the union type, because two places need it
 * -- the nav and the `?group=` fallback -- and a hand-written list in each is a
 * place they can disagree, which shows up as a key that appears in the nav and
 * renders nothing.
 */
export const GROUP_KEYS: readonly GroupKey[] = [
  'none',
  'month',
  'year',
  'studio',
  'performer',
  'tag',
  'folder',
  'organized',
  'rating',
];

/**
 * The key a URL asks for, or `none` if it asks for something this build has
 * never heard of.
 *
 * A stale bookmark shows the grid rather than an empty wall. An unrecognised
 * value is not an error in a URL -- the group list grows with the schema, and a
 * link shared before a field was added has to keep working.
 */
export function parseGroupKey(raw: string | null | undefined): GroupKey {
  return GROUP_KEYS.includes(raw as GroupKey) ? (raw as GroupKey) : 'none';
}

/**
 * What a section is called, for a key.
 *
 * A label function rather than a string, because the ungrouped wall has no
 * section at all and a header saying "All" above every item is noise. `null`
 * means render no header.
 */
export function groupLabel(key: GroupKey): string | null {
  switch (key) {
    case 'none':
      return null;
    case 'month':
      return 'Month';
    case 'year':
      return 'Year';
    case 'studio':
      return 'Studio';
    case 'performer':
      return 'Performer';
    case 'tag':
      return 'Tag';
    case 'folder':
      return 'Folder';
    case 'organized':
      return 'Organized';
    case 'rating':
      return 'Rating';
  }
}

/**
 * The group value for one row, as a string.
 *
 * `null` for a row that has no value for the key, and `null` groups are
 * dropped rather than filed under "Unknown" — a section called "Unknown" with
 * 4,000 items in it is the thing a user has to click through to find the three
 * rows they wanted.
 */
export function groupValue(row: ObjectRow, key: GroupKey): string | null {
  switch (key) {
    case 'none':
      return null;
    case 'month':
      return monthOf(row.date);
    case 'year':
      return yearOf(row.date);
    case 'studio':
      return row.producer ?? null;
    case 'performer':
      // ObjectRow carries performers as names already; there is no id to group
      // by and no ids in the grid query, and grouping by a name the user typed
      // is grouping by what they can see.
      return row.performers?.[0] ?? null;
    case 'tag':
      return row.tags?.[0] ?? null;
    case 'folder':
      return row.folder ?? null;
    case 'organized':
      // `String(null)` is the four characters "null", which would render as a
      // section called "null". Every unfiled row would land in it, and it is
      // exactly the "Unknown" bucket this whole module exists to avoid.
      return row.organized ?? null;
    case 'rating':
      // Rating 0 is a real rating and must group; only null/absent is no group.
      // `String(null)` is the four characters "null", which would render as a
      // section called "null" holding every unrated row.
      return row.rating === null || row.rating === undefined ? null : String(row.rating);
  }
}

/**
 * The `YYYY-MM` a date falls in, or `null`.
 *
 * The month is formatted from the date STRING and not by constructing a `Date`,
 * because `new Date('2024-06-01')` is midnight UTC in JS and `getMonth()`
 * returns May in any timezone west of Greenwich. A wall grouped by month that
 * puts every item on the first of a month in the previous section is the
 * canonical version of that bug, and it is invisible to a test run in UTC.
 */
export function monthOf(date: string | undefined | null): string | null {
  const m = /^(\d{4})-(\d{2})/.exec(date ?? '');
  if (m === null) return null;
  return `${m[1]}-${m[2]}`;
}

export function yearOf(date: string | undefined | null): string | null {
  // Not `^(\d{4})-`: a year-only date is what a scan of a badly-named file
  // produces, and that is a real year to group by. Requiring the separator
  // silently dropped every one of them, so "group by year" on such a library
  // produced no sections at all.
  const m = /^(\d{4})\b/.exec(date ?? '');
  if (m === null) return null;
  return m[1]!;
}

/** One section of the wall. */
export interface Group {
  /** The group value, never `null` — a group with no value is not a group. */
  readonly value: string;
  /** Indices into the source array, in order. */
  readonly indices: readonly number[];
  /**
   * Rows in this group that have not arrived yet.
   *
   * Non-zero exactly when the group is still being paged in, and the reason
   * `groupExtents` reports a height rather than a count. A keyset query
   * returns pages, so a group that straddles a page boundary is partly known
   * and the wall must not pretend otherwise.
   */
  readonly pending: number;
  /** 0-based position of this group among the groups. */
  readonly index: number;
}

/**
 * Group rows by `key`, preserving order.
 *
 * Groups appear in FIRST-SEEN order rather than sorted, because the query is
 * already sorted and re-sorting groups would put a descending date query's
 * months in ascending order. The wall is a view of the query, not a second
 * opinion about it.
 *
 * Rows with no value for the key are DROPPED and counted by the caller's
 * arithmetic — this returns only real groups. A "no studio" row is not a studio
 * called "no studio".
 */
export function groupRows(rows: readonly ObjectRow[], key: GroupKey): Group[] {
  if (key === 'none') {
    return rows.length === 0
      ? []
      : [{ value: '', indices: rows.map((_, i) => i), pending: 0, index: 0 }];
  }

  const byValue = new Map<string, number[]>();
  for (const [i, row] of rows.entries()) {
    const value = groupValue(row, key);
    if (value === null || value === '') continue;
    const existing = byValue.get(value);
    if (existing === undefined) byValue.set(value, [i]);
    else existing.push(i);
  }

  return [...byValue.entries()].map(([value, indices], index) => ({
    value,
    indices,
    pending: 0,
    index,
  }));
}

/**
 * How many rows are still unloaded in each group.
 *
 * `known` is what has arrived; `total` is the query's reported count. A group
 * whose known rows are fewer than the query says exist is incomplete, and the
 * difference is what the wall reserves space for.
 */
export function pendingPerGroup(
  groups: readonly Group[],
  knownPerGroup: readonly number[],
  total: number,
): number[] {
  const known = knownPerGroup.reduce((a, b) => a + b, 0);
  if (total <= known) return groups.map(() => 0);
  // The shortfall is spread across the groups in proportion to their known size,
  // so a group that is mostly loaded reserves little and an untouched one
  // reserves a lot. Spreading it evenly instead would make the first group —
  // which is usually the newest and the biggest — reserve a share it does not
  // need, and the scrollbar would be wrong at the top, which is where the user
  // starts.
  if (known === 0) return groups.map(() => total / Math.max(1, groups.length));
  const out = groups.map((_, i) => {
    const share = knownPerGroup[i] ?? 0;
    return Math.round(((total - known) * share) / known);
  });
  return out;
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/** The measurements a wall needs. All in CSS pixels. */
export interface WallMetrics {
  /** Tile width, which sets the column count. */
  readonly tile: number;
  /** Gap between tiles, and between a header and its rows. */
  readonly gap: number;
  /** Height of one section header. */
  readonly headerHeight: number;
  /** How many columns fit in `viewportWidth`. */
  readonly columns: number;
}

/** How many columns fit, given a tile width and a gap. Never below 1. */
export function columnCount(
  viewportWidth: number,
  tile: number,
  gap: number,
): number {
  if (tile <= 0) return 1;
  const fit = Math.floor((viewportWidth + gap) / (tile + gap));
  return Math.max(1, fit);
}

/** The height of a group with `count` items: header, rows, gaps. */
export function groupHeight(count: number, tileHeight: number, m: WallMetrics): number {
  if (count <= 0) return m.headerHeight;
  const rows = Math.ceil(count / Math.max(1, m.columns));
  return m.headerHeight + rows * tileHeight + (rows - 1) * m.gap;
}

/**
 * The median height of the groups that are fully known.
 *
 * Median, not mean: one group of 5,000 items must not make every other section
 * reserve 5,000 items' worth of blank space. With the mean, a library with one
 * enormous group and fifty small ones is mostly blank.
 *
 * Falls back to a single group's height when exactly one is known, and to
 * `headerHeight + one row` when none are — the smallest wall that is still
 * scrollable, rather than a zero-height wall the user cannot drag.
 */
export function medianGroupHeight(
  knownHeights: readonly number[],
  fallbackTileHeight: number,
  m: WallMetrics,
): number {
  if (knownHeights.length === 0) {
    return groupHeight(1, fallbackTileHeight, m);
  }
  const sorted = [...knownHeights].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[mid]! : (sorted[mid - 1]! + sorted[mid]!) / 2;
}

/**
 * The scroll geometry of the whole wall.
 *
 * Every group's height is exact when it is fully known and the median-height
 * placeholder when it is not, so `totalHeight` is exact only when nothing is
 * pending — and `exact` says so, because a scrollbar that is approximately
 * right must not be presented as though it is exact. The user's scrollbar drag
 * is an estimate until every group has loaded, and the component says which it
 * is.
 */
export interface WallExtents {
  /** Total scrollable height. */
  readonly totalHeight: number;
  /** Height of each group, in order. */
  readonly groupHeights: readonly number[];
  /** `false` while any group is still loading. */
  readonly exact: boolean;
  /** How many groups are incomplete. */
  readonly pendingGroups: number;
}

export function groupExtents(
  groups: readonly Group[],
  pending: readonly number[],
  tileHeight: number,
  m: WallMetrics,
): WallExtents {
  const knownHeights: number[] = [];
  for (const [i, g] of groups.entries()) {
    if ((pending[i] ?? 0) === 0) knownHeights.push(groupHeight(g.indices.length, tileHeight, m));
  }
  const placeholder = medianGroupHeight(knownHeights, tileHeight, m);

  let pendingGroups = 0;
  const groupHeights = groups.map((g, i) => {
    const p = pending[i] ?? 0;
    if (p === 0) return groupHeight(g.indices.length, tileHeight, m);
    pendingGroups += 1;
    // A partially-loaded group is at least this big. Reserving the placeholder
    // alone would make the wall SHRINK as the real rows arrive, which moves
    // everything under the user's cursor.
    return Math.max(placeholder, groupHeight(g.indices.length + p, tileHeight, m));
  });

  const totalHeight =
    groupHeights.reduce((a, b) => a + b, 0) + Math.max(0, groupHeights.length - 1) * m.gap;

  return { totalHeight, groupHeights, exact: pendingGroups === 0, pendingGroups };
}

// ---------------------------------------------------------------------------
// Auto-scroll
// ---------------------------------------------------------------------------

/**
 * Where auto-scroll should be, as a function of the state.
 *
 * Pure, and it is the whole of #6955. The rule: if the user is already at (or
 * within `slack` of) the bottom, stay pinned to the bottom as more items
 * arrive. If they have scrolled UP even slightly, stop pinning — because a user
 * who scrolled up to read something is not asking to be dragged back down, and
 * an auto-scroll that ignores that is the single most complained-about
 * behaviour in an infinite feed.
 *
 * `slack` exists because "within a few pixels" is what a trackpad's momentum
 * produces, and a strict `=== bottom` would unpin on the last pixel of every
 * flick.
 */
export const AUTOSCROLL_SLACK = 48;

export interface AutoScrollState {
  /** Current scroll position. */
  readonly scrollTop: number;
  /** Current scroll height. */
  readonly scrollHeight: number;
  /** Viewport height. */
  readonly clientHeight: number;
  /** Was the wall pinned to the bottom on the previous update? */
  readonly pinned: boolean;
}

export interface AutoScrollResult {
  /** `true` when the wall should be pinned to the bottom after this update. */
  readonly pinned: boolean;
  /** The scroll position to apply, or `null` to leave it alone. */
  readonly scrollTop: number | null;
  /** Why, for a log line and for a test to assert on. */
  readonly reason: 'initially-pinned' | 'still-pinned' | 'user-scrolled-up' | 'unpinned';
}

export function autoScroll(state: AutoScrollState, slack = AUTOSCROLL_SLACK): AutoScrollResult {
  const maxScroll = Math.max(0, state.scrollHeight - state.clientHeight);
  const atBottom = state.scrollTop >= maxScroll - slack;

  if (!state.pinned) {
    // Once unpinned, stay unpinned until the user returns to the ACTUAL bottom,
    // not the slack around it. Re-pinning on "close enough" is what yanks the
    // view down in the middle of a trackpad flick, and it is also how a user
    // who scrolled up by forty pixels gets dragged straight back down.
    if (state.scrollTop >= maxScroll) {
      return { pinned: true, scrollTop: maxScroll, reason: 'initially-pinned' };
    }
    return { pinned: false, scrollTop: null, reason: 'unpinned' };
  }

  if (!atBottom) {
    return { pinned: false, scrollTop: null, reason: 'user-scrolled-up' };
  }
  return { pinned: true, scrollTop: maxScroll, reason: 'still-pinned' };
}

/**
 * The first scroll position for a wall that opens at the top.
 *
 * A wall that opens at the bottom is a wall the user cannot find the top of, so
 * the initial position is 0. Auto-scroll pins only AFTER the user has scrolled
 * down once, which is what `pinned: false` in the initial state means.
 */
export function initialScrollTop(): number {
  return 0;
}
