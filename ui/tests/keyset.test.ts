/**
 * The keyset pagination state machine. No Svelte, no DOM — which is the whole
 * reason it is not in the component.
 *
 * These cover the branches that are hard to reach by hand: a response that
 * arrives after its query changed, a query change that must drop the cursor,
 * and a filter that returns nothing.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  KeysetStore,
  appendRows,
  sameQuery,
  type GridQuery
} from '../src/lib/api/keyset.js';
import type { ObjectRow, PageInput } from '../src/lib/api/client.js';
import { makeRow } from './helpers/row.js';

function row(i: number): ObjectRow {
  return makeRow({ id: `obj-${i}`, title: `Item ${i}` });
}

/** A transport that serves `total` rows in pages, recording every input. */
function makeServer(total: number, pageSize = 50) {
  const calls: PageInput[] = [];
  const fetchPage = async (input: PageInput) => {
    calls.push(input);
    // The cursor here is just the next index, standing in for whatever the
    // server really encodes. The client must treat it as opaque.
    const start = input.after ? Number(input.after) : 0;
    const end = Math.min(start + (input.first ?? pageSize), total);
    const nodes = Array.from({ length: Math.max(0, end - start) }, (_, i) => row(start + i));
    return {
      objects: {
        nodes,
        pageInfo: { hasNextPage: end < total, endCursor: String(end) },
        totalCount: total
      }
    };
  };
  return { calls, fetchPage };
}

describe('appendRows', () => {
  test('appends new rows in order', () => {
    const out = appendRows([row(0), row(1)], [row(2)]);
    assert.deepEqual(
      out.map((r) => r.id),
      ['obj-0', 'obj-1', 'obj-2']
    );
  });

  test('drops a row that is already present', () => {
    // A keyset cursor can still overlap when rows are inserted or deleted
    // between two requests, and a duplicate React key crashes with a message
    // pointing at the wrong component.
    const out = appendRows([row(0), row(1)], [row(1), row(2)]);
    assert.deepEqual(
      out.map((r) => r.id),
      ['obj-0', 'obj-1', 'obj-2']
    );
  });

  test('an empty page leaves the list untouched', () => {
    const before = [row(0)];
    assert.equal(appendRows(before, []), before, 'should be the same array');
  });
});

describe('sameQuery', () => {
  test('two empty queries are the same', () => {
    assert.ok(sameQuery({}, {}));
  });

  test('a different filter is a different query', () => {
    assert.ok(!sameQuery({ filter: 'a' }, { filter: 'b' }));
    assert.ok(!sameQuery({ filter: 'a' }, {}));
  });

  test('a different sort or direction is a different query', () => {
    assert.ok(!sameQuery({ sort: 'date' }, { sort: 'title' }));
    assert.ok(!sameQuery({ direction: 'ASC' }, { direction: 'DESC' }));
  });

  test('tier order does not matter but tier membership does', () => {
    // The tiers arrive from a multi-select, so the same set in a different
    // order is the same view.
    assert.ok(sameQuery({ tiers: ['a', 'b'] }, { tiers: ['b', 'a'] }));
    assert.ok(!sameQuery({ tiers: ['a'] }, { tiers: ['a', 'b'] }));
  });

  test('a tier change invalidates the query even with the same filter', () => {
    // Spec 14.1: a consent-tier change can change which rows exist, so a
    // cursor pointing into the old set is meaningless. This is the rule
    // showing up in the pagination layer — keep the cursor and the user sees
    // rows they may not see.
    assert.ok(!sameQuery({ filter: 'x', tiers: ['public'] }, { filter: 'x', tiers: ['private'] }));
  });

  test('null and undefined are the same absence', () => {
    assert.ok(sameQuery({ filter: null }, {}));
    assert.ok(sameQuery({ tiers: null }, { tiers: undefined }));
  });
});

