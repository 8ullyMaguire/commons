// @ts-check
/**
 * Mutation-check the create result model.
 *
 * The claims worth attacking are the ones a user reads: a count that collapses
 * "already there" into "created", a blocked run that reads as "nothing
 * happened", and a button that can be pressed while an import is in flight.
 */
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, rmSync, copyFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

// scripts/ -> repo root. Three dirnames, not two: the file lives one level
// deeper than the shell scripts beside it.
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const SRC = join(root, 'ui/src/lib/api/create.ts');
const BACKUP = mkdtempSync(join(tmpdir(), 'create-mut-'));
const saved = join(BACKUP, 'create.ts');
copyFileSync(SRC, saved);

function verdict() {
  const r = spawnSync('node', ['./tests/run-tests.mjs', 'create'], {
    cwd: join(root, 'ui'),
    encoding: 'utf8'
  });
  const out = (r.stdout ?? '') + (r.stderr ?? '');
  // The result line is read FIRST: the runner prints `error: test failed` at
  // the end of every failed run, so a compile check placed earlier converts
  // every kill into "did not compile".
  if (/^ℹ fail [1-9]/m.test(out)) return 'killed';
  if (/^ℹ fail 0$/m.test(out)) return '*** SURVIVED ***';
  if (/^error(\[|:)/m.test(out)) return 'DID NOT COMPILE -- malformed mutation, not a survivor';
  return 'no result; tail:\n' + out.split('\n').slice(-6).join('\n');
}

const MUTATIONS = [
  {
    label: 'the already-there count is dropped from the summary',
    from: "    `${plural(created, 'row')} created, ` +\n    `${existing === 1 ? '1 was' : `${existing} were`} already in your library.`",
    to: "    `${plural(created, 'row')} created.`"
  },
  {
    label: 'an all-duplicates run renders as a success count',
    from: "    return existing === 1\n      ? 'That row was already in your library.'\n      : `${existing} rows were already in your library.`;",
    to: "    return `${plural(created, 'row')} created.`;"
  },
  {
    label: 'blocked and done collapse into one state',
    from: "  if (outcome.refused > 0) {\n    return outcome.created > 0 ? 'partial' : 'blocked';\n  }\n  return 'done';",
    to: "  if (outcome.refused > 0 && outcome.created === 0) {\n    return 'done';\n  }\n  return 'done';"
  },
  {
    label: 'a partial run drops the refused count',
    from: "    return `${plural(created, 'row')} created, ${plural(refused, 'row')} refused.`;",
    to: "    return `${plural(created, 'row')} created.`;"
  },
  {
    label: 'the button stays live while an import is running',
    from: "  if (result.state === 'running') return false;\n  return draftCount > 0;",
    to: "  return draftCount > 0;"
  },
  {
    label: 'the button names the pasted count, not what will be created',
    from: "  const willCreate = Math.min(draftCount, distinctIds);",
    to: "  const willCreate = draftCount;"
  },
  {
    label: 'a stale distinct count renders a negative duplicate count',
    from: "  if (distinctIds > draftCount) return 0;",
    to: ""
  }
];

const original = readFileSync(saved, 'utf8');
console.log(`baseline, source UNMUTATED -- want green: ${verdict()}`);
console.log();

for (const [i, m] of MUTATIONS.entries()) {
  if (!original.includes(m.from)) {
    console.log(`${m.label}: COULD NOT APPLY -- the text was not found`);
    continue;
  }
  writeFileSync(SRC, original.replace(m.from, m.to));
  console.log(`${m.label}: ${verdict()}`);
}

writeFileSync(SRC, original);
console.log();
console.log(`restored, source UNMUTATED -- want green: ${verdict()}`);
rmSync(BACKUP, { recursive: true, force: true });
