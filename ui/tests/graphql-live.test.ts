/**
 * T-P6-008 step 6: the REAL client against a REAL server.
 *
 * Every other test in this directory injects a transport. That is the right
 * way to test the client's own logic — parsing, the error throw, cursor
 * handling — and it is also how a wire-format disagreement hides: both sides are
 * written by whoever is writing the test, so `camelCase` becomes `snake_case`
 * on the wire and both ends agree, and the shipped UI cannot read the shipped
 * server. A round-trip between two implementations you control proves the two
 * agree, never that either is right.
 *
 * So this file starts the actual server binary on a real port and points the
 * real `query()` at it over real HTTP. Nothing is stubbed. If this passes, the
 * thing a user runs works; if it fails, the disagreement is between two
 * artifacts that actually ship, and the message says which field.
 *
 * # Why a spawned binary and not an in-process server
 *
 * A test that imports the server would share the client's idea of the types at
 * the TypeScript level only — the wire is JSON in both cases, so the risk being
 * tested (a field name, a casing, an operation name) survives compilation
 * either way. The binary is the artifact the user runs, and it is the only way
 * this test can fail for the reason it exists.
 *
 * # It skips, loudly, when the binary is absent
 *
 * Unlike the Postgres parity tests, which refuse to skip — and are right to,
 * because the ticket IS the equality — this one skips, and says so. A parity
 * test that cannot run is a parity test that never runs. But a test that skips
 * *silently* is worse than either, so the skip prints the path it looked for.
 * Anyone reading CI output can see the coverage is missing rather than having
 * to notice an absence.
 */

import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { after, before, describe, it } from 'node:test';
import assert from 'node:assert/strict';

import {
  query,
  setTransport,
  GraphQLError,
  OBJECTS_QUERY,
  fetchObjects,
  fetchTags
} from '../src/lib/api/client.js';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

/**
 * The client's own document, IMPORTED rather than retyped.
 *
 * A copy in this file would be a second statement of the query, and the two
 * would drift: someone changes `OBJECTS_QUERY` to select a fourteenth field and
 * this test keeps passing against the old shape, which is precisely the drift
 * it exists to catch. Importing means a rename or a changed selection set
 * breaks THIS file, loudly, which is the behaviour a test of a shared contract
 * should have.
 */
const OBJECTS = OBJECTS_QUERY;

/** Find the server binary, or return null. */
function findBinary(): string | null {
  const target = process.env.CARGO_TARGET_DIR ?? join(repoRoot, 'target');
  const name = 'commons-server';
  for (const profile of ['debug', 'release']) {
    const p = join(target, profile, name);
    if (existsSync(p)) return p;
  }
  return null;
}

/** A port the OS says is free. */
async function freePort(): Promise<number> {
  const net = await import('node:net');
  return new Promise((res, rej) => {
    const srv = net.createServer();
    srv.on('error', rej);
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address();
      const port = typeof addr === 'object' && addr ? addr.port : 0;
      srv.close(() => res(port));
    });
  });
}

let child: ChildProcess | null = null;
let baseUrl = '';
let dataDir = '';
const restore: Array<() => void> = [];

const bin = findBinary();

