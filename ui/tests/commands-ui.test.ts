/*
  Tests for `handleKey`, `paletteRows` and `moveHighlight` — the DOM-adjacent
  half of the command system.

  T-P5-006 item 5. Spec 10.7; plan §T-P5-006 item 5.

  # Why this file exists

  The first run of `scripts/mutate-commands-ui.py` reported that *every* mutant in
  `commands-ui.ts` survived, and that eleven of the search mutants survived too.
  Not because the behaviour is wrong — the e2e proved the palette works — but
  because no unit test touched those functions. The e2e goes through a real
  browser and asserts what a user sees; it never asks whether `handleKey` left a
  key alone.

  That gap matters more than the count suggests. A `preventDefault` is invisible
  in an e2e unless the keystroke has a visible consequence, and the two cases
  here — a key in a text field, and a command that declines — are precisely the
  ones whose whole job is to have *no* visible consequence. The only way to test
  "nothing happened" is to test the nothing.
 */

import { describe, it } from 'node:test';
import { strict as assert } from 'node:assert';
import {
  handleKey,
  paletteRows,
  moveHighlight,
  isPaletteChord,
  commands,
  PALETTE_ID,
  type Handled
} from '../src/lib/api/commands-ui.js';
import { CommandRegistry, type Command } from '../src/lib/api/commands.js';
import type { KeyLike } from '../src/lib/api/keys.js';

const CMD = (over: Partial<Command> & { id: string }): Command => ({
  title: over.title ?? over.id,
  scope: 'app',
  run: () => {},
  ...over
});

/** A keyboard event that records whether the app took the key. */
function keyEvent(over: Partial<KeyLike> & { key: string }) {
  const prevented: string[] = [];
  const e = {
    key: over.key,
    code: over.code ?? '',
    ctrlKey: over.ctrlKey ?? false,
    altKey: over.altKey ?? false,
    shiftKey: over.shiftKey ?? false,
    metaKey: over.metaKey ?? false,
    target: over.target ?? null,
    preventDefault: () => prevented.push(over.key)
  } as KeyLike & { preventDefault: () => void };
  return { e, prevented };
}

describe('handleKey — who takes the key', () => {
  it('runs a resolved command and takes the key', () => {
    const r = new CommandRegistry();
    const ran: string[] = [];
    r.register(CMD({ id: 'a', title: 'Alpha', binding: { key: 'a', code: 'KeyA', mods: [] }, run: () => ran.push('a') }));
    const { e, prevented } = keyEvent({ key: 'a', code: 'KeyA' });
    assert.equal(handleKey(e, r, (id) => void ran.push(id)), 'ran');
    assert.deepEqual(ran, ['a']);
    // The browser must not also see the key, or the page scrolls and types.
    assert.deepEqual(prevented, ['a']);
  });

  it('leaves an unbound key to the browser', () => {
    const r = new CommandRegistry();
    const { e, prevented } = keyEvent({ key: 'z', code: 'KeyZ' });
    assert.equal(handleKey(e, r, () => true), 'unbound');
    // Not preventing is the whole contract. A handler that claims every key
    // makes the page unusable, and the e2e cannot see that: there is nothing
    // visible to assert.
    assert.deepEqual(prevented, []);
  });

  it('leaves a key in a text field to the browser', () => {
    const r = new CommandRegistry();
    r.register(CMD({ id: 'a', title: 'Alpha', binding: { key: 'a', code: 'KeyA', mods: [] } }));
    const { e, prevented } = keyEvent({
      key: 'a',
      code: 'KeyA',
      target: { tagName: 'INPUT', type: 'text' } as unknown as EventTarget
    });
    assert.equal(handleKey(e, r, () => true), 'text');
    assert.deepEqual(prevented, [], 'typing a letter in a field must not be swallowed');
  });

  it('still takes a chord aimed at a text field', () => {
    const r = new CommandRegistry();
    const ran: string[] = [];
    r.register(
      CMD({
        id: 'p',
        title: 'Palette',
        global: true,
        binding: { key: 'p', code: 'KeyP', mods: ['ctrl'] },
        run: () => ran.push('p')
      })
    );
    const { e, prevented } = keyEvent({
      key: 'p',
      code: 'KeyP',
      ctrlKey: true,
      target: { tagName: 'INPUT', type: 'text' } as unknown as EventTarget
    });
    assert.equal(handleKey(e, r, (id) => void ran.push(id)), 'ran');
    assert.deepEqual(ran, ['p']);
    assert.deepEqual(prevented, ['p'], 'the user cannot type a control character, so this is ours');
  });

  it('leaves the key alone when the command declines', () => {
    // The bulk modal's Escape: the frame wins the key so the app scope's
    // `select.clear` does not run, and then declines so the browser's native
    // dialog dismissal still happens. Prevent-and-do-nothing is a modal that
    // will not close, and no e2e would catch it, because nothing visibly
    // changed either way.
    const r = new CommandRegistry();
    r.register(CMD({ id: 'esc', title: 'Close', binding: { key: 'Escape', code: 'Escape', mods: [] } }));
    const { e, prevented } = keyEvent({ key: 'Escape', code: 'Escape' });
    assert.equal(handleKey(e, r, () => false), 'ran');
    assert.deepEqual(prevented, [], 'a declined command must hand the key back');
  });

  it('runs nothing on a collision, and does not eat the key either', () => {
    const r = new CommandRegistry();
    const ran: string[] = [];
    r.register(CMD({ id: 'one', binding: { key: 'q', code: 'KeyQ', mods: [] }, run: () => ran.push('one') }));
    r.register(CMD({ id: 'two', binding: { key: 'q', code: 'KeyQ', mods: [] }, run: () => ran.push('two') }));
    const { e, prevented } = keyEvent({ key: 'q', code: 'KeyQ' });
    assert.equal(handleKey(e, r, (id) => void ran.push(id)), 'collision');
    // Picking a winner silently is how two features end up fighting; eating the
    // key on top of that means neither feature *nor* the browser gets it.
    assert.deepEqual(ran, []);
    assert.deepEqual(prevented, []);
  });

  it('does not run a gated command with nothing selected', () => {
    const r = new CommandRegistry();
    const ran: string[] = [];
    r.register(
      CMD({
        id: 'bulk',
        needsSelection: true,
        binding: { key: 'b', code: 'KeyB', mods: ['ctrl'] },
        run: () => ran.push('bulk')
      })
    );
    const { e } = keyEvent({ key: 'b', code: 'KeyB', ctrlKey: true });
    assert.equal(handleKey(e, r, (id) => void ran.push(id), 0), 'disabled');
    assert.deepEqual(ran, []);
  });

  it('runs a gated command once something is selected', () => {
    const r = new CommandRegistry();
    const ran: string[] = [];
    r.register(
      CMD({
        id: 'bulk',
        needsSelection: true,
        binding: { key: 'b', code: 'KeyB', mods: ['ctrl'] },
        run: () => ran.push('bulk')
      })
    );
    const { e } = keyEvent({ key: 'b', code: 'KeyB', ctrlKey: true });
    assert.equal(handleKey(e, r, (id) => void ran.push(id), 3), 'ran');
    assert.deepEqual(ran, ['bulk']);
  });
});

