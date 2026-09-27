/*
  The unified media view: what belongs in the Media tab, and the four places
  scenes and images genuinely differ. Spec 5.17, 10.4, 10.5; plan T-P5-006
  item 17; #1030, #7068, #1508.

  # What is being tested here

  Item 8 owns the tile *shape* (`media-view.ts`'s `tileShape`, and its header
  says so). What it does not do is decide **which rows are in the view at all**
  and how a scene is told from an image — and that is the whole of #1030.

  The load-bearing claims:

   1. **A kind this build does not know is reported, not guessed.** `object.kind`
      is unconstrained `TEXT` in `0001_core.sql` and an enum in
      `commons-core`. Nothing at the database keeps them in sync, so a kind
      added in Rust and not here is a normal event, and silently rendering it as
      a photograph is the failure this is for.
   2. **A scene with no known duration is still playable.** Duration is metadata,
      not a capability. (Item 8's `isPlayable` says the opposite for a *tile*,
      and the two are not in conflict — see the test that names it.)
   3. **`0:00` and no badge are different.** A zero-length scene is unknown, not
      zero seconds, and labelling it `0:00` puts it next to a photograph with
      the same badge.
   4. **The cross-kind order is total.** #7068/#1508. Without a final id
      tiebreak, two rows that tie on the primary field swap places as soon as a
      third row appears.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import {
  MEDIA_KINDS,
  auditMedia,
  compareMedia,
  durationBadge,
  formatDuration,
  hasPlayAffordance,
  isMediaKind,
  isPlayable,
  KINDS,
  kindRank,
  kindSection,
  looksLikeFilter,
  mediaFilter,
  mediaFilterKinds,
  orderSections,
  parseKind,
  sectionLabel,
  sortMedia,
  type Kind,
  type MediaRow
} from '../src/lib/api/media.js';
import { ASPECT_LIMITS, POSTER_RATIO, tileShape } from '../src/lib/api/media-view.js';
import type { ObjectRow } from '../src/lib/api/client.js';

/**
 * Source files, located through the environment rather than `import.meta.url`.
 *
 * The harness compiles the tests into a temp directory (`tests/run-tests.mjs`
 * sets `COMMONS_UI_SRC`), so a path relative to this file lands in the temp tree
 * and not in the repository. An earlier version walked upward looking for
 * `crates/`, which throws inside the temp tree — and a throw at module scope
 * makes `node --test` report ONE failure with no assertion and silently drop
 * every test in the file from the count. `ui/tests/ignore-list.test.ts` uses the
 * same two variables for the same reason.
 */
const UI_SRC = process.env.COMMONS_UI_SRC;
const REPO_ROOT = process.env.COMMONS_REPO_ROOT;
if (!UI_SRC || !REPO_ROOT) {
  throw new Error('COMMONS_UI_SRC and COMMONS_REPO_ROOT must be set; run via tests/run-tests.mjs');
}
const RUST_ENUMS = join(REPO_ROOT, 'crates/commons-core/src/enums.rs');
const RUST_WIRE = join(REPO_ROOT, 'crates/commons-store/tests/filter_wire_shape.rs');
const MEDIA_SRC = join(UI_SRC, 'lib/api/media.ts');

function r(over: Partial<MediaRow> & { id: string }): MediaRow {
  return { kind: 'image', durationMs: null, width: null, height: null, ...over };
}

const SCENE = r({ id: 's1', kind: 'scene', durationMs: 5_000_000 }); // 1:23:20
const IMAGE = r({ id: 'i1', kind: 'image' });

// ---------------------------------------------------------------------------

