/*
  A command, a binding, and the key that produced it.

  T-P5-006 item 5. Spec 10.7; plan §T-P5-006 item 5; full reasoning in
  docs/spec/t-p5-006-commands.md §3.2.

  # Why a Chord and not a string

  A shortcut map that stores `"ctrl+k"` cannot be checked. Splitting it into a
  `code`, a list of modifiers, and a `key` means two of those are booleans that
  either match or do not, and the comparison is a function you can test rather
  than a string comparison you can only test by constructing the right string.

  # Why `code` and not `key`, and why both

  `KeyboardEvent.key` is the *character* the layout produces, so on an AZERTY
  keyboard the physical key labelled `q` produces `a`. A map keyed on `key` is
  therefore correct only for users on a US layout, and silently wrong for
  everyone else -- and "silently" is the operative word, because a shortcut map
  that does not fire looks exactly like a shortcut map that is not bound yet.

  `code` is the physical key position and is layout-independent, which is what a
  shortcut map has to be: the user is pressing the key their keyboard says is
  `Q`, and the command is "select all", not "the letter a".

  So `code` is the primary match. `key` is also accepted, because some keys have
  no useful `code` -- `Escape`, the function row, and the arrows all report a
  `key` and a `code`, but a `code` is meaningless to a reader for `Escape` and a
  future browser could name it anything. A binding satisfied by either is
  permissive in the safe direction: a binding that fires when it should not is a
  visible bug, and one that fails to fire is invisible.

  # Modifier order is canonical

  `Ctrl+Shift+K` and `Shift+Ctrl+K` are the same chord and must produce the same
  string, because that string is what the palette displays and what a mutation
  test compares. The canonical order is the one the spec's examples use, and it
  is fixed in `MODIFIER_ORDER` rather than derived from the event, whose
  modifier flags are booleans with no order of their own.
 */

/** A modifier, in the one order a chord is ever written in. */
export const MODIFIER_ORDER = ['ctrl', 'alt', 'shift', 'meta'] as const;

export type Modifier = (typeof MODIFIER_ORDER)[number];

/**
 * `KeyboardEvent.key` for each modifier.
 *
 * Not the same strings as `MODIFIER_ORDER`, and the difference is the whole
 * reason this table exists: the *flag* is `ctrlKey` but the *key* a browser
 * reports is `'Control'`. `Shift`, `Alt` and `Meta` coincide across the two
 * vocabularies, which is why a case-insensitive compare looked correct and
 * silently let `Control` through as a bindable chord -- a command that fired on
 * every Control press.
 *
 * A future modifier has to be added to both, and the `satisfies` makes that a
 * compile error rather than a runtime surprise.
 */
export const MODIFIER_KEY_NAMES: Record<Modifier, string> = {
  ctrl: 'Control',
  alt: 'Alt',
  shift: 'Shift',
  meta: 'Meta'
};

/** A subset of `KeyboardEvent` — what `normalizeKey` needs and no more. */
export interface KeyLike {
  key: string;
  code?: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  /**
   * Where the keystroke landed, for the text-field rule.
   *
   * Part of the input rather than a separate argument because it is part of the
   * event: a resolver handed a keystroke with no target has to guess, and the
   * guess is "not typing", which is the guess that eats a user's keystroke.
   */
  target?: EventTarget | null;
}

/** A binding, as a comparable value. */
export interface Chord {
  /** The physical key position, e.g. `KeyK`. Preferred. */
  code: string;
  /** The produced character, e.g. `k`. Also matched. */
  key: string;
  mods: Modifier[];
}

/**
 * A chord as a display string: `Ctrl+Shift+K`.
 *
 * Canonical, because this is what the palette shows and what two equal chords
 * must produce the same string for. `MODIFIER_ORDER` fixes the order, so
 * `Ctrl+Shift+K` and `Shift+Ctrl+K` are the same string rather than two.
 */
export function formatChord(c: Chord): string {
  const parts = MODIFIER_ORDER.filter((m) => c.mods.includes(m)).map(cap);
  const label = displayKey(c);
  return [...parts, label].join('+');
}

/** `key` is the friendlier of the two for a human, when there is one. */
function displayKey(c: Chord): string {
  if (c.key && c.key.length === 1) return c.key.toUpperCase();
  if (c.code === 'Escape') return 'Esc';
  // A `code` is a machine name; `Space` and friends read better from `key`.
  if (c.key) return c.key;
  return c.code;
}

