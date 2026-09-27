/*
  Tests for the CSV importer. Spec 10.10, plan T-P5-006 item 12; #1296, #431.

  # What is being tested here

  The decisions, at their boundaries. A CSV is a table, and the boundaries are
  the places where two plausible readings disagree:

   - a newline INSIDE a quoted field, which is data and is the reason this is
     a state machine and not a line split;
   - a quote that is data (`he said "hi"`) versus a quote that is syntax;
   - a blank line in the middle of a file, which is a gap and not a row;
   - a delimiter that appears only inside a quoted field, which must not
     convince the sniffer it is the delimiter;
   - UTF-16 without a byte-order mark, which is UTF-8 that decodes to
     something that is not UTF-8;
   - a truncated file, which is the one hard error.

  Every one of these is a case where a plausible implementation is wrong and a
  user would see the wrong value rather than an error. `parsePaste` in
  `paste.test.ts` covers the paste path, which shares the result type.
*/

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import {
  decodeCsvBytes,
  describeImport,
  delimiterName,
  importCsv,
  parseCsv,
  sniffDelimiter,
} from '../src/lib/api/csv.js';

/** The cell values of one row, which is what most assertions want. */
function row(text: string, index = 0, delimiter?: string | null): string[] {
  return parseCsv(text, delimiter).rows[index]!.cells.map((c) => c.value);
}

/**
 * The first cell of every row, flattened.
 *
 * A single-column file is a column of values, which is what a tag import is,
 * so the assertions that read one want ALL of them rather than row 0. Having
 * both helpers is what stopped `handles a single column file` from passing by
 * accident against a parser that returned only the first row.
 */
function column(text: string, index = 0, delimiter?: string | null): string[] {
  return parseCsv(text, delimiter).rows.map((r) => r.cells[index]!.value);
}

describe('decodeCsvBytes', () => {
  const utf8 = (s: string): Uint8Array => new TextEncoder().encode(s);
  const utf16le = (s: string): Uint8Array => {
    const out = new Uint8Array(s.length * 2);
    for (let i = 0; i < s.length; i += 1) {
      out[i * 2] = s.charCodeAt(i);
    }
    return out;
  };
  const utf16be = (s: string): Uint8Array => {
    const out = new Uint8Array(s.length * 2);
    for (let i = 0; i < s.length; i += 1) {
      out[i * 2 + 1] = s.charCodeAt(i);
    }
    return out;
  };

  // The heuristic's reason for existing. A UTF-16 file with no BOM decoded as
  // UTF-8 comes out with NULs between every character, and NUL is a VALID
  // UTF-8 character -- so a "is this valid UTF-8" test passes and the file
  // imports as mojibake. The first version of this function did exactly that.
  test('reads BOM-less UTF-16LE, where every NUL is at an odd offset', () => {
    const d = decodeCsvBytes(utf16le('a,b,c'));
    assert.equal(d.encoding, 'utf-16le');
    assert.equal(d.text, 'a,b,c');
    assert.equal(d.hadBom, false);
  });

  test('reads BOM-less UTF-16BE, where every NUL is at an even offset', () => {
    const d = decodeCsvBytes(utf16be('a,b,c'));
    assert.equal(d.encoding, 'utf-16be');
    assert.equal(d.text, 'a,b,c');
  });

  test('strips a UTF-8 BOM and says so', () => {
    const d = decodeCsvBytes(utf8('﻿a,b'));
    assert.equal(d.text, 'a,b');
    assert.equal(d.hadBom, true);
    assert.equal(d.encoding, 'utf-8');
  });

  // A BOM is checked in full before the heuristic, and a UTF-8 BOM's third byte
  // is 0xBF while its first two are NUL-free -- so an ordering bug here would
  // send a BOM'd UTF-8 file down the UTF-16 path.
  test('strips a UTF-16LE BOM', () => {
    const d = decodeCsvBytes(new Uint8Array([0xff, 0xfe, ...utf16le('a,b')]));
    assert.equal(d.encoding, 'utf-16le');
    assert.equal(d.text, 'a,b');
    assert.equal(d.hadBom, true);
  });

  test('strips a UTF-16BE BOM', () => {
    const d = decodeCsvBytes(new Uint8Array([0xfe, 0xff, ...utf16be('a,b')]));
    assert.equal(d.encoding, 'utf-16be');
    assert.equal(d.text, 'a,b');
  });

  // Plain UTF-8 must not be sent down the UTF-16 path by an over-eager
  // heuristic: a file that happens to contain a NUL character is legal UTF-8,
  // and mangling it would be worse than the mojibake case this fixes.
  test('leaves plain UTF-8 alone', () => {
    const d = decodeCsvBytes(utf8('a,b,c'));
    assert.equal(d.encoding, 'utf-8');
    assert.equal(d.text, 'a,b,c');
    assert.equal(d.hadBom, false);
  });

  test('handles an empty file', () => {
    const d = decodeCsvBytes(new Uint8Array([]));
    assert.equal(d.text, '');
    assert.equal(d.encoding, 'utf-8');
  });

  // The BOM checks are ORDERED, and no reachable mutation can express a swap --
  // the UTF-8 branch returns, so the UTF-16LE branch's condition can be widened
  // to anything and stay unreachable. The order therefore has to be asserted
  // directly: a UTF-8 BOM must decode as utf-8, and specifically NOT as
  // utf-16le, which is where a reordering would send it.
  test('a UTF-8 BOM is checked before the UTF-16 BOMs', () => {
    const d = decodeCsvBytes(new Uint8Array([0xef, 0xbb, 0xbf, 0x61, 0x2c, 0x62]));
    assert.equal(d.encoding, 'utf-8');
    assert.notEqual(d.encoding, 'utf-16le');
    assert.equal(d.text, 'a,b');
  });

  test('keeps a non-ASCII value intact through the sniff path', () => {
    const d = decodeCsvBytes(utf8('Ünïcode,日本'));
    assert.equal(d.text, 'Ünïcode,日本');
  });
});

