/*
  Tests for the per-field ignore lists. Spec 10.10, plan T-P5-006 item 13;
  #2318, #2399.

  # What is being tested here

  The decisions, at their boundaries. An ignore list looks like a `Set`, and a
  `Set` alone is the bug: with one, "ignore nothing" and "ignore everything" are
  both an empty set, and #2399's actual use case is an allow-list wearing an
  ignore-list's clothes.

  One test here is not a unit test at all: the vocabulary in
  `tagger-fields.ts` is claimed to be derived from the Rust schema, and that
  claim rots the moment a struct gains a field. So a test reads
  `crates/commons-core/src/domain.rs` and checks both directions.
*/

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import {
  NO_IGNORES,
  describeScope,
  ignoreAllExcept,
  ignoreEverything,
  ignoredFields,
  isIgnored,
  isValidFieldName,
  keptFields,
  normaliseAll,
  normaliseField,
  rejectedFields,
  scopeFrom,
  scopeToQuery,
  splitByField,
  unknownEntries,
} from '../src/lib/api/ignore-list.js';
import {
  FIELDS_BY_SUBJECT,
  isOfferable,
  KNOWN_TAGGER_FIELDS,
  NOT_OFFERABLE_BY_SUBJECT,
  NOT_OFFERABLE_FIELDS,
  OBJECT_FIELDS,
  PERFORMER_FIELDS,
  PRODUCER_FIELDS,
  TAG_FIELDS,
} from '../src/lib/api/tagger-fields.js';

describe('normaliseField', () => {
  test('trims and lower-cases', () => {
    assert.equal(normaliseField('  Title  '), 'title');
  });

  // Without this, a list containing both is one entry that looks like two, and a
  // user who cannot see why only one took effect concludes the feature is
  // broken.
  test('two spellings differing only in case are one field', () => {
    const scope = scopeFrom(['Title', 'title', 'TITLE']);
    assert.deepEqual(scope.fields, ['title']);
  });
});

describe('isValidFieldName', () => {
  test('accepts a plain name', () => {
    assert.equal(isValidFieldName('title'), true);
  });

  // A scraper with an unusual field name must still be silenceable, so the rule
  // is permissive about shape and strict only about what would break a query.
  test('accepts dots, dashes and underscores', () => {
    for (const f of ['studio.alias', 'performer-fav', 'birth_date', 'ns2']) {
      assert.equal(isValidFieldName(f), true, f);
    }
  });

  test('rejects the empty string', () => {
    assert.equal(isValidFieldName(''), false);
  });

  test('rejects whitespace-only', () => {
    assert.equal(isValidFieldName('   '), false);
  });

  test('rejects an inner space', () => {
    assert.equal(isValidFieldName('performer name'), false);
  });

  test('rejects a slash, which would break a URL', () => {
    assert.equal(isValidFieldName('performer/name'), false);
  });

  test('rejects a quote, which would break a selector', () => {
    assert.equal(isValidFieldName(`tit"le`), false);
  });

  test('accepts a name that is valid only after normalising', () => {
    assert.equal(isValidFieldName(' Title '), true);
  });
});

describe('normaliseAll', () => {
  test('sorts, so two users with the same list have the same list', () => {
    assert.deepEqual(normaliseAll(['title', 'date', 'producer']), ['date', 'producer', 'title']);
  });

  test('drops duplicates', () => {
    assert.deepEqual(normaliseAll(['a', 'a', 'b']), ['a', 'b']);
  });

  test('drops empties', () => {
    assert.deepEqual(normaliseAll(['a', '', '   ']), ['a']);
  });
});

