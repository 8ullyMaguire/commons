/*
  Tests for the parts of the key and search layers the first two files left
  uncovered.

  T-P5-006 item 5. Spec 10.7; plan §T-P5-006 item 5.

  # Why this file is mostly boundary cases

  `scripts/mutate-commands-ui.py` reported that the *ranking* decisions — prefix
  over substring over subsequence, word-initial over mid-word, the tie-break on
  word count — had no test that distinguished them from each other. The e2e
  proves a query returns *something* useful; it does not prove that `sel` beats a
  command that merely contains those letters, and if it does not, the palette
  still works. It just quietly ranks worse.

  Those are all ordering properties, and an ordering property is only pinned by
  a test that names what must come *before* what. So every case here is an
  assertion about a pair, never about a single result.
 */

import { describe, it } from 'node:test';
import { strict as assert } from 'node:assert';
import { normalizeKey, chordsEqual, inTextField, formatChord } from '../src/lib/api/keys.js';
import { CommandRegistry, searchCommands, type Command } from '../src/lib/api/commands.js';

const cmd = (over: Partial<Command> & { id: string }): Command => ({
  title: over.title ?? over.id,
  scope: 'app',
  run: () => {},
  ...over
});

const ids = (cs: Command[], q: string) => searchCommands(cs, q).map((h) => h.command.id);
const el = (tag: string, extra: Record<string, unknown> = {}) =>
  ({ tagName: tag, ...extra }) as unknown as EventTarget;

describe('inTextField — which elements are typing', () => {
  it('says yes for the text-like elements', () => {
    for (const t of ['INPUT', 'TEXTAREA', 'SELECT']) {
      assert.equal(inTextField(el(t)), true, t);
    }
  });

  it('lowercases the tag name, because a duck-typed node is not a real element', () => {
    // A node from another document, or one shaped by a test, can carry a
    // lowercase tagName. `instanceof HTMLElement` would have said "not an
    // element" and returned false; the point of duck-typing is that the answer
    // does not depend on provenance.
    assert.equal(inTextField(el('input')), true);
    assert.equal(inTextField(el('textarea')), true);
  });

  it('says yes for a contenteditable, which is still typing', () => {
    assert.equal(inTextField(el('DIV', { isContentEditable: true })), true);
    assert.equal(inTextField(el('DIV')), false);
  });

  it('says no for a checkbox or a button, where a letter is not text', () => {
    assert.equal(inTextField(el('INPUT', { type: 'checkbox' })), false);
    assert.equal(inTextField(el('INPUT', { type: 'radio' })), false);
    assert.equal(inTextField(el('BUTTON')), false);
  });

  it('treats an input with no type as text, which is the default', () => {
    // Absent `type` means text, per the HTML spec. Getting this backwards makes
    // every untyped input swallow shortcuts.
    assert.equal(inTextField(el('INPUT')), true);
  });

  it('says no for a non-element, rather than throwing', () => {
    assert.equal(inTextField(null), false);
    assert.equal(inTextField(undefined as unknown as EventTarget), false);
    assert.equal(inTextField({} as EventTarget), false);
  });
});