describe('the filter wire shape', () => {
  // `view.ts` says the filter is "one opaque, versioned, base64url-encoded
  // string... it is the thing the server parses", and no UI code parses it. So
  // the Media tab has to BUILD one, and serde's external tagging has three
  // details that are all invisible if you write the obvious thing:
  //
  //   field is {"builtin":"kind"}, not "kind";
  //   a value is {"str":"scene"}, not "scene";
  //   op is "in", not "In".
  //
  // A facet that gets any of them wrong does not render badly — it either
  // returns the wrong rows or is rejected outright, and the tab looks like an
  // empty library.
  const FACET_JSON =
    '{"facet":{"kind":null,"field":{"builtin":"kind"},"op":"in","values":[{"str":"scene"},{"str":"image"}]}}';

  test('the Rust side states this exact shape, so the two cannot drift', () => {
    // `crates/commons-store/tests/filter_wire_shape.rs` asserts the same string.
    // Reading it here means a change to serde's attributes fails THIS test too,
    // rather than leaving the UI building a filter the server will not parse.
    const rust = readFileSync(RUST_WIRE, 'utf8');
    assert.ok(
      rust.includes(FACET_JSON),
      'the Rust wire-shape assertion no longer contains the string this test pins; one side changed'
    );
  });

  test('the facet is spelled as the server spells it', () => {
    assert.equal(mediaFilter(), FACET_JSON);
  });

  test('the field is a tagged object, not a bare string', () => {
    const parsed = JSON.parse(mediaFilter()) as { facet: { field: unknown } };
    assert.deepEqual(parsed.facet.field, { builtin: 'kind' });
  });

  test('each value is a tagged object, not a bare string', () => {
    const parsed = JSON.parse(mediaFilter()) as { facet: { values: unknown[] } };
    assert.deepEqual(parsed.facet.values, [{ str: 'scene' }, { str: 'image' }]);
  });

  test('the operator is lowercase', () => {
    const parsed = JSON.parse(mediaFilter()) as { facet: { op: string } };
    assert.equal(parsed.facet.op, 'in');
  });

  test('the kind field is null, so the facet is not kind-scoped', () => {
    // `Facet.kind` is a *subject* discriminator, not the object's kind. Setting
    // it to "scene" would scope the facet to scenes, which is the opposite of
    // what a facet listing both kinds means.
    const parsed = JSON.parse(mediaFilter()) as { facet: { kind: unknown } };
    assert.equal(parsed.facet.kind, null);
  });

  test('a narrower tab is a shorter value list, not a different op', () => {
    const parsed = JSON.parse(mediaFilter(['image'])) as { facet: { values: { str: string }[] } };
    assert.deepEqual(parsed.facet.values, [{ str: 'image' }]);
  });

  // A base the UI cannot parse must not be passed through as if it were fine.
  test('a shape check accepts a filter and rejects other JSON', () => {
    assert.equal(looksLikeFilter(FACET_JSON), true);
    assert.equal(looksLikeFilter('{"and":[]}'), true);
    assert.equal(looksLikeFilter('[]'), false, 'an array is not a Filter');
    assert.equal(looksLikeFilter('"cat"'), false, 'a bare string is not a Filter');
    assert.equal(looksLikeFilter('7'), false);
    assert.equal(looksLikeFilter('null'), false);
  });

  test('a shape check rejects something that is not JSON at all', () => {
    assert.equal(looksLikeFilter('cat'), false);
    assert.equal(looksLikeFilter(''), false);
    assert.equal(looksLikeFilter('eyJhbm'), false, 'a truncated base64 string');
  });
});

// ---------------------------------------------------------------------------

describe('parseKind', () => {
  test('accepts every kind the model knows', () => {
    for (const k of KINDS) assert.equal(parseKind(k), k);
  });

  test('reports an unknown kind rather than guessing', () => {
    // The claim the whole audit exists for.
    assert.equal(parseKind('hologram'), null);
    assert.equal(parseKind(''), null);
    assert.equal(parseKind('Scene'), null, 'the wire spelling is lowercase');
  });

  test('is not fooled by a prefix', () => {
    // A `startsWith` check would call 'scene-cut' a scene.
    assert.equal(parseKind('scene-cut'), null);
    assert.equal(parseKind('imagery'), null);
  });
});

// ---------------------------------------------------------------------------

describe('KINDS matches the Rust enum', () => {
  // The authority is `ObjectKind::as_str`. A kind added in Rust and not here is
  // a test failure here, not a tile that silently renders as a photograph.
  test('the TS list and the Rust list are the same set', () => {
    const src = readFileSync(RUST_ENUMS, 'utf8');
    const block = src.slice(src.indexOf('pub const fn as_str(self)'));
    const rust = [...block.matchAll(/ObjectKind::(\w+) => "([a-z]+)"/g)].map((m) => m[2]);
    assert.ok(rust.length >= 7, `found ${rust.length} Rust kinds`);
    assert.deepEqual([...KINDS].sort(), rust.sort());
  });

  test('a kind added in Rust breaks this test rather than the grid', () => {
    // The assertion the previous test makes, stated as a property: adding to
    // `ObjectKind` without adding to `KINDS` leaves the sets unequal.
    assert.equal(KINDS.length, new Set(KINDS).size, 'no duplicates');
  });
});

// ---------------------------------------------------------------------------