describe('isIgnored', () => {
  test('nothing is ignored with no scope', () => {
    assert.equal(isIgnored(NO_IGNORES, 'title'), false);
  });

  test('a listed field is ignored', () => {
    const scope = scopeFrom(['title']);
    assert.equal(isIgnored(scope, 'title'), true);
  });

  test('an unlisted field is not ignored', () => {
    const scope = scopeFrom(['title']);
    assert.equal(isIgnored(scope, 'date'), false);
  });

  // THE boundary. A scope is checked in both directions, and a test that only
  // checks one of them is a test that half the implementation could break.
  test('ignoring everything inverts the test', () => {
    const scope = ignoreEverything();
    assert.equal(isIgnored(scope, 'title'), true);
    assert.equal(isIgnored(scope, 'date'), true);
    assert.equal(isIgnored(scope, 'anything_at_all'), true);
  });

  // #2399's real use case: search on title and date, ignore the other forty
  // fields. An allow-list wearing an ignore-list's clothes.
  test('ignoring all but two keeps exactly those two', () => {
    const scope = ignoreAllExcept(['title', 'date']);
    assert.equal(isIgnored(scope, 'title'), false);
    assert.equal(isIgnored(scope, 'date'), false);
    assert.equal(isIgnored(scope, 'description'), true);
  });

  test('an empty field name is not ignored', () => {
    // Nothing to ignore, and a list that somehow contains '' must not make every
    // proposal disappear.
    assert.equal(isIgnored(ignoreEverything(), ''), false);
  });

  test('matching is case-insensitive on the query side too', () => {
    assert.equal(isIgnored(scopeFrom(['title']), 'TITLE'), true);
  });
});

describe('ignoredFields and keptFields', () => {
  const all = ['title', 'description', 'date'];

  test('lists the ignored ones in the order given', () => {
    assert.deepEqual(ignoredFields(scopeFrom(['description']), all), ['description']);
  });

  test('lists the kept ones in the order given', () => {
    assert.deepEqual(keptFields(scopeFrom(['description']), all), ['title', 'date']);
  });

  // Order follows the input, not the scope, and the inputs here are DELIBERATELY
  // not in alphabetical order. The first version of this test used
  // `['title', 'description', 'date']` for `ignoredFields`, which is already
  // alphabetical -- so adding a `.sort()` changed nothing and the mutation
  // survived. Naming a boundary means picking inputs where the two behaviours
  // differ, which here means an input that is not already sorted.
  test('preserves the input order rather than sorting', () => {
    assert.deepEqual(keptFields(NO_IGNORES, ['date', 'title']), ['date', 'title']);
    assert.deepEqual(ignoredFields(scopeFrom(['date']), ['title', 'date']), ['date']);
    // Unambiguous: a sorted implementation would return this differently.
    assert.deepEqual(keptFields(NO_IGNORES, ['zebra', 'apple', 'mango']), [
      'zebra',
      'apple',
      'mango',
    ]);
    // TWO ignored fields, because ONE sorts to itself and the first version of
    // this assertion used a single field, so adding `.sort()` to ignoredFields
    // changed nothing and the mutation survived. Two in non-alphabetical input
    // order is the smallest case where the two implementations differ.
    assert.deepEqual(ignoredFields(scopeFrom(['mango', 'apple']), ['zebra', 'mango', 'apple']), [
      'mango',
      'apple',
    ]);
  });

  test('the two partition the input', () => {
    const scope = scopeFrom(['date']);
    assert.deepEqual([...ignoredFields(scope, all), ...keptFields(scope, all)].sort(), [...all].sort());
  });
});

describe('splitByField', () => {
  interface P {
    field: string;
    value: string;
  }
  const proposals: P[] = [
    { field: 'title', value: 'A' },
    { field: 'date', value: '2024-01-01' },
    { field: 'description', value: 'long' },
  ];

  test('puts an ignored field on the ignored side', () => {
    const { kept, ignored } = splitByField(scopeFrom(['date']), proposals, (p) => p.field);
    assert.deepEqual(kept.map((p) => p.field), ['title', 'description']);
    assert.deepEqual(ignored.map((p) => p.field), ['date']);
  });

  // Both halves, not just the kept one. A tagger that shows only the kept
  // proposals cannot tell the user that a field they expected was dropped.
  test('returns both halves, and they partition the input', () => {
    const { kept, ignored } = splitByField(NO_IGNORES, proposals, (p) => p.field);
    assert.equal(kept.length + ignored.length, proposals.length);
  });

  test('preserves order within each half', () => {
    const { ignored } = splitByField(ignoreEverything(), proposals, (p) => p.field);
    assert.deepEqual(ignored.map((p) => p.field), ['title', 'date', 'description']);
  });

  test('an empty input gives two empty halves', () => {
    const { kept, ignored } = splitByField(NO_IGNORES, [], () => 'x');
    assert.deepEqual(kept, []);
    assert.deepEqual(ignored, []);
  });
});

