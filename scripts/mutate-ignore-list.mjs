#!/usr/bin/env node
/**
 * Mutation-check the per-field ignore lists.
 *
 * The claims worth attacking are the ones a user notices being wrong: an
 * inverted scope that keeps everything instead of ignoring everything, a
 * normalisation that lets two spellings through, and a vocabulary that drifts
 * from the schema it claims to describe.
 *
 * Run: node scripts/mutate-ignore-list.mjs
 */

import { readFileSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const TARGETS = [
  join(repo, 'ui/src/lib/api/ignore-list.ts'),
  join(repo, 'ui/src/lib/api/tagger-fields.ts'),
];
const originals = new Map(TARGETS.map((p) => [p, readFileSync(p, 'utf8')]));

const MUTATIONS = [
  {
    file: 'ignore-list.ts',
    label: 'the inverted test is not inverted, so ignoreAll keeps everything',
    from: '  return scope.ignoreAll ? !scope.fields.includes(n) : scope.fields.includes(n);',
    to: '  return scope.fields.includes(n);',
  },
  {
    file: 'ignore-list.ts',
    label: 'the normal test is inverted instead',
    from: '  return scope.ignoreAll ? !scope.fields.includes(n) : scope.fields.includes(n);',
    to: '  return scope.ignoreAll ? scope.fields.includes(n) : !scope.fields.includes(n);',
  },
  {
    file: 'ignore-list.ts',
    label: 'normalisation does not lower-case, so two spellings are two entries',
    from: '  return field.trim().toLowerCase();',
    to: '  return field.trim();',
  },
  {
    file: 'ignore-list.ts',
    label: 'normalisation does not trim',
    from: '  return field.trim().toLowerCase();',
    to: '  return field.toLowerCase();',
  },
  {
    file: 'ignore-list.ts',
    label: 'an empty field name is ignored by an inverted scope',
    from: "  if (n === '') return false;",
    to: '',
  },
  {
    file: 'ignore-list.ts',
    label: 'duplicates survive normalisation',
    from: '  return [...set].sort();',
    to: '  return fields.map((f) => normaliseField(f)).filter((f) => f !== \'\');',
  },
  {
    file: 'ignore-list.ts',
    label: 'the list is not sorted, so two equal lists differ',
    from: '  return [...set].sort();',
    to: '  return [...set];',
  },
  {
    file: 'ignore-list.ts',
    label: 'the query joins with a comma instead of repeating the parameter',
    from: "    return scope.fields.map((f) => `ignore=${encodeURIComponent(f)}`).join('&');",
    to: "    return `ignore=${scope.fields.map(encodeURIComponent).join(',')}`;",
  },
  {
    file: 'ignore-list.ts',
    label: 'an inverted scope serialises as an ignore list',
    from: "  if (!scope.ignoreAll) {",
    to: '  if (true) {',
  },
  {
    file: 'ignore-list.ts',
    label: 'an inverted scope with nothing kept omits the parameter entirely',
    from: "  if (keep.length === 0) return prefix;",
    to: "  if (keep.length === 0) return '';",
  },
  {
    file: 'ignore-list.ts',
    label: 'an invalid field name is accepted rather than dropped',
    from: '    fields: normaliseAll(fields.filter(isValidFieldName)),',
    to: '    fields: normaliseAll(fields),',
  },
  {
    file: 'ignore-list.ts',
    label: 'rejectedFields reports nothing',
    from: '  return fields.filter((f) => !isValidFieldName(f));',
    to: '  return [];',
  },
  {
    file: 'ignore-list.ts',
    label: 'splitByField puts an ignored field on the kept side',
    from: '    (isIgnored(scope, fieldOf(item)) ? ignored : kept).push(item);',
    to: '    (isIgnored(scope, fieldOf(item)) ? kept : ignored).push(item);',
  },
  {
    file: 'ignore-list.ts',
    label: 'splitByField returns only the kept half',
    from: '  return { kept, ignored };',
    to: '  return { kept, ignored: [] };',
  },
  {
    file: 'ignore-list.ts',
    label: 'keptFields returns the ignored ones',
    from: '  return all.filter((f) => !isIgnored(scope, f));',
    to: '  return all.filter((f) => isIgnored(scope, f));',
  },
  {
    file: 'ignore-list.ts',
    label: 'ignoredFields sorts rather than keeping the input order',
    from: '  return all.filter((f) => isIgnored(scope, f));',
    to: '  return all.filter((f) => isIgnored(scope, f)).sort();',
  },
  {
    file: 'ignore-list.ts',
    label: 'an unknown entry is silently dropped',
    from: '  return scope.fields.filter((f) => !known.has(f));',
    to: '  return [];',
  },
  {
    file: 'ignore-list.ts',
    label: 'describeScope uses the wrong plural for one',
    from: "  return `${list(scope.fields)} ${scope.fields.length === 1 ? 'is' : 'are'} ignored.`;",
    to: "  return `${list(scope.fields)} are ignored.`;",
  },
  {
    file: 'ignore-list.ts',
    label: 'describeScope counts instead of naming',
    from: "  return `${list(scope.fields)} ${scope.fields.length === 1 ? 'is' : 'are'} ignored.`;",
    to: "  return `${scope.fields.length} fields ignored.`;",
  },
  {
    file: 'ignore-list.ts',
    label: 'an empty scope is described as ignoring fields',
    from: "  if (scope.fields.length === 0) return 'No fields are ignored.';",
    to: '',
  },
  {
    file: 'ignore-list.ts',
    label: 'ignoreAllExcept does not set the inverted flag',
    from: '  return { ignoreAll: true, fields: normaliseAll(fields) };',
    to: '  return { ignoreAll: false, fields: normaliseAll(fields) };',
  },
  {
    file: 'tagger-fields.ts',
    label: 'Object.kind becomes offerable, so a scraper can change what an item IS',
    from: "  object: [\n    'id',\n    'kind',",
    to: "  object: [\n    'id',",
  },
  {
    file: 'tagger-fields.ts',
    label: 'Producer.career_start becomes offerable, contradicting 7.11',
    from: "    'id',\n    'career_start',",
    to: "    'id',",
  },
  {
    file: 'tagger-fields.ts',
    label: 'Tag.parent_id becomes offerable, so a scrape can move a tag in the tree',
    from: "  tag: ['id', 'parent_id'],",
    to: "  tag: ['id'],",
  },
  {
    file: 'tagger-fields.ts',
    label: 'Object.rating_sum becomes offerable, so a machine can vote on a rating',
    from: "    'rating_sum',\n    'rating_count',",
    to: "    'rating_count',",
  },
  {
    file: 'tagger-fields.ts',
    label: 'a field is dropped from the vocabulary, so it cannot be ignored',
    from: "  'description',\n  'date',",
    to: "  'date',",
  },
  {
    file: 'tagger-fields.ts',
    label: 'the flat list is de-duplicated, losing the per-subject counts',
    from: "export const KNOWN_TAGGER_FIELDS: readonly string[] = Object.values(FIELDS_BY_SUBJECT).flat();",
    to: "export const KNOWN_TAGGER_FIELDS: readonly string[] = [\n  ...new Set(Object.values(FIELDS_BY_SUBJECT).flat()),\n];",
  },
  {
    file: 'tagger-fields.ts',
    label: 'isOfferable is inverted',
    from: '  return !NOT_OFFERABLE_BY_SUBJECT[subject].includes(field);',
    to: '  return NOT_OFFERABLE_BY_SUBJECT[subject].includes(field);',
  },
];

let killed = 0;
let survived = 0;
let couldNotApply = 0;
const failures = [];

console.log('baseline, sources UNMUTATED -- want green:');
runTests();

for (const m of MUTATIONS) {
  const path = TARGETS.find((p) => p.endsWith(m.file));
  const original = originals.get(path);
  if (!original.includes(m.from)) {
    couldNotApply += 1;
    console.log(`\n${m.label}: COULD NOT APPLY -- the text was not found`);
    failures.push(`${m.label} (stale mutation)`);
    continue;
  }
  writeFileSync(path, original.replace(m.from, m.to));
  const ok = runTests();
  writeFileSync(path, original);

  if (ok) {
    survived += 1;
    console.log(`\n${m.label}: *** SURVIVED ***`);
    failures.push(`${m.label} (SURVIVED)`);
  } else {
    killed += 1;
    console.log(`\n${m.label}: killed`);
  }
}

for (const [p, text] of originals) writeFileSync(p, text);

const applied = killed + survived;
console.log(`\n${'-'.repeat(60)}`);
console.log(`applied ${applied}/${MUTATIONS.length}, killed ${killed}, survived ${survived}, stale ${couldNotApply}`);
if (failures.length > 0) {
  console.log('\nNOT KILLED:');
  for (const f of failures) console.log(`  - ${f}`);
}
process.exit(survived > 0 ? 1 : 0);

function runTests() {
  try {
    execFileSync('node', ['./tests/run-tests.mjs'], {
      cwd: join(repo, 'ui'),
      stdio: 'pipe',
    });
    return true;
  } catch {
    return false;
  }
}
