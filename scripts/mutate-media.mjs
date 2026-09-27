#!/usr/bin/env node
/**
 * Mutation-check the unified media view.
 *
 * The claims worth attacking are the ones that fail invisibly: a filter the
 * server cannot parse, a kind silently dropped, an ordering that reshuffles when
 * a row arrives. None of them throws, and all of them make a tab that looks
 * finished and is wrong.
 *
 * Each mutation is a real edit to `ui/src/lib/api/media.ts`, run through the
 * whole UI suite, and reverted. A mutant that survives means the tests do not pin
 * the claim it is named for.
 *
 * Run with `--list` to see the mutants without running them.
 */

import { readFileSync, writeFileSync, mkdtempSync, rmSync, cpSync, symlinkSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

const ROOT = resolve(dirname(new URL(import.meta.url).pathname), '..');
const UI = join(ROOT, 'ui');
const TARGET = join(UI, 'src/lib/api/media.ts');

/** @type {{name: string, from: string, to: string, note: string}[]} */
const MUTANTS = [
  {
    name: 'the tab lists scenes and images',
    from: "export const MEDIA_KINDS: readonly Kind[] = ['scene', 'image'];",
    to: "export const MEDIA_KINDS: readonly Kind[] = ['scene', 'image', 'gallery'];",
    note: 'A gallery is a container, so listing it lists things twice: the container, and the images inside it.'
  },
  {
    name: 'a known kind outside the tab is not a media kind',
    from: '  return k !== null && MEDIA_KINDS.includes(k);',
    to: '  return k !== null;',
    note: 'A Media tab with text objects in it is a Library tab that lost the plot.'
  },
  {
    name: 'an unknown kind is not a media kind',
    from: '  return k !== null && MEDIA_KINDS.includes(k);',
    to: '  return MEDIA_KINDS.includes(kind as Kind);',
    note: 'Only the recognised ones; a new kind upstream must not become a tab member by accident.'
  },
  {
    name: 'parseKind does not match by prefix',
    from: "  return (KINDS as readonly string[]).includes(s) ? (s as Kind) : null;",
    to: "  return KINDS.find((k) => s.startsWith(k)) ?? null;",
    note: 'A startsWith check calls "scene-cut" a scene, and a compound name upstream would be placed as video.'
  },
  {
    name: 'an unknown kind is not guessed',
    from: "  return (KINDS as readonly string[]).includes(s) ? (s as Kind) : null;",
    to: "  return (KINDS as readonly string[]).includes(s) ? (s as Kind) : 'image';",
    note: 'Guessing renders a new kind as a photograph, which is the failure the audit exists to prevent.'
  },

  // --- the wire shape ---
  {
    name: 'the field is a tagged object',
    from: "      field: { builtin: 'kind' },",
    to: "      field: 'kind',",
    note: 'FieldRef is externally tagged, so a bare string is a shape serde rejects -- and the tab then looks like an empty library.'
  },
  {
    name: 'each value is a tagged object',
    from: '      values: kinds.map((k) => ({ str: k }))',
    to: '      values: kinds.map((k) => k)',
    note: 'Value is externally tagged for the same reason: {"str":"a"} and a bare "a" are different types.'
  },
  {
    name: 'the operator is lowercase',
    from: "      op: 'in',",
    to: "      op: 'In',",
    note: 'serde renames the variant to snake_case, and "In" is not a CmpOp.'
  },
  {
    name: 'the subject discriminator is null',
    from: '      kind: null,',
    to: "      kind: 'scene',",
    note: 'Facet.kind is a SUBJECT discriminator, not the object kind. Setting it scopes the facet to scenes, which is the opposite of what a facet listing both kinds means.'
  },

  // --- playability ---
  {
    name: 'a scene with no duration is still playable',
    from: "  return k === 'scene' || k === 'audio';",
    to: "  return (k === 'scene' || k === 'audio') && typeof row.durationMs === 'number' && row.durationMs > 0;",
    note: 'Duration is metadata, not capability. Refusing to play a video because its length is unknown hides it behind a broken measurement.'
  },
  {
    name: 'an image with a stray duration is not playable',
    from: "  return k === 'scene' || k === 'audio';",
    to: "  return k === 'scene' || k === 'audio' || (k === 'image' && (row.durationMs ?? 0) > 0);",
    note: 'An image with a stray duration is a data bug, and treating it as video gives a photograph a play button that opens onto nothing.'
  },
  {
    name: 'the affordance follows playability',
    from: '  return isPlayable(row);',
    to: '  return row.durationMs !== null;',
    note: 'A duration is not a play button.'
  },

  // --- duration ---
  {
    name: 'a zero duration gets no badge',
    from: '  if (ms === null || !Number.isFinite(ms) || ms <= 0) return null;',
    to: '  if (ms === null || !Number.isFinite(ms) || ms < 0) return null;',
    note: 'A zero-length scene is unmeasured, not zero seconds, and 0:00 next to a photograph merges two different rows.'
  },
  {
    name: 'a still gets no badge',
    from: '  if (!isPlayable(row)) return null;',
    to: '  if (false) return null;',
    note: 'An image has no duration to show.'
  },
  {
    name: 'a negative duration is not a duration',
    from: '  if (ms === null || !Number.isFinite(ms) || ms < 0) return null;',
    to: '  if (ms === null || !Number.isFinite(ms)) return null;',
    note: 'A negative duration is a broken probe, not a number to format.'
  },
  {
    name: 'the hour part is not dropped',
    from: '  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;',
    to: '  return `${m}:${pad(s)}`;',
    note: 'Dropping it renders a two-hour video as "119:59", which reads as a bug in the player rather than in the label.'
  },
  {
    name: 'the seconds are padded',
    from: '  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;',
    to: '  return h > 0 ? `${h}:${m}:${s}` : `${m}:${s}`;',
    note: 'An unpadded seconds field is a badge whose width changes as it counts.'
  },

  // --- ordering ---
  {
    name: 'the id tiebreak is present',
    from: '  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;',
    to: '  return 0;',
    note: 'Without it the comparator is not a total order, and rows that tie exchange places across a page boundary.'
  },
  {
    name: 'the kind tiebreak is present',
    from: '  if (ka !== kb) return ka - kb;',
    to: '  if (false) return ka - kb;',
    note: '#7068/#1508: a secondary sort defined only WITHIN a kind interleaves two kinds in an order that shifts as rows arrive.'
  },
  {
    name: 'scenes sort before images',
    from: '  if (ka !== kb) return ka - kb;',
    to: '  if (ka !== kb) return kb - ka;',
    note: 'The facet order is the order the tab lists them, and the reverse of it is a decision nobody made.'
  },
  {
    name: 'an unknown kind sorts last',
    from: '  if (k === null) return order.length;',
    to: '  if (k === null) return -1;',
    note: 'A kind this build cannot place should not lead the list.'
  },
  {
    name: 'sortMedia does not mutate its input',
    from: '  return [...rows].sort((a, b) => compareMedia(a, b, kindOrder));',
    to: "  return rows.sort((a, b) => compareMedia(a, b, kindOrder));",
    note: "A sort that mutates its argument reorders the caller's array, which is the store's row list."
  },

  // --- the audit ---
  {
    name: 'an unknown kind is reported',
    from: '      if (!unknown.includes(r.kind)) unknown.push(r.kind);',
    to: '      if (false) unknown.push(r.kind);',
    note: 'A kind silently dropped is content silently lost, and the user cannot diagnose it from the screen in front of them.'
  },
  {
    name: 'an unknown kind is reported once',
    from: '      if (!unknown.includes(r.kind)) unknown.push(r.kind);',
    to: '      unknown.push(r.kind);',
    note: '4,000 rows of one new kind is one problem, not four thousand.'
  },
  {
    name: 'byKind is keyed in the facet order',
    from: '  for (const k of kinds) {',
    to: '  for (const k of Object.keys(counts)) {',
    note: 'Keyed by first-row order, the same library reports its keys in a different order once one new scene arrives, and a legend built from it moves.'
  },
  {
    name: 'a row outside the tab is excluded',
    from: '    if (!kinds.includes(k)) {\n      excluded += 1;\n      continue;\n    }',
    to: '    if (false) {\n      excluded += 1;\n      continue;\n    }',
    note: 'A narrower tab excludes; it does not widen.'
  },
  {
    name: 'a row outside the tab is not called unknown',
    from: '    if (!kinds.includes(k)) {\n      excluded += 1;\n      continue;\n    }',
    to: '    if (!kinds.includes(k)) {\n      if (!unknown.includes(r.kind)) unknown.push(r.kind);\n      excluded += 1;\n      continue;\n    }',
    note: '"This build cannot place it" and "this tab was asked for a subset" are different facts, and only the first is a bug.'
  },
  {
    name: 'sections follow the facet order',
    from: '  return [...sections].sort((a, b) => kindRank(a, kinds) - kindRank(b, kinds));',
    to: '  return [...sections].sort();',
    note: 'Alphabetical puts Images before Scenes, and a wall whose sections move when a kind is added is the same complaint as a folder tree that re-sorts on rename.'
  },
  {
    name: 'an unknown section keeps its wire spelling',
    from: '      return kind;',
    to: "      return 'undefined';",
    note: '"undefined" tells the user nothing, and the wire spelling tells a developer what to go and look at.'
  }
];

/**
 * Replace `from` with `to`. Returns null when `from` is absent.
 *
 * A STRING pattern is matched literally, not as a regex: these are source lines
 * full of metacharacters, and a regex built from one does not match it, so the
 * mutant reports STALE and the claim it was meant to check is never tested.
 */
function substitute(src, from, to) {
  if (!src.includes(from)) return null;
  return src.split(from).join(to);
}

function runSuite(cwd) {
  try {
    execFileSync(process.execPath, ['./tests/run-tests.mjs'], { cwd, stdio: 'pipe', timeout: 600_000 });
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
  console.error(
    base.out
      .split('\n')
      .filter((l) => /✖|fail [1-9]/.test(l))
      .slice(0, 20)
      .join('\n')
  );
  process.exit(1);
}
console.log('baseline green.\n');

const work = mkdtempSync(join(tmpdir(), 'media-mut-'));
const workUi = join(work, 'ui');
cpSync(UI, workUi, {
  recursive: true,
  filter: (src) => !src.includes('node_modules') && !src.includes('/.svelte-kit')
});
try {
  symlinkSync(join(UI, 'node_modules'), join(workUi, 'node_modules'));
} catch {
  /* best effort */
}

const workTarget = join(workUi, 'src/lib/api/media.ts');
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
