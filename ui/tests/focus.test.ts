/**
 * Focus management, without a browser. T-P5-007, spec §10.8.
 *
 * The e2e covers the one case that has to be real -- a keypress moving focus
 * in a real browser. These cover the rest as a table, which is the only way to
 * cover "every combination of disabled, tabindex and tag" without writing
 * thirty browser tests that each take a second and each pass.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  initialFocus,
  isTabbable,
  nextFocusable,
  restoreTarget,
  tabbables,
  type Focusable
} from '../src/lib/theme/focus.js';

/** An element, with only what `Focusable` asks for. */
function el(
  localName: string,
  attrs: Record<string, string> = {},
  disabled = false
): Focusable {
  return {
    localName,
    disabled,
    getAttribute: (n: string) => attrs[n] ?? null
  };
}

const button = () => el('button');
const input = () => el('input');
const div = () => el('div');
const dialog = () => el('div', { role: 'dialog', tabindex: '-1' });

describe('what is in the tab order', () => {
  it('takes the natively focusable elements', () => {
    for (const tag of ['button', 'input', 'select', 'textarea']) {
      assert.ok(isTabbable(el(tag)), tag);
    }
  });

  it('takes an anchor only with an href', () => {
    // An `<a>` with no href is not a link. It is not focusable, not clickable,
    // and a dialog built from one is a dialog the keyboard cannot enter -- and
    // it is the single most common way a hand-built dialog loses its first Tab
    // stop.
    assert.equal(isTabbable(el('a')), false);
    assert.equal(isTabbable(el('a', { href: '/x' })), true);
    // A fragment href counts; it is a valid target.
    assert.equal(isTabbable(el('a', { href: '#' })), true);
  });

  it('takes a positive tabindex and refuses a negative one', () => {
    assert.ok(isTabbable(el('div', { tabindex: '0' })));
    assert.ok(isTabbable(el('div', { tabindex: '1' })));
    // -1 is programmatically focusable and NOT in the tab order. That is the
    // whole reason a dialog container can be a focus target.
    assert.equal(isTabbable(el('div', { tabindex: '-1' })), false);
  });

  it('refuses a disabled element', () => {
    // A trap that includes a disabled button strands the user on it: Tab does
    // not move, the element is not clickable, and nothing explains why.
    assert.equal(isTabbable(el('button', {}, true)), false);
    assert.equal(isTabbable(el('input', { type: 'checkbox' }, true)), false);
  });

  it('treats a non-numeric tabindex as absent, not as zero', () => {
    // The browser drops an unparseable tabindex, so the element falls back to
    // being natively focusable. A div with `tabindex="foo"` is NOT focusable.
    assert.equal(isTabbable(el('div', { tabindex: 'foo' })), false);
    assert.equal(isTabbable(el('button', { tabindex: 'foo' })), false);
  });

  it('takes a plain div only when it says so', () => {
    assert.equal(isTabbable(div()), false);
  });
});

describe('collecting the stops', () => {
  it('keeps DOM order and drops everything unreachable', () => {
    const stops = tabbables([
      el('button'),
      el('div'),
      el('button', {}, true),
      el('a'),
      el('a', { href: '/y' }),
      el('input')
    ]);
    assert.equal(stops.length, 3);
    assert.deepEqual(
      stops.map((s) => s.localName),
      ['button', 'a', 'input']
    );
  });

  it('returns nothing for a dialog with no controls', () => {
    // An empty dialog still has to be dismissible, which is what
    // `initialFocus`'s fallback is for.
    assert.deepEqual(tabbables([div(), el('div', { tabindex: '-1' })]), []);
  });
});

describe('Tab inside the trap', () => {
  const stops = [el('button'), el('input'), el('a', { href: '#' })];
  const [first, second, third] = stops;

  it('advances and wraps', () => {
    assert.equal(nextFocusable(stops, first, false), second);
    assert.equal(nextFocusable(stops, second, false), third);
    // The wrap is the part that makes it a trap rather than a dead end. Without
    // it the user presses Tab at the last control and nothing happens, which
    // reads as the page having frozen.
    assert.equal(nextFocusable(stops, third, false), first);
  });

  it('goes back and wraps backwards', () => {
    assert.equal(nextFocusable(stops, third, true), second);
    assert.equal(nextFocusable(stops, first, true), third);
  });

  it('does nothing when focus is not one of ours', () => {
    // A click on the backdrop, or focus arriving from browser chrome. Returning
    // null means the caller does not preventDefault, and the browser's own
    // behaviour is the right one.
    const stranger = el('button');
    assert.equal(nextFocusable(stops, stranger, false), null);
  });

  it('does nothing when there is nowhere to go', () => {
    assert.equal(nextFocusable([], button(), false), null);
  });

  it('handles a single stop without losing focus', () => {
    const only = [el('button')];
    // Both directions from the only element are itself. Returning null here
    // would drop focus to <body> on the first Tab, which is the bug this whole
    // module exists to prevent.
    assert.equal(nextFocusable(only, only[0], false), only[0]);
    assert.equal(nextFocusable(only, only[0], true), only[0]);
  });
});

describe('where focus goes when a dialog opens', () => {
  it('takes the first control', () => {
    const stops = [el('button'), el('input')];
    // Not the dialog itself, even though `tabindex="-1"` on the container is
    // the standard trick: a focus ring around an empty box is a ring around
    // nothing, and "dialog" with no content selected is a user guessing.
    assert.equal(initialFocus(stops, dialog()), stops[0]);
  });

  it('falls back to the dialog when it has no controls', () => {
    const d = dialog();
    assert.equal(initialFocus([], d), d);
  });
});

describe('where focus comes back to', () => {
  it('returns to the opener while it is still on the page', () => {
    const opener = el('button');
    assert.equal(restoreTarget(opener, () => true), opener);
  });

  it('returns nothing when the opener is gone', () => {
    // The common case: the dialog deleted the row that opened it. Focusing a
    // detached element throws in some engines and silently resets to <body> in
    // others, and "focus jumped to the top of the page" is the defect.
    assert.equal(restoreTarget(el('button'), () => false), null);
  });

  it('returns nothing when there was no opener', () => {
    assert.equal(restoreTarget(null, () => true), null);
    assert.equal(restoreTarget(undefined, () => true), null);
  });
});