before(async () => {
  if (!bin) {
    console.log(
      `\n  SKIP: no commons-server binary found under ${process.env.CARGO_TARGET_DIR ?? join(repoRoot, 'target')}.\n` +
      '        Build it with: cargo build -p commons-server\n' +
      '        (This file is the only test that needs the binary, which is why\n' +
      '        it skips rather than failing the suite.)\n'
    );
    return;
  }

  dataDir = mkdtempSync(join(tmpdir(), 'commons-e2e-'));
  const port = await freePort();
  baseUrl = `http://127.0.0.1:${port}`;

  // `--mode library` opens SQLite inside the data dir. Without it the server
  // picks its own default, which on a developer machine is the REAL library
  // directory -- and a test that reads someone's actual library is a test that
  // must never be allowed to fail silently, because "it passed" would then mean
  // "it saw your files".
  child = spawn(
    bin,
    [
      '--mode', 'library',
      '--bind', `127.0.0.1:${port}`,
      '--data-dir', dataDir,
      '--public-base-url', `http://127.0.0.1:${port}`
    ],
    {
    cwd: repoRoot,
      stdio: ['ignore', 'pipe', 'pipe']
    }
  );

  let log = '';
  child.stdout?.on('data', (d) => (log += d));
  child.stderr?.on('data', (d) => (log += d));

  // Poll the health route rather than sleeping a fixed interval: a fixed sleep
  // is either too short (a flake) or too long (a slow suite), and the failure
  // mode of the first is a test that fails on a busy machine.
  const deadline = Date.now() + 30_000;
  for (;;) {
    if (child.exitCode !== null) {
      throw new Error(`the server exited with ${child.exitCode} before it was ready:\n${log}`);
    }
    try {
      const res = await fetch(`${baseUrl}/healthz`);
      if (res.ok) break;
    } catch {
      /* not up yet */
    }
    if (Date.now() > deadline) {
      throw new Error(`the server never became healthy in 30s:\n${log}`);
    }
    await new Promise((r) => setTimeout(r, 100));
  }

  // Point the real client at the real server, and put it back afterwards.
  restore.push(
    setTransport((body, signal) =>
      fetch(`${baseUrl}/graphql`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body,
        signal
      })
    )
  );
});

after(async () => {
  for (const fn of restore) fn();
  if (child && child.exitCode === null) {
    child.kill('SIGTERM');
    // Give it a moment to close the SQLite file, or the tempdir removal below
    // races the process's own cleanup on a slow disk.
    await new Promise((r) => setTimeout(r, 300));
  }
  if (dataDir) rmSync(dataDir, { recursive: true, force: true });
});