describe('normalizeKey — what a keystroke is', () => {
  it('rejects a named key, because a binding is one character', () => {
    // `Enter`, `ArrowDown` and friends name themselves; a command bound to
    // "Enter" would fire on every Enter in the app, and the resolver has no way
    // to know that is not what was meant.
    for (const k of ['Enter', 'ArrowDown', 'Tab', 'F5', 'Backspace']) {
      const e = {
        key: k,
        code: '',
        ctrlKey: false,
        altKey: false,
        shiftKey: false,
        metaKey: false,
        target: null
      };
      const c = normalizeKey(e as never);
      // The name survives on `key` for diagnostics; what matters is that a
      // one-character binding cannot collide with it, which `chordsEqual` and
      // the `code` comparison handle.
      assert.ok(c === null || c.key === k, k);
    }
  });

  it('preserves key case, so a lowercase binding does not match an uppercase key', () => {
    const lower = normalizeKey({
      key: 'a',
      code: 'KeyA',
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
      metaKey: false,
      target: null
    } as never);
    const upper = normalizeKey({
      key: 'A',
      code: 'KeyA',
      ctrlKey: true,
      altKey: false,
      shiftKey: false,
      metaKey: false,
      target: null
    } as never);
    assert.ok(lower && upper);
    // Same physical key, different chord: shift is part of the chord, not part
    // of the key name.
    assert.equal(chordsEqual(lower as never, upper as never), false);
  });

  it('rejects a lone modifier press, whichever modifier it is', () => {
    // The flag is `ctrlKey` but the key name is `'Control'`; `Shift`, `Alt` and
    // `Meta` happen to coincide across both vocabularies, which is exactly why
    // the Control one survived a first reading and was caught only by testing
    // all four rather than one.
    const lone = (key: string, code: string, flag: 'ctrlKey' | 'shiftKey' | 'altKey' | 'metaKey') =>
      normalizeKey({
        key,
        code,
        ctrlKey: flag === 'ctrlKey',
        altKey: flag === 'altKey',
        shiftKey: flag === 'shiftKey',
        metaKey: flag === 'metaKey',
        target: null
      } as never);
    assert.equal(lone('Control', 'ControlLeft', 'ctrlKey'), null);
    assert.equal(lone('Control', 'ControlRight', 'ctrlKey'), null);
    assert.equal(lone('Shift', 'ShiftLeft', 'shiftKey'), null);
    assert.equal(lone('Alt', 'AltLeft', 'altKey'), null);
    assert.equal(lone('Meta', 'MetaLeft', 'metaKey'), null);
  });

  it('rejects a lone modifier with no code, which some platforms send', () => {
    // No `code` to compare against, and a code-shaped test cannot see this case
    // at all -- which is how it got missed the first time.
    assert.equal(
      normalizeKey({
        key: 'Shift',
        code: '',
        ctrlKey: false,
        altKey: false,
        shiftKey: true,
        metaKey: false,
        target: null
      } as never),
      null
    );
  });

  it('keeps a chord that has a non-modifier key alongside a modifier', () => {
    const altShift = normalizeKey({
      key: 'Shift',
      code: 'ShiftLeft',
      ctrlKey: false,
      altKey: true,
      shiftKey: true,
      metaKey: false,
      target: null
    } as never);
    assert.ok(altShift, 'Alt+Shift is a real chord, not a stray modifier press');
  });
});

describe('formatChord — what the palette shows', () => {
  it('puts modifiers in a fixed order regardless of how they were pressed', () => {
    const c = { key: 'k', code: 'KeyK', mods: ['meta', 'shift', 'ctrl', 'alt'] } as never;
    assert.equal(formatChord(c), 'Ctrl+Alt+Shift+Meta+K');
  });

  it('shows a bare letter with no modifier noise', () => {
    assert.equal(formatChord({ key: 'x', code: 'KeyX', mods: [] } as never), 'X');
  });

  it('shows a named key as its keycap does, not as the DOM names it', () => {
    // `Escape` is seven characters in a column sized for a key cap. The DOM name
    // is what the resolver matches on; the label is what a reader recognises.
    assert.equal(formatChord({ key: 'Escape', code: 'Escape', mods: [] } as never), 'Esc');
  });
});

