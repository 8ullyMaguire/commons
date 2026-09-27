#!/usr/bin/env node
/**
 * Mutation-check the CSV importer.
 *
 * The claims worth attacking are the ones a user would notice being wrong: a
 * newline inside a quoted field split anyway, a delimiter sniffed from inside
 * quotes, a blank line that merges two rows, a truncated file that imports
 * silently, and an encoding that guesses UTF-8 for a UTF-16 file.
 *
 * Run: node scripts/mutate-csv.mjs
 */

import { readFileSync, writeFileSync, unlinkSync, mkdtempSync, rmSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SOURCE = join(repo, 'ui/src/lib/api/csv.ts');
const original = readFileSync(SOURCE, 'utf8');

/** Each mutation: a name, an exact `from` in the source, and its replacement. */
const MUTATIONS = [
  {
    label: 'a newline inside a quoted field ends the row',
    from: "      if (ch === '\\n') {\n        current += '\\n';\n        line += 1;\n        continue;\n      }",
    to: "      if (ch === '\\n') {\n        break;\n      }",
  },
  {
    label: 'a quoted value is split on the delimiter',
    from: "    if (text.startsWith(delim, i)) {\n      endField();",
    to: "    if (!inQuotes && false) {\n      endField();",
  },
  {
    label: 'a blank line is kept as a row',
    from: '    const isGap = isBlank && current === \'\' && !openedByQuote && !cellQuoted;',
    to: '    const isGap = false;',
  },
  {
    label: 'a blank line merges the next row into the previous one',
    from: '    if (isGap && rows.length > 0) {\n      cells = [];',
    to: '    if (isGap && rows.length > 0) {\n      /* cells kept */',
  },
  {
    label: 'a truncated file is not reported',
    from: '    truncatedAtLine = cellLine;\n    endRow(false);',
    to: '    endRow(false);',
  },
  {
    label: 'a truncated file drops the rows that did parse',
    from: '    truncatedAtLine = cellLine;\n    endRow(false);',
    to: '    truncatedAtLine = cellLine;',
  },
  {
    label: 'the header row is not excluded',
    from: '  const dataRows = hasHeader ? rows.slice(1) : rows;',
    to: '  const dataRows = rows;',
  },
  {
    label: 'a quoted empty field is dropped as an empty row',
    from: "    if (value === '' && !cell.quoted) {",
    to: "    if (value === '') {",
  },
  {
    label: 'a quoted empty survives the blank-line check',
    from: '    const isGap = isBlank && current === \'\' && !openedByQuote && !cellQuoted;',
    to: '    const isGap = isBlank && current === \'\';',
  },
  {
    label: 'an escaped quote produces two quotes',
    from: "          current += '\"';\n          i += 1;\n          continue;",
    to: "          current += '\"\"';\n          i += 1;\n          continue;",
  },
  {
    label: 'a quote after the first character opens a quoted run',
    from: "    if (ch === '\"') {\n      // A quote after a closing one, or in the middle of a bare field.",
    to: "    if (ch === '\"' && false) {\n      // A quote after a closing one, or in the middle of a bare field.",
  },
  {
    // A final row with no trailing newline. Removing the guard drops it: a file
    // whose last line has no newline at the end loses its last value, which is
    // the shape of every file produced by a script rather than a spreadsheet.
    label: 'a final row with no trailing newline is dropped',
    from: "  } else if (fieldStarted || current !== '' || cells.length > 0) {\n    // A final row with no trailing newline.\n    endRow(false);\n  }",
    to: "  } else if (false) {\n    endRow(false);\n  }",
  },
  {
    label: 'a final row of one empty cell is added where the file ended with a newline',
    from: "  } else if (fieldStarted || current !== '' || cells.length > 0) {",
    to: '  } else if (true) {',
  },
  {
    // The mutation the FIRST version of the sniffer test could not kill,
    // because that test used a comma inside quotes -- where the naive and the
    // quote-aware counts agree on the modal value. The replacement test puts a
    // semicolon only inside quotes, where they do not.
    label: 'the sniffer counts delimiters inside quotes',
    from: '    if (!inside && line.startsWith(needle, i)) {',
    to: '    if (line.startsWith(needle, i)) {',
  },
  {
    // EXEMPT, and proved exempt by exhaustive search rather than by argument.
    //
    // This function only COUNTS delimiters outside quotes; it does not emit
    // values. Skipping an escaped `""` as a pair and toggling `inside` twice
    // leave `inside` in the same state, and both consume the same two
    // characters, so the two implementations agree on every input. Checked
    // exhaustively over all 1,093 strings of length <= 6 in {a, ", ;} against
    // both `,` and `;`: zero differences.
    //
    // The branch is therefore not dead code in spirit -- it documents that an
    // escaped quote is not a boundary -- but it is not *load-bearing* here. The
    // same handling IS load-bearing in `splitLine` and `parseCsv`, where the
    // characters are kept, and those are covered by mutations that do kill.
    label: 'the sniffer treats an escaped quote as a delimiter boundary [EXEMPT: equivalent]',
    from: "      if (inside && line[i + 1] === '\"') {\n        i += 1;\n        continue;\n      }",
    to: '      /* escaped quote not handled */',
  },
  {
    label: 'the sniffer takes the first delimiter seen rather than the agreed one',
    from: '    if (modeSeen > bestScore) {',
    to: '    if (bestScore === -1) {',
  },
  {
    label: 'the sniffer scores by magnitude rather than agreement',
    from: '    if (modeSeen > bestScore) {',
    to: '    if (mode > bestScore) {',
  },
  {
    label: 'the sniffer prefers semicolon over comma on a tie',
    from: "const CANDIDATE_DELIMITERS = [',', ';', '\\t', '|'] as const;",
    to: "const CANDIDATE_DELIMITERS = [';', ',', '\\t', '|'] as const;",
  },
  {
    label: 'BOM-less UTF-16 is assumed to be UTF-8',
    from: "  const head = bytes.subarray(0, Math.min(bytes.length, 512));\n  if (countNulsAt(head, 0) === 0 && countNulsAt(head, 1) === 0) {",
    to: "  const head = bytes.subarray(0, Math.min(bytes.length, 512));\n  if (true) {",
  },
  {
    label: 'the NUL test asks whether the text is valid UTF-8 instead',
    // The first version's bug, verbatim in shape: U+FFFD never appears, so
    // UTF-16 sails through as mojibake.
    from: '  if (countNulsAt(head, 0) === 0 && countNulsAt(head, 1) === 0) {',
    to: "  if (!decode(head, 'utf-8').includes('\\ufffd')) {",
  },
  {
    // EXEMPT, and proved exempt rather than assumed.
    //
    // `TextDecoder` strips a leading BOM itself: `ignoreBOM` defaults to false,
    // which means "do not ignore it" and therefore "consume it". So decoding
    // `bytes` and decoding `bytes.subarray(3)` produce the same string, and no
    // test can separate them. Verified directly:
    //
    //   new TextDecoder('utf-8').decode([0xEF,0xBB,0xBF,0x61]) === 'a'
    //   new TextDecoder('utf-16le').decode([0xFF,0xFE,0x61,0x00]) === 'a'
    //
    // The `subarray` is kept anyway: it makes the intent explicit rather than
    // depending on a default of the platform decoder, and it is what a reader
    // has to verify. That is a different reason from "a test covers it", and
    // the comment says which one it is.
    label: 'the BOM is detected but not explicitly stripped [EXEMPT: equivalent]',
    from: "    return { text: decode(bytes.subarray(3), 'utf-8'), encoding: 'utf-8', hadBom: true };",
    to: "    return { text: decode(bytes, 'utf-8'), encoding: 'utf-8', hadBom: true };",
  },
  {
    label: 'the UTF-16LE BOM is detected but not explicitly stripped [EXEMPT: equivalent]',
    from: "    return { text: decode(bytes.subarray(2), 'utf-16le'), encoding: 'utf-16le', hadBom: true };",
    to: "    return { text: decode(bytes, 'utf-16le'), encoding: 'utf-16le', hadBom: true };",
  },
  {
    // EXEMPT, and the reason is worth more than the mutation.
    //
    // This is the mistake a careless reordering of the BOM checks produces, and
    // it is unreachable precisely BECAUSE the UTF-8 check runs first and returns.
    // A byte sequence starting `0xEF 0xBB 0xBF` can never reach the UTF-16LE
    // branch, so widening that branch's condition changes nothing for any input.
    // Verified: with the mutation applied, a UTF-8 BOM file still decodes as
    // { text: 'a,b', encoding: 'utf-8', hadBom: true }.
    //
    // The order is the thing under test, and it is asserted directly instead --
    // a test that a UTF-8 BOM file decodes as utf-8 and NOT as utf-16le fails
    // the moment the order is actually swapped, which no reachable mutation can
    // express as a textual edit.
    label: 'the UTF-16LE check fires on a UTF-8 BOM [EXEMPT: unreachable]',
    from: '  if (startsWith(bytes, [0xff, 0xfe])) {',
    to: '  if (startsWith(bytes, [0xff, 0xfe]) || startsWith(bytes, [0xef, 0xbb, 0xbf])) {',
  },
  {
    label: 'duplicates already in the field are added anyway',
    from: "    if (seen.has(value)) {\n      duplicates += 1;\n      continue;\n    }",
    to: "    if (false) {\n      duplicates += 1;\n      continue;\n    }",
  },
  {
    label: 'duplicates within the file are both added',
    from: '    seen.add(value);',
    to: '    /* not recorded */',
  },
  {
    label: 'case is folded, merging two values that differ only in case',
    from: "    if (seen.has(value)) {",
    to: "    if (seen.has(value.toLowerCase())) {",
  },
  {
    label: 'a short row is padded with an empty value',
    from: '    if (cell === undefined) {',
    to: '    if (false) {',
  },
  {
    label: 'a value is always trimmed',
    from: "    const value = options.trimValues === true ? cell.value.trim() : cell.value;",
    to: '    const value = cell.value.trim();',
  },
  {
    label: 'a value is never trimmed',
    from: "    const value = options.trimValues === true ? cell.value.trim() : cell.value;",
    to: '    const value = cell.value;',
  },
  {
    label: 'an unknown column name throws away the import',
    from: '    index = found === -1 ? 0 : found;',
    to: '    index = found;',
  },
  {
    label: 'the truncation message is not shown',
    from: "  if (truncatedAtLine !== null) {\n    return `File is truncated",
    to: "  if (false) {\n    return `File is truncated",
  },
  {
    label: 'the duplicate count is dropped from the message',
    from: '  if (duplicates > 0) parts.push(`${count(duplicates, \'value\')} already in the field`);',
    to: "  if (false) parts.push(`${count(duplicates, 'value')} already in the field`);",
  },
  {
    label: 'an empty file is reported as rows with no values',
    from: '  if (rowCount === 0) {',
    to: '  if (false) {',
  },
  {
    label: 'a short row reports the wrong line for its value',
    from: '    values.push({ value, duplicate: false, line: cell.line });',
    to: '    values.push({ value, duplicate: false, line: row.line });',
  },
];

// --- harness ---------------------------------------------------------------

const work = mkdtempSync(join(tmpdir(), 'mutate-csv-'));
const uiDir = join(repo, 'ui');
const testFile = join(uiDir, 'tests/csv.test.ts');
const origTest = readFileSync(testFile, 'utf8');

let killed = 0;
let survived = 0;
let couldNotApply = 0;
const failures = [];

console.log('baseline, source UNMUTATED -- want green:');
runTests();

for (const m of MUTATIONS) {
  if (!original.includes(m.from)) {
    couldNotApply += 1;
    console.log(`\n${m.label}: COULD NOT APPLY -- the text was not found`);
    failures.push(`${m.label} (stale mutation: the code it names no longer exists)`);
    continue;
  }
  const mutated = original.replace(m.from, m.to);
  if (mutated === original) {
    couldNotApply += 1;
    console.log(`\n${m.label}: COULD NOT APPLY -- the replacement was identical`);
    continue;
  }
  writeFileSync(SOURCE, mutated);
  const ok = runTests();
  writeFileSync(SOURCE, original);

  if (ok) {
    survived += 1;
    console.log(`\n${m.label}: *** SURVIVED ***`);
    failures.push(`${m.label} (SURVIVED)`);
  } else {
    killed += 1;
    console.log(`\n${m.label}: killed`);
  }
}

writeFileSync(SOURCE, original);
rmSync(work, { recursive: true, force: true });

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
    execFileSync('node', ['./tests/run-tests.mjs', 'csv'], { cwd: uiDir, stdio: 'pipe' });
    return true;
  } catch {
    return false;
  }
}