describe('sniffDelimiter', () => {
  test('finds a comma', () => {
    assert.equal(sniffDelimiter('a,b,c\nd,e,f'), ',');
  });

  test('finds a semicolon', () => {
    assert.equal(sniffDelimiter('a;b;c\nd;e;f'), ';');
  });

  // Excel on Windows writes tabs, and the files users actually have come from
  // Excel far more often than from a spec-compliant exporter.
  test('finds a tab', () => {
    assert.equal(sniffDelimiter('a\tb\tc\nd\te\tf'), '\t');
  });

  test('finds a pipe', () => {
    assert.equal(sniffDelimiter('a|b|c\nd|e|f'), '|');
  });

  // THE sniffer test, and the one that took two attempts to write correctly.
  //
  // The first version was `'"a,b,c",d\ne,f,g'` -- a comma inside a quoted
  // field. A naive `line.count(',')` gives [3, 2] and the quote-aware scan gives
  // [1, 2]; both have a modal count that names the comma, so the test passed
  // with the quotes ignored. The mutation that removed the quote tracking
  // SURVIVED it. Naming the boundary is not the same as straddling it.
  //
  // The case that separates them puts the semicolon ONLY inside quotes, so the
  // naive count sees a delimiter on every line and the quote-aware count sees
  // none:
  //
  //   '"a;b;c",d\ne,f,g'   naive ';' -> [2, 0]   quote-aware -> [0, 0]
  //
  // A sniffer that counted them would answer ';' and then every value in the
  // file would land in one column.
  test('ignores a delimiter that appears only inside a quoted field', () => {
    assert.equal(sniffDelimiter('"a;b;c",d\ne,f,g'), ',');
  });

  // And the same in the other direction: a comma only inside quotes must not
  // make a semicolon file read as comma-separated.
  test('ignores a comma inside quotes in a semicolon file', () => {
    assert.equal(sniffDelimiter('"a,b,c";d\ne,f;g'), ';');
  });

  // The case that finally separates the two, and the reason the two tests above
  // were not enough. Here the semicolon appears BOTH inside a quoted field on
  // line 1 and as a real delimiter on line 2:
  //
  //   '"a;b;c",d\ne;f;g'    naive ';' -> [2, 1]  modal 2 on one line
  //                          quote-aware -> [0, 1]  modal 1 on one line
  //
  // Both scoring rules are then asked which candidate wins. A naive count puts
  // two semicolons on line 1 and one on line 2, so the modal count is 2 and only
  // ONE line agrees with it; the quote-aware count puts zero on line 1 and one
  // on line 2, so the modal count is 1 and again only one line agrees. Agreement
  // is 1 either way, so the tie-break is the magnitude -- and the naive count
  // has the larger modal, so it answers ';' and the file is read as
  // semicolon-separated. The real delimiter is the comma: there is exactly one
  // per line, and the semicolons live inside a quoted value.
  //
  // Checked by hand, because the test suite alone cannot tell you WHY a
  // mutation survives and this is the reason.
  test('is not fooled when the wrong delimiter appears in quotes AND as a real separator', () => {
    assert.equal(sniffDelimiter('"a;b;c",d\ne;f;g'), ',');
  });

  test('ignores a quoted comma when the real delimiter is a semicolon', () => {
    assert.equal(sniffDelimiter('"a,b,c";d\ne,f;g'), ';');
  });

  // The score is the number of AGREEING lines, not the magnitude, so one row
  // with six delimiters cannot outvote nine rows with one. Exports are ragged.
  test('prefers the delimiter more lines agree on, over the one that appears more', () => {
    const text = ['a,b,c', 'd,e,f', 'g,h,i', 'j;k;l;m;n;o;p'].join('\n');
    assert.equal(sniffDelimiter(text), ',');
  });

  test('defaults to comma for an empty file', () => {
    assert.equal(sniffDelimiter(''), ',');
  });

  test('defaults to comma for a single-column file', () => {
    assert.equal(sniffDelimiter('a\nb\nc'), ',');
  });

  test('breaks a tie toward comma, the format default', () => {
    assert.equal(sniffDelimiter('a,b;c\nd,e;f'), ',');
  });
});