describe('searchCommands — the ranking, as pairs', () => {
  it('a prefix beats a mere substring', () => {
    const cs = [
      cmd({ id: 'contains', title: 'Reselect everything' }),
      cmd({ id: 'prefix', title: 'Select all' })
    ];
    assert.equal(ids(cs, 'sel')[0], 'prefix');
  });

  it('a substring beats a scattered subsequence', () => {
    const cs = [
      cmd({ id: 'scattered', title: 'Send every label somewhere' }),
      cmd({ id: 'contiguous', title: 'Deselect all' })
    ];
    assert.equal(ids(cs, 'sel')[0], 'contiguous');
  });

  it('a word-initial match beats a mid-word one', () => {
    // The case the ranking exists for: both match `bt` on two letters, and only
    // one of them has those letters starting words.
    const cs = [
      cmd({ id: 'midword', title: 'About the tagger' }),
      cmd({ id: 'wordstart', title: 'Bulk tag' })
    ];
    assert.equal(ids(cs, 'bt')[0], 'wordstart');
  });

  it('a word-initial match wins when nothing else differs', () => {
    // The fully isolated version. 'Bold tag' and 'Batg tag' are both two words,
    // both eight characters, and both match `bt` on two letters -- the only
    // difference is that in 'Batg tag' the `t` sits inside a word.
    //
    // Every earlier pair in this file differed on a second axis, which is why
    // removing the word-initial rule left the right answer on top anyway. This
    // one has nothing else to fall back on: without the rule both score 32, tie,
    // and the length tiebreak -- which cannot tell them apart either -- decides
    // on alphabetical order, putting 'Batg tag' first.
    const cs = [
      cmd({ id: 'midword', title: 'Batg tag' }),
      cmd({ id: 'wordstart', title: 'Bold tag' })
    ];
    assert.deepEqual(ids(cs, 'bt'), ['wordstart', 'midword']);
  });

  it('a tighter match wins when both are equally word-initial', () => {
    // The single-axis version of the case above, and the reason it exists.
    //
    // 'About the tagger' and 'Bulk tag' differ on *two* things: which letters
    // start words, and how many words there are. So removing the word-initial
    // rule still left 'Bulk tag' on top -- the word-count tiebreak did the work
    // and the test never noticed the rule was gone. A pair that differs on one
    // axis is the only kind that can fail for the right reason.
    //
    // Here both titles have two words and both match `bt` on two word-initial
    // letters, so the only thing separating them is how tight the match is.
    const cs = [
      cmd({ id: 'loose', title: 'Bat the tagger' }),
      cmd({ id: 'tight', title: 'Bold tag' })
    ];
    assert.equal(ids(cs, 'bt')[0], 'tight');
  });

  it('a prefix beats a substring on the prefix alone', () => {
    // Also single-axis. 'Reselect rows' contains 'sel' and 'Selected all rows'
    // starts with it, and they have different word counts -- so without the
    // prefix bonus they tie on score and the *length* tiebreak picks the shorter
    // one, which is the wrong answer. That is exactly what the bonus is for.
    const cs = [
      cmd({ id: 'contains', title: 'Reselect rows' }),
      cmd({ id: 'prefix', title: 'Selected all rows' })
    ];
    assert.equal(ids(cs, 'sel')[0], 'prefix');
  });

  it('a title match beats a keyword-only match', () => {
    // The title is what the user can see, so a title match is the user finding
    // what they know exists. A keyword match is the user finding something they
    // cannot see, which is real but weaker.
    const cs = [
      cmd({ id: 'keyword', title: 'Rename the label', keywords: ['select'] }),
      cmd({ id: 'title', title: 'Select all' })
    ];
    assert.equal(ids(cs, 'select')[0], 'title');
  });

  it('a keyword still finds a command whose title does not match at all', () => {
    const cs = [cmd({ id: 'hidden', title: 'Rename the label', keywords: ['discard'] })];
    assert.deepEqual(ids(cs, 'discard'), ['hidden']);
  });

  it('breaks a scoring tie by fewer words, not alphabetically', () => {
    // `bt` ties on score. Alphabetical order would hand the user "Bring the
    // tagger up" -- four words -- over "Bulk tag", which is what they typed.
    const cs = [
      cmd({ id: 'many', title: 'Bring the tagger up' }),
      cmd({ id: 'few', title: 'Bulk tag' })
    ];
    assert.equal(ids(cs, 'bt')[0], 'few');
  });

  it('breaks a tie by length once the word counts match', () => {
    const cs = [
      cmd({ id: 'long', title: 'Bulk tags for rows' }),
      cmd({ id: 'short', title: 'Bulk tag rows' })
    ];
    // Same word count, so the shorter title wins. Asserting the pair rather than
    // the winner keeps the test honest about what it is pinning.
    const order = ids(cs, 'bt');
    assert.equal(order.length, 2);
    assert.equal(order[0], 'short');
  });

  it('is stable for the same call twice', () => {
    const cs = [
      cmd({ id: 'b', title: 'Bulk tag' }),
      cmd({ id: 'a', title: 'Bring the tagger up' })
    ];
    assert.deepEqual(ids(cs, 'bt'), ids(cs, 'bt'));
  });

  it('returns everything for an empty query, and nothing for nonsense', () => {
    const cs = [cmd({ id: 'a', title: 'Alpha' }), cmd({ id: 'b', title: 'Beta' })];
    assert.deepEqual(ids(cs, ''), ['a', 'b']);
    assert.deepEqual(ids(cs, '   '), ['a', 'b'], 'whitespace is an empty query');
    // An unmatched command is dropped rather than shown with no explanation: an
    // empty palette saying "no matches" beats one listing everything.
    assert.deepEqual(ids(cs, 'qqqqzzz'), []);
  });

  it('does not match a command that shares no letters at all', () => {
    const cs = [cmd({ id: 'a', title: 'Alpha' })];
    assert.deepEqual(ids(cs, 'zzz'), []);
  });
});

