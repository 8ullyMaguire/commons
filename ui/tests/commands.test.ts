/*
  Tests for the command registry, key normalisation, and the palette filter.

  T-P5-006 item 5. Spec 10.7; plan §T-P5-006 item 5. The reasoning is in
  docs/spec/t-p5-006-commands.md; the e2e in e2e/commands.spec.ts carries the
  parts a unit test cannot reach, which is the same split as items 3 and 4.

  # The four cases a naive implementation fails

  1. Collision resolution. Two commands, one key, and the question is not
     "which wins" but "does the loser also run". Independent `addEventListener`
     calls -- which is what the codebase has today in three places -- run both,
     and the user gets one keystroke interpreted twice.
  2. Layout independence. A binding matched on `event.key` is correct only on a
     US keyboard and silently wrong everywhere else.
  3. Text fields. A bare letter in an input is a letter. A chord in an input is
     still a command, and the line between those two is the whole rule.
  4. A lone modifier. `Shift` on its own is not `Shift+Shift`, and treating it
     as a binding produces a command that fires on every shift press.
 */

import { strict as assert } from 'node:assert';
import { describe, it } from 'node:test';

import {
  CommandRegistry,
  bindingLabel,
  searchCommands,
  type Command
} from '../src/lib/api/commands.js';
import {
  chordsEqual,
  formatChord,
  normalizeKey,
  type Chord,
  type KeyLike
} from '../src/lib/api/keys.js';

/** A keystroke, with everything unspecified false. */
function press(
  key: string,
  code = '',
  mods: Partial<Record<'ctrl' | 'alt' | 'shift' | 'meta', boolean>> = {},
  target: EventTarget | null = null
): KeyLike {
  return {
    key,
    code,
    ctrlKey: mods.ctrl ?? false,
    altKey: mods.alt ?? false,
    shiftKey: mods.shift ?? false,
    metaKey: mods.meta ?? false,
    target
  };
}

const chord = (code: string, key: string, mods: Chord['mods'] = []): Chord => ({ code, key, mods });

function cmd(over: Partial<Command> & { id: string; title: string }): Command {
  return {
    scope: 'app',
    binding: null,
    ...over
  };
}

// ---------------------------------------------------------------- keys.ts

describe('normalizeKey', () => {
  it('reads a bare letter as itself', () => {
    const c = normalizeKey(press('x', 'KeyX'));
    assert.ok(c);
    assert.equal(c.code, 'KeyX');
    assert.equal(c.key, 'x');
    assert.deepEqual(c.mods, []);
  });

  it('collects modifiers in one fixed order regardless of press order', () => {
    // The order is a property of the format string, not of the event: the
    // palette shows `Ctrl+Shift+K` and two equal chords must produce the same
    // string for a mutation test to compare them.
    const a = normalizeKey(press('K', 'KeyK', { shift: true, ctrl: true }));
    const b = normalizeKey(press('K', 'KeyK', { ctrl: true, shift: true }));
    assert.ok(a && b);
    assert.deepEqual(a.mods, ['ctrl', 'shift']);
    assert.deepEqual(b.mods, ['ctrl', 'shift']);
    assert.equal(formatChord(a), formatChord(b));
  });

  // Case 4 above. `Shift` alone is a non-event.
  it('treats a lone modifier press as nothing at all', () => {
    assert.equal(normalizeKey(press('Shift', 'ShiftLeft', { shift: true })), null);
    assert.equal(normalizeKey(press('Control', 'ControlLeft', { ctrl: true })), null);
    assert.equal(normalizeKey(press('Alt', 'AltLeft', { alt: true })), null);
    assert.equal(normalizeKey(press('Meta', 'MetaLeft', { meta: true })), null);
    // And with no `code` at all, which is what some platforms send.
    assert.equal(normalizeKey(press('Shift', '', { shift: true })), null);
  });

  it('keeps a modified key that happens to be named after a modifier', () => {
    // `Shift+Alt` pressed together produces a real chord, and the `key` is
    // `Shift` on some platforms. The rule is "no non-modifier key", not "key is
    // not a modifier name".
    const c = normalizeKey(press('Shift', 'ShiftLeft', { shift: true, alt: true }));
    assert.ok(c);
    assert.deepEqual(c.mods, ['alt', 'shift']);
  });

  it('formats a chord for display', () => {
    assert.equal(formatChord(chord('KeyK', 'k', ['ctrl'])), 'Ctrl+K');
    assert.equal(formatChord(chord('KeyK', 'k', ['ctrl', 'shift'])), 'Ctrl+Shift+K');
    assert.equal(formatChord(chord('Escape', 'Escape')), 'Esc');
  });
});