describe('KeysetStore', () => {
  test('loads the first page and reports the total', async () => {
    const { fetchPage } = makeServer(500);
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.loadMore();
    assert.equal(s.state.rows.length, 50);
    assert.equal(s.state.totalCount, 500);
    assert.equal(s.state.atEnd, false);
    assert.equal(s.state.loading, false);
    assert.equal(s.state.error, null);
  });

  test('appends successive pages without duplicating', async () => {
    const { fetchPage } = makeServer(500);
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.loadMore();
    await s.loadMore();
    await s.loadMore();
    assert.equal(s.state.rows.length, 150);
    const ids = new Set(s.state.rows.map((r) => r.id));
    assert.equal(ids.size, 150, 'no duplicates');
  });

  test('sends a cursor, never an offset', async () => {
    // Spec rule 2 (stash#6455, #6390). The assertion is on the *type* of the
    // request: there is no offset field to send, so this cannot regress into
    // OFFSET pagination without failing.
    const { calls, fetchPage } = makeServer(500);
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.loadMore();
    await s.loadMore();

    assert.equal(calls[0].after, null, 'the first page has no cursor');
    assert.equal(calls[1].after, '50', 'the second page carries the cursor');
    for (const c of calls) {
      assert.equal(
        (c as unknown as Record<string, unknown>).offset,
        undefined,
        'a request must never carry an offset'
      );
    }
  });

  test('reaches the end and stops asking', async () => {
    const { calls, fetchPage } = makeServer(120);
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.loadMore();
    await s.loadMore();
    await s.loadMore();
    assert.equal(s.state.rows.length, 120);
    assert.ok(s.state.atEnd, 'the last page sets atEnd');

    const before = calls.length;
    await s.loadMore();
    assert.equal(calls.length, before, 'no request after the end');
  });

  test('a failed fetch records the error and keeps the rows', async () => {
    let n = 0;
    const fetchPage = async (input: PageInput) => {
      n += 1;
      if (n === 1) {
        return {
          objects: {
            nodes: [row(0)],
            pageInfo: { hasNextPage: true, endCursor: '1' },
            totalCount: 10
          }
        };
      }
      throw new Error('boom');
    };
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.loadMore();
    await s.loadMore();
    assert.equal(s.state.error, 'boom');
    assert.equal(s.state.loading, false);
    assert.equal(s.state.rows.length, 1, 'the rows we had are still there');
  });

  test('retry clears the error and reloads', async () => {
    let n = 0;
    const fetchPage = async () => {
      n += 1;
      if (n === 1) throw new Error('boom');
      return {
        objects: {
          nodes: [row(0)],
          pageInfo: { hasNextPage: false, endCursor: '1' },
          totalCount: 1
        }
      };
    };
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.loadMore();
    assert.ok(s.state.error);
    await s.retry();
    assert.equal(s.state.error, null);
    assert.equal(s.state.rows.length, 1);
  });

  test('an empty result is not an error', async () => {
    const fetchPage = async () => ({
      objects: { nodes: [], pageInfo: { hasNextPage: false, endCursor: null }, totalCount: 0 }
    });
    const s = new KeysetStore({ fetchPage });
    await s.loadMore();
    assert.equal(s.state.rows.length, 0);
    assert.equal(s.state.error, null);
    assert.ok(s.state.atEnd);
  });

  test('a second load while one is in flight is a no-op', async () => {
    // A flung scrollbar fires dozens of scroll events; forty concurrent
    // requests for the same window is forty times the work and forty chances
    // to duplicate a row.
    let calls = 0;
    const gate = deferred();
    const fetchPage = async () => {
      calls += 1;
      await gate.promise;
      return {
        objects: { nodes: [row(0)], pageInfo: { hasNextPage: false, endCursor: '1' }, totalCount: 1 }
      };
    };
    const s = new KeysetStore({ fetchPage });
    const a = s.loadMore();
    const b = s.loadMore();
    gate.settle();
    await Promise.all([a, b]);
    assert.equal(calls, 1, 'only one request went out');
  });

  test('a response that arrives after the query changed is dropped', async () => {
    // The symptom this prevents is invisible otherwise: a list containing
    // items the current filter excludes, with nothing in the console.
    const slowGate = deferred();
    const fetchPage = async (input: PageInput) => {
      if (input.filter === 'slow') {
        await slowGate.promise;
        return {
          objects: {
            nodes: [row(99)],
            pageInfo: { hasNextPage: false, endCursor: '100' },
            totalCount: 100
          }
        };
      }
      return {
        objects: {
          nodes: [row(0)],
          pageInfo: { hasNextPage: false, endCursor: '1' },
          totalCount: 1
        }
      };
    };
    const s = new KeysetStore({ fetchPage, pageSize: 50 });

    const slow = s.setQuery({ filter: 'slow' });
    // The user types again before the first request comes back.
    const fast = s.setQuery({ filter: 'fast' });
    slowGate.settle();
    await Promise.all([slow, fast]);

    assert.equal(s.state.rows.length, 1);
    assert.equal(s.state.rows[0].id, 'obj-0', 'the stale response did not land');
  });

  test('a query change drops the cursor and starts from the first page', async () => {
    const { calls, fetchPage } = makeServer(500);
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.loadMore();
    await s.loadMore();
    assert.equal(s.state.rows.length, 100);

    await s.setQuery({ filter: 'x' });
    assert.equal(s.state.rows.length, 50, 'only the first page of the new query');
    const last = calls[calls.length - 1];
    assert.equal(last.after, null, 'the old cursor was not reused');
    assert.equal(last.filter, 'x');
  });

  test('setting the same query does not refetch', async () => {
    const { calls, fetchPage } = makeServer(500);
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.setQuery({ filter: 'x' });
    const n = calls.length;
    await s.setQuery({ filter: 'x' });
    assert.equal(calls.length, n, 'an identical query is a no-op');
  });

  test('reset clears everything', async () => {
    const { fetchPage } = makeServer(500);
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.loadMore();
    s.reset({ filter: 'y' });
    assert.equal(s.state.rows.length, 0);
    assert.equal(s.state.totalCount, null);
    assert.equal(s.state.atEnd, false);
    assert.equal(s.state.error, null);
  });

  test('a null total falls back to the loaded count rather than showing zero', async () => {
    // Some queries cannot answer a COUNT cheaply. Reporting 0 there would put
    // "0 items" in the toolbar for a library full of results.
    const fetchPage = async () => ({
      objects: { nodes: [row(0), row(1)], pageInfo: { hasNextPage: true, endCursor: '2' }, totalCount: null }
    });
    const s = new KeysetStore({ fetchPage, pageSize: 50 });
    await s.loadMore();
    assert.equal(s.state.totalCount, null, 'the server said it does not know');
    assert.equal(s.state.rows.length, 2, 'and we still have rows');
  });

  test('handles a 5,000 item library in pages of 200', async () => {
    const { fetchPage } = makeServer(5_000);
    const s = new KeysetStore({ fetchPage, pageSize: 200 });
    while (!s.state.atEnd) await s.loadMore();
    assert.equal(s.state.rows.length, 5_000);
    const ids = new Set(s.state.rows.map((r) => r.id));
    assert.equal(ids.size, 5_000);
  });
});