describe('scopeFrom', () => {
  test('normalises, dedupes and sorts', () => {
    assert.deepEqual(scopeFrom([' Title ', 'title', 'date']).fields, ['date', 'title']);
  });

  // A settings file is user-editable and one bad entry should cost that entry,
  // not the setting.
  test('drops an invalid entry rather than throwing', () => {
    const scope = scopeFrom(['title', 'performer name']);
    assert.deepEqual(scope.fields, ['title']);
  });

  test('reports which entries were dropped', () => {
    assert.deepEqual(rejectedFields(['title', 'performer name', 'a/b']), [
      'performer name',
      'a/b',
    ]);
  });

  test('reports nothing dropped when everything is valid', () => {
    assert.deepEqual(rejectedFields(['title', 'date']), []);
  });

  test('can build an inverted scope', () => {
    const scope = scopeFrom(['title'], true);
    assert.equal(scope.ignoreAll, true);
    assert.deepEqual(scope.fields, ['title']);
  });
});

describe('scopeToQuery', () => {
  // Repeated parameters rather than a joined string: a field name is not going
  // to contain a comma, but the separator a caller would pick if it were is
  // exactly what eventually splits a setting in the wrong place.
  // Sorted, not insertion order: a scope is stored sorted so two users who
  // built the same list by hand have the same list, and a query that came out in
  // insertion order would make two identical settings look different.
  test('repeats the parameter for an ignore list, in sorted order', () => {
    assert.equal(scopeToQuery(scopeFrom(['title', 'date'])), 'ignore=date&ignore=title');
    assert.equal(
      scopeToQuery(scopeFrom(['title', 'date'])),
      scopeToQuery(scopeFrom(['date', 'title'])),
    );
  });

  // "ignore everything except X, Y" cannot be an ignore list without a second
  // parameter meaning two things.
  test('inverts to keep= for an inverted scope', () => {
    assert.equal(scopeToQuery(ignoreAllExcept(['title', 'date'])), 'keep=date&keep=title');
  });

  test('an empty ignore list is an empty query', () => {
    assert.equal(scopeToQuery(NO_IGNORES), '');
  });

  test('an inverted scope with no exceptions still says keep=', () => {
    // Not an empty query: an empty query means "no filter", which would be read
    // as "ignore nothing" rather than "ignore everything".
    assert.equal(scopeToQuery(ignoreEverything(), ['title', 'date']), 'keep=');
  });

  test('encodes a name that needs it', () => {
    assert.equal(scopeToQuery(scopeFrom(['studio.alias'])), 'ignore=studio.alias');
  });
});

describe('unknownEntries', () => {
  // Reported, never removed. A scraper with a field this build has never seen is
  // a normal thing, and the entry is still doing its job.
  test('reports an entry this build has never heard of', () => {
    assert.deepEqual(unknownEntries(scopeFrom(['title', 'from_the_year_3000'])), [
      'from_the_year_3000',
    ]);
  });

  test('reports nothing for a known entry', () => {
    assert.deepEqual(unknownEntries(scopeFrom(['title', 'date'])), []);
  });

  test('reports nothing for an empty scope', () => {
    assert.deepEqual(unknownEntries(NO_IGNORES), []);
  });
});

describe('describeScope', () => {
  // A settings row that says "3 fields ignored" tells a user nothing they can
  // check. §10.7: an action states its scope.
  test('names the fields when there are few', () => {
    assert.equal(describeScope(scopeFrom(['title', 'date'])), 'date and title are ignored.');
  });

  test('uses the singular for one', () => {
    assert.equal(describeScope(scopeFrom(['title'])), 'title is ignored.');
  });

  test('says so when nothing is ignored', () => {
    assert.equal(describeScope(NO_IGNORES), 'No fields are ignored.');
  });

  test('says so when everything is ignored', () => {
    assert.equal(
      describeScope(ignoreEverything(), ['title', 'date']),
      'Every field is ignored.',
    );
  });

  test('names the exceptions for an inverted scope', () => {
    assert.equal(
      describeScope(ignoreAllExcept(['title', 'date']), ['title', 'date', 'x']),
      'Every field is ignored except title and date.',
    );
  });

  test('inline-joins three with commas and an and', () => {
    assert.equal(describeScope(scopeFrom(['a', 'b', 'c'])), 'a, b and c are ignored.');
  });
});