describe('chordsEqual', () => {
  it('matches on code even when the character differs', () => {
    // The AZERTY case: physical `KeyQ` produces the character `a`. A map keyed
    // on `key` gets this wrong; a map keyed on `code` does not.
    assert.ok(
      chordsEqual(chord('KeyQ', 'q'), chord('KeyQ', 'a')),
      'same physical key, different character'
    );
  });

  it('does not match different keys that produce the same character', () => {
    // The inverse, and the reason `code` is checked first: `a` and `q` on
    // AZERTY are different keys and must not be interchangeable.
    assert.equal(chordsEqual(chord('KeyQ', 'a'), chord('KeyA', 'a')), false);
  });

  it('requires an exact modifier set', () => {
    assert.equal(chordsEqual(chord('KeyK', 'k', ['ctrl']), chord('KeyK', 'k')), false);
    assert.equal(chordsEqual(chord('KeyK', 'k', ['ctrl']), chord('KeyK', 'k', ['ctrl', 'shift'])), false);
    assert.ok(chordsEqual(chord('KeyK', 'k', ['ctrl', 'shift']), chord('KeyK', 'k', ['shift', 'ctrl'])));
  });
});

// ----------------------------------------------------------- commands.ts

describe('a registry with one command', () => {
  it('resolves its binding', () => {
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'A', binding: chord('KeyX', 'x') }));
    assert.deepEqual(r.resolve(press('x', 'KeyX')), { kind: 'ran', id: 'a' });
  });

  it('resolves nothing for an unbound key', () => {
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'A', binding: chord('KeyX', 'x') }));
    assert.deepEqual(r.resolve(press('z', 'KeyZ')), { kind: 'unbound' });
  });

  it('replaces rather than duplicating on re-registration', () => {
    // A component re-renders and re-registers. A registry that appended would
    // grow a second entry and then report a collision against itself.
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'First', binding: chord('KeyX', 'x') }));
    r.register(cmd({ id: 'a', title: 'Second', binding: chord('KeyX', 'x') }));
    assert.equal(r.all().length, 1);
    assert.equal(r.all()[0].title, 'Second');
    assert.deepEqual(r.resolve(press('x', 'KeyX')), { kind: 'ran', id: 'a' });
  });
});

describe('collision resolution — case 1', () => {
  // The failure this whole item exists to prevent: two independent keydown
  // listeners, one keystroke, two commands. The user gets one press interpreted
  // twice, and neither component knows the other exists.
  it('runs only the innermost scope, not both', () => {
    const r = new CommandRegistry();
    r.register(cmd({ id: 'outer', title: 'Outer', binding: chord('Escape', 'Escape') }));
    r.register(cmd({ id: 'inner', title: 'Inner', scope: 'modal', binding: chord('Escape', 'Escape') }));

    // With the modal open, the inner scope is on top.
    assert.deepEqual(r.scopes(), ['app', 'modal']);
    assert.deepEqual(r.resolve(press('Escape', 'Escape')), { kind: 'ran', id: 'inner' });
  });

  it('falls back to the outer scope once the inner one pops', () => {
    const r = new CommandRegistry();
    r.register(cmd({ id: 'outer', title: 'Outer', binding: chord('Escape', 'Escape') }));
    r.register(cmd({ id: 'inner', title: 'Inner', scope: 'modal', binding: chord('Escape', 'Escape') }));
    assert.ok(r.popScope('modal'));
    assert.deepEqual(r.scopes(), ['app']);
    assert.deepEqual(r.resolve(press('Escape', 'Escape')), { kind: 'ran', id: 'outer' });
  });

  it('reports a collision rather than picking, when one scope has two claimants', () => {
    // A priority model would silently pick one here. Reporting is the honest
    // answer: this is a bug in the app's bindings, and the fix is to remove one.
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'A', binding: chord('KeyX', 'x') }));
    r.register(cmd({ id: 'b', title: 'B', binding: chord('KeyX', 'x') }));
    assert.deepEqual(r.resolve(press('x', 'KeyX')), { kind: 'collision', ids: ['a', 'b'] });
  });

  it('never pops the app frame', () => {
    const r = new CommandRegistry();
    assert.equal(r.popScope(), false);
    assert.equal(r.popScope('app'), false);
    assert.deepEqual(r.scopes(), ['app']);
  });

  it('refuses to pop a frame that is not on top', () => {
    // Two modals, the older dismissed first. Popping the top would close the
    // wrong one.
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'A', scope: 'first' }));
    r.register(cmd({ id: 'b', title: 'B', scope: 'second' }));
    assert.deepEqual(r.scopes(), ['app', 'first', 'second']);
    assert.equal(r.popScope('first'), false);
    assert.deepEqual(r.scopes(), ['app', 'first', 'second']);
  });

  it('does not run a command whose frame has been popped', () => {
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'A', scope: 'modal', binding: chord('Escape', 'Escape') }));
    assert.ok(r.popScope('modal'));
    // The command is still *registered* — it may be re-registered when the
    // surface remounts — but it is not reachable from the keyboard.
    assert.equal(r.all().length, 1);
    assert.deepEqual(r.resolve(press('Escape', 'Escape')), { kind: 'unbound' });
  });
});