describe('the real client against a real server', () => {
  it('answers the client\'s own Objects document', async (t) => {
    if (!bin) return t.skip('no commons-server binary');

    const res = await query<{
      objects: {
        totalCount: number | null;
        pageInfo: { hasNextPage: boolean; hasPreviousPage: boolean; startCursor: string | null; endCursor: string | null };
        nodes: { id: string; kind: string; title: string }[];
      };
    }>(OBJECTS, { input: { first: 10, after: null } });

    // The shape, not the contents. A fresh library is empty, and an assertion
    // about rows would only be asserting about the fixture.
    assert.ok(Array.isArray(res.objects.nodes), 'nodes must be an array');
    assert.equal(typeof res.objects.pageInfo.hasNextPage, 'boolean');
    assert.equal(res.objects.pageInfo.hasPreviousPage, false);
    assert.equal(res.objects.totalCount, null);
  });

  it('serves the field NAMES the client selects, in the casing it expects', async (t) => {
    if (!bin) return t.skip('no commons-server binary');

    // The load-bearing test in this file. A server answering `cover_path` where
    // the client asked for `coverPath` returns HTTP 200 with a document that
    // looks right in a log and is unreadable in the browser — and since the
    // other tests in this directory inject their own transport, nothing else in
    // the suite would notice.
    const res = await fetch(`${baseUrl}/graphql`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ query: OBJECTS, variables: { input: { first: 1, after: null } } })
    });
    const body: any = await res.json();
    assert.equal(body.errors, undefined, `server errored: ${JSON.stringify(body.errors)}`);

    const node = body.data.objects.nodes[0];
    // `nodes` is empty on a fresh library, so ask the connection for the keys
    // that must be present regardless — and, once there is a row, for the row's
    // keys. The connection shape is checked on the first test; here the point
    // is the camelCase names.
    const keys = Object.keys(body.data.objects);
    for (const expected of ['totalCount', 'pageInfo', 'nodes']) {
      assert.ok(keys.includes(expected), `connection must carry \`${expected}\`, got ${keys}`);
    }
    if (node) {
      for (const expected of ['id', 'kind', 'title']) {
        assert.ok(expected in node, `node must carry \`${expected}\`, got ${Object.keys(node)}`);
      }
    }
  });

  it('refuses an after cursor with a message the client can show', async (t) => {
    if (!bin) return t.skip('no commons-server binary');

    // `query()` throws the FIRST error, and that message is what a user sees.
    // Asserting on it here is the point: a refusal whose message is empty
    // renders as a blank toast and is indistinguishable from a network failure.
    await assert.rejects(
      () => query(OBJECTS, { input: { first: 10, after: 'eyJ2IjoxfQ' } }),
      (e: unknown) => {
        assert.ok(e instanceof GraphQLError, `expected a GraphQLError, got ${e}`);
        assert.match((e as Error).message, /after/);
        return true;
      }
    );
  });

  it('serves BulkTags, whose document has no variables at all', async (t) => {
    if (!bin) return t.skip('no commons-server binary');

    // A document with no `variables` key is a case the injected-transport tests
    // cannot produce, because they build the body themselves and always pass
    // `{}`. The real client sends `{}` too, but the server must cope with a
    // document that declares no variable definitions at all.
    const res = await fetch(`${baseUrl}/graphql`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ query: 'query BulkTags { tags { id name } }' })
    });
    const body: any = await res.json();
    assert.equal(body.errors, undefined, `server errored: ${JSON.stringify(body.errors)}`);
    assert.ok(Array.isArray(body.data.tags));
  });

  it("runs the client's OWN fetchObjects against the server", async (t) => {
    if (!bin) return t.skip('no commons-server binary');

    // The strongest assertion in this file: the exported function the UI
    // actually calls, the exported document it actually sends, the real
    // transport, a real server. Nothing here is written by the test except the
    // base URL, so a disagreement about a field name, an operation name, a
    // variable shape or an error path has nowhere to hide.
    // `fetchObjects` returns `{ objects: Connection<ObjectRow> }` -- the GraphQL
    // DATA shape, wrapper included. Asserting on `res.nodes` instead of
    // `res.objects.nodes` would have been a test that passes by throwing
    // `undefined is not an array` -- a failure that looks like a server bug and
    // is actually a misread of the client's own type.
    const { objects } = await fetchObjects({ first: 10, after: null });

    assert.ok(Array.isArray(objects.nodes), `nodes must be an array, got ${JSON.stringify(objects)}`);
    assert.equal(typeof objects.pageInfo.hasNextPage, 'boolean');
    // A fresh library: no rows, no next page. Asserting the emptiness is not
    // the point -- it is that the call RETURNED rather than throwing, which is
    // what a wire disagreement would prevent.
    assert.equal(objects.nodes.length, 0, 'a fresh temp library has no objects');
    assert.equal(objects.pageInfo.hasNextPage, false);
    assert.equal(objects.totalCount, null);
  });

  it("runs the client's OWN fetchTags against the server", async (t) => {
    if (!bin) return t.skip('no commons-server binary');

    // `BulkTags` declares no variable definitions at all, so it is the one
    // document where "does the client send `variables` or nothing?" is a real
    // question rather than a distinction without a difference. The injected
    // tests cannot ask it: they construct the body themselves.
    // Also the data shape, wrapper included: `{ tags: [...] }`.
    const { tags } = await fetchTags();
    assert.ok(Array.isArray(tags), `tags must be an array, got ${JSON.stringify(tags)}`);
  });

  it('turns an unknown operation into a GraphQLError, not a transport failure', async (t) => {
    if (!bin) return t.skip('no commons-server binary');

    // The distinction the client's own comment insists on: a 200 carrying
    // `errors` is "the server said no", a network failure is "the server is
    // not there", and the two need different responses from the UI.
    await assert.rejects(
      () => query('query NotARealOperation { nope }'),
      (e: unknown) => {
        assert.ok(e instanceof GraphQLError);
        assert.match((e as Error).message, /NotARealOperation/);
        assert.doesNotMatch((e as Error).message, /^network:/);
        return true;
      }
    );
  });
});
