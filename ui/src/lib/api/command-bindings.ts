/*
  Registering the app's commands.

  T-P5-006 item 5. Spec 10.7; plan §T-P5-006 item 5.

  # Why this is a function and not a module body

  A module body runs once, at import, which is before any component exists and
  therefore before there is a selection count to gate a command on. Registering
  is cheap and idempotent by id, so doing it from the shell -- where the app's
  state actually lives -- costs one pass at mount and keeps the bindings next to
  the handlers that satisfy them.

  # The bindings, and why these keys

  They are the ones the spec's issues name, and none of them is arbitrary:

  - `Ctrl+P` the palette, handled before the stack (see `commands-ui.ts`)
  - `/` focus search, because `/` is what every list-and-filter UI uses and a
    user reaching for it will not read this file
  - `j`/`k` move the cursor, `x` toggles selection: vim's motions, which is the
    expectation of anyone who installs a keyboard map in the first place
  - `Ctrl+A` select all -- deliberately the *browser's* select-all in a text
    field and *our* select-all outside one, which is why it is a bare-letter-free
    chord and why `resolve` lets chords through text fields
  - `Escape` clears the selection, and is claimed by the top scope when there is
    one, so a modal takes it first

  Every one of these is a command, which is why every one of them appears in the
  palette. That is the "complete shortcut map" the spec asks for, with no
  separate documentation to fall out of date.
 */

import { commands, PALETTE_CHORD, PALETTE_ID } from './commands-ui.js';
import type { Chord, Modifier } from './keys.js';

const chord = (code: string, key: string, ...mods: Modifier[]): Chord => ({
  code,
  key,
  mods
});

/** The ids, so a surface can register a handler for one without a string. */
export const CMD = {
  palette: PALETTE_ID,
  search: 'view.search',
  moveDown: 'select.moveDown',
  moveUp: 'select.moveUp',
  toggle: 'select.toggle',
  selectAll: 'select.all',
  clear: 'select.clear',
  bulkTag: 'bulk.tag'
} as const;

/**
 * Register the app-scope commands.
 *
 * `selected` is read at *resolve* time, not here -- see `needsSelection` on the
 * command. Registering with a stale count would freeze the gate at mount.
 */
export function registerAppCommands(): void {
  commands.register({
    id: CMD.palette,
    title: 'Command palette',
    detail: 'Find and run any command',
    keywords: ['find', 'search', 'start'],
    scope: 'app',
    binding: PALETTE_CHORD,
    // The one global binding in the app. See `Command.global`.
    global: true
  });

  commands.register({
    id: CMD.search,
    title: 'Search this view',
    detail: 'Filter the current result',
    keywords: ['filter', 'find', 'query'],
    scope: 'app',
    binding: chord('Slash', '/')
  });

  commands.register({
    id: CMD.moveDown,
    title: 'Move down',
    detail: 'Move the cursor to the next row',
    keywords: ['next', 'down'],
    scope: 'app',
    binding: chord('KeyJ', 'j')
  });

  commands.register({
    id: CMD.moveUp,
    title: 'Move up',
    detail: 'Move the cursor to the previous row',
    keywords: ['previous', 'up'],
    scope: 'app',
    binding: chord('KeyK', 'k')
  });

  commands.register({
    id: CMD.toggle,
    title: 'Toggle selection',
    detail: 'Select or deselect the row under the cursor',
    keywords: ['select', 'check', 'mark'],
    scope: 'app',
    binding: chord('KeyX', 'x'),
    needsSelection: false
  });

  commands.register({
    id: CMD.selectAll,
    title: 'Select all',
    detail: 'Everything in the current result',
    keywords: ['every', 'all'],
    scope: 'app',
    binding: chord('KeyA', 'a', 'ctrl')
  });

  commands.register({
    id: CMD.clear,
    title: 'Clear selection',
    detail: 'Deselect everything',
    keywords: ['none', 'reset', 'deselect'],
    scope: 'app',
    binding: chord('Escape', 'Escape')
  });

  commands.register({
    id: CMD.bulkTag,
    title: 'Bulk tag selected',
    detail: 'Add a tag to everything selected',
    keywords: ['apply', 'edit', 'tag'],
    scope: 'app',
    binding: chord('KeyB', 'b', 'ctrl'),
    // The one command here that genuinely needs a selection. Gating it is the
    // point: an ungated bulk write is a destructive action with an empty scope.
    needsSelection: true
  });
}
