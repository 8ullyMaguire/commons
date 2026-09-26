/**
 * View state ↔ URL. Spec 5.16 and 15.10: the whole filter serializes into the
 * URL, and every list view is fully described by its query string.
 *
 * The properties worth testing are the round trip and the rejection of a
 * hand-edited URL. A URL is the one piece of this UI a user will edit by hand,
 * share in a chat, and paste from a bookmark three months later.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  decodeView,
  encodeView,
  defaultView,
  viewToHref,
  type ViewState
} from '../src/lib/api/view.js';

describe('view state round trips through the URL', () => {
  test('the default view encodes to an empty query string', () => {
    // A URL full of defaults is unreadable, and it hides the difference
    // between two views when you compare them by eye.
    assert.equal(encodeView(defaultView), '');
    assert.equal(viewToHref(defaultView), '/');
  });

  test('a filter survives the round trip', () => {
    const v: ViewState = { ...defaultView, filter: 'rating > 4' };
    assert.deepEqual(decodeView(encodeView(v)), v);
  });

  test('every field survives the round trip', () => {
    const v: ViewState = {
      filter: 'tags:a AND (b OR c)',
      sort: 'title',
      direction: 'ASC',
      tiers: ['public', 'private'],
      mode: 'list',
      density: 320
    };
    assert.deepEqual(decodeView(encodeView(v)), v);
  });

  test('a filter with URL-reserved characters is encoded, not dropped', () => {
    // The filter is a grammar; it is carried as one opaque string, and a
    // space or a slash in it must not truncate the query string.
    const v: ViewState = { ...defaultView, filter: 'a&b=c d/e?f#g' };
    const q = encodeView(v);
    assert.ok(!q.includes('#'), 'a fragment would truncate the URL');
    assert.deepEqual(decodeView(q), v);
  });

  test('a leading question mark is accepted and not treated as data', () => {
    const v: ViewState = { ...defaultView, sort: 'title' };
    assert.deepEqual(decodeView('?' + encodeView(v)), v);
    assert.deepEqual(decodeView(encodeView(v)), v);
  });
});

describe('a hand-edited URL is validated, not trusted', () => {
  test('an unknown mode falls back to the default', () => {
    // A URL that can put the UI into an impossible state is a bug that
    // surfaces as a blank page with no stack trace.
    assert.equal(decodeView('?mode=nonsense').mode, 'grid');
    assert.equal(decodeView('?mode=list').mode, 'list', 'a valid one is kept');
  });

  test('an unknown direction falls back to the default', () => {
    assert.equal(decodeView('?dir=SIDEWAYS').direction, 'DESC');
    assert.equal(decodeView('?dir=ASC').direction, 'ASC');
  });

  test('a density below the minimum or above the maximum is rejected', () => {
    // Density 0 renders tiles narrower than their own content; a non-numeric
    // density is NaN, which silently renders nothing at all.
    assert.equal(decodeView('?density=0').density, defaultView.density);
    assert.equal(decodeView('?density=-5').density, defaultView.density);
    assert.equal(decodeView('?density=99999').density, defaultView.density);
    assert.equal(decodeView('?density=abc').density, defaultView.density);
    assert.equal(decodeView('?density=320').density, 320);
  });

  test('an empty query string is the default view', () => {
    assert.deepEqual(decodeView(''), defaultView);
    assert.deepEqual(decodeView('?'), defaultView);
  });

  test('a tier list with empty entries drops them', () => {
    assert.deepEqual(decodeView('?tiers=a,,b,').tiers, ['a', 'b']);
  });

  test('an unknown parameter is ignored rather than fatal', () => {
    // Forward compatibility: a URL from a newer Commons must still open.
    const v = decodeView('?somethingNew=1&sort=title');
    assert.equal(v.sort, 'title');
  });
});

describe('viewToHref', () => {
  test('produces a root path when everything is default', () => {
    assert.equal(viewToHref(defaultView), '/');
  });

  test('produces a root path with a query otherwise', () => {
    const h = viewToHref({ ...defaultView, sort: 'title' });
    assert.ok(h.startsWith('/?'), h);
    assert.ok(h.includes('sort=title'), h);
  });
});