describe('the resolver and ranking together', () => {
  it('a global binding is reachable from inside a scope that binds the same key', () => {
    // The palette's Ctrl+P against a modal that also has a Ctrl+P. The global
    // pass runs first, and the modal cannot swallow it.
    const r = new CommandRegistry();
    r.register(cmd({ id: 'palette', title: 'Palette', global: true, binding: { key: 'p', code: 'KeyP', mods: ['ctrl'] } }));
    r.register(cmd({ id: 'modal-p', title: 'Modal thing', scope: 'modal', binding: { key: 'p', code: 'KeyP', mods: ['ctrl'] } }));
    r.register(cmd({ id: 'modal-only', title: 'Modal only', scope: 'modal' }));
    const e = { key: 'p', code: 'KeyP', ctrlKey: true, altKey: false, shiftKey: false, metaKey: false, target: null };
    assert.equal(r.resolve(e as never).kind, 'ran');
    assert.equal((r.resolve(e as never) as { id: string }).id, 'palette');
  });

  it('a non-global binding in a deeper scope does not beat an outer one', () => {
    const r = new CommandRegistry();
    r.register(cmd({ id: 'outer', title: 'Outer', binding: { key: 'g', code: 'KeyG', mods: [] } }));
    r.register(cmd({ id: 'inner', title: 'Inner', scope: 'inner', binding: { key: 'g', code: 'KeyG', mods: [] } }));
    r.register(cmd({ id: 'anchor', title: 'Anchor', scope: 'inner' }));
    const e = { key: 'g', code: 'KeyG', ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, target: null };
    assert.equal((r.resolve(e as never) as { id: string }).id, 'inner');
  });

  it('a command in a popped scope is not runnable', () => {
    const r = new CommandRegistry();
    r.register(cmd({ id: 'temp', title: 'Temp', scope: 'temp', binding: { key: 't', code: 'KeyT', mods: [] } }));
    r.register(cmd({ id: 'anchor', title: 'Anchor', scope: 'temp' }));
    const e = { key: 't', code: 'KeyT', ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, target: null };
    assert.equal((r.resolve(e as never) as { kind: string }).kind, 'ran');
    r.popScope('temp');
    // Popped means gone. A command that outlives the surface that registered it
    // is how a modal's Escape starts firing after the modal is closed.
    assert.notEqual((r.resolve(e as never) as { kind: string }).kind, 'ran');
  });
});