describe('selection-gated commands', () => {
  it('reports disabled rather than running with nothing selected', () => {
    const r = new CommandRegistry();
    r.register(
      cmd({ id: 'bulk', title: 'Bulk tag', binding: chord('KeyB', 'b', ['ctrl']), needsSelection: true })
    );
    assert.deepEqual(r.resolve(press('b', 'KeyB', { ctrl: true }), 0), {
      kind: 'disabled',
      id: 'bulk'
    });
    assert.deepEqual(r.resolve(press('b', 'KeyB', { ctrl: true }), 3), { kind: 'ran', id: 'bulk' });
  });
});

describe('text fields — case 3', () => {
  // Minimal element stand-ins. The node test runner has no `document`, and the
  // rule under test duck-types on `tagName` precisely so that it does not need
  // one -- a real `HTMLElement` here would test the runner, not the rule.
  const el = (tag: string, extra: Record<string, unknown> = {}) =>
    ({ tagName: tag.toUpperCase(), ...extra }) as unknown as EventTarget;
  const input = () => el('input', { type: 'text' });

  it('leaves a bare letter in a text field as text', () => {
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'A', binding: chord('KeyX', 'x') }));
    assert.deepEqual(r.resolve(press('x', 'KeyX', {}, input())), { kind: 'text' });
  });

  it('still takes a chord in a text field', () => {
    // The interesting half of the rule: the user cannot type a control
    // character, so `Ctrl+K` is ours even while they are typing.
    const r = new CommandRegistry();
    r.register(cmd({ id: 'p', title: 'Palette', binding: chord('KeyK', 'k', ['ctrl']) }));
    assert.deepEqual(r.resolve(press('k', 'KeyK', { ctrl: true }, input())), { kind: 'ran', id: 'p' });
  });

  it('treats a textarea and a contenteditable as text', () => {
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'A', binding: chord('KeyX', 'x') }));
    const ta = el('textarea');
    const ce = el('div', { isContentEditable: true });
    assert.deepEqual(r.resolve(press('x', 'KeyX', {}, ta)), { kind: 'text' });
    assert.deepEqual(r.resolve(press('x', 'KeyX', {}, ce)), { kind: 'text' });
  });

  it('does not treat a checkbox as a text field', () => {
    // A bare letter on a checkbox is not text, and swallowing it there is how a
    // shortcut map ends up "not working on the settings page".
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'A', binding: chord('KeyX', 'x') }));
    const cb = el('input', { type: 'checkbox' });
    assert.deepEqual(r.resolve(press('x', 'KeyX', {}, cb)), { kind: 'ran', id: 'a' });
  });
});