describe('isPaletteChord', () => {
  it('is Ctrl+P and not a bare p', () => {
    assert.equal(isPaletteChord(keyEvent({ key: 'p', code: 'KeyP', ctrlKey: true }).e), true);
    assert.equal(isPaletteChord(keyEvent({ key: 'p', code: 'KeyP' }).e), false);
  });

  it('is not Alt+Ctrl+P or Meta+P', () => {
    assert.equal(isPaletteChord(keyEvent({ key: 'p', code: 'KeyP', ctrlKey: true, altKey: true }).e), false);
    assert.equal(isPaletteChord(keyEvent({ key: 'p', code: 'KeyP', metaKey: true }).e), false);
  });

  it('tolerates a stray shift, because a chord that only works one way gets abandoned', () => {
    // A user reaching for Ctrl+P with Shift held is not asking for a different
    // gesture. No command binds Ctrl+Shift+P, so the only effect is that the
    // palette opens.
    assert.equal(isPaletteChord(keyEvent({ key: 'P', code: 'KeyP', ctrlKey: true, shiftKey: true }).e), true);
  });
});

describe('paletteRows', () => {
  it('lists the registry the palette is showing', () => {
    const r = new CommandRegistry();
    r.register(CMD({ id: 'one', title: 'First' }));
    r.register(CMD({ id: 'two', title: 'Second' }));
    assert.deepEqual(
      paletteRows(r, '').map((row) => row.command.id),
      ['one', 'two']
    );
  });

  it('filters, so the palette is not a list that ignores its own input', () => {
    const r = new CommandRegistry();
    r.register(CMD({ id: 'one', title: 'Bulk tag selected' }));
    r.register(CMD({ id: 'two', title: 'Bring the tagger up' }));
    r.register(CMD({ id: 'three', title: 'Select all' }));
    assert.deepEqual(
      paletteRows(r, 'bt').map((row) => row.command.id),
      ['one', 'two'],
      'two match; the third does not and must not appear'
    );
  });

  it('shows a gated command, disabled, rather than hiding it', () => {
    // Hiding it would make the palette an incomplete map, and "why is bulk tag
    // not in here" is a worse answer than "it is, and it needs a selection".
    const r = new CommandRegistry();
    r.register(CMD({ id: 'bulk', title: 'Bulk tag selected', needsSelection: true }));
    const rows = paletteRows(r, '', 0);
    assert.equal(rows.length, 1);
    assert.equal(rows[0].disabled, true);
    assert.equal(paletteRows(r, '', 2)[0].disabled, false);
  });

  it('gates on the command, not on there being any command', () => {
    const r = new CommandRegistry();
    r.register(CMD({ id: 'plain', title: 'Search this view' }));
    assert.equal(paletteRows(r, '', 0)[0].disabled, false, 'an ungated command is never disabled');
  });

  it('spells out each binding, and says so when there is none', () => {
    const r = new CommandRegistry();
    r.register(CMD({ id: 'with', title: 'With', binding: { key: 'a', code: 'KeyA', mods: ['ctrl'] } }));
    r.register(CMD({ id: 'without', title: 'Without' }));
    const rows = paletteRows(r, '');
    // The em dash is the point: a gap in that column reads as a rendering bug,
    // and "this command is not on your keyboard" is what it means.
    assert.equal(rows[0].binding, 'Ctrl+A');
    assert.equal(rows[1].binding, '');
  });
});

describe('moveHighlight', () => {
  it('moves down and up', () => {
    assert.equal(moveHighlight(0, 1, 3), 1);
    assert.equal(moveHighlight(2, -1, 3), 1);
  });

  it('wraps at both ends', () => {
    // A list that stops means a user who overshoots has to know they overshot,
    // and the usual sign of that is a cursor that does not move at all.
    assert.equal(moveHighlight(2, 1, 3), 0, 'down from the last wraps to the first');
    assert.equal(moveHighlight(0, -1, 3), 2, 'up from the first wraps to the last');
  });

  it('reports no highlight for an empty list', () => {
    // 0 would name the first row of a list with no rows, and the caller would
    // then try to highlight a command that is not in it.
    assert.equal(moveHighlight(0, 1, 0), -1);
    assert.equal(moveHighlight(0, -1, 0), -1);
  });
});
