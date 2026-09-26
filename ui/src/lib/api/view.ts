/**
 * View state lives in the URL, and only in the URL (spec 5.16, 15.10).
 *
 * # Why a virtualized view cannot hold state in memory
 *
 * Spec 4.2 says it directly: the UI holds no more than a window of rows, so
 * it cannot hold a scroll offset, a loaded-page count, or a filter that it
 * read from a component's local state — there is nowhere reliable to put it.
 * The URL is the only state that survives a reload, a bookmark, a shared
 * link, and the back button, and those are four requirements the spec makes of
 * every list view rather than a nicety.
 *
 * # What is in the URL and what is not
 *
 * IN: filter, sort, direction, density, view mode. These describe the query
 * and the layout, and every one of them is shareable.
 *
 * OUT: the scroll offset and the loaded-page count. Those are not view state,
 * they are consequences of it — two people opening the same URL should both
 * start at the top, and a URL carrying someone's exact scroll position makes
 * the back button feel broken.
 *
 * The filter is one serialized string rather than a set of query parameters.
 * Spec 5.16 requires the whole filter AST to round-trip through the URL, and
 * `?rating=>4&tags=a,b&date=2020..2026&(x or y)` is not a grammar anyone can
 * extend. One opaque, versioned, base64url-encoded string is a grammar with
 * exactly one rule: it is the thing the server parses.
 */

import type { GridQuery } from './keyset.js';

/** Bump when the encoded filter format changes incompatibly. */
export const VIEW_VERSION = 1;

export interface ViewState extends GridQuery {
  /** Wall / list / table. */
  readonly mode: 'grid' | 'list';
  /** Tile size, in CSS pixels. Affects layout only, but is still view state. */
  readonly density: number;
}

export const defaultView: ViewState = {
  filter: null,
  sort: 'date',
  direction: 'DESC',
  tiers: null,
  mode: 'grid',
  density: 240
};

/**
 * Encode view state to a query string.
 *
 * Only non-default values are written. A URL full of defaults is unreadable
 * and makes the difference between two views invisible when you are comparing
 * them by eye, which is the main thing people do with URLs.
 */
export function encodeView(v: ViewState): string {
  const p = new URLSearchParams();
  if (v.filter) p.set('q', v.filter);
  if (v.sort && v.sort !== defaultView.sort) p.set('sort', v.sort);
  if (v.direction && v.direction !== defaultView.direction) p.set('dir', v.direction);
  if (v.mode !== defaultView.mode) p.set('mode', v.mode);
  if (v.density !== defaultView.density) p.set('density', String(v.density));
  if (v.tiers && v.tiers.length) p.set('tiers', v.tiers.join(','));
  return p.toString();
}

/**
 * Decode a query string back to view state.
 *
 * Every field is validated rather than trusted. A hand-edited or stale URL is
 * an ordinary thing to arrive at, and a URL that can put the UI into an
 * impossible state (`density=0`, `mode=nonsense`) is a bug that surfaces as a
 * blank page with no stack trace pointing anywhere useful.
 */
export function decodeView(search: string): ViewState {
  const p = new URLSearchParams(search.startsWith('?') ? search.slice(1) : search);
  const mode = p.get('mode');
  const dir = p.get('dir');
  const density = Number(p.get('density'));

  return {
    filter: p.get('q'),
    sort: p.get('sort') ?? defaultView.sort,
    direction: dir === 'ASC' || dir === 'DESC' ? dir : defaultView.direction,
    tiers: p.get('tiers') ? p.get('tiers')!.split(',').filter(Boolean) : null,
    mode: mode === 'list' || mode === 'grid' ? mode : defaultView.mode,
    // A density below the minimum renders tiles narrower than their own
    // content, and a non-numeric one is NaN, which silently renders nothing.
    density:
      Number.isFinite(density) && density >= 80 && density <= 1000
        ? density
        : defaultView.density
  };
}

/** The path + query for a view, for handing to the router. */
export function viewToHref(v: ViewState): string {
  const q = encodeView(v);
  return q ? `/?${q}` : '/';
}

/** Read view state out of a full URL or a location-like object. */
export function viewFromLocation(loc: { search: string }): ViewState {
  return decodeView(loc.search);
}