// ---------------------------------------------------------------------------
// The vocabulary, pinned to the schema
// ---------------------------------------------------------------------------

/**
 * The Rust source the vocabulary claims to come from.
 *
 * Located through `COMMONS_UI_SRC` rather than through `import.meta.url`. The
 * compiled test runs from a temp directory, so a path relative to the module
 * resolves into that temp tree and `readFileSync` throws — which is a test
 * failure that looks like a broken vocabulary and is actually a broken path.
 * `tests/invariants.test.ts` already uses this variable for the same reason, and
 * `tests/run-tests.mjs` is what sets it.
 *
 * Read rather than duplicated: a hand-copied struct in a test file is a second
 * place to update and a second thing to forget, and the failure mode of
 * forgetting is a green suite over a stale list.
 */
/**
 * Offered names that are not the column's own name.
 *
 * One entry today. It lives in the test rather than in `tagger-fields.ts`
 * because it is a claim ABOUT the schema, and the schema is what the test
 * checks — putting the answer next to the question would make the test agree
 * with itself.
 */
const RENAMED_FIELDS: ReadonlyMap<string, string> = new Map([['producer', 'producer_id']]);

const UI_SRC = process.env.COMMONS_UI_SRC;
if (UI_SRC === undefined) {
  throw new Error('COMMONS_UI_SRC is not set; run the tests via tests/run-tests.mjs');
}
const DOMAIN_RS = resolve(UI_SRC, '../../crates/commons-core/src/domain.rs');

function schemaFields(structName: string): string[] {
  const src = readFileSync(DOMAIN_RS, 'utf8');
  const start = src.indexOf(`pub struct ${structName} {`);
  assert.notEqual(start, -1, `${structName} not found in domain.rs`);
  const body = src.slice(start, src.indexOf('\n}', start));
  return [...body.matchAll(/pub (\w+):/g)].map((m) => m[1]!);
}

