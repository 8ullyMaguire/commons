#!/usr/bin/env node
/**
 * Mutation-check the paste parser.
 *
 * The claims worth attacking are the ones a user would notice being wrong: a
 * separator inside a quoted value splitting anyway, a duplicate being added
 * twice, a blank line becoming an empty value, and a CRLF paste leaving a
 * carriage return on every value after the first.
 */
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, rmSync, copyFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const SRC = join(root, 'ui/src/lib/api/paste.ts');
const BACKUP = mkdtempSync(join(tmpdir(), 'paste-mut-'));
const saved = join(BACKUP, 'paste.ts');
copyFileSync(SRC, saved);

function verdict() {
  const r = spawnSync('node', ['./tests/run-tests.mjs', 'paste'], {
    cwd: join(root, 'ui'),
    encoding: 'utf8'
  });
  const out = (r.stdout ?? '') + (r.stderr ?? '');
  // The result line is read FIRST: the runner prints `error: test failed` at
  // the end of every FAILED run, so a compile check placed earlier converts
  // every kill into "did not compile".
  if (/^ℹ fail [1-9]/m.test(out)) return 'killed';
  if (/^ℹ fail 0$/m.test(out)) return '*** SURVIVED ***';
  if (/^error(\[|:)/m.test(out)) return 'DID NOT COMPILE -- malformed mutation, not a survivor';
  return 'no result; tail:\n' + out.split('\n').slice(-8).join('\n');
}

const MUTATIONS = [
  {
    label: 'a separator inside quotes splits anyway',
    from: "    if (!inside && (ch === ';' || ch === ',')) {",
    to: "    if (ch === ';' || ch === ',') {"
  },
  {
    label: 'the quote state is not tracked at all',
    from: "      if (!inside && current.trim() === '') quoted = true;",
    to: ''
  },
  {
    label: 'a doubled quote is read as a close, then an open',
    from: "        current += '\"';\n        i += 1;\n        continue;",
    to: "        current += '\"\"';\n        i += 1;\n        continue;"
  },
  {
    label: 'a value already in the field is added again',
    from:
      '      const duplicate = seen.has(part);\n' +
      '      if (duplicate) {\n' +
      '        duplicates += 1;\n' +
      '        continue;\n' +
      '      }',
    to: '      const duplicate = seen.has(part);\n      duplicates += 1;'
  },
  {
    label: 'a value repeated inside one paste is added twice',
    from: '      seen.add(part);\n      values.push({ value: part, duplicate: false, line: index + 1 });',
    to: '      values.push({ value: part, duplicate: false, line: index + 1 });'
  },
  {
    label: 'a blank line becomes an empty value',
    from:
      '    if (parts.length === 0) {\n' +
      '      // A blank line is not a value and is not an error; counting it as a\n' +
      '      // value would put an empty string in a tag field, and counting it as\n' +
      '      // "skipped" is only useful if the number is shown to someone.\n' +
      "      if (line.trim() === '') skipped += 1;\n" +
      '      continue;\n' +
      '    }',
    to:
      '    if (parts.length === 0) {\n' +
      "      values.push({ value: '', duplicate: false, line: index + 1 });\n" +
      '      continue;\n' +
      '    }'
  },
  {
    label: 'only LF splits, so a CRLF paste leaves a carriage return',
    from: "  const lines = text.split(/\\r\\n|\\r|\\n/);",
    to: "  const lines = text.split('\\n');"
  },
  {
    label: 'case is folded, merging two tags that differ only in case',
    from: '  const seen = new Set<string>(existing);',
    to: '  const seen = new Set<string>(existing.map((e) => e.toLowerCase()));'
  },
  {
    label: 'a trailing separator produces an empty value',
    from: "    .filter((v) => v !== '');",
    to: ''
  },
  {
    label: 'the duplicate count is dropped from the preview',
    from: "  return `${add}, ${plural(duplicates, 'value')} already in the field.`;",
    to: '  return `${add}.`;'
  },
  {
    label: 'the preview names the pasted count rather than what will be added',
    from: "  const add = `Will add ${plural(values.length, 'value')}`;",
    to: "  const add = `Will add ${plural(duplicates + values.length, 'value')}`;"
  },
  {
    label: 'a split value is not unquoted, so it keeps its quotes',
    from: "      return value.startsWith('\"') ? unquote(value) : value;",
    to: '      return value;'
  },
  {
    label: 'unquote strips only the first character of a quoted pair',
    from: '  return trimmed.slice(1, -1).replace(/""/g, \'"\');',
    to: "  return trimmed.slice(1).replace(/\"\"/g, '\"');"
  },
  {
    label: 'a quoted blank is dropped like an unquoted one',
    from: "      if (!inside && current.trim() === '') quoted = true;",
    to: "      if (false) quoted = true;"
  }
];

const original = readFileSync(saved, 'utf8');
console.log(`baseline, source UNMUTATED -- want green: ${verdict()}`);
console.log();

for (const m of MUTATIONS) {
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
