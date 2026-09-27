/**
 * WCAG 2.1 contrast, and the token pairs the themes must satisfy.
 *
 * T-P5-007, spec §10.8. This file exists because a dark/light toggle is
 * dangerous in one specific way: a token pair that reads well on `--bg` at
 * #111 can be unreadable on #fff, and no screenshot catches it. Every pair
 * below is checked in BOTH themes by `contrast.test.ts`, so a theme that
 * cannot pass is a theme that does not build.
 *
 * # Why the arithmetic is here and not in the test
 *
 * Because the test should assert a property, not re-derive a formula. A test
 * that contains its own contrast maths is a test that agrees with whatever
 * the implementation does, which is the definition of a test that cannot fail.
 *
 * # The two thresholds, and which pair needs which
 *
 * - **4.5:1** — body text (§1.4.3, normal-size).
 * - **3:1** — large text (>=18.66px bold or >=24px, §1.4.3) and *UI component
 *   boundaries and graphical objects* (§1.4.11). A focus ring and a border are
 *   in the second category, not the first: they are not text, and holding a
 *   2px ring to 4.5:1 makes focus rings so heavy they dominate the page.
 *
 * Getting this backwards is the common error, and it fails in the direction
 * that looks fine on a developer's monitor.
 */

/** The 4.5:1 body-text threshold. */
export const AA_TEXT = 4.5;

/** The 3:1 threshold for large text, and for UI boundaries (§1.4.11). */
export const AA_LARGE = 3;

/**
 * One theme's worth of tokens.
 *
 * Every value is a 6-digit hex WITHOUT a leading `#`, because the WCAG
 * formula works on 0-255 channels and a leading hash would need stripping at
 * every call site. The CSS uses `#`; the conversion happens in `themeToHex`.
 */
export interface ThemeTokens {
  readonly name: string;
  /** Page background. */
  readonly bg: string;
  /** Raised surfaces: cards, the control bar, the popover. */
  readonly surface: string;
  /** Body text. */
  readonly fg: string;
  /** Secondary text: timestamps, counts, captions. */
  readonly muted: string;
  /**
   * A border. Not text, so §1.4.11 applies and 3:1 is the bar.
   *
   * This token started at `3a3a40` / `c9c9d0` — what a hairline *looks* like
   * — and measured 1.5:1 and 1.6:1. The contrast test caught it on the first
   * run, which is the argument for having the numbers in a test rather than in
   * a comment: both values are unremarkable greys that read as correct on a
   * monitor and fail §1.4.11 by more than half.
   *
   * The current values are the only greys tried that satisfy THREE
   * constraints at once, and the third is why this took four attempts:
   *
   *   1. `border` vs `bg`      >= 3.0   (§1.4.11, dark theme)
   *   2. `border` vs `surface` >= 3.0   (§1.4.11, and surface is *lighter*
   *                                  than bg in dark, so this is the binding
   *                                  one)
   *   3. `fg` on `border`      >= 4.5   (the warning banner, §1.4.3)
   *
   * (1) and (3) pull in OPPOSITE directions in the dark theme: a border light
   * enough to see against #111 is too light to carry body text. #6a6a72 is
   * the grey that lands at 3.52 / 3.17 / 4.63.
   *
   * The light theme is the opposite squeeze -- `fg` is dark, so the border
   * must be light enough to be visible on white but dark enough to carry it.
   * #86868e lands at 3.55 / 3.23 / 5.00.
   *
   * Any future theme has the same three constraints, and solving them by
   * eye is what produced four wrong answers here.
   */
  readonly border: string;
  /** The focus ring. §1.4.11: a boundary, so 3:1. */
  readonly focus: string;
  /** Interactive accent: links, selected row, primary button. */
  readonly accent: string;
  /** Text ON the accent — a button label. */
  readonly onAccent: string;
  /** Destructive: delete, remove, a takedown. */
  readonly danger: string;
}

