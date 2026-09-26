/**
 * Keyset pagination state for a virtualized list.
 *
 * # Why this is not a Svelte component
 *
 * The hard part of infinite scroll is not the rendering, it is the state
 * machine: what is loaded, what is in flight, what failed, whether the cursor
 * is still valid after the filter changed. None of that needs a DOM, and
 * putting it in a component makes it untestable — the only way to test a
 * component's error handling is to mount it and race it.
 *
 * So the state machine is here, with no Svelte import, and `VirtualGrid.svelte`
 * renders it. `keyset.test.ts` drives every branch.
 *
 * # Why cursors, not page numbers
 *
 * Spec rule 2. `OFFSET 40000` makes Postgres walk and discard forty thousand
 * rows, so deep pages get slower the deeper they are, and an item inserted
 * above the cursor shifts every later page and the user sees the same item
 * twice. A keyset cursor is an index seek: "after (sort, id) = (x, y)".
 *
 * The consequence for this file is that there is no `page` number anywhere.
 * The list is a sequence of appended windows and a cursor pointing past the
 * last one. You cannot ask for page 400; you can only ask for what comes next,
 * which is the only operation a keyset store supports.
 *
 * # Why stale responses are dropped rather than applied
 *
 * A user scrolling fast fires five fetches; they can resolve out of order, and
 * a filter change makes all of them irrelevant. Applying a response that
 * arrives after its request was superseded would append the wrong rows, and
 * the symptom — a list that contains items the current filter excludes — is
 * very hard to trace back. Every fetch carries a generation number and a
 * response whose generation is stale is dropped on arrival.
 */

import { fetchObjects, type ObjectRow, type PageInput } from './client.js';

export interface GridState {
  /** Every row loaded so far, in order. Never contains duplicates. */
  readonly rows: readonly ObjectRow[];
  /** True while a window is in flight. */
  readonly loading: boolean;
  /** The error from the last failed fetch, or null. */
  readonly error: string | null;
  /** True when the server said there is no next page. */
  readonly atEnd: boolean;
  /** The server's total, when it can answer cheaply, else null. */
  readonly totalCount: number | null;
  /**
   * How many rows have actually arrived at least once.
   *
   * Not the same as `rows.length` for the purpose of "is this list done":
   * an empty result leaves `rows.length` at 0, which is indistinguishable from
   * a list that has not started.
   */
  readonly loaded: number;
  /**
   * Has a load been attempted for the current query?
   *
   * This is the flag the component uses to decide whether to fetch, and it has
   * to be a real part of the state rather than something the component infers.
   * When it was missing from this interface the TypeScript check was the only
   * thing that noticed, and it noticed it late: `initialState` set it, the
   * success path rebuilt the state object and dropped it, and the component
   * re-fetched forever. The unit tests all passed because they asserted on
   * `rows`, and a store that returns the right rows while a flag the caller
   * reads reverts looks correct from every angle they can see.
   */
  readonly started: boolean;
}

export const initialState: GridState = {
  rows: [],
  loading: false,
  error: null,
  atEnd: false,
  totalCount: null,
  loaded: 0,
  started: false
};

/** The query the list is showing. Changing any field invalidates the cursor. */
export interface GridQuery {
  readonly filter?: string | null;
  readonly sort?: string | null;
  readonly direction?: 'ASC' | 'DESC' | null;
  readonly tiers?: readonly string[] | null;
}

/**
 * Are two queries the same? Used to decide whether a cursor survives.
 *
 * The tiers matter as much as the filter: a consent-tier change can change
 * which rows exist, so a cursor pointing into the old set is meaningless. This
 * is the spec 14.1 rule showing up in the pagination layer — if the tier list
 * changes and the cursor is kept, the user sees rows they may not see.
 */
export function sameQuery(a: GridQuery, b: GridQuery): boolean {
  const norm = (t: readonly string[] | null | undefined) =>
    t === null || t === undefined ? '' : [...t].sort().join(',');
  return (
    (a.filter ?? '') === (b.filter ?? '') &&
    (a.sort ?? '') === (b.sort ?? '') &&
    (a.direction ?? '') === (b.direction ?? '') &&
    norm(a.tiers) === norm(b.tiers)
  );
}

/**
 * Append `incoming` to `existing`, dropping rows already present.
 *
 * Dedup by id because a keyset cursor can still produce an overlap when the
 * underlying data changes between two requests — a new item that sorts before
 * the cursor, then a delete that shifts things. Duplicate React keys crash
 * with a message that points at the wrong component entirely, so the
 * invariant is enforced here rather than left to the renderer.
 */
