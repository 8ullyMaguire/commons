/**
 * Per-user density: a stored default that a URL can still override.
 *
 * Spec §10.4, plan T-P5-006 item 8. §5 of
 * `docs/spec/t-p5-006-view-modes.md`.
 *
 * # Why this file exists rather than three lines in the view
 *
 * §5.16 says view state lives in the URL. A URL cannot be a preference — a
 * shared link is a URL, and a preference is not shareable by definition. So
 * density needs somewhere else to live, and the interesting part is not the
 * storage: it is the RULE for when the two disagree. That rule is the whole
 * reason this is a module with tests.
 *
 * # The rule, and what it is defending
 *
 * **The stored value is a default, not an override.** Opening a link that
 * carries `?density=400` shows 400 even if the recipient's stored preference is
 * 180. Opening the bare URL shows whatever the recipient last chose.
 *
 * The alternative — stored preference wins — makes a shared link render
 * differently on every machine, which is the failure mode that makes people stop
 * trusting links. §5.16's whole reason for putting state in the URL is that four
 * things have to survive: a reload, a bookmark, a shared link, and the back
 * button. A preference that overrides the URL breaks three of the four.
 *
 * And the direction is asymmetric on purpose: an explicit URL value is a
 * *statement* by the sender, a stored value is a *default* the receiver happens
 * to have. When those conflict the statement wins.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  decodeView,
  defaultView,
  localDensityStore,
  storedDensitySet,
  viewForLocation,
  DENSITY_LIMITS,
  type DensityStore
} from '../src/lib/api/view.js';

function makeStore(initial: number | null): DensityStore & { saved: number | null } {
  let saved = initial;
  return {
    get saved() {
      return saved;
    },
    read(): number | null {
      return saved;
    },
    write(v: number): void {
      saved = v;
    }
  };
}

test('with no stored preference and no URL, the default stands', () => {
  const s = makeStore(null);
  const v = viewForLocation({ search: '' }, s);
  assert.equal(v.density, defaultView.density);
});

test('a stored preference is used when the URL says nothing about density', () => {
  const s = makeStore(320);
  const v = viewForLocation({ search: '' }, s);
  assert.equal(v.density, 320, 'the last density the user chose');
});

test('an explicit density in the URL beats the stored preference', () => {
  // THE rule. A shared link must render the same on the sender's machine and
  // the recipient's, or it is not a shared link -- and §5.16 requires the URL
  // to survive being shared.
  const s = makeStore(180);
  const v = viewForLocation({ search: '?density=400' }, s);
  assert.equal(v.density, 400, 'the link wins over my own habit');
});

test('a URL carrying other view state still takes the stored density', () => {
  // The converse: the preference is not consulted only when the URL is
  // completely empty. A link that sets a filter and no density is still a link
  // that does not speak about density, so the recipient's own choice stands.
  const s = makeStore(300);
  const v = viewForLocation({ search: '?sort=rating' }, s);
  assert.equal(v.density, 300);
  assert.equal(v.sort, 'rating', 'and the URL state is honoured normally');
});

test('a stored preference outside the limits is discarded, not clamped', () => {
  // Discarded rather than clamped, and the difference matters: clamping 40 to
  // the minimum of 80 shows the user tiles they did not ask for and did not
  // choose, and there is no way back to 40 because the stored value is gone.
  // An out-of-range value is a corrupt or hand-edited store, and the default is
  // the honest answer to "I do not know what this should be".
  for (const bad of [0, 40, 5000, Number.NaN]) {
    const s = makeStore(bad);
    assert.equal(
      viewForLocation({ search: '' }, s).density,
      defaultView.density,
      `${bad} is not a density anyone chose`
    );
  }
});

test('the limits are the same ones the URL enforces', () => {
  // One range, not two. If the URL bounds and the store bounds could drift,
  // then a density could be stored that the URL would reject, and the
  // behaviour would depend on which path a value happened to arrive by.
  assert.equal(DENSITY_LIMITS.min, 80);
  assert.equal(DENSITY_LIMITS.max, 1000);
});

test('a value is only stored when it differs from what is already there', () => {
  // Not a performance thing. Writing on every render means a slider drag
  // writes N times, and a store that is written during a render is a store that
  // can be written by a component that unmounts mid-drag. The write is the
  // user's decision, so it happens on the decision.
  const s = makeStore(300);
  storedDensitySet(300, s);
  assert.equal(s.saved, 300, 'unchanged: nothing written');
  storedDensitySet(420, s);
  assert.equal(s.saved, 420, 'changed: written');
});

test('an out-of-range value is never written to the store', () => {
  // The mirror of the read rule. A corrupt value that got in would be
  // re-validated on every read, so the failure is permanent-but-harmless; not
  // writing it at all is one line and removes the case.
  const s = makeStore(300);
  storedDensitySet(9999, s);
  assert.equal(s.saved, 300, 'rejected, not stored');
  storedDensitySet(0, s);
  assert.equal(s.saved, 300);
});

test('a store that throws does not take the view down with it', () => {
  // `localStorage` throws in a real browser for reasons that have nothing to do
  // with this app: Safari private mode, a full quota, third-party storage
  // blocked in an embedded frame. Every one of those is an environment, not a
  // bug, and the correct response to a preference that cannot be read is to
  // render the default -- not a blank page.
  const exploding: DensityStore = {
    read() {
      throw new Error('QuotaExceededError');
    },
    write() {
      throw new Error('QuotaExceededError');
    }
  };
  assert.equal(viewForLocation({ search: '' }, exploding).density, defaultView.density);
  assert.equal(viewForLocation({ search: '?density=400' }, exploding).density, 400);
  // And writing through it is a no-op rather than a throw.
  storedDensitySet(400, exploding);
});

test('a corrupt stored string is rejected by the real localStorage store', () => {
  // `localStorage` holds strings, so the read path parses. `parseInt` is
  // deliberately the loose parse -- it is what reads "320" out of "320" -- and
  // the strictness lives in `isDensity`, which insists on an integer.
  //
  // So the honest test drives the REAL store with a fake `localStorage`, rather
  // than a fake store: a hand-built `DensityStore` that returned
  // `Number.parseInt('320px')` returns 320 whatever the validation does, and a
  // test built on it asserts nothing about the code that ships. I wrote it that
  // way first and it passed a corrupt value through -- which is exactly the bug
  // a test written against a mock cannot find.
  const withStorage = (raw: string | null): DensityStore => {
    const g = globalThis as unknown as { localStorage?: { getItem(k: string): string | null; setItem(k: string, v: string): void } };
    g.localStorage = {
      getItem: () => raw,
      setItem: () => {}
    };
    return localDensityStore();
  };

  assert.equal(viewForLocation({ search: '' }, withStorage('320')).density, 320, 'a good value');
  assert.equal(
    viewForLocation({ search: '' }, withStorage('320px')).density,
    defaultView.density,
    'a trailing suffix means the string is not a number at all'
  );
  assert.equal(
    viewForLocation({ search: '' }, withStorage('  ')).density,
    defaultView.density,
    'whitespace parses to NaN'
  );
  assert.equal(
    viewForLocation({ search: '' }, withStorage('99999')).density,
    defaultView.density,
    'out of range'
  );
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
});

test('decodeView still owns the URL bounds, unchanged', () => {
  // The store rule is additive: `decodeView` alone must keep behaving exactly
  // as it did, because a caller that never touches a store still has to get a
  // safe density. If this fails, the new module has changed the old contract.
  assert.equal(decodeView('?density=40').density, defaultView.density);
  assert.equal(decodeView('?density=400').density, 400);
});