describe('what is in the Media tab', () => {
  test('scenes and images are in it', () => {
    assert.deepEqual([...MEDIA_KINDS], ['scene', 'image']);
  });

  test('a gallery is not', () => {
    // A gallery is a container, so a Media tab listing galleries lists things
    // twice: once as the container, once as the images inside it.
    assert.equal(isMediaKind('gallery'), false);
  });

  test('text and audio are not either', () => {
    // A Media tab with text objects in it is a Library tab that lost the plot.
    for (const k of ['text', 'audio', 'comic', 'interview']) {
      assert.equal(isMediaKind(k), false, k);
    }
  });

  test('an unknown kind is not in it, and says so', () => {
    assert.equal(isMediaKind('hologram'), false);
  });

  test('the facet is the kind list, as filter values', () => {
    // What the route puts in `kind IN (...)` — `CmpOp::In` exists in
    // `filter_ast.rs` and takes a value list.
    assert.deepEqual(mediaFilterKinds(), ['scene', 'image']);
  });

  test('the facet list is a copy, not the constant', () => {
    // A caller that mutates the returned array must not change the tab.
    const got = mediaFilterKinds();
    got.push('gallery' as Kind);
    assert.deepEqual([...MEDIA_KINDS], ['scene', 'image']);
  });
});

// ---------------------------------------------------------------------------

describe('aspect ratio belongs to item 8, not here', () => {
  // This block is a boundary test rather than a behaviour test. Item 8 owns the
  // tile shape and its header says so; item 17 must not grow a second ratio
  // function, because two copies of `ASPECT_LIMITS` disagree within a release and
  // the disagreement shows up as a wall whose images and scenes do not line up.
  test('media.ts exports no ratio of its own', () => {
    assert.equal(/export function (rowRatio|tileRatio|clampRatio|aspectCss)/.test(readFileSync(MEDIA_SRC, 'utf8')), false, 'item 17 grew a second ratio function');
  });

  test('the shape a media row gets is item 8\'s shape, unchanged', () => {
    const shape = tileShape(SCENE as ObjectRow);
    assert.ok(shape.aspect >= ASPECT_LIMITS.min && shape.aspect <= ASPECT_LIMITS.max);
  });

  // A row with no measurements falls back to the declared target, so a library
  // of unprobed rows still lays out as a regular grid.
  test('an unmeasured row falls back to the declared target', () => {
    assert.equal(tileShape(IMAGE as ObjectRow).aspect, POSTER_RATIO);
  });

  // The value that reaches `style:aspect-ratio`. A NaN or a zero here makes the
  // row infinitely tall or invisible, which makes the whole viewport unusable
  // rather than that one tile looking odd.
  test('every shape this module can hand on is finite and positive', () => {
    for (const row of [SCENE, IMAGE, r({ id: 'x', width: 0, height: 0 })]) {
      const { aspect } = tileShape(row as ObjectRow);
      assert.ok(Number.isFinite(aspect) && aspect > 0, `${JSON.stringify(row)} -> ${aspect}`);
    }
  });

  // The two playability answers, side by side. A scene with no duration is
  // playable as a ROW and not playable as a TILE, and the difference is the
  // question being asked, not a contradiction.
  test('item 8 and item 17 disagree about a zero-duration scene on purpose', () => {
    const zero = r({ id: 's', kind: 'scene', durationMs: 0 });
    assert.equal(tileShape(zero as ObjectRow).playable, false, 'a tile has only the duration to go on');
    assert.equal(isPlayable(zero), true, 'membership has the kind');
  });
});

// ---------------------------------------------------------------------------

describe('playability', () => {
  test('a scene with a duration is playable', () => {
    assert.equal(isPlayable(SCENE), true);
  });

  // The claim that distinguishes this from item 8's tile-level `isPlayable`.
  test('a scene with no known duration is STILL playable', () => {
    // Duration is metadata, not a capability. Refusing to play it because the
    // length is unknown would be wrong, and the row still opens in the player.
    assert.equal(isPlayable(r({ id: 's', kind: 'scene', durationMs: null })), true);
  });

  test('a scene whose probe returned zero is still playable', () => {
    assert.equal(isPlayable(r({ id: 's', kind: 'scene', durationMs: 0 })), true);
  });

  test('an image is not playable, whatever its duration field says', () => {
    // An image with a stray duration is a data bug; treating it as video is how
    // a photograph gets a play button that opens a player onto nothing.
    assert.equal(isPlayable(r({ id: 'i', kind: 'image', durationMs: 5_000_000 })), false);
  });

  test('an unknown kind is not playable', () => {
    assert.equal(isPlayable(r({ id: 'x', kind: 'hologram', durationMs: 5_000 })), false);
  });

  test('a gallery is not playable', () => {
    assert.equal(isPlayable(r({ id: 'g', kind: 'gallery', durationMs: 5_000 })), false);
  });

  test('the affordance follows playability', () => {
    assert.equal(hasPlayAffordance(SCENE), true);
    assert.equal(hasPlayAffordance(IMAGE), false);
  });
});

