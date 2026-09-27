#!/usr/bin/env node
/**
 * Mutation-check the folder tree view.
 *
 * The claims worth attacking are the ones a user notices being wrong: a folder
 * that silently disappears, a tree that re-sorts when something is renamed, a
 * breadcrumb trail that hangs on a cycle.
 *
 * Each mutation is a real edit to `folder-tree.ts`, run through the whole UI
 * suite, and reverted. A mutant that survives means the tests do not pin the
 * claim it is named for.
 *
 * Run with `--list` to see the mutants without running them.
 */

import { readFileSync, writeFileSync, mkdtempSync, rmSync, cpSync, symlinkSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

const ROOT = resolve(dirname(new URL(import.meta.url).pathname), '..');
const UI = join(ROOT, 'ui');
const TARGET = join(UI, 'src/lib/api/folder-tree.ts');

/** @type {{name: string, from: string|RegExp, to: string, note: string}[]} */
const MUTANTS = [
  {
    name: 'a child is not placed again from the root loop',
    from: '      if (parent !== null && reachable(parent.id)) continue;',
    to: '      if (false) continue;',
    note: 'Placing a child again renders every folder twice: once nested, once loose.',
  },
  {
    name: 'an orphan is hoisted to the roots',
    from: '      if (parent === null) orphans.push(f);',
    to: '      if (parent === null) continue;',
    note: 'A folder whose parent was deleted must still be visible and fixable.',
  },
  {
    name: 'an orphan is reported',
    from: '      if (parent === null) orphans.push(f);',
    to: '      if (false) orphans.push(f);',
    note: 'A folder that vanishes with nothing said is the worst outcome a navigator has.',
  },
  {
    name: 'a self-parented folder is detected as a cycle',
    from: '    if (f.parentId === f.id) {\n      cycles.push(f.id);\n      return null;\n    }',
    to: '    if (f.parentId === "never") {\n      cycles.push(f.id);\n      return null;\n    }',
    note: 'A self-parented folder must be reported, and the fix for one is a one-liner a user will do.',
  },
  {
    name: 'the root loop detects an unreachable parent as a cycle',
    from: '      if (parent !== null && reachable(parent.id)) continue;',
    to: '      if (parent !== null) continue;',
    note: 'a -> b -> a has no root, so without this both folders are placed by nobody and both vanish.',
  },
  {
    name: 'reachable treats returning to the start as reachable',
    from: '      if (seen.has(cursor)) return false;',
    to: '      if (seen.has(cursor)) return true;',
    note: 'A chain that returns to its start is a cycle, and a cycle has no root.',
  },
  {
    name: 'reachable treats a missing parent as reachable',
    from: '      if (next === undefined) return false; // missing parent',
    to: '      if (next === undefined) return true; // missing parent',
    note: 'A missing parent is not a root.',
  },
  {
    name: 'the tree terminates on a long chain',
    from: '    if (depth > 64) {',
    to: '    if (depth > 1) {',
    note: 'A legitimate 20-deep chain must not be cut as if it were a cycle.',
  },
  {
    name: 'siblings sort by name rather than position',
    from: '  if (a.position !== b.position) return a.position - b.position;',
    to: '  return a.name < b.name ? -1 : a.name > b.name ? 1 : 0;',
    note: 'A tree that re-sorts when something is renamed moves rows under the cursor.',
  },
  {
    name: 'the id tiebreak is dropped',
    from: '  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;',
    to: '  return 0;',
    note: 'Without a total order, a listing can repeat or skip a row across a page.',
  },
  {
    name: 'descendantCount counts children only',
    from: '      descendantCount: childNodes.reduce((n, c) => n + 1 + c.descendantCount, 0),',
    to: '      descendantCount: childNodes.length,',
    note: '"3 items" under a folder with 2 children and 1 grandchild is the number a user can check.',
  },
  {
    name: 'notify is not inherited by ancestors',
    from: '      notifies: f.notify || childNodes.some((c) => c.notifies),',
    to: '      notifies: f.notify,',
    note: 'The dot must be on the folder the user has to open to find the notification.',
  },
  {
    name: 'the depth is not recorded',
    from: '    if (visited.has(f.id)) return null;',
    to: '    if (visited.has(f.id)) return null;\n    if (depth === 99) return null;',
    note: 'Indentation is derived from depth; a wrong depth indents a leaf as if it had children.',
  },
  {
    name: 'flatten stops at the top level',
    from: '      walk(n.children);',
    to: '      /* no recursion */',
    note: 'Depth-first order is what the sidebar renders rows in and what a keyboard traversal moves through.',
  },
  {
    name: 'the breadcrumb trail does not stop on a cycle',
    from: '      if (seen.has(cursor)) break;',
    to: '      if (false) break;',
    note: 'A tree cycle is not something the filter resolver can see, because resolution walks filters and not parents.',
  },
  {
    name: 'a missing ancestor ends the trail with an error rather than a gap',
    from: '      out.push(null);\n      break;',
    to: '      out.push(null);\n      return out.filter((x) => x !== null);',
    note: 'A trail that stops early is better than one that fails to appear at all.',
  },
  {
    name: 'the open folder is not forced expanded',
    from: '  if (nav.open === id) return true;\n  if (id === nav.open) return true;',
    to: '  if (nav.open === id) return true;',
    note: 'A row that cannot be opened further is a dead end.',
  },
  {
    name: 'opening a folder does not expand its ancestors',
    from: '  const ancestors = trail.slice(1).filter((f): f is Folder => f !== null).map((f) => f.id);',
    to: '  const ancestors: string[] = [];',
    note: 'Opening a deep folder and having the pane collapse to one row is how a user gets lost.',
  },
  {
    name: 'going up from a root goes to the root',
    from: '  if (current === undefined || current.parentId === null) {\n    // Either the folder is gone or it is a root: either way the tree root is\n    // the nearest thing above it that exists.\n    return ROOT_NAV;\n  }',
    to: '  if (current === undefined) return ROOT_NAV;',
    note: 'A root folder with a null parent has nothing above it; going up must stay put.',
  },
  {
    name: 'search does not match the filter',
    from: "    .filter((f) => f.name.toLowerCase().includes(needle) || f.filter.toLowerCase().includes(needle))",
    to: '    .filter((f) => f.name.toLowerCase().includes(needle))',
    note: 'A user who remembers a folder as "the one that looks for tagged x" is searching for it by that.',
  },
  {
    name: 'an empty search returns nothing',
    from: "  if (needle === '') return [...folders].sort(compareFolders);",
    to: "  if (needle !== '') return [...folders].sort(compareFolders);\n  return [];",
    note: 'Clearing the box must show the tree again.',
  },
  {
    name: 'keeping ancestors keeps only the matches',
    from: '  return folders.filter((f) => wanted.has(f.id)).sort(compareFolders);',
    to: "  return folders.filter((f) => matches.some((m) => m.id === f.id)).sort(compareFolders);",
    note: 'A hit whose parents are hidden cannot be opened in context.',
  },
  {
    name: 'an unknown count renders as zero',
    from: "  return n === null ? '—' : String(n);",
    to: '  return String(n ?? 0);',
    note: 'A column of zeroes before the counts land looks like an empty library.',
  },
  {
    name: 'the tree root gets a filter label',
    from: '  if (folderId === null) return null;',
    to: '  if (folderId === null) return "all";',
    note: '"Everything" is the answer at the root and there is no filter to name.',
  },
  {
    name: 'isCurrentCrumb marks the first crumb current',
    from: '  return i === trail.length - 1;',
    to: '  return i === 0;',
    note: 'The last crumb is the one the user is in; the others are links back.',
  },
];

/**
 * Replace `from` with `to`. Returns null when `from` is absent.
 *
 * A STRING pattern is matched literally, not as a regex: these are source lines
 * full of metacharacters, and a regex built from one does not match it, so the
 * mutant reports STALE and the claim it was meant to check is never tested.
 */
function substitute(src, from, to) {
  if (from instanceof RegExp) {
    const flags = from.flags.includes('g') ? from.flags : from.flags + 'g';
    const re = new RegExp(from.source, flags);
    if (!re.test(src)) return null;
    return src.replace(re, () => to);
  }
  if (!src.includes(from)) return null;
  return src.split(from).join(to);
}

function runSuite(cwd) {
  try {
    execFileSync(process.execPath, ['./tests/run-tests.mjs'], {
      cwd,
      stdio: 'pipe',
      env: { ...process.env },
      timeout: 600_000,
    });
    return { pass: true, out: '' };
  } catch (e) {
    return { pass: false, out: `${e.stdout ?? ''}${e.stderr ?? ''}` };
  }
}

if (process.argv.includes('--list')) {
  for (const m of MUTANTS) console.log(`${m.name}\n  ${m.note}`);
  process.exit(0);
}

const original = readFileSync(TARGET, 'utf8');

// A red baseline makes every surviving mutant meaningless, so refuse to run.
console.log('baseline…');
const base = runSuite(UI);
if (!base.pass) {
  console.error('BASELINE IS RED — every result below would be meaningless. Fix the suite first.');
  console.error(base.out.split('\n').filter((l) => /✖|fail [1-9]/.test(l)).slice(0, 20).join('\n'));
  process.exit(1);
}
console.log('baseline green.\n');

const work = mkdtempSync(join(tmpdir(), 'folder-mut-'));
const workUi = join(work, 'ui');
cpSync(UI, workUi, {
  recursive: true,
  filter: (src) => !src.includes('node_modules') && !src.includes('/.svelte-kit'),
});
try {
  symlinkSync(join(UI, 'node_modules'), join(workUi, 'node_modules'));
} catch {
  /* best effort */
}
cpSync(join(UI, 'tests'), join(workUi, 'tests'), { recursive: true });

const workTarget = join(workUi, 'src/lib/api/folder-tree.ts');
let killed = 0;
let survived = 0;
let stale = 0;
const survivors = [];

for (const [i, m] of MUTANTS.entries()) {
  const mutated = substitute(original, m.from, m.to);
  if (mutated === null) {
    console.log(`  STALE  ${i + 1}/${MUTANTS.length}  ${m.name} — the source no longer matches`);
    stale += 1;
    continue;
  }
  if (mutated === original) {
    console.log(`  NO-OP  ${i + 1}/${MUTANTS.length}  ${m.name} — the edit changed nothing`);
    survived += 1;
    survivors.push(m);
    continue;
  }
  writeFileSync(workTarget, mutated);
  const r = runSuite(workUi);
  if (r.pass) {
    survived += 1;
    survivors.push(m);
    console.log(`  SURVIVED  ${i + 1}/${MUTANTS.length}  ${m.name}`);
    console.log(`            ${m.note}`);
  } else {
    killed += 1;
    console.log(`  killed    ${i + 1}/${MUTANTS.length}  ${m.name}`);
  }
}

rmSync(work, { recursive: true, force: true });
writeFileSync(TARGET, original);

console.log(`\n${killed} killed, ${survived} survived, ${stale} stale (of ${MUTANTS.length}).`);
if (survived > 0 || stale > 0) {
  console.log('\nSurvivors are gaps, or dead code. Check the line is load-bearing before testing it:');
  for (const m of survivors) console.log(`  - ${m.name}: ${m.note}`);
  process.exit(1);
}
