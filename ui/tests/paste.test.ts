/*
  Tests for the paste parser. Spec 10.10, plan T-P5-006 item 11; #7139.

  # What is being tested here

  The decisions, at their boundaries. A paste is one string that has to become
  N values, and every rule below is a guess that can be wrong:

   - a separator only counts inside a quote, so `"Smith, John"` is ONE value and
     `A, B, C` is three. This is the whole design and it is the easiest thing
     to get subtly wrong, so most of the tests are about it;
   - a duplicate is dropped, not appended as "B (2)";
   - a blank line is not a value — an empty string in a tag field matches
     nothing and shows up in every autocomplete;
   - CRLF, lone CR and LF all split, because a paste from a spreadsheet on
     Windows is CRLF and from a Linux tool is LF, and the user does not know
     which they copied.

  A test asserting `splitLine('A, B')` is two values is a test of the
  implementation. Each test below is about a decision a user could disagree
  with, and each would still fail if the function were rewritten to be more or
  less clever.
 */

import { strict as assert } from 'node:assert';
import { describe, it } from 'node:test';

import {
  addCount,
  hasOpenQuote,
  parsePaste,
  preview,
  splitLine,
  unquote
} from '../src/lib/api/paste.js';

const values = (r: { values: readonly { value: string }[] }): string[] =>
  r.values.map((v) => v.value);

describe('hasOpenQuote', () => {
  it('is false for a line with no quotes', () => {
    assert.equal(hasOpenQuote('A, B, C'), false);
  });

  it('is false for a balanced pair', () => {
    assert.equal(hasOpenQuote('"A, B"'), false);
  });

  it('is true for an unclosed quote', () => {
    assert.equal(hasOpenQuote('"Smith, John'), true);
  });

  // The case a `.includes('"')` gets wrong: a doubled quote inside a quoted
  // field is an escaped quote, not a close followed by an open.
  it('is false when a doubled quote is the only quote in the line', () => {
    assert.equal(hasOpenQuote('"say ""hi"""'), false);
  });
});

describe('unquote', () => {
  it('strips one layer of surrounding quotes', () => {
    assert.equal(unquote('"A, B"'), 'A, B');
  });

  it('unescapes a doubled quote', () => {
    assert.equal(unquote('"say ""hi"""'), 'say "hi"');
  });

  it('leaves an unquoted value alone', () => {
    assert.equal(unquote('  A, B  '), 'A, B');
  });

  it('leaves a lone quote character alone', () => {
    // A one-character string cannot be a quoted pair; stripping its only
    // character would erase the user's data.
    assert.equal(unquote('"'), '"');
  });
});

describe('splitLine', () => {
  it('splits on a comma', () => {
    assert.deepEqual(splitLine('A, B, C'), ['A', 'B', 'C']);
  });

  it('splits on a semicolon', () => {
    assert.deepEqual(splitLine('A; B; C'), ['A', 'B', 'C']);
  });

  it('returns nothing for a blank line', () => {
    assert.deepEqual(splitLine('   '), []);
  });

  it('returns one value for a single unquoted value', () => {
    assert.deepEqual(splitLine('A'), ['A']);
  });

  // The design decision, stated as a test.
  it('keeps a separator inside a quoted value as data', () => {
    assert.deepEqual(splitLine('"Smith, John", Jr'), ['Smith, John', 'Jr']);
  });

  it('keeps a whole quoted line as one value', () => {
    assert.deepEqual(splitLine('"A, B, C"'), ['A, B, C']);
  });

  it('tolerates unbalanced whitespace around each value', () => {
    assert.deepEqual(splitLine('  A  ,  B  '), ['A', 'B']);
  });

  it('drops an empty value from a trailing separator', () => {
    // "A, B," must not produce a third empty value: an empty string in a tag
    // field matches nothing and appears in every autocomplete.
    assert.deepEqual(splitLine('A, B,'), ['A', 'B']);
  });

  it('drops empty values from a doubled separator', () => {
    assert.deepEqual(splitLine('A,,B'), ['A', 'B']);
  });

  it('handles a mix of both separators', () => {
    assert.deepEqual(splitLine('A, B; C'), ['A', 'B', 'C']);
  });

  // Found by the mutation pass, not by reading the code. The first version kept
  // quote characters in the buffer and stripped them at the end, so this line --
  // balanced quotes, a separator OUTSIDE them -- read as one value with stray
  // quotes in it. A quote protects a separator only while the separator is
  // inside it.
  it('splits a line whose quotes do not wrap a separator', () => {
    assert.deepEqual(splitLine('"A", B'), ['A', 'B']);
  });

  it('does not leave quote characters in a split value', () => {
    for (const v of splitLine('"A", "B"')) {
      assert.ok(!v.includes('"'), `value kept a quote: ${JSON.stringify(v)}`);
    }
  });

  it('keeps an escaped quote as one literal quote', () => {
    const input = '"say ""hi""", B';
    assert.deepEqual(splitLine(input), ['say "hi"', 'B']);
  });
});

