/**
 * Every token pair, in both themes, against its WCAG threshold.
 *
 * T-P5-007, spec §10.8. The ticket's bar is "dark/light, contrast"; this file
 * is the half of it that cannot be checked by looking.
 *
 * # The three things this asserts that a screenshot cannot
 *
 * 1. **Both themes, all pairs.** A dark theme is trivially high-contrast and
 *    a light theme is where a #999 muted grey goes to die. The whole risk of
 *    adding a light mode lives in the light mode.
 * 2. **The right threshold per pair.** Body text at 4.5:1, boundaries at 3:1.
 *    Getting this backwards is the error that survives review, because the
 *    4.5:1-everywhere version looks stricter and is not.
 * 3. **That the pair LIST is complete.** Every foreground/background pairing
 *    the UI renders is declared in `PAIRS`. A token that nobody paired is a
 *    colour nobody checked, and the only way to notice is for the list to be
 *    the contract and the test to iterate it.
 *
 * # Why the known-answer cases are here
 *
 * `contrast()` is a formula, and a formula with no known-answer test is a
 * formula that is wrong in a way nobody notices. The WCAG-published pairs are
 * the ground truth: black on white is exactly 21:1, and #777 on #fff is
 * 4.48:1 — just under the text threshold, which is precisely why muted text
 * is the first thing to fail when a light theme is added.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  AA_LARGE,
  AA_TEXT,
  DARK,
  LIGHT,
  PAIRS,
  contrast,
  failures,
  luminance,
  ratioOf,
  thresholdFor,
  type ThemeTokens
} from '../src/lib/theme/contrast.js';

describe('WCAG contrast arithmetic', () => {
  it('black on white is exactly 21:1', () => {
    assert.equal(contrast('#000000', '#ffffff').toFixed(2), '21.00');
  });

  it('a colour on itself is 1:1', () => {
    assert.equal(contrast('#4a9eff', '#4a9eff').toFixed(2), '1.00');
  });

  it('is symmetric, whichever way round the pair is given', () => {
    // The formula is NOT symmetric as written, so a caller with the background
    // first would otherwise get a ratio below 1 and a passing threshold.
    assert.equal(contrast('#ffffff', '#000000'), contrast('#000000', '#ffffff'));
    assert.equal(contrast('#fdfdfd', '#16161a'), contrast('#16161a', '#fdfdfd'));
  });

  it('#777777 on white is just under the text threshold', () => {
    // The published figure is 4.48:1. It is the reason a muted grey is the
    // first casualty of a light theme, and the reason DARK.muted is not a
    // mid-grey carried over from the dark palette.
    assert.equal(contrast('#777777', '#ffffff').toFixed(2), '4.48');
  });

  it('relative luminance of white is 1 and of black is 0', () => {
    assert.equal(luminance('#ffffff').toFixed(4), '1.0000');
    assert.equal(luminance('#000000').toFixed(4), '0.0000');
  });

  it('accepts a hex with or without the leading hash', () => {
    assert.equal(luminance('#ffffff'), luminance('ffffff'));
  });

  it('rejects a colour that is not six hex digits', () => {
    // Silently treating a bad colour as black would make every ratio against
    // it read 21:1 and the theme would pass.
    assert.throws(() => luminance('#fff'), /not a 6-digit hex/);
    assert.throws(() => luminance('nonsense'), /not a 6-digit hex/);
  });
});

describe('the declared pairs', () => {
  it('every pair clears its threshold in the dark theme', () => {
    assert.deepEqual(failures(DARK), []);
  });

  it('every pair clears its threshold in the light theme', () => {
    assert.deepEqual(failures(LIGHT), []);
  });

  it('assigns 4.5:1 to text and 3:1 to boundaries', () => {
    for (const p of PAIRS) {
      assert.equal(thresholdFor(p), p.large ? AA_LARGE : AA_TEXT, p.what);
    }
  });

  it('covers every token that can be a foreground', () => {
    // A token nobody pairs is unchecked colour. `name` is excluded: it is a
    // label, never painted.
    const seen = new Set(PAIRS.map((p) => p.fg));
    for (const key of Object.keys(DARK) as (keyof ThemeTokens)[]) {
      if (key === 'name') continue;
      assert.ok(seen.has(key), `token ${key} is never used as a foreground`);
    }
  });

  it('covers every token that is ever used as a fill', () => {
    // Not "every token": `focus` and `fg` are outlines and foregrounds, and
    // demanding a `bg`-on-`focus` pairing would add a fiction to satisfy a
    // check. So the fill set is declared, and the check is that nothing in it
    // is unchecked -- which is the property that actually matters, because a
    // fill nobody paired is colour nobody verified.
    const FILLS: (keyof ThemeTokens)[] = ['bg', 'surface', 'muted', 'border', 'accent', 'fg', 'danger'];
    const seen = new Set(PAIRS.map((p) => p.bg));
    for (const key of FILLS) {
      assert.ok(seen.has(key), `fill token ${key} is never used as a background`);
    }
    // And the two that are not fills stay unpainted, so a future reader does
    // not "fix" this by adding a nonsense pair.
    for (const key of ['name', 'focus'] as (keyof ThemeTokens)[]) {
      assert.ok(!seen.has(key), `${key} is not a fill and must not be paired as one`);
    }
  });

  it('pairs every surface with the text that sits on it', () => {
    // The asymmetry this catches: a card whose body text is checked against
    // the page but not against the card itself. `fg on bg` passing says
    // nothing about `fg on surface`.
    assert.ok(PAIRS.some((p) => p.fg === 'fg' && p.bg === 'surface'));
    assert.ok(PAIRS.some((p) => p.fg === 'muted' && p.bg === 'surface'));
  });

  it('has no duplicate pair', () => {
    // A duplicate would silently make the list look more thorough than the
    // coverage is, which is the failure the completeness checks above exist
    // to prevent.
    const seen = new Set<string>();
    for (const p of PAIRS) {
      const k = `${p.fg}-on-${p.bg}`;
      assert.ok(!seen.has(k), `duplicate pair ${k}`);
      seen.add(k);
    }
  });
});

describe('a theme that cannot pass', () => {
  // The meta-test. `failures()` returning a list is only useful if the list is
  // ever non-empty, and a validator that has only ever seen a passing theme
  // has not been shown to reject anything.
  const BAD: ThemeTokens = {
    ...DARK,
    name: 'bad',
    // #767676 on #111111 is 3.9:1 — passes as "large text", fails as body.
    muted: '767676'
  };

  it('names the failing pair, the theme, the numbers and the reason', () => {
    const f = failures(BAD);
    assert.ok(f.length > 0, 'a theme with 3.9:1 body text must fail');
    const line = f.join('\n');
    assert.match(line, /^bad: /, 'the theme name is in the message');
    assert.match(line, /timestamps and counts/, 'what the pairing is for');
    assert.match(line, /needs 4\.5:1/, 'the threshold it needed');
    assert.match(line, /3\.\d\d:1/, 'the ratio it got');
  });

  it('clears the same theme once the token is fixed', () => {
    assert.deepEqual(failures({ ...BAD, muted: 'a8a8b0' }), []);
  });

  it('treats a focus ring as a boundary, not as text', () => {
    // A ring that is only 3.4:1 would fail as text and pass as a boundary.
    // The threshold is the judgement, and this pins it.
    // A ring at 3.4:1 against the page, chosen because it FAILS as text
    // (4.5) and PASSES as a boundary (3). A ring that also cleared 4.5 would
    // make the threshold untestable — the assertion below would pass for the
    // wrong reason, which is the failure this precondition exists to prevent.
    const ring: ThemeTokens = { ...LIGHT, focus: '4f84cf' };  // 3.40:1 on surface, 3.73 on bg
    const pair = PAIRS.find((p) => p.fg === 'focus' && p.bg === 'bg')!;
    const r = ratioOf(ring, pair);
    assert.ok(r < AA_TEXT, `precondition: ${r.toFixed(2)} is under 4.5`);
    assert.ok(r >= AA_LARGE, `precondition: ${r.toFixed(2)} is at least 3`);
    assert.ok(!failures(ring).some((l) => l.includes('focus ring')));
  });
});