export function appendRows(
  existing: readonly ObjectRow[],
  incoming: readonly ObjectRow[]
): readonly ObjectRow[] {
  if (incoming.length === 0) return existing;
  const seen = new Set(existing.map((r) => r.id));
  const out = existing.slice();
  for (const row of incoming) {
    if (seen.has(row.id)) continue;
    seen.add(row.id);
    out.push(row);
  }
  return out;
}

export interface StoreOptions {
  /** Rows per fetch. */
  readonly pageSize?: number;
  /** Injected for tests; defaults to the real client. */
  readonly fetchPage?: (input: PageInput) => Promise<{
    objects: { nodes: readonly ObjectRow[]; pageInfo: { hasNextPage: boolean; endCursor: string | null }; totalCount: number | null };
  }>;
}

const DEFAULT_PAGE_SIZE = 200;

/**
 * A paginating list. Not a Svelte store — a plain object with callbacks, so
 * the component subscribes however it likes and the tests drive it directly.
 */
export class KeysetStore {
  #state: GridState = initialState;
  #cursor: string | null = null;
  #query: GridQuery = {};
  #generation = 0;
  readonly #pageSize: number;
  readonly #fetchPage: NonNullable<StoreOptions['fetchPage']>;

  constructor(opts: StoreOptions = {}) {
    this.#pageSize = opts.pageSize ?? DEFAULT_PAGE_SIZE;
    this.#fetchPage =
      opts.fetchPage ??
      ((input) => fetchObjects(input) as unknown as ReturnType<NonNullable<StoreOptions['fetchPage']>>);
  }

  get state(): GridState {
    return this.#state;
  }

  /**
   * Start a new list. Any in-flight response is invalidated by bumping the
   * generation, so a slow page from the previous filter cannot land in the
   * new one.
   */
  reset(query: GridQuery = {}): void {
    this.#generation += 1;
    this.#query = query;
    this.#cursor = null;
    this.#state = initialState;
  }

  /**
   * Fetch the next window. A no-op when one is in flight or the list is
   * finished, so a scroll handler that fires forty times while the user
   * flings the scrollbar does not fire forty requests.
   */
  async loadMore(): Promise<void> {
    if (this.#state.loading || this.#state.atEnd) return;

    const generation = this.#generation;
    this.#state = { ...this.#state, loading: true, error: null, started: true };

    try {
      const input: PageInput = {
        first: this.#pageSize,
        after: this.#cursor,
        filter: this.#query.filter ?? null,
        sort: this.#query.sort ?? null,
        direction: this.#query.direction ?? null,
        tiers: this.#query.tiers ?? null
      };

      const result = await this.#fetchPage(input);

      // Stale: the query changed or a newer load started while this was in
      // flight. Dropping it is the only correct action.
      if (generation !== this.#generation) return;

      const { nodes, pageInfo, totalCount } = result.objects;
      this.#cursor = pageInfo.endCursor;
      // One append, one length. The previous version called appendRows twice
      // and threw the first result away, which is both wasted work and a trap
      // for the next reader: it reads as if the two calls could differ.
      const rows = appendRows(this.#state.rows, nodes);
      this.#state = {
        ...this.#state,
        rows,
        loading: false,
        error: null,
        atEnd: !pageInfo.hasNextPage,
        totalCount,
        loaded: rows.length
        // `started` is carried over from the spread above. The previous
        // version rebuilt the object from scratch and dropped it, so after the
        // first successful load the component's "has anything been requested
        // yet" test went back to false and called refresh() again -- forever.
        // It only showed up when the whole result fit on one screen: with more
        // rows than fit, the scroll handler's own fetch was what advanced the
        // list, and this loop ran alongside it, so the tests that used 5,000
        // items passed. The store's unit tests never noticed because they
        // assert on `rows`, not on `started`.
      };
    } catch (e) {
      if (generation !== this.#generation) return;
      this.#state = {
        ...this.#state,
        loading: false,
        error: e instanceof Error ? e.message : String(e)
      };
    }
  }

  /**
   * Change the query and reload from the first page.
   *
   * The cursor is dropped, not carried: it points into the old result set and
   * reusing it is the "offset drift" bug wearing a different hat. When only
   * the filter changed and the list is already at the end, the old state is
   * also cleared so the user does not briefly see rows the new filter
   * excludes.
   */
  async setQuery(query: GridQuery): Promise<void> {
    if (sameQuery(this.#query, query)) return;
    this.reset(query);
    await this.loadMore();
  }

  /** Retry after an error. */
  async retry(): Promise<void> {
    this.#state = { ...this.#state, error: null };
    await this.loadMore();
  }
}
