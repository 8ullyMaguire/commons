/**
 * Compile TypeScript for `node --test` using the esbuild that is already
 * installed as a vite dependency.
 *
 * Why not `node --experimental-strip-types`: strip-only mode rejects
 * TypeScript's parameter properties (`constructor(private x: T)`), which are
 * ordinary TypeScript and which this codebase uses. Enabling `--experimental-transform-types`
 * works but is node-version-dependent, and node changes these flags without
 * warning. esbuild is a real compiler, is already a transitive dependency of
 * vite, and has a stable CLI.
 *
 * So: compile the test and the sources it imports into a temp directory, then
 * let `node --test` run plain JavaScript. Import specifiers keep their `.js`
 * extensions, which is why the rewrite below works.
 *
 *   node ./tests/run-tests.mjs            # all tests
 *   node ./tests/run-tests.mjs keyset     # one file
 */

import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const esbuild = join(root, 'node_modules', '.bin', 'esbuild');

const out = mkdtempSync(join(tmpdir(), 'commons-ui-test-'));

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: 'inherit', cwd: root, ...opts });
  if (r.status !== 0) process.exit(r.status ?? 1);
}

try {
  // 1. The test files and every source under src/, compiled together. esbuild
  //    follows the imports, so listing the entry points is enough.
  run(esbuild, [
    'tests/*.test.ts',
    // Recursive, because `tests/helpers/` holds shared factories that a test
    // imports. Without this the helper is never compiled into the temp dir, the
    // importing test file fails to resolve it, and the file fails to LOAD --
    // which `node --test` reports as one failure with no assertion, and the
    // whole file's tests silently disappear from the count.
    'tests/helpers/*.ts',
    'src/lib/**/*.ts',
    '--outdir=' + out,
    '--outbase=.',
    '--platform=node',
    '--format=esm',
    '--target=node20',
    '--sourcemap=inline',
    '--log-level=warning'
  ]);

  // 2. Run them. The output mirrors the source tree (src/, tests/), so the
  //    test entries live in <out>/tests rather than at the top level.
  const testDir = join(out, 'tests');
  const entries = existsSync(testDir)
    ? readdirSync(testDir)
        .filter((f) => f.endsWith('.test.js'))
        .map((f) => join(testDir, f))
    : [];

  if (entries.length === 0) {
    console.error('no compiled tests found in ' + out);
    process.exit(1);
  }
  run(process.execPath, ['--test', ...entries], {
    env: { ...process.env, COMMONS_UI_SRC: join(root, 'src') }
  });
} finally {
  rmSync(out, { recursive: true, force: true });
}