describe('the tagger vocabulary matches the schema', () => {
  // `producer` is the ONE offered name that is not a column: the column is
  // `producer_id`, and the tagger proposes a producer by NAME. A settings row
  // that said "producer_id" would be a foreign key, and ignoring it would not
  // stop a producer being attached -- the control would lie. The exclusion is
  // declared rather than hard-coded here so that the mapping has one home.
  test('every object field offered is a real column, or a declared rename', () => {
    const real = new Set(schemaFields('Object'));
    const renames = new Map(RENAMED_FIELDS);
    for (const f of OBJECT_FIELDS) {
      const column = renames.get(f) ?? f;
      assert.ok(real.has(column), `object.${f} is offered but Object has no column ${column}`);
    }
  });

  test('every performer field offered is a real column', () => {
    const real = new Set(schemaFields('Performer'));
    for (const f of PERFORMER_FIELDS) {
      assert.ok(real.has(f), `performer.${f} is offered but Performer has no such column`);
    }
  });

  test('every tag field offered is a real column', () => {
    const real = new Set(schemaFields('Tag'));
    for (const f of TAG_FIELDS) {
      assert.ok(real.has(f), `tag.${f} is offered but Tag has no such column`);
    }
  });

  // The direction that rots. A struct gains a field, this list does not, and
  // the field is then un-ignorable with nothing indicating why.
  test('every user-facing column of Object is offered', () => {
    const real = new Set(schemaFields('Object'));
    const notOfferable = new Set(NOT_OFFERABLE_FIELDS);
    // Reverse the rename map: a column is covered when it is offered under the
    // name that maps to it, not only under its own name.
    const offeredColumns = new Set(
      OBJECT_FIELDS.map((f) => RENAMED_FIELDS.get(f) ?? f),
    );
    for (const f of real) {
      if (notOfferable.has(f)) continue;
      assert.ok(
        offeredColumns.has(f),
        `Object.${f} is a real column and is neither in OBJECT_FIELDS nor in NOT_OFFERABLE_FIELDS — add it to one or the other, deliberately`,
      );
    }
  });

  // §7.11 says `career_start` and `career_end` are computed from item dates and
  // never authored. A tagger that could overwrite them would be writing a
  // derived value, which is the whole thing the section forbids -- so the
  // exclusion is asserted by name, because the general "every user-facing
  // column is offered" test treats them as user-facing and skips them silently.
  test('the derived producer columns are excluded by name', () => {
    for (const f of ['career_start', 'career_end']) {
      assert.ok(
        NOT_OFFERABLE_BY_SUBJECT.producer.includes(f),
        `Producer.${f} is derived per 7.11 and must be excluded explicitly`,
      );
    }
  });

  test('the structural tag column is excluded by name', () => {
    // §5.15's tree is set by a move, not by a scrape.
    assert.ok(NOT_OFFERABLE_BY_SUBJECT.tag.includes('parent_id'));
  });

  test('the rating aggregates are excluded by name', () => {
    // §8.8's ratings come from users and the sum is derived.
    for (const f of ['rating_sum', 'rating_count']) {
      assert.ok(NOT_OFFERABLE_BY_SUBJECT.object.includes(f), `Object.${f} must be excluded`);
    }
  });

  test('every user-facing column of Performer is offered', () => {
    const real = new Set(schemaFields('Performer'));
    const notOfferable = new Set(NOT_OFFERABLE_FIELDS);
    for (const f of real) {
      if (notOfferable.has(f)) continue;
      assert.ok(
        (PERFORMER_FIELDS as readonly string[]).includes(f),
        `Performer.${f} is neither offered nor listed as not offerable`,
      );
    }
  });

  test('every user-facing column of Tag is offered', () => {
    const real = new Set(schemaFields('Tag'));
    const notOfferable = new Set(NOT_OFFERABLE_FIELDS);
    for (const f of real) {
      if (notOfferable.has(f)) continue;
      assert.ok(
        (TAG_FIELDS as readonly string[]).includes(f),
        `Tag.${f} is neither offered nor listed as not offerable`,
      );
    }
  });

  // PER SUBJECT, not across the flat list. `kind` is the discriminator on an
  // Object and is not offerable; `kind` is also a real column on a Producer
  // (studio, circle, individual, collective, §5.12) and IS offerable. Both are
  // correct, and a test over the flat list would call the contradiction and
  // force one of them to be deleted.
  test('no field is both offered and not offerable for the SAME subject', () => {
    for (const [subject, fields] of Object.entries(FIELDS_BY_SUBJECT)) {
      const excluded = NOT_OFFERABLE_BY_SUBJECT[
        subject as keyof typeof NOT_OFFERABLE_BY_SUBJECT
      ] as readonly string[];
      const overlapping = (fields as readonly string[]).filter((f) => excluded.includes(f));
      assert.deepEqual(
        overlapping,
        [],
        `${subject} offers ${overlapping.join(', ')} and also excludes it`,
      );
    }
  });

  // The whole point of the test above: the same NAME, both verdicts, different
  // subjects. If this ever stops being true the two lists have been merged by
  // accident and the per-subject test is no longer proving anything.
  test('the name "kind" is not offerable on an object and offerable on a producer', () => {
    // This is the test the per-subject design exists for. With a flat name list
    // it is IMPOSSIBLE to state: the list has one entry for `kind`, and the two
    // subjects need opposite verdicts.
    assert.ok(
      NOT_OFFERABLE_BY_SUBJECT.object.includes('kind'),
      'Object.kind should be excluded',
    );
    assert.ok(
      (PRODUCER_FIELDS as readonly string[]).includes('kind'),
      'Producer.kind should be offerable',
    );
    assert.equal(
      (OBJECT_FIELDS as readonly string[]).includes('kind'),
      false,
      'Object.kind should not be offered',
    );
    // And the union contains it precisely because one side excludes it and the
    // other does not -- which is the case a flat list cannot represent.
    assert.ok(NOT_OFFERABLE_FIELDS.includes('kind'));
  });

  // A declared rename whose target is also on the not-offerable list is a
  // contradiction: the field would be offered under one name and forbidden
  // under the other.
  // `producer_id` IS on the not-offerable list, and this is why that is correct
  // rather than a contradiction. The list means "do not offer a checkbox that
  // edits this column"; `producer` is a checkbox for a DIFFERENT thing — a
  // producer's name, proposed and resolved to an id elsewhere. Ignoring
  // `producer` stops a producer being attached, and ignoring `producer_id`
  // would not, which is the whole reason the rename exists.
  //
  // So the test asserts the relationship holds, not that it is absent.
  test('a renamed field is never offered under its column name as well', () => {
    const offered = new Set<string>(KNOWN_TAGGER_FIELDS);
    for (const [name, column] of RENAMED_FIELDS) {
      assert.ok(offered.has(name), `${name} should be offered`);
      assert.equal(
        offered.has(column),
        false,
        `${column} is the column behind ${name} and should not be offered as its own field`,
      );
    }
  });

  // `name` appears on three subjects and `description` on two, which is
  // CORRECT: they are different fields of different things, and a settings row
  // reads "performer > name", not "name". What must not happen is a duplicate
  // WITHIN one subject, where the list would offer the same checkbox twice.
  test('no subject lists the same field twice', () => {
    for (const [subject, fields] of Object.entries(FIELDS_BY_SUBJECT)) {
      assert.equal(
        new Set(fields).size,
        fields.length,
        `${subject} lists a field twice: ${fields.join(', ')}`,
      );
    }
  });

  // The flat list IS allowed to repeat, and this asserts that knowingly, so a
  // future reader does not "fix" it by de-duplicating and breaking the
  // per-subject counts.
  test('the flat list repeats names that belong to different subjects', () => {
    // Counted from the source lists rather than asserted as a constant, so the
    // number stays right if a subject is added -- which is the point of the
    // test. Asserting `3` outright would fail the day a fourth `name` appeared,
    // and the failure would read as a bug rather than as the list growing.
    const subjectsWithName = Object.values(FIELDS_BY_SUBJECT).filter((f) =>
      (f as readonly string[]).includes('name'),
    ).length;
    const inFlat = KNOWN_TAGGER_FIELDS.filter((f) => f === 'name').length;
    assert.equal(
      inFlat,
      subjectsWithName,
      'the flat list should contain one "name" per subject that has one',
    );
    assert.ok(subjectsWithName > 1, 'the premise needs at least two subjects with a name');
  });

  test('every offered name is a valid field name', () => {
    for (const f of KNOWN_TAGGER_FIELDS) {
      assert.equal(isValidFieldName(f), true, `${f} would be rejected by scopeFrom`);
    }
  });

  // `isOfferable` is the function a settings UI calls, and a mutation that
  // inverts it was not caught by anything: the other tests read the lists
  // directly, so the accessor had no test of its own. A function with no caller
  // under test and no test of its own is the easiest kind of bug to ship.
  test('isOfferable agrees with the per-subject lists', () => {
    for (const [subject, fields] of Object.entries(FIELDS_BY_SUBJECT)) {
      for (const f of fields) {
        assert.equal(
          isOfferable(subject as keyof typeof FIELDS_BY_SUBJECT, f),
          true,
          `${subject}.${f} is offered so isOfferable should say true`,
        );
      }
      for (const f of NOT_OFFERABLE_BY_SUBJECT[subject as keyof typeof NOT_OFFERABLE_BY_SUBJECT]) {
        assert.equal(
          isOfferable(subject as keyof typeof FIELDS_BY_SUBJECT, f),
          false,
          `${subject}.${f} is excluded so isOfferable should say false`,
        );
      }
    }
  });

  // The pair that only the per-subject design can express, through the accessor
  // a UI would actually call.
  test('isOfferable gives opposite answers for "kind" on two subjects', () => {
    assert.equal(isOfferable('object', 'kind'), false);
    assert.equal(isOfferable('producer', 'kind'), true);
  });

  test('every subject maps to a non-empty list', () => {
    for (const [subject, fields] of Object.entries(FIELDS_BY_SUBJECT)) {
      assert.ok(fields.length > 0, `${subject} offers no fields`);
    }
  });
});