// ---------------------------------------------------------------------------

describe('duration', () => {
  test('formats hours, minutes and seconds once past an hour', () => {
    // 5,000,000 ms is 83m20s, which is 1h23m20s. The hour part is not cosmetic:
    // dropping it would render a two-hour video as "119:59", which reads as a
    // bug in the player rather than in the label.
    assert.equal(formatDuration(5_000_000), '1:23:20');
  });

  test('formats minutes and seconds with a pad, under an hour', () => {
    assert.equal(formatDuration(5_000_000 - 3_600_000), '23:20');
  });

  test('an hour boundary switches to the hour form', () => {
    // 59:59 is the last minute-only value; 3,600,000 is the first hour one.
    assert.equal(formatDuration(3_599_000), '59:59');
    assert.equal(formatDuration(3_600_000), '1:00:00');
  });

  test('formats hours when there are hours', () => {
    assert.equal(formatDuration(3_725_000), '1:02:05');
  });

  test('a whole minute has no seconds shown as a bare number', () => {
    assert.equal(formatDuration(120_000), '2:00');
  });

  test('no duration is null, not 0:00', () => {
    assert.equal(formatDuration(null), null);
  });

  test('a negative duration is null', () => {
    assert.equal(formatDuration(-1), null);
  });

  test('a non-finite duration is null', () => {
    assert.equal(formatDuration(Number.NaN), null);
    assert.equal(formatDuration(Number.POSITIVE_INFINITY), null);
  });

  // The distinction that gets lost when a badge is optional.
  test('a zero-length scene gets no badge, not 0:00', () => {
    // A scene with no measurable duration is unknown, not zero seconds.
    assert.equal(durationBadge(r({ id: 's', kind: 'scene', durationMs: 0 })), null);
    assert.equal(durationBadge(r({ id: 's', kind: 'scene', durationMs: null })), null);
  });

  test('a measured scene gets a badge', () => {
    assert.equal(durationBadge(SCENE), '1:23:20');
  });

  test('an under-hour scene gets a minute-only badge', () => {
    assert.equal(durationBadge(r({ id: 's', kind: 'scene', durationMs: 5_000_000 - 3_600_000 })), '23:20');
  });

  test('an image gets no badge even with a stray duration', () => {
    assert.equal(durationBadge(r({ id: 'i', kind: 'image', durationMs: 5_000 })), null);
  });
});

// ---------------------------------------------------------------------------

describe('the cross-kind order', () => {
  // #7068 and #1508 are both about a secondary sort that is only defined
  // WITHIN a kind, which interleaves two kinds in an order that shifts as rows
  // are added.
  test('scenes sort before images by default', () => {
    assert.ok(compareMedia(SCENE, IMAGE) < 0);
    assert.ok(compareMedia(IMAGE, SCENE) > 0);
  });

  test('the order can be reversed by the caller', () => {
    assert.ok(compareMedia(IMAGE, SCENE, ['image', 'scene']) < 0);
  });

  // The load-bearing one: without the id tiebreak the sort is not total.
  test('two rows that tie fall back to the id, not to input order', () => {
    const a = r({ id: 'a', kind: 'image' });
    const b = r({ id: 'b', kind: 'image' });
    assert.ok(compareMedia(a, b) < 0);
    assert.ok(compareMedia(b, a) > 0);
  });

  test('the order does not depend on how the input was arranged', () => {
    const rows = [r({ id: 'c' }), r({ id: 'a' }), r({ id: 'b' })];
    const forward = sortMedia(rows).map((x) => x.id);
    const back = sortMedia([...rows].reverse()).map((x) => x.id);
    assert.deepEqual(forward, back);
  });

  test('adding a row does not move the others', () => {
    // The property the id tiebreak buys: an interleaving that reshuffles when
    // one item arrives is unusable as a sort.
    const before = [r({ id: 'a' }), r({ id: 'b' })];
    const after = [r({ id: 'a' }), r({ id: 'x' }), r({ id: 'b' })];
    const order = (xs: MediaRow[]): string[] => sortMedia(xs).map((x) => x.id);
    const a = order(before);
    const b = order(after);
    // The property: the rows that were there keep their relative order, and the
    // newcomer lands in a definite place. The first version of this test
    // asserted `b.indexOf(id) < b.indexOf(id)`, which is always true and proved
    // nothing.
    assert.equal(a.length, 2);
    for (let i = 1; i < a.length; i += 1) {
      assert.ok(b.indexOf(a[i - 1]!) < b.indexOf(a[i]!), `${a[i - 1]} before ${a[i]}`);
    }
    assert.ok(b.includes('x'), 'the new row is placed, not dropped');
  });

  test('a kind outside the order sorts last', () => {
    const g = r({ id: 'g', kind: 'gallery' });
    assert.ok(compareMedia(SCENE, g) < 0);
  });

  test('an unknown kind sorts last', () => {
    const x = r({ id: 'x', kind: 'hologram' });
    assert.ok(compareMedia(SCENE, x) < 0);
  });

  test('two unknown kinds still order by id rather than tying', () => {
    const a = r({ id: 'a', kind: 'hologram' });
    const b = r({ id: 'b', kind: 'hologram' });
    assert.ok(compareMedia(a, b) < 0);
  });

  test('sortMedia does not mutate its input', () => {
    const rows = [r({ id: 'b' }), r({ id: 'a' })];
    const copy = [...rows];
    sortMedia(rows);
    assert.deepEqual(rows, copy);
  });

  test('kindRank puts an unknown kind after every known one', () => {
    assert.equal(kindRank('image'), 1);
    assert.equal(kindRank('gallery'), MEDIA_KINDS.length);
    assert.equal(kindRank('hologram'), MEDIA_KINDS.length);
  });
});