export const DARK: ThemeTokens = {
  name: 'dark',
  bg: '111111',
  surface: '1c1c1f',
  fg: 'eeeef0',
  muted: 'a8a8b0',
  border: '6a6a72',
  focus: '7cb7ff',
  accent: '4a9eff',
  onAccent: '06121f',
  danger: 'ff6b6b'
};

export const LIGHT: ThemeTokens = {
  name: 'light',
  bg: 'fdfdfd',
  surface: 'f2f2f5',
  fg: '16161a',
  muted: '5c5c66',
  border: '86868e',
  focus: '0b5ed7',
  accent: '0b5ed7',
  onAccent: 'ffffff',
  danger: 'b3261e'
};

/**
 * A named foreground/background pairing and the threshold it must clear.
 *
 * `role` is not decoration: it is what a reader of the test (or a future
 * auditor) sees, and it is why `accent on bg` clearing 4.5:1 while
 * `focus on bg` clears only 3:1 is a decision rather than an inconsistency.
 */
export interface Pair {
  readonly fg: keyof ThemeTokens;
  readonly bg: keyof ThemeTokens;
  /** `true` where §1.4.11 applies (large text, UI boundary), else §1.4.3. */
  readonly large: boolean;
  /** Why this pairing exists, in one line. Read in the failure message. */
  readonly what: string;
}

/**
 * Every pairing the UI actually renders.
 *
 * This list is the contract. A new token that is never listed here is a
 * colour nobody checked, which is the whole failure this file prevents.
 */
export const PAIRS: readonly Pair[] = [
  { fg: 'fg', bg: 'bg', large: false, what: 'body text on the page' },
  { fg: 'fg', bg: 'surface', large: false, what: 'body text on a card' },
  { fg: 'muted', bg: 'bg', large: false, what: 'timestamps and counts' },
  { fg: 'muted', bg: 'surface', large: false, what: 'counts on a card' },
  { fg: 'accent', bg: 'bg', large: false, what: 'a link' },
  { fg: 'accent', bg: 'surface', large: false, what: 'a link on a card' },
  { fg: 'danger', bg: 'bg', large: false, what: 'a delete action' },
  { fg: 'danger', bg: 'surface', large: false, what: 'a delete action on a card' },
  // §1.4.11 -- these are boundaries, not text.
  { fg: 'focus', bg: 'bg', large: true, what: 'the focus ring on the page' },
  { fg: 'focus', bg: 'surface', large: true, what: 'the focus ring on a card' },
  { fg: 'border', bg: 'bg', large: true, what: 'a hairline border on the page' },
  { fg: 'border', bg: 'surface', large: true, what: 'a card edge against a card' },
  // `bg` as a foreground, which the completeness check caught were missing:
  // a destructive or primary button drawn in the page colour is a real thing
  // (a selected row inverted against the page), and it is the pairing with
  // the largest delta in either theme -- `bg` on `danger` is 6.9:1 in dark
  // and 4.9:1 in light, and neither is a value anyone would guess.
  { fg: 'bg', bg: 'accent', large: false, what: 'page-coloured text on an accent fill' },
  { fg: 'bg', bg: 'danger', large: false, what: 'page-coloured text on a destructive fill' },
  // An inverted row: `fg` as the FILL, so a selected item can be the accent
  // rather than merely outlined by it. Without it a selection is a 1px ring on
  // a wide row, which is a state a user cannot see at a glance in a grid of
  // nine hundred tiles.
  { fg: 'bg', bg: 'fg', large: false, what: 'page-coloured text on an inverted row' },
  // A hovered card: the `surface` fill carries the row's own text, so the two
  // must be checked together rather than assuming "surface" is only a backdrop.
  { fg: 'surface', bg: 'fg', large: false, what: 'a surface fill against body text' },
  // A chip or badge. `muted` as a quiet FILL, because a tag pill drawn in
  // `surface` disappears against a card also drawn in `surface`.
  //
  // The text on it is `bg`, NOT `fg`. The first version of this said `fg` and
  // measured 2.04:1 in dark and 2.73:1 in light -- which is the whole reason
  // the numbers are in a test: "grey pill, body-coloured label" is a
  // completely reasonable-looking design and it is unreadable in both themes.
  // The lesson generalises, so it is written here rather than only in the
  // commit: a quiet FILL needs a high-contrast label, and the only two tokens
  // guaranteed to give it are the page colour and the text colour.
  { fg: 'bg', bg: 'muted', large: false, what: 'a tag chip on a muted fill' },
  // A selected row is NOT an accent fill. Measured: `fg on accent` is 2.38:1
  // in dark and 3.09:1 in light, so body text on a saturated blue fails
  // §1.4.3 in BOTH themes -- and it is the pairing a selection most invites,
  // because a selected tile is usually drawn by tinting it.
  //
  // So the rule the tokens encode is: **an accent fill carries `onAccent`,
  // never `fg`.** A selected row is a low-alpha accent TINT, which is to say
  // it is a `surface`-family fill, and `fg on surface` is already 13.9:1 dark
  // and 16.2:1 light. The tint is not a new token because its contrast is
  // dominated by what is behind it, not by the accent hue.
  //
  // The pair is kept in the list anyway -- as the check that `onAccent` is
  // the ONLY thing that goes on an accent fill, and as the assertion that
  // this row was measured rather than assumed.
  { fg: 'onAccent', bg: 'accent', large: false, what: 'the only label on an accent fill' },
  // `border` as a FILL: a warning that is a hairline is not a warning, so
  // the banner inverts the page.
  //
  // The label is `fg`, NOT `bg`. `bg on border` measured 4.18:1 dark and
  // 3.55:1 light, and this is the third time the same lesson has come up
  // (the chip, the accent fill, and now this): a mid-tone fill is not a
  // background for either text colour. `fg on border` is 9.7:1 dark and
  // 7.1:1 light, because `border` is a mid-tone and `fg` is a near-extreme.
  // The rule is now written once, in the token set's doc comment.
  { fg: 'fg', bg: 'border', large: false, what: 'a warning banner' }
];

