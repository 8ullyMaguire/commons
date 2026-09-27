#!/usr/bin/env node
/**
 * Mutation-check the wall: group-by and auto-scroll.
 *
 * The claims worth attacking are the ones a user notices being wrong. Grouping
 * that re-sorts a descending query, an auto-scroll that follows a user who
 * scrolled up, and a placeholder that makes the wall shrink under the cursor
 * all "work" -- they just behave like a different product.
 *
 * Each mutation is a real edit to `wall.ts`, run through the whole UI suite,
 * and reverted. A mutant that survives means the tests do not pin the claim
 * they are named for.
 *
 * Run with `--list` to see the mutants without running them.
 */

import { readFileSync, writeFileSync, readdirSync, mkdtempSync, rmSync, cpSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

const ROOT = resolve(dirname(new URL(import.meta.url).pathname), '..');
const UI = join(ROOT, 'ui');
const TARGET = join(UI, 'src/lib/api/wall.ts');

/** @type {{name: string, from: RegExp|string, to: string, note: string}[]} */
const MUTANTS = [
  {
    name: 'groupRows sorts the groups',
    from: /return \[\.\.\.byValue\.entries\(\)\]\.map\(/,
    to: 'return [...byValue.entries()].sort((a, b) => a[0].localeCompare(b[0])).map(',
    note: '#6544: a descending date query must not have its months re-sorted ascending.',
  },
  {
    name: 'groupRows keeps the valueless rows, under ""',
    from: "    if (value === null || value === '') continue;",
    to: "    if (value === null) { /* fall through: filed under the empty key */ }",
    note: 'A row with no studio is dropped, not filed under a section called "".',
  },
  {
    name: 'groupRows keeps the empty string',
    from: "    if (value === null || value === '') continue;",
    to: "    if (value === null) continue;",
    note: 'A blank producer is not a producer called "".',
  },
  {
    name: 'groupRows gives "none" a header value',
    from: "return null;\n    case 'month':",
    to: "return 'all';\n    case 'month':",
    note: 'The ungrouped wall has no header; a header saying "all" is above every item.',
  },
  {
    name: 'groupRows returns one empty group for an empty list',
    from: "return rows.length === 0\n      ? []\n      : [{ value: '', indices: rows.map((_, i) => i), pending: 0, index: 0 }];",
    to: "return [{ value: '', indices: rows.map((_, i) => i), pending: 0, index: 0 }];",
    note: 'An empty result renders a header and an empty body.',
  },
  {
    name: 'monthOf parses with Date and getMonth',
    from: "  const m = /^(\\d{4})-(\\d{2})/.exec(date ?? '');\n  if (m === null) return null;\n  return `${m[1]}-${m[2]}`;",
    to: "  const d = new Date(date ?? '');\n  if (Number.isNaN(d.getTime())) return null;\n  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`;",
    note: 'new Date("2024-06-01") is midnight UTC, so getMonth() files it under May west of Greenwich.',
  },
  {
    name: 'yearOf requires the month separator',
    from: "const m = /^(\\d{4})\\b/.exec(date ?? '');",
    to: "const m = /^(\\d{4})-/.exec(date ?? '');",
    note: 'A year-only date is a real year to group by.',
  },
  {
    name: 'groupValue stringifies a null organized',
    from: 'return row.organized ?? null;',
    to: 'return String(row.organized);',
    note: 'String(null) is the four characters "null" -- a section literally called "null".',
  },
  {
    name: 'groupValue drops a rating of 0',
    from: 'return row.rating === null || row.rating === undefined ? null : String(row.rating);',
    to: 'return row.rating ? String(row.rating) : null;',
    note: 'Rating 0 is a real rating and must group.',
  },
  {
    name: 'groupValue files an empty list under ""',
    from: 'return row.performers?.[0] ?? null;',
    to: "return row.performers?.[0] ?? '';",
    note: 'An empty list is not a value.',
  },
  {
    name: 'pendingPerGroup spreads the shortfall evenly',
    from: "  if (known === 0) return groups.map(() => total / Math.max(1, groups.length));\n",
    to: "  if (true) return groups.map(() => Math.max(0, total - known) / Math.max(1, groups.length));\n",
    note: 'Proportional keeps the scrollbar right at the top, which is where the user starts.',
  },
  {
    name: 'columnCount allows zero columns',
    from: '  return Math.max(1, fit);',
    to: '  return fit;',
    note: 'Zero columns divides by zero in every row-height calculation downstream.',
  },
  {
    name: 'columnCount returns 1 for a zero tile width',
    from: '  if (tile <= 0) return 1;',
    to: '  if (tile <= 0) return 0;',
    note: 'A zero tile width is not a layout.',
  },
  {
    name: 'groupHeight counts a gap after the last row too',
    from: '  return m.headerHeight + rows * tileHeight + (rows - 1) * m.gap;',
    to: '  return m.headerHeight + rows * (tileHeight + m.gap);',
    note: 'The gap after the last row belongs to the NEXT group.',
  },
  {
    name: 'groupHeight gives an empty group a row',
    from: '  if (count <= 0) return m.headerHeight;',
    to: '  if (count <= 0) return m.headerHeight + tileHeight;',
    note: 'An empty group is a header with no rows.',
  },
  {
    name: 'medianGroupHeight uses the mean',
    from: '  return sorted.length % 2 === 1 ? sorted[mid]! : (sorted[mid - 1]! + sorted[mid]!) / 2;',
    to: '  return sorted.reduce((a, b) => a + b, 0) / sorted.length;',
    note: 'One group of 5,000 must not make every other section reserve 5,000 items of blank space.',
  },
  {
    name: 'medianGroupHeight takes the lower middle value',
    from: '  return sorted.length % 2 === 1 ? sorted[mid]! : (sorted[mid - 1]! + sorted[mid]!) / 2;',
    to: '  return sorted[mid]!;',
    note: 'An even count takes the middle PAIR, not the lower one.',
  },
  {
    name: 'medianGroupHeight returns zero when nothing is known',
    from: '  if (knownHeights.length === 0) {\n    return groupHeight(1, fallbackTileHeight, m);\n  }',
    to: '  if (knownHeights.length === 0) {\n    return 0;\n  }',
    note: 'A zero-height wall is a wall the user cannot drag. This is the freshly-opened state.',
  },
  // The one that cost a rewrite.
  {
    name: 'groupExtents reserves only the placeholder for a pending group',
    from: 'return Math.max(placeholder, groupHeight(g.indices.length + p, tileHeight, m));',
    to: 'return placeholder;',
    note: 'A wall that shrinks as rows arrive moves everything under the user\'s cursor.',
  },
  {
    name: 'groupExtents reserves only what is loaded',
    from: 'return Math.max(placeholder, groupHeight(g.indices.length + p, tileHeight, m));',
    to: 'return groupHeight(g.indices.length, tileHeight, m);',
    note: 'The same shrink, by a different route: the pending rows are not counted at all.',
  },
  {
    name: 'groupExtents reports an approximate wall as exact',
    from: 'return { totalHeight, groupHeights, exact: pendingGroups === 0, pendingGroups };',
    to: 'return { totalHeight, groupHeights, exact: true, pendingGroups };',
    note: 'An approximate scrollbar must not be presented as exact.',
  },
  {
    name: 'groupExtents adds a trailing gap after the last group',
    from: 'groupHeights.reduce((a, b) => a + b, 0) + Math.max(0, groupHeights.length - 1) * m.gap;',
    to: 'groupHeights.reduce((a, b) => a + b, 0) + groupHeights.length * m.gap;',
    note: 'A trailing gap is scroll range containing nothing; it reads as a rendering bug.',
  },
  {
    name: 'autoScroll drops the slack',
    from: 'const atBottom = state.scrollTop >= maxScroll - slack;',
    to: 'const atBottom = state.scrollTop >= maxScroll;',
    note: 'A strict comparison unpins on the last pixel of every trackpad flick.',
  },
  {
    name: 'autoScroll keeps following a user who scrolled up',
    from: '  if (!atBottom) {\n    return { pinned: false, scrollTop: null, reason: \'user-scrolled-up\' };\n  }',
    to: '  if (!atBottom) {\n    return { pinned: true, scrollTop: maxScroll, reason: \'still-pinned\' };\n  }',
    note: 'The single most complained-about behaviour in an infinite feed.',
  },
  {
    name: 'autoScroll re-pins an unpinned wall near the bottom',
    from: 'if (state.scrollTop >= maxScroll) {',
    to: 'if (atBottom) {',
    note: 'Re-pinning on "close enough" yanks the view down mid-flick, and drags back a user who scrolled up by 40px.',
  },
  {
    name: 'autoScroll moves the view even when it does not pin',
    from: "    return { pinned: false, scrollTop: null, reason: 'user-scrolled-up' };",
    to: "    return { pinned: false, scrollTop: maxScroll, reason: 'user-scrolled-up' };",
    note: 'A null scrollTop is what leaves the position alone; the whole point of unpinning.',
  },
  {
    name: 'autoScroll computes a negative max scroll',
    from: 'const maxScroll = Math.max(0, state.scrollHeight - state.clientHeight);',
    to: 'const maxScroll = state.scrollHeight - state.clientHeight;',
    note: 'A wall shorter than its viewport; the browser would silently clamp a negative.',
  },
  {
    name: 'initialScrollTop opens at the bottom',
    from: 'export function initialScrollTop(): number {\n  return 0;\n}',
    to: 'export function initialScrollTop(): number {\n  return Number.MAX_SAFE_INTEGER;\n}',
    note: 'A wall that opens at the bottom is a wall whose top the user cannot find.',
  },
];

/**
 * Replace `from` with `to` once. Returns null when `from` is absent.
 *
 * A STRING pattern is matched literally, not as a regex. Half these patterns
 * are source lines full of metacharacters -- `(` in `groupRows(...)`, `?` in
 * `performers?.[0]`, `[]` in the array indexing -- and compiling those as a
 * regex silently fails to match, so the mutant is reported STALE and the claim
 * it was meant to check is never tested. A mutation script that reports 22
 * stale out of 28 is not a test result, it is a broken script that looks like
 * one.
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

const work = mkdtempSync(join(tmpdir(), 'wall-mut-'));
const workUi = join(work, 'ui');
cpSync(UI, workUi, {
  recursive: true,
  filter: (src) => !src.includes('node_modules') && !src.includes('/.svelte-kit'),
});
// The suite needs node_modules and the compiled app; link rather than copy.
try {
  execFileSync('ln', ['-s', join(UI, 'node_modules'), join(workUi, 'node_modules')]);
} catch {
  /* best effort */
}
cpSync(join(UI, 'tests'), join(workUi, 'tests'), { recursive: true });

const workTarget = join(workUi, 'src/lib/api/wall.ts');
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
if (survived > 0) {
  console.log('\nSurvivors are gaps, or dead code. Check the line is load-bearing before testing it:');
  for (const m of survivors) console.log(`  - ${m.name}: ${m.note}`);
  process.exit(1);
}