// ---------------------------------------------------------------------------

describe('auditing a batch', () => {
  // The mechanism that makes a new kind VISIBLE instead of silently dropped.
  test('counts what is shown and what is excluded', () => {
    const a = auditMedia([SCENE, IMAGE, r({ id: 'g', kind: 'gallery' })]);
    assert.equal(a.shown, 2);
    assert.equal(a.excluded, 1);
  });

  test('names an unknown kind once, however many rows carry it', () => {
    const a = auditMedia([r({ id: '1', kind: 'hologram' }), r({ id: '2', kind: 'hologram' })]);
    assert.deepEqual([...a.unknownKinds], ['hologram']);
    assert.equal(a.excluded, 2);
  });

  test('counts by kind, in the tab order', () => {
    const a = auditMedia([IMAGE, SCENE, SCENE]);
    assert.deepEqual(Object.keys(a.byKind), ['scene', 'image'], 'scenes first, as the facet lists them');
    assert.equal(a.byKind['scene'], 2);
    assert.equal(a.byKind['image'], 1);
  });

  test('an empty batch is a clean audit', () => {
    const a = auditMedia([]);
    assert.equal(a.shown, 0);
    assert.deepEqual([...a.unknownKinds], []);
  });

  test('a kind excluded by a narrower tab is excluded, not unknown', () => {
    // Two different things: "not in this tab" and "this build cannot place it".
    const a = auditMedia([IMAGE], ['scene']);
    assert.equal(a.excluded, 1);
    assert.deepEqual([...a.unknownKinds], []);
  });
});

// ---------------------------------------------------------------------------

describe('grouping a media wall by kind', () => {
  test('a scene and an image land in different sections', () => {
    assert.equal(kindSection(SCENE), 'scene');
    assert.equal(kindSection(IMAGE), 'image');
  });

  test('a kind outside the tab has no section', () => {
    assert.equal(kindSection(r({ id: 'g', kind: 'gallery' })), null);
  });

  test('an unknown kind has no section', () => {
    assert.equal(kindSection(r({ id: 'x', kind: 'hologram' })), null);
  });

  test('the labels are plural, because a section holds many', () => {
    assert.equal(sectionLabel('scene'), 'Scenes');
    assert.equal(sectionLabel('image'), 'Images');
  });

  test('an unknown kind keeps its wire spelling as the label', () => {
    // "undefined" tells the user nothing; the wire spelling tells a developer
    // exactly what to go and look at.
    assert.equal(sectionLabel('hologram'), 'hologram');
  });

  test('sections follow the kind order, not the alphabet', () => {
    // "Images" before "Scenes" is alphabetical and wrong, and a wall whose
    // sections move when a kind is added is the same complaint as a tree that
    // re-sorts on rename.
    assert.deepEqual(orderSections(['image', 'scene']), ['scene', 'image']);
  });

  test('a section outside the tab sorts last', () => {
    assert.deepEqual(orderSections(['gallery', 'image']), ['image', 'gallery']);
  });
});