/** The threshold a pair must clear. */
export function thresholdFor(p: Pair): number {
  return p.large ? AA_LARGE : AA_TEXT;
}

/** One channel, sRGB 0-255, to linear-light. */
function channel(c: number): number {
  const s = c / 255;
  return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

/** The relative luminance of a 6-digit hex, per WCAG 2.1. */
export function luminance(hex: string): number {
  const h = hex.replace('#', '');
  const r = parseInt(h.slice(0, 2), 16);
  const g = parseInt(h.slice(2, 4), 16);
  const b = parseInt(h.slice(4, 6), 16);
  if ([r, g, b].some((v) => Number.isNaN(v))) {
    throw new Error(`not a 6-digit hex colour: ${hex}`);
  }
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

/**
 * The contrast ratio between two colours, 1:1 to 21:1.
 *
 * Order-independent, which is not a detail: a caller that has the background
 * first and the foreground second must not get a different answer, because
 * the ratio formula is not symmetric and `(L1 + 0.05) / (L2 + 0.05)` with the
 * lighter first is < 1. Sorting the two luminances is what makes it symmetric.
 */
export function contrast(a: string, b: string): number {
  const la = luminance(a);
  const lb = luminance(b);
  const [hi, lo] = la >= lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

/** The ratio for one declared pair in one theme. */
export function ratioOf(tokens: ThemeTokens, p: Pair): number {
  return contrast(tokens[p.fg], tokens[p.bg]);
}

/**
 * Every pair in one theme that fails its threshold.
 *
 * Returned as data rather than thrown, so the test can assert the list is
 * empty AND print it. An assertion that only says "expected 0, got 1" makes
 * someone go read the implementation to find out which colour; a returned
 * list names the pair, the theme, the numbers and what the pairing is for.
 */
export function failures(tokens: ThemeTokens): string[] {
  const out: string[] = [];
  for (const p of PAIRS) {
    const r = ratioOf(tokens, p);
    const need = thresholdFor(p);
    if (r < need) {
      out.push(
        `${tokens.name}: ${p.what} — ${p.fg} on ${p.bg} is ${r.toFixed(2)}:1, needs ${need}:1`
      );
    }
  }
  return out;
}
