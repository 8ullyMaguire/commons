/*
  The command palette, and the global keydown handler.

  T-P5-006 item 5. Spec 10.7 (#2542 Esc closes modals, #6218/#5587 ordering
  shortcuts, #2833 collision handling); plan §T-P5-006 item 5. The registry and
  the filter are pure and unit-tested in `tests/commands.test.ts`; this file is
  the part a mutation cannot reach, which is the same split as items 3 and 4.

  # One listener, and why it is not three

  The codebase had three independent `window.addEventListener('keydown', ...)`
  calls -- in the lightbox, the bulk modal, and the list table -- with no
  coordination. Escape in the lightbox closed the lightbox *and* was seen by
  every other handler, so one keystroke was interpreted by components that
  could not know about each other. #2833 is not a feature that can be added to
  that arrangement; it is a consequence of having one resolver, and it is
  impossible to add one binding at a time.

  So: exactly one listener, on `window`, in the shell. It asks the registry what
  the keystroke names and runs that, and the outcome tells it whether to
  `preventDefault`. A handler that claims nothing lets the browser have the key,
  which is what keeps typing working without anyone special-casing it.

  # Why the palette chord bypasses the scope stack

  `Ctrl+P` is handled before resolution, and always. A palette you cannot open
  from inside a modal is a palette that does not exist for half your users, and
  the alternative -- a `modal` frame that claims Ctrl+P -- means every surface
  has to remember to re-bind it. One unconditional chord beats N correct ones.

  The registry still lists it, so the palette lists it too and the shortcut is
  discoverable; it is simply resolved before the stack is consulted.

  # Why the query input is exempt from its own text rule

  The palette's own input is a text field, so `resolve` would classify every
  keystroke in it as `text` and the palette would find nothing. It passes
  `target: null` for exactly that reason -- not to bypass the rule, but to state
  that this input's keystrokes are the palette's business, not the page's.
 */

import { CommandRegistry, searchCommands, bindingLabel, type Command, type Outcome } from './commands.js';
import type { Chord, KeyLike } from './keys.js';

/**
 * The one registry.
 *
 * Module scope, not a context, for the reason the unsaved guard's registry is:
 * the keydown listener and the palette must read the same list, and a context
 * read by a component mounted under a different provider is a silent `undefined`
 * -- which here would be an empty palette and no shortcuts, with nothing failing.
 */
export const commands = new CommandRegistry();

/** The palette chord. Handled before the scope stack; see the header. */
export const PALETTE_CHORD: Chord = { key: 'p', code: 'KeyP', mods: ['ctrl'] };

/**
 * The palette command's id.
 *
 * A named constant rather than a bare string, because the alternative -- a
 * handler that recognises the chord itself and returns a magic outcome -- is
 * exactly what broke in the first version: the layout discarded the return
 * value, so `Ctrl+P` was recognised, `preventDefault`ed, and did nothing. Every
 * layer reported success and no palette appeared.
 *
 * `command-bindings.ts` registers under this, so the two cannot drift.
 */
export const PALETTE_ID = 'palette.open';

/**
 * Is this keystroke the palette chord?
 *
 * Compared by the same rules as any other binding, so it is layout-independent
 * and modifier-exact. A bare `p` is not the chord -- that is a command's job, if
 * any command wants it.
 */
export function isPaletteChord(e: KeyLike): boolean {
  if (!e.ctrlKey) return false;
  if (e.altKey || e.metaKey) return false;
  // `shiftKey` is deliberately not checked: `Ctrl+Shift+P` is a different
  // gesture in some apps and a muscle-memory slip in others, and accepting it
  // costs nothing -- no command is bound to it, so the only effect is that the
  // palette opens. Being permissive here is what makes the chord feel
  // reliable, and a chord that only works if you hold exactly the right
  // modifiers is a chord users stop trying.
  return e.key.toLowerCase() === 'p' || e.code === 'KeyP';
}

/** What the keydown handler decided to do. Returned so the e2e can assert it. */
export type Handled = 'ran' | 'opened-palette' | 'text' | 'unbound' | 'collision' | 'disabled';

/**
 * Resolve one keystroke and, if it names a command, run it.
 *
 * `run` is passed in rather than looked up, so this is testable without a
 * registry full of side effects and so the caller decides what "running" means
 * for each command -- a command that opens a modal needs a component, and a
 * registry that held components would be a second app.
 */
export function handleKey(
  e: KeyLike,
  registry: CommandRegistry,
  /**
   * Perform the command. Return `false` to decline -- the key is then left to
   * the browser rather than swallowed.
   */
  run: (id: string) => boolean | void,
  selected = 0
): Handled {
  const outcome = registry.resolve(e, selected);
  switch (outcome.kind) {
    case 'ran':
      // Only a command that actually ran may take the key. `unbound` must fall
      // through to the browser, or the page stops scrolling and the user cannot
      // type `j` into a field the registry has no binding for.
      //
      // `run` is called first, and its return value is what decides
      // `preventDefault`. A command that declines to act -- because a modal
      // handled the key natively, or because the surface that owns it is not
      // mounted -- must leave the key to the browser. Preventing it and doing
      // nothing is the one outcome worse than not handling the key at all: the
      // user pressed something, the app appeared to consider it seriously, and
      // nothing happened.
      if (run(outcome.id) !== false) e.preventDefault?.();
      return 'ran';
    case 'text':
      // The browser owns it. Explicit, because "we decided it was text" and
      // "we did not handle it" are the same action with different reasons, and
      // the e2e asserts the reason.
      return 'text';
    case 'collision':
      // Deliberately *not* prevented and deliberately not run. Two commands in
      // one frame claiming one key is an app bug; the honest response is to do
      // nothing visible and let the developer see it in the log, rather than
      // silently picking a winner.
      console.warn('commons: shortcut collision', outcome.ids);
      return 'collision';
    case 'disabled':
      return 'disabled';
    case 'unbound':
    default:
      return 'unbound';
  }
}

/** One row in the palette. */
export interface PaletteRow {
  command: Command;
  /** `''` when the command has no binding -- a real, shown state. */
  binding: string;
  disabled: boolean;
}

/**
 * The rows to show for a query.
 *
 * One function, so the palette component holds no filtering logic of its own
 * and a change to the ranking cannot be made in one place and not the other.
 */
export function paletteRows(
  registry: CommandRegistry,
  query: string,
  selected = 0
): PaletteRow[] {
  return searchCommands(registry.all(), query).map(({ command }) => ({
    command,
    binding: bindingLabel(command),
    disabled: command.needsSelection === true && selected === 0
  }));
}

/** Move the highlighted row, clamping at both ends. */
export function moveHighlight(current: number, delta: number, count: number): number {
  if (count === 0) return -1;
  const next = current + delta;
  // Wraps. A list that stops at the ends means a user who overshoots has to
  // know they overshot, and the usual sign of that is a selection that does not
  // move at all.
  if (next < 0) return count - 1;
  if (next >= count) return 0;
  return next;
}