describe('parseCsv', () => {
  test('reads a plain row', () => {
    assert.deepEqual(row('a,b,c'), ['a', 'b', 'c']);
  });

  test('reads multiple rows', () => {
    const r = parseCsv('a,b\nc,d');
    assert.equal(r.rows.length, 2);
    assert.deepEqual(r.rows[1]!.cells.map((x) => x.value), ['c', 'd']);
  });

  // THE reason this is a state machine. A newline inside a quoted field is
  // data: an export from Postgres puts a paragraph in one cell, and a
  // line-splitting parser turns that paragraph into three rows and attributes
  // the pieces to three different values.
  test('keeps a newline inside a quoted field', () => {
    const r = parseCsv('"line one\nline two",b');
    assert.equal(r.rows.length, 1);
    assert.deepEqual(r.rows[0]!.cells.map((c) => c.value), ['line one\nline two', 'b']);
  });

  test('normalises CRLF inside a quoted field to one newline', () => {
    // A CRLF is one newline, not two. Counting it as two puts a blank line in
    // the middle of the user's value.
    const r = parseCsv('"a\r\nb",c');
    assert.equal(r.rows[0]!.cells[0]!.value, 'a\nb');
  });

  test('handles a quoted value with the delimiter inside it', () => {
    assert.deepEqual(row('"a,b",c'), ['a,b', 'c']);
  });

  test('handles an escaped quote', () => {
    assert.deepEqual(row('"say ""hi""",c'), ['say "hi"', 'c']);
  });

  // A quote only opens a quoted run at the START of a field. Anywhere else it
  // is a character, and treating it as syntax is how a hand-edited file
  // becomes unimportable.
  test('treats a quote in the middle of a bare field as data', () => {
    const r = parseCsv('he said "hi", ok');
    assert.deepEqual(r.rows[0]!.cells.map((c) => c.value), ['he said "hi"', ' ok']);
  });

  test('warns about a stray quote without rejecting the row', () => {
    const r = parseCsv('he said "hi", ok');
    assert.equal(r.warnings.length, 2);
    assert.equal(r.truncatedAtLine, null);
    assert.match(r.warnings[0]!.message, /stray quote/);
  });

  // A blank line in the middle is a GAP, not a row. `a\n\nb` is two values.
  // Counting it as a row is how a 500-row export reports 503.
  test('does not make a blank line in the middle into a row', () => {
    assert.equal(parseCsv('a\n\nb').rows.length, 2);
  });

  test('does not make a trailing newline into a row', () => {
    assert.equal(parseCsv('a,b\n').rows.length, 1);
  });

  test('does not need a trailing newline for a final row', () => {
    assert.equal(parseCsv('a,b').rows.length, 1);
  });

  // A quoted empty is DATA. A user who wrote `""` wrote a value, and dropping
  // it silently is the thing this whole module exists to avoid.
  test('keeps a quoted empty field as a value', () => {
    const r = parseCsv('a\n""\nb');
    assert.equal(r.rows.length, 3);
    assert.equal(r.rows[1]!.cells[0]!.value, '');
    assert.equal(r.rows[1]!.cells[0]!.quoted, true);
  });

  // The one hard error. The unterminated field has swallowed the rest of the
  // file, so `truncatedAtLine` tells the caller to refuse the import -- but the
  // rows that DID parse are still returned, because a file that reads as empty
  // and a file that reads as complete-but-wrong are different bug reports.
  test('reports an unterminated quoted field', () => {
    const r = parseCsv('a,"b\nc');
    assert.equal(r.truncatedAtLine, 1);
  });

  test('keeps the rows that parsed before a truncation', () => {
    const r = parseCsv('a\nb\n"c\nd');
    assert.equal(r.rows.length, 3);
    assert.equal(r.truncatedAtLine, 3);
  });

  test('a well-formed file is not truncated', () => {
    assert.equal(parseCsv('a,b\nc,d').truncatedAtLine, null);
  });

  test('reports the line a cell starts on, not where it ends', () => {
    // A message about a bad value should point at where the user would look for
    // it in their editor, which is the start of the value.
    const r = parseCsv('a\n"b\nc",d');
    assert.equal(r.rows[1]!.cells[0]!.line, 2);
  });

  test('reports the row a truncated field starts on', () => {
    const r = parseCsv('a\nb\n"c');
    assert.equal(r.rows[2]!.cells[0]!.line, 3);
  });

  test('an explicit delimiter overrides the sniff', () => {
    assert.deepEqual(row('a,b;c', 0, ';'), ['a,b', 'c']);
  });

  test('an empty string delimiter falls back to the sniff', () => {
    assert.deepEqual(row('a,b', 0, ''), ['a', 'b']);
  });

  test('records the delimiter it used', () => {
    assert.equal(parseCsv('a;b').delimiter, ';');
  });

  test('handles a single column file', () => {
    assert.deepEqual(column('a\nb\nc'), ['a', 'b', 'c']);
  });

  test('handles an empty file as no rows', () => {
    assert.equal(parseCsv('').rows.length, 0);
  });
});