function cap(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/**
 * Turn an event into a chord, or `null` if it is not a chord at all.
 *
 * `null` for a bare modifier press: the user is not asking for anything, they
 * are getting ready to. Treating a lone `Shift` as `Shift+Shift` produces a
 * binding that fires on every shift press, which is the kind of thing that only
 * shows up once a user is trying to type a capital letter somewhere else.
 */
export function normalizeKey(e: KeyLike): Chord | null {
  const mods: Modifier[] = [];
  if (e.ctrlKey) mods.push('ctrl');
  if (e.altKey) mods.push('alt');
  if (e.shiftKey) mods.push('shift');
  if (e.metaKey) mods.push('meta');

  // A lone modifier press is not a chord: the user is getting ready to type,
  // not asking for anything.
  //
  // "Was the pressed key itself a modifier" -- NOT "does the key name look like
  // a modifier". `Shift+Alt` arrives on some layouts as `key: 'Shift'` with
  // `code: 'ShiftLeft'`, and it is a real chord that must survive; matching on
  // the name alone rejected it.
  //
  // So the test is the *count* of modifiers: a chord made of nothing but
  // modifiers is a non-event, and a chord with a non-modifier in it is real
  // whatever its `key` happens to be. That also handles the `code: ''` case
  // some platforms send, which a code-shaped test cannot see at all.
  const keyIsModifier = MODIFIER_KEY_NAMES[mods[0] as Modifier] === e.key;
  if (keyIsModifier && mods.length <= 1) return null;

  return { code: e.code ?? '', key: e.key, mods };
}

/** Do two chords name the same thing? */
export function chordsEqual(a: Chord, b: Chord): boolean {
  if (a.mods.length !== b.mods.length) return false;
  for (const m of a.mods) if (!b.mods.includes(m)) return false;
  // `code` decides when both sides have one; `key` is the fallback for keys
  // that have no meaningful `code` (Escape, the function row, and anything a
  // future browser names differently).
  //
  // An unconditional OR on `key` would be too permissive: on an AZERTY layout
  // `KeyQ` and `KeyA` both produce the character `a`, so the two physical keys
  // would become interchangeable -- which is the exact substitution `code`
  // exists to prevent. Preferring `code` when present keeps `a`-on-`KeyQ`
  // distinct from `a`-on-`KeyA`, and only falls back when there is nothing else
  // to go on.
  if (a.code !== '' && b.code !== '') return a.code === b.code;
  return a.key === b.key;
}

/**
 * Is this keystroke happening in a place where it is text?
 *
 * The rule is not "is the target an input" — it is "would this keystroke change
 * the document". A modifier-free key in a text field is text. A chord *with* a
 * modifier is a command even in a text field, because `Ctrl+A` in an input is
 * select-all-in-the-input and the browser handles it, while `Ctrl+K` is ours.
 *
 * `contentEditable` is in here because a rich-text surface is still typing, and
 * leaving it out is how a palette swallows a keystroke while a user is writing
 * a note.
 */
export function inTextField(target: EventTarget | null): boolean {
  if (target === null || target === undefined) return false;
  // Duck-typed on `tagName` rather than `instanceof HTMLElement`.
  //
  // `HTMLElement` is a *global*, and this module is imported by the node test
  // runner, where it does not exist -- so an `instanceof` check throws a
  // ReferenceError at the first keystroke in a unit test, which reads as "the
  // registry is broken" rather than "the test environment has no DOM". Worse, it
  // makes the rule untestable outside a browser, and the rule is the part worth
  // testing.
  //
  // Duck-typing is also more correct: a node from another document or a
  // polyfilled environment is still a text field, and `instanceof` says it is
  // not.
  const el = target as Partial<HTMLElement> & { tagName?: string };
  const tag = typeof el.tagName === 'string' ? el.tagName.toUpperCase() : '';
  if (tag === '' ) return false;
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true;
  // `isContentEditable` is absent on non-elements, and absent means false.
  if (el.isContentEditable === true) return true;
  // An `<input>` that is not a text-like type is a button or a checkbox, where a
  // bare letter is not text. `type` is absent for `<input>` in older DOMs, and
  // the default is text, so absent means text.
  if (tag !== 'INPUT') return false;
  const type = String((el as HTMLInputElement).type ?? '').toLowerCase();
  return (
    type === '' ||
    type === 'text' ||
    type === 'search' ||
    type === 'email' ||
    type === 'url' ||
    type === 'tel' ||
    type === 'password' ||
    type === 'number'
  );
}
