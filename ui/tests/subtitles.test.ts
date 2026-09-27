/**
 * Subtitles in the player. Spec 6 / plan T-P6-002.
 *
 * The pure half of the feature, and every wrong answer it can give is a string
 * or a number that looks fine: a `<track>` with a URL that 404s, a caption
 * showing `5 &gt; 3` because someone escaped the wrong character first, a cue
 * that fires on the wrong frame because the boundary test was inclusive at both
 * ends. None of that is visible in a screenshot, which is the same argument the
 * rest of `player.ts` makes.
 *
 * Two claims in here are load-bearing enough to name:
 *
 * 1. **The VTT writer and the Rust extractor must agree.** The extractor parses
 *    what this writes, so a disagreement about the timestamp shape or the
 *    half-open convention is a round trip that silently loses every cue. The
 *    tests below assert the exact strings the parser accepts, not just that
 *    something was produced.
 * 2. **ASS cannot be rendered by a browser, and the UI has to say so.** Not a
 *    bug — 6 of the spec states it as a ceiling — but a user who picks an ASS
 *    track and silently gets flat text concludes the feature is broken.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  applyOffset,
  cueAt,
  escapeCueText,
  groupByLanguage,
  languageDisplay,
  NO_SUBTITLES,
  selectedDoc,
  stylingHonoured,
  stylingWarning,
  subtitleOptions,
  toVttTimestamp,
  toWebVtt,
  toWebVttWithOffset,
  type SubtitleDoc,
} from '../src/lib/player/player.js';

const doc = (id: number, language: string | null, format = 'webvtt', label?: string): SubtitleDoc => ({
  id,
  language,
  format,
  label: label ?? null,
  cueCount: 10,
});

describe('toVttTimestamp', () => {
  it('always writes two-digit hours and a dot fraction', () => {
    // The shape ffmpeg's muxer writes and the shape the Rust parser accepts.
    assert.equal(toVttTimestamp(0), '00:00:00.000');
    assert.equal(toVttTimestamp(1), '00:00:00.001');
    assert.equal(toVttTimestamp(1337), '00:00:01.337');
    assert.equal(toVttTimestamp(359_999), '00:05:59.999');
    assert.equal(toVttTimestamp(3_600_000), '01:00:00.000');
    assert.equal(toVttTimestamp(3_723_337), '01:02:03.337');
  });

  it('clamps a negative timestamp to zero instead of emitting an invalid one', () => {
    // `-00:00:01.000` is not a timestamp any parser accepts, and a file with
    // one fails entirely rather than losing one cue. Zero is representable and
    // means "from the start".
    assert.equal(toVttTimestamp(-1), '00:00:00.000');
    assert.equal(toVttTimestamp(-99_999), '00:00:00.000');
  });

  it('treats a non-finite value as zero rather than emitting NaN', () => {
    assert.equal(toVttTimestamp(NaN), '00:00:00.000');
    assert.equal(toVttTimestamp(Infinity), '00:00:00.000');
  });
});

describe('escapeCueText', () => {
  it('escapes ampersand before less-than, in that order', () => {
    // Order is load-bearing. Escaping `<` first turns `&lt;` into `&amp;lt;`
    // and every ampersand in a caption becomes visible noise.
    assert.equal(escapeCueText('a & b'), 'a &amp; b');
    assert.equal(escapeCueText('<b>bold</b>'), '&lt;b>bold&lt;/b>');
    assert.equal(escapeCueText('&lt;'), '&amp;lt;');
  });

  it('leaves a bare greater-than alone', () => {
    // WebVTT does not require it escaped, and escaping it turns a caption
    // reading "5 > 3" into "5 &gt; 3" on screen — a regression in the reading
    // for no gain.
    assert.equal(escapeCueText('5 > 3'), '5 > 3');
  });

  it('escapes a bare ampersand that was never markup', () => {
    // The common case: a caption with `Q&A` in it. Unescaped, the browser reads
    // `&A` as an entity reference and the text silently changes.
    assert.equal(escapeCueText('Q&A'), 'Q&amp;A');
  });
});

describe('toWebVtt', () => {
  const cues = [
    { start_ms: 0, end_ms: 2_000, text: 'first' },
    { start_ms: 2_500, end_ms: 4_000, text: 'second' },
  ];

  it('emits the magic header and one block per cue', () => {
    const vtt = toWebVtt(cues);
    assert.ok(vtt.startsWith('WEBVTT'), `header missing: ${vtt}`);
    assert.ok(vtt.includes('00:00:00.000 --> 00:00:02.000\nfirst'));
    assert.ok(vtt.includes('00:00:02.500 --> 00:00:04.000\nsecond'));
  });

  it('produces exactly what the Rust parser accepts', () => {
    // The round trip, stated as a string. The extractor reads `MM:SS.mmm` and
    // `HH:MM:SS.mmm`; this writes the long form, and the cue body is the line
    // after the timing line with nothing between them.
    assert.equal(toWebVtt([{ start_ms: 1_337, end_ms: 2_500, text: 'x' }]),
      'WEBVTT\n\n00:00:01.337 --> 00:00:02.500\nx\n');
  });

  it('drops a cue whose end is not after its start', () => {
    // A browser discards such a cue silently, so emitting one gives a document
    // that is valid, parses cleanly, and is missing a caption. Dropping it
    // makes the omission visible instead.
    const vtt = toWebVtt([
      { start_ms: 0, end_ms: 0, text: 'zero length' },
      { start_ms: 5_000, end_ms: 1_000, text: 'inverted' },
      { start_ms: 1_000, end_ms: 2_000, text: 'kept' },
    ]);
    assert.ok(!vtt.includes('zero length'), 'a zero-length cue was emitted');
    assert.ok(!vtt.includes('inverted'), 'an inverted cue was emitted');
    assert.ok(vtt.includes('kept'), 'the one real cue was dropped');
  });

  it('escapes cue text on the way out', () => {
    const vtt = toWebVtt([{ start_ms: 0, end_ms: 1_000, text: '<i>x</i> & y' }]);
    assert.ok(vtt.includes('&lt;i>x&lt;/i> &amp; y'), `not escaped: ${vtt}`);
  });
});

describe('toWebVttWithOffset', () => {
  const cues = [{ start_ms: 10_000, end_ms: 12_000, text: 'x' }];

  it('applies the offset to both ends, not just the start', () => {
    // Applying it to the start alone would stretch or compress every cue by
    // the offset, so a 500 ms nudge makes the last cue linger half a second
    // too long.
    const vtt = toWebVttWithOffset(cues, 500);
    assert.ok(vtt.includes('00:00:10.500 --> 00:00:12.500'), vtt);
  });

  it('clamps a cue that the offset pushes before zero', () => {
    // The clamp in toVttTimestamp, reached through the offset path.
    const vtt = toWebVttWithOffset([{ start_ms: 100, end_ms: 200, text: 'x' }], -1_000);
    assert.ok(vtt.includes('00:00:00.000 --> 00:00:00.000'), vtt);
  });

  it('treats a null offset as zero', () => {
    assert.equal(toWebVttWithOffset(cues, null), toWebVtt(cues));
    assert.equal(toWebVttWithOffset(cues, undefined), toWebVtt(cues));
  });
});

describe('applyOffset', () => {
  it('is the identity with no offset', () => {
    assert.equal(applyOffset(1_234, null), 1_234);
    assert.equal(applyOffset(1_234, undefined), 1_234);
  });

  it('applies a signed nudge in either direction', () => {
    assert.equal(applyOffset(1_000, 250), 1_250);
    assert.equal(applyOffset(1_000, -250), 750);
  });

  it('ignores a non-finite offset rather than producing NaN', () => {
    // A NaN here would propagate into every timestamp and make the whole
    // document unparseable — one bad input, every caption gone.
    assert.equal(applyOffset(1_000, NaN), 1_000);
  });
});

describe('cueAt', () => {
  // Abutting cues, which is what every file where one line of dialogue follows
  // another looks like, and where an inclusive boundary test shows up.
  const cues = [
    { start_ms: 0, end_ms: 1_000 },
    { start_ms: 1_000, end_ms: 2_000 },
    { start_ms: 2_000, end_ms: 3_000 },
  ];

  it('is half-open: a position on a boundary belongs to the later cue', () => {
    assert.equal(cueAt(cues, 0), 0);
    assert.equal(cueAt(cues, 999), 0);
    assert.equal(cueAt(cues, 1_000), 1, 'the boundary is the start of cue 1');
    assert.equal(cueAt(cues, 2_000), 2);
  });

  it('returns -1 outside every cue, which is the normal answer', () => {
    // A subtitle-less frame is not an error; the browser handles it.
    assert.equal(cueAt(cues, 5_000), -1);
    assert.equal(cueAt(cues, -100), -1);
  });

  it('handles an empty list', () => {
    assert.equal(cueAt([], 0), -1);
  });

  it('ignores a zero-length cue rather than matching it', () => {
    // `[start, end)` on a zero-length cue is the empty set, so it can never
    // match. Returning it would put a caption on screen for no duration.
    assert.equal(cueAt([{ start_ms: 500, end_ms: 500 }], 500), -1);
  });
});

describe('languageDisplay', () => {
  it('capitalises a bare tag', () => {
    assert.equal(languageDisplay('en'), 'En');
  });

  it('uses the last segment of a regional tag', () => {
    assert.equal(languageDisplay('pt-BR'), 'Br');
    assert.equal(languageDisplay('en_US'), 'Us');
  });

  it('names a missing language rather than showing an empty row', () => {
    assert.equal(languageDisplay(null), 'Unknown');
    assert.equal(languageDisplay(undefined), 'Unknown');
    assert.equal(languageDisplay(''), 'Unknown');
  });
});

describe('subtitleOptions', () => {
  it('always offers (none), even with no documents', () => {
    // A select with no options is indistinguishable from a request that
    // failed, and the user cannot tell "no subtitles" from "the list did not
    // load".
    const rows = subtitleOptions([]);
    assert.equal(rows.length, 1);
    assert.equal(rows[0].value, NO_SUBTITLES);
    assert.equal(rows[0].label, '(none)');
  });

  it('puts (none) first, so "off" is the state a player opens in', () => {
    const rows = subtitleOptions([doc(7, 'en'), doc(8, 'es')]);
    assert.equal(rows[0].value, NO_SUBTITLES);
    assert.equal(rows.length, 3);
  });

  it('labels by language, and by label when one is given', () => {
    const rows = subtitleOptions([doc(1, 'en', 'webvtt', 'Director commentary')]);
    assert.equal(rows[1].label, 'Director commentary — En');
  });
});

describe('groupByLanguage', () => {
  it('groups by tag and keeps first-seen order', () => {
    const groups = groupByLanguage([doc(1, 'es'), doc(2, 'en'), doc(3, 'es')]);
    assert.equal(groups.length, 2);
    assert.equal(groups[0].language, 'es');
    assert.equal(groups[0].docs.length, 2);
    assert.equal(groups[1].language, 'en');
  });

  it('is stable across two calls on the same data', () => {
    // The menu must not reshuffle between two renders of the same list.
    const docs = [doc(1, 'es'), doc(2, 'en'), doc(3, 'fr')];
    assert.deepEqual(
      groupByLanguage(docs).map((g) => g.language),
      groupByLanguage(docs).map((g) => g.language)
    );
  });

  it('keeps a null language as its own group rather than merging it', () => {
    // "No language recorded" is a different fact from a language tag, and
    // merging them hides a track that exists behind one that is named.
    const groups = groupByLanguage([doc(1, null), doc(2, 'en')]);
    assert.equal(groups.length, 2);
    assert.equal(groups[0].language, null);
  });
});

describe('selectedDoc', () => {
  const docs = [doc(1, 'en'), doc(2, 'es')];

  it('resolves an id to its document', () => {
    assert.equal(selectedDoc('2', docs)?.id, 2);
  });

  it('treats (none) as no selection', () => {
    assert.equal(selectedDoc(NO_SUBTITLES, docs), null);
  });

  it('returns null for an id that is not in the list', () => {
    // A stored preference can name a document that was deleted. Rendering a
    // <track> for it would request a 404 on every cue change.
    assert.equal(selectedDoc('999', docs), null);
  });

  it('returns null for a non-numeric selection', () => {
    assert.equal(selectedDoc('', docs), null);
    assert.equal(selectedDoc('abc', docs), null);
    assert.equal(selectedDoc(null, docs), null);
    assert.equal(selectedDoc(undefined, docs), null);
  });
});

describe('stylingHonoured', () => {
  it('is false for ASS and SSA, and the ceiling is stated to the user', () => {
    assert.equal(stylingHonoured('ass'), false);
    assert.equal(stylingHonoured('ssa'), false);
    assert.equal(stylingHonoured('ASS'), false, 'the check is case-insensitive');
    assert.ok(stylingWarning('ass'), 'a warning is shown');
  });

  it('is true for the formats a browser renders natively', () => {
    assert.equal(stylingHonoured('webvtt'), true);
    assert.equal(stylingHonoured('mov_text'), true);
    assert.equal(stylingHonoured('subrip'), true);
    assert.equal(stylingHonoured(null), true, 'an unknown format is not ASS');
    assert.equal(stylingWarning('webvtt'), null, 'and says nothing');
  });
});