describe('importCsv', () => {
  test('imports one column by default', () => {
    const r = importCsv('a\nb\nc');
    assert.deepEqual(r.values.map((v) => v.value), ['a', 'b', 'c']);
    assert.equal(r.rowCount, 3);
  });

  test('drops duplicates already in the field', () => {
    const r = importCsv('a\nb\nc', { existing: ['b'] });
    assert.deepEqual(r.values.map((v) => v.value), ['a', 'c']);
    assert.equal(r.duplicates, 1);
  });

  // Matching `parsePaste` exactly, so that pasting and importing the same list
  // cannot produce two different sets.
  test('drops duplicates within the file itself', () => {
    const r = importCsv('a\na\nb');
    assert.deepEqual(r.values.map((v) => v.value), ['a', 'b']);
  });

  test('does not fold case, because two tags differing in case are two entities', () => {
    const r = importCsv('Best', { existing: ['best'] });
    assert.deepEqual(r.values.map((v) => v.value), ['Best']);
  });

  // The header is the CALLER's decision. A column of tags whose first tag is
  // `name` is a real possibility, and a parser that guesses would drop a tag
  // and call the import clean.
  test('excludes the header row when told there is one', () => {
    const r = importCsv('name,age\nfoo,3\nbar,4', { hasHeader: true });
    assert.deepEqual(r.values.map((v) => v.value), ['foo', 'bar']);
  });

  test('includes the first row when told there is no header', () => {
    const r = importCsv('name,age', {});
    assert.deepEqual(r.values.map((v) => v.value), ['name']);
  });

  test('returns the column names so a UI can offer the choice', () => {
    const r = importCsv('name,age\nfoo,3', { hasHeader: true });
    assert.deepEqual(r.columnNames, ['name', 'age']);
  });

  test('returns no column names without a header', () => {
    assert.deepEqual(importCsv('a,b').columnNames, []);
  });

  test('reads the column named', () => {
    const r = importCsv('name,age\nfoo,3\nbar,4', { hasHeader: true, column: 'age' });
    assert.deepEqual(r.values.map((v) => v.value), ['3', '4']);
  });

  test('reads the column at an index', () => {
    const r = importCsv('a,b\nc,d', { column: 1 });
    assert.deepEqual(r.values.map((v) => v.value), ['b', 'd']);
  });

  // A stale choice must not crash an import. A UI that offered every column
  // can have one deselected by the time the file is read.
  test('an out-of-range column imports nothing rather than throwing', () => {
    const r = importCsv('a,b', { column: 7 });
    assert.deepEqual(r.values, []);
    assert.equal(r.skipped, 1);
  });

  test('an unknown column name falls back to the first column', () => {
    const r = importCsv('name,age\nfoo,3', { hasHeader: true, column: 'nope' });
    assert.deepEqual(r.values.map((v) => v.value), ['foo']);
  });

  // RFC 4180 says a space after a comma is part of the field, and that is right
  // for a general reader. A tag importer wants it gone, so it is a choice.
  test('keeps a leading space by default', () => {
    // Reading column 1 explicitly, because with the default of column 0 this
    // file's only visible value is `a` and the assertion would pass whatever
    // the parser did to the space.
    const r = importCsv('a, b', { column: 1 });
    assert.deepEqual(r.values.map((v) => v.value), [' b']);
  });

  test('trims when asked', () => {
    const r = importCsv('a, b', { column: 1, trimValues: true });
    assert.deepEqual(r.values.map((v) => v.value), ['b']);
  });

  // The reason `trimValues` exists as an option rather than being always on: a
  // tool importing free text must not have its data silently changed.
  test('trimming turns a padded value into a duplicate of the bare one', () => {
    const r = importCsv(' a \na', { trimValues: true });
    assert.deepEqual(r.values.map((v) => v.value), ['a']);
    assert.equal(r.duplicates, 1);
  });

  test('keeps a quoted empty as a value', () => {
    const r = importCsv('a\n""\nb');
    assert.deepEqual(r.values.map((v) => v.value), ['a', '', 'b']);
  });

  // A blank line in the middle is a GAP. It is not a row -- `a\n\nb` is two
  // values -- but it is also not nothing, and a count of 0 for "we discarded a
  // line" would be a lie. It is reported on its own, separate from `skipped`,
  // which counts rows that existed and held no value.
  test('counts a blank line as a gap, not as a row and not as nothing', () => {
    const r = importCsv('a\n\nb');
    assert.deepEqual(r.values.map((v) => v.value), ['a', 'b']);
    assert.equal(r.rowCount, 2);
    assert.equal(r.blankLines, 1);
    assert.equal(r.skipped, 0);
  });

  test('counts each of several blank lines', () => {
    const r = importCsv('a\n\n\n\nb');
    assert.equal(r.blankLines, 3);
    assert.deepEqual(r.values.map((v) => v.value), ['a', 'b']);
  });

  // A file of nothing but empty cells has rows and no values, which is a
  // different sentence from a file that does not exist. The FIRST blank line
  // is kept as a row so the file is not reported as zero rows; the rest are
  // gaps.
  test('distinguishes rows-with-no-values from an empty file', () => {
    const r = importCsv('\n\n');
    assert.equal(r.rowCount, 1);
    assert.equal(r.skipped, 1);
    assert.equal(r.blankLines, 1);
    assert.match(describeImport(r), /No values in 1 row/);
  });

  // A short row is a malformed row. Defaulting the missing cell to the empty
  // string would put an untaggable value in the field.
  test('counts a short row as skipped, not as an empty value', () => {
    const r = importCsv('a,b\nc', { column: 1 });
    assert.deepEqual(r.values.map((v) => v.value), ['b']);
    assert.equal(r.skipped, 1);
  });

  test('carries the source line of each value', () => {
    const r = importCsv('name\nfoo\nbar', { hasHeader: true });
    assert.deepEqual(r.values.map((v) => v.line), [2, 3]);
  });

  // `cell.line` and `row.line` are the same number for an ordinary row, so the
  // test above cannot tell them apart -- a mutation swapping the two survives
  // it. They differ only when a quoted field holds a newline: the ROW starts
  // earlier than the CELL being read, because the preceding cells on that row
  // are what pushed the line counter forward. Reading the second column of such
  // a row is the only place the two disagree, and it is a real report to a user
  // chasing a bad value in their editor.
  test('a cell line is its own start, not its row\'s start', () => {
    const r = importCsv('name,note\nfoo,"line one\nline two",x', {
      hasHeader: true,
      column: 2,
    });
    // The row begins on line 2; the cell being read begins on line 3.
    assert.deepEqual(r.values.map((v) => v.line), [3]);
    // And the parsed row itself says 2, which is the number the mutation would
    // have reported.
    assert.equal(parseCsv('name,note\nfoo,"line one\nline two",x').rows[1]!.line, 2);
  });

  test('passes the truncation through', () => {
    const r = importCsv('a\n"b');
    assert.equal(r.truncatedAtLine, 2);
  });

  test('passes the warnings through', () => {
    const r = importCsv('a "b" c');
    assert.ok(r.warnings.length > 0);
  });

  test('reports the delimiter it used', () => {
    assert.equal(importCsv('a;b').delimiter, ';');
  });
});

