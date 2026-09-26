/**
 * The "no second private API" rule, as a test (spec 3.3).
 *
 * The ticket says the desktop shell is just a browser pointed at a host, so
 * the UI must have exactly one transport. That is the kind of rule which is
 * true on the day it is written and quietly false six months later, because
 * adding a `fetch('/api/thumbs')` to one component is a two-line change that
 * nothing complains about.
 *
 * So it is a test: grep the source tree for `fetch(`, `XMLHttpRequest`, and
 * `EventSource`/`WebSocket`, and allow them only in the one transport module.
 * A new call site fails here rather than in a review that nobody reads.
 *
 * Also covered: the pagination request type has no `offset` field, which is
 * rule 2 enforced by the type system instead of by this grep. The grep would
 * catch a REST call, not a GraphQL query that used `offset` as a variable
 * name, and the type is what actually closes that.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, dirname, relative, extname } from 'node:path';
import { fileURLToPath } from 'node:url';

// This test walks the source tree to GREP it, so it needs the real sources
// and not the compiled output sitting next to it. The runner compiles into a
// temp directory, so `../` from here is that temp directory.
//
// The source root arrives in an env var rather than being inferred. Walking
// upward to find it looks robust and is not: the temp directory is outside the
// repository, so the walk runs off the top of the filesystem and throws. A
// grep that silently checks nothing is worse than no test, so the runner sets
// it and this asserts it is set.
const src = process.env.COMMONS_UI_SRC;
if (!src) {
  throw new Error('COMMONS_UI_SRC is not set; run the tests via tests/run-tests.mjs');
}

/** The one file allowed to talk to the network. */
const ALLOWED = new Set(['lib/api/client.ts']);

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...walk(p));
    else if (['.ts', '.svelte', '.js'].includes(extname(p))) out.push(p);
  }
  return out;
}

const files = walk(src);

describe('the no-second-API invariant', () => {
  test('there are source files to check', () => {
    // A grep that matches nothing because the path is wrong is worse than no
    // test: it passes and checks nothing.
    assert.ok(files.length >= 6, `only found ${files.length} source files under src/`);
    assert.ok(files.some((f) => f.endsWith('client.ts')), 'the transport is missing');
  });

  test('no module outside the transport calls fetch, XHR, or a socket', () => {
    const banned = [
      /\bfetch\s*\(/,
      /XMLHttpRequest/,
      /new\s+WebSocket\b/,
      /new\s+EventSource\b/,
      /navigator\.sendBeacon/
    ];
    const offenders: string[] = [];
    for (const f of files) {
      const rel = relative(src, f);
      if (ALLOWED.has(rel)) continue;
      const text = readFileSync(f, 'utf8');
      for (const re of banned) {
        if (re.test(text)) offenders.push(`${rel}: ${re}`);
      }
    }
    assert.deepEqual(
      offenders,
      [],
      `network access outside the transport (${[...ALLOWED].join(', ')}):\n` +
        offenders.join('\n')
    );
  });

  test('the transport is the only file that names a remote host', () => {
    // A hardcoded absolute URL anywhere else means a second deployment shape:
    // a URL that only resolves in the developer's browser.
    //
    // `.d.ts` files are skipped because they carry documentation links, and
    // the first version of this test failed on `app.d.ts` matching
    // https://svelte.dev/docs in a comment. A grep test that trips over a
    // documentation URL trains people to ignore it, so the exclusions are
    // explicit rather than smuggled into the regex.
    const offenders: string[] = [];
    for (const f of files) {
      const rel = relative(src, f);
      if (ALLOWED.has(rel) || rel.endsWith('.d.ts')) continue;
      // Strip comments first: a doc link in a comment is not a call site.
      const code = readFileSync(f, 'utf8')
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .replace(/(^|[^:])\/\/.*$/gm, '$1');
      if (/https?:\/\/(?!127\.0\.0\.1|localhost)/.test(code)) {
        offenders.push(rel);
      }
    }
    assert.deepEqual(offenders, [], `absolute URLs outside the transport: ${offenders}`);
  });
});

describe('never OFFSET (spec rule 2, stash#6455 and #6390)', () => {
  test('the page request type has no offset field', () => {
    const client = readFileSync(join(src, 'lib/api/client.ts'), 'utf8');
    const m = client.match(/export interface PageInput \{([\s\S]*?)\n\}/);
    assert.ok(m, 'PageInput not found');
    assert.ok(
      !/\boffset\b/i.test(m[1]),
      'PageInput must not have an offset: OFFSET pagination is forbidden by rule 2'
    );
  });

  test('no query or component mentions an offset', () => {
    const offenders: string[] = [];
    for (const f of files) {
      const rel = relative(src, f);
      // Comments in client.ts and keyset.ts explain why offset is forbidden,
      // so only flag it where it would be USED.
      if (rel === 'lib/api/client.ts' || rel === 'lib/api/keyset.ts') continue;
      if (/\boffset\s*[:=]|\$offset\b/.test(readFileSync(f, 'utf8'))) offenders.push(rel);
    }
    assert.deepEqual(offenders, [], `offset pagination in: ${offenders}`);
  });
});