describe('parsePaste', () => {
  it('splits a newline-separated paste', () => {
    assert.deepEqual(values(parsePaste('A\nB\nC')), ['A', 'B', 'C']);
  });

  it('splits a CRLF paste', () => {
    // A spreadsheet on Windows puts CRLF on the clipboard, and a parser that
    // splits only on LF leaves a trailing CR on every value after the first.
    assert.deepEqual(values(parsePaste('A\r\nB\r\nC')), ['A', 'B', 'C']);
  });

  it('splits a lone-CR paste', () => {
    assert.deepEqual(values(parsePaste('A\rB\rC')), ['A', 'B', 'C']);
  });

  it('does not split a CRLF paste into values with a CR', () => {
    // The bug this asserts against: `split('\n')` alone leaves "B\r", and the
    // CR is invisible in a UI and ruins every later comparison against the
    // stored value.
    for (const v of values(parsePaste('A\r\nB\r\nC'))) {
      assert.ok(!v.includes('\r'), `value has a CR: ${JSON.stringify(v)}`);
    }
  });

  it('parses a spreadsheet-style comma paste', () => {
    assert.deepEqual(values(parsePaste('A, B, C')), ['A', 'B', 'C']);
  });

  it('parses a mixed newline-and-comma paste', () => {
    assert.deepEqual(values(parsePaste('A, B\nC, D')), ['A', 'B', 'C', 'D']);
  });

  it('drops a value the field already has', () => {
    const r = parsePaste('A\nB\nC', ['B']);
    assert.deepEqual(values(r), ['A', 'C']);
    assert.equal(r.duplicates, 1);
  });

  it('drops a duplicate that appears twice in the paste itself', () => {
    // Not only a value already in the FIELD: a paste that repeats a value must
    // not add it twice, or "B, B" becomes two tags with one name.
    const r = parsePaste('A\nB\nB');
    assert.deepEqual(values(r), ['A', 'B']);
    assert.equal(r.duplicates, 1);
  });

  it('is case-sensitive, because two tags differing in case are two entities', () => {
    const r = parsePaste('Best\nbest', ['Best']);
    assert.deepEqual(values(r), ['best']);
  });

  it('counts a blank line as skipped, not as a value', () => {
    const r = parsePaste('A\n\n\nB');
    assert.deepEqual(values(r), ['A', 'B']);
    assert.equal(r.skipped, 2);
    assert.equal(r.values.length, 2);
  });

  it('reports the line each value came from', () => {
    // A parser that cannot point at a row cannot be argued with by the user
    // who pasted 400 of them.
    const r = parsePaste('A\n\nB');
    assert.deepEqual(
      r.values.map((v) => v.line),
      [1, 3]
    );
  });

  it('parses an empty paste as nothing', () => {
    const r = parsePaste('');
    assert.deepEqual(values(r), []);
    assert.equal(addCount(r), 0);
  });

  it('parses a whitespace-only paste as nothing', () => {
    assert.deepEqual(values(parsePaste('   \n  \n')), []);
  });

  it('keeps a value that is only whitespace inside quotes', () => {
    // An explicitly quoted blank is DATA -- a tag called " " is odd but the
    // user asked for it, and dropping it silently is worse than keeping it.
    assert.deepEqual(splitLine('" "'), [' ']);
  });
});

describe('preview', () => {
  it('says what will be added', () => {
    assert.equal(preview(parsePaste('A\nB')), 'Will add 2 values.');
  });

  it('uses the singular for one', () => {
    assert.equal(preview(parsePaste('A')), 'Will add 1 value.');
  });

  it('names the duplicates rather than hiding them', () => {
    // "Will add 1 value" for a paste of 2 where 1 already exists reads as a
    // mistake; the user needs to know the other one was not lost.
    assert.equal(
      preview(parsePaste('A\nB', ['A'])),
      'Will add 1 value, 1 value already in the field.'
    );
  });

  it('says so when a paste adds nothing', () => {
    assert.equal(
      preview(parsePaste('A\nB', ['A', 'B'])),
      'Nothing new — all 2 values already in the field.'
    );
  });

  it('says so when there is nothing at all to paste', () => {
    assert.equal(preview(parsePaste('')), 'Nothing to paste.');
  });
});

describe('addCount', () => {
  it('is the number of values the paste would add', () => {
    assert.equal(addCount(parsePaste('A\nB\nC', ['B'])), 2);
  });

  it('is zero for an all-duplicate paste', () => {
    assert.equal(addCount(parsePaste('A', ['A'])), 0);
  });
});