/**
 * `started` must survive a successful load.
 *
 * The component decides whether to fetch by testing `!state.started`, so
 * losing the flag on success makes it fetch forever. The store's other
 * assertions all look at `rows`, which is why this went unnoticed: a store
 * that returns the right rows while a flag that only the component reads
 * quietly reverts to its initial value looks correct from every angle the
 * unit tests can see.
 */
test('started stays true after a successful load', async () => {
  const calls: PageInput[] = [];
  const store = new KeysetStore({
    fetchPage: async (input) => {
      calls.push(input);
      return {
        objects: {
          nodes: [row(0)],
          pageInfo: { hasNextPage: false, endCursor: '1' },
          totalCount: 1
        }
      };
    }
  });

  await store.loadMore();
  assert.equal(store.state.started, true, 'started was true while loading');
  assert.equal(store.state.rows.length, 1);

  // The next call is refused because the list is finished, not because the
  // component forgot it had already asked.
  await store.loadMore();
  assert.equal(store.state.started, true, 'started reverted after a successful load');
  assert.equal(calls.length, 1, 'the finished list was re-fetched');
});

/**
 * The same flag, after an error, because the error path is the other way the
 * component learns it has already asked.
 */
test('started stays true after a failed load', async () => {
  let n = 0;
  const store = new KeysetStore({
    fetchPage: async () => {
      n += 1;
      if (n === 1) throw new Error('the database is on fire');
      return { objects: { nodes: [row(0)], pageInfo: { hasNextPage: false, endCursor: '1' }, totalCount: 1 } };
    }
  });

  await store.loadMore();
  assert.match(store.state.error ?? '', /on fire/);
  assert.equal(store.state.started, true, 'started reverted after a failure');

  await store.retry();
  assert.equal(store.state.error, null);
  assert.equal(store.state.started, true, 'started reverted after a successful retry');
  assert.equal(store.state.rows.length, 1);
});

/**
 * A promise plus the function that settles it.
 *
 * The tests need to hold a request open, drive the store while it is in
 * flight, and only then let it finish -- that is the only way to reach the
 * "a response arrives after its query changed" and "a second request is
 * refused" branches. Writing that inline meant declaring `let release:
 * (() => void) | null = null` and calling `release?.()`, which TypeScript
 * rejects: it narrows the variable to `null` because every assignment happens
 * inside a callback it does not follow, so the call is "not callable". The
 * assertion moves that knowledge to where it is true, once, instead of
 * spreading it over every call site.
 */
function deferred<T = void>() {
  let settle!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((res) => {
    settle = res;
  });
  return { promise, settle };
}