describe('describeImport', () => {
  test('counts what will be added', () => {
    assert.equal(describeImport(importCsv('a\nb')), 'Will add 2 values.');
  });

  test('uses the singular for one', () => {
    assert.equal(describeImport(importCsv('a')), 'Will add 1 value.');
  });

  // The two counts, because one number silently chooses between "you added two
  // things" and "you asked about five".
  test('accounts for duplicates as well as additions', () => {
    const text = 'a\nb\nc\nd\ne';
    const r = importCsv(text, { existing: ['c', 'd'] });
    assert.equal(describeImport(r), 'Will add 3 values, 2 values already in the field.');
  });

  test('says so when everything is a duplicate', () => {
    const r = importCsv('a\nb', { existing: ['a', 'b'] });
    assert.equal(describeImport(r), 'Nothing new — all 2 values already in the field.');
  });

  // The truncation message is the most important sentence in the module: the
  // file is NOT what the user thinks it is, and importing it silently loses the
  // tail.
  test('refuses a truncated file and says which line is unclosed', () => {
    const r = importCsv('a\n"b\nc');
    assert.match(describeImport(r), /^File is truncated/);
    assert.match(describeImport(r), /line 2/);
  });

  // A clean import does not name the delimiter. It is only worth a user's
  // attention when the file is ragged, and a sentence that always mentions the
  // delimiter is a sentence nobody reads.
  test('names the delimiter only when a value was skipped', () => {
    const clean = importCsv('a\nb');
    assert.ok(!describeImport(clean).includes('semicolon'));

    // The trailing separator is what makes the last cell empty, which is what
    // makes a row get skipped -- a file of `a;b;c` has nothing to report.
    const r = importCsv('a;b;c\n\nc;d;', { column: 2 });
    assert.match(describeImport(r), /semicolon/);
  });

  // A file with no rows is not a file whose rows are empty. "No values in 0
  // rows" describes a file that does not exist, which is not what the user
  // picked.
  test('says the file is empty when it has no rows at all', () => {
    assert.equal(describeImport(importCsv('')), 'Nothing to import — the file is empty.');
  });
});

describe('delimiterName', () => {
  test('names the four it sniffs', () => {
    assert.equal(delimiterName(','), 'comma');
    assert.equal(delimiterName(';'), 'semicolon');
    assert.equal(delimiterName('\t'), 'tab');
    assert.equal(delimiterName('|'), 'pipe');
  });

  // An unknown delimiter is shown as itself rather than as an escape, because
  // the reader has to recognise it to understand the sentence.
  test('shows any other delimiter literally', () => {
    assert.equal(delimiterName('~'), '"~"');
  });
});