describe('the palette filter — case 4', () => {
  const menu: Command[] = [
    cmd({ id: 'bulk', title: 'Bulk tag selected', keywords: ['apply', 'edit'] }),
    cmd({ id: 'bring', title: 'Bring the tagger up' }),
    cmd({ id: 'palette', title: 'Command palette' }),
    cmd({ id: 'undo', title: 'Undo last action' })
  ];

  it('returns everything for an empty query', () => {
    assert.equal(searchCommands(menu, '').length, menu.length);
    assert.equal(searchCommands(menu, '   ').length, menu.length);
  });

  it('matches across word boundaries, not just substrings', () => {
    // `bte` means "bulk tag edit". A substring match finds nothing here, which
    // is what makes most palettes feel broken.
    const hits = searchCommands(menu, 'bte');
    assert.ok(hits.length > 0);
    assert.equal(hits[0].command.id, 'bulk');
  });

  // The tie, which is the case worth having a test for. "Bulk tag selected" and
  // "Bring the tagger up" both match `bt` on two word-initial letters and score
  // identically -- there is no scoring difference to find. Without a tiebreak
  // the order falls to alphabetical and the user gets "Bring", which is neither
  // more likely nor more relevant. The tiebreak is the feature.
  it('breaks a scoring tie toward the shorter command', () => {
    const hits = searchCommands(
      [cmd({ id: 'long', title: 'Bring the tagger up' }), cmd({ id: 'short', title: 'Bulk tag' })],
      'bt'
    );
    assert.equal(hits[0].command.id, 'short');
  });

  it('is deterministic when the tiebreak cannot separate two commands', () => {
    // Same word count, same length: only alphabetical remains, and it has to be
    // stable so a test can assert it and a user does not see the list reshuffle
    // between keystrokes.
    const a = [cmd({ id: 'z', title: 'Alpha Beta' }), cmd({ id: 'a', title: 'Alto Beta' })];
    // Same call twice gives the same order -- that is the property. Which one
    // comes first is alphabetical, and 'Alto' sorts before 'Alpha' because
    // 'l' < 'p' at the third character.
    assert.deepEqual(
      searchCommands(a, 'ab').map((h) => h.command.title),
      searchCommands(a, 'ab').map((h) => h.command.title)
    );
    assert.equal(searchCommands(a, 'ab')[0].command.title, 'Alto Beta');
  });

  it('ranks a title match above a keyword-only match', () => {
    // The user can see the title and not the keywords, so a title hit is the
    // more likely intent.
    const hits = searchCommands(
      [cmd({ id: 'kw', title: 'Zebra', keywords: ['palette'] }), cmd({ id: 'ti', title: 'Palette' })],
      'palette'
    );
    assert.equal(hits[0].command.id, 'ti');
  });

  it('returns nothing for a query that matches nothing', () => {
    // An empty palette with "no matches" beats a palette showing every command
    // because the query was nonsense.
    assert.deepEqual(searchCommands(menu, 'qqqqzzz'), []);
  });

  it('shows a command with no binding, labelled as having none', () => {
    // Discoverable and not mistypeable is a real state, not a stub.
    const r = new CommandRegistry();
    r.register(cmd({ id: 'a', title: 'A', binding: chord('KeyX', 'x') }));
    r.register(cmd({ id: 'b', title: 'B' }));
    assert.equal(bindingLabel(r.all()[0]), 'X');
    assert.equal(bindingLabel(r.all()[1]), '');
  });

  it('finds a command by a keyword that is not in its title', () => {
    const hits = searchCommands(menu, 'apply');
    assert.equal(hits[0].command.id, 'bulk');
  });
});

describe('reorder', () => {
  it('moves an item and returns a new array', () => {
    // A copy, not an in-place sort: a selection reorder that mutates its input
    // also mutates a sorted view the caller still holds.
    const src = ['a', 'b', 'c', 'd'];
    const out = CommandRegistry.reorder(src, 0, 2);
    assert.deepEqual(out, ['b', 'c', 'a', 'd']);
    assert.deepEqual(src, ['a', 'b', 'c', 'd']);
    assert.notEqual(out, src);
  });

  it('clamps a target past the end', () => {
    assert.deepEqual(CommandRegistry.reorder(['a', 'b'], 0, 99), ['b', 'a']);
  });

  it('is a no-op for an out-of-range source', () => {
    assert.deepEqual(CommandRegistry.reorder(['a', 'b'], 9, 0), ['a', 'b']);
    assert.deepEqual(CommandRegistry.reorder(['a', 'b'], -1, 0), ['a', 'b']);
  });
});
