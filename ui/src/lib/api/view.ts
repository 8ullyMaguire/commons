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

/**
 * The sorts the UI offers.
 *
 * Exported rather than inlined at each use, so the `<select>`'s options and
 * `decodeView`'s validation cannot disagree -- which is the bug this list was
 * added to fix. `sort` was the ONE view field `decodeView` passed through
 * unvalidated, so `?sort=nonsense` produced a state the select has no option
 * for: the control rendered BLANK while the page queried with the nonsense
 * value. A user who edited a shared link saw an empty sort dropdown and a
 * grid sorted by nothing in particular. Every other field was already
 * validated; this one was simply missed.
 */
export const SORTS = ['date', 'title', 'rating', 'added'] as const;
export type Sort = (typeof SORTS)[number];

function isSort(v: unknown): v is Sort {
  return typeof v === 'string' && (SORTS as readonly string[]).includes(v);
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
    sort: isSort(p.get('sort')) ? (p.get('sort') as Sort) : defaultView.sort,
    direction: dir === 'ASC' || dir === 'DESC' ? dir : defaultView.direction,
    tiers: p.get('tiers') ? p.get('tiers')!.split(',').filter(Boolean) : null,
    mode: mode === 'list' || mode === 'grid' ? mode : defaultView.mode,
    // A density below the minimum renders tiles narrower than their own
    // content, and a non-numeric one is NaN, which silently renders nothing.
    density: isDensity(density) ? density : defaultView.density
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

// --------------------------------------------------------------------------
// Per-user density (T-P5-006 item 8, spec 10.4)
// --------------------------------------------------------------------------

/**
 * The density range, in one place.
 *
 * One range, not two. If the URL bounds and the stored-preference bounds could
 * drift apart, a density could be stored that the URL would then reject, and
 * what the user saw would depend on which path a value happened to arrive by.
 */
export const DENSITY_LIMITS = { min: 80, max: 1000 } as const;

/** A plain integer tile width inside the range. Anything else is not one. */
export function isDensity(v: unknown): v is number {
  return typeof v === 'number' && Number.isInteger(v) && v >= DENSITY_LIMITS.min && v <= DENSITY_LIMITS.max;
}

/**
 * Somewhere to keep a per-user preference.
 *
 * An interface rather than a direct `localStorage` call, for the reason every
 * test in this codebase injects its store: `localStorage` throws for reasons
 * that have nothing to do with this app -- Safari private mode, a full quota,
 * storage blocked in a third-party frame. Every one of those is an environment,
 * and the right response to a preference that cannot be read is the default
 * rather than a blank page.
 */
export interface DensityStore {
  read(): number | null;
  write(density: number): void;
}

/**
 * The stored density, or `null` if there is not a usable one.
 *
 * Every failure -- absent, unparseable, out of range, or a store that throws --
 * comes back as `null`, and the caller renders the default. Discarding rather
 * than clamping is deliberate: clamping a stored 40 to the minimum of 80 shows
 * tiles the user did not ask for and leaves no way back to 40, and a value
 * outside the range is a corrupt or hand-edited store rather than a preference.
 */
export function storedDensity(store: DensityStore | null | undefined): number | null {
  if (!store) return null;
  let raw: number | null;
  try {
    raw = store.read();
  } catch {
    return null;
  }
  return isDensity(raw) ? raw : null;
}

/**
 * Record a density the user chose.
 *
 * Only when it differs from what is already there, and only if it is a density
 * at all. The "only when it differs" is not a micro-optimisation: writing on
 * every render means a slider drag writes N times, and a store written during a
 * render is a store a component can write on its way out. The write is the
 * user's decision, so it happens on the decision.
 *
 * Returns whether anything was written, which is what a test asserts rather
 * than reaching into the store to see.
 */
export function storedDensitySet(density: number, store: DensityStore | null | undefined): boolean {
  if (!store || !isDensity(density)) return false;
  if (storedDensity(store) === density) return false;
  try {
    store.write(density);
    return true;
  } catch {
    return false;
  }
}

/**
 * View state for a location, with the stored density as the fallback.
 *
 * THE RULE, and the reason this function exists: **the stored value is a
 * default, not an override.** A link carrying `?density=400` shows 400 even if
 * the recipient's own preference is 180; the bare URL shows whatever they last
 * chose.
 *
 * The alternative -- stored preference wins -- makes a shared link render
 * differently on every machine, and spec 5.16 requires the URL to survive being
 * shared. Reload, bookmark, share, back: a preference that overrides the URL
 * breaks three of those four. The asymmetry is the point: an explicit URL value
 * is a statement by the sender, a stored value is a default the receiver
 * happens to have, and a statement beats a default.
 */
export function viewForLocation(loc: { search: string }, store?: DensityStore | null): ViewState {
  const fromUrl = decodeView(loc.search);
  if (loc.search.includes('density=')) return fromUrl;
  const stored = storedDensity(store);
  return stored === null ? fromUrl : { ...fromUrl, density: stored };
}

/**
 * A `DensityStore` over `localStorage`, for the app to use.
 *
 * Reads a string and parses it with `parseInt`, which is why
 * {@link isDensity} insists on an integer: `parseInt('320px')` is 320, so the
 * parse is deliberately loose and the check after it is what makes the result
 * safe.
 */
export function localDensityStore(key = 'commons.density'): DensityStore {
  return {
    read(): number | null {
      const raw = globalThis.localStorage?.getItem(key);
      if (raw === null || raw === undefined) return null;
      // `Number`, not `parseInt`.
      //
      // `parseInt('320px')` is 320, so a lenient parse would accept a value
      // the user never chose and silently read past the part it did not
      // understand. `Number('320px')` is NaN, which `isDensity` rejects -- so
      // a stored string has to BE a number, all of it, or it is discarded.
      // This is the same reasoning as `isDensity` refusing a float: the point
      // is not that the bad value is harmless, it is that accepting it means
      // the store's meaning depends on the parser.
      const n = Number(raw.trim());
      return Number.isNaN(n) ? null : n;
    },
    write(density: number): void {
      globalThis.localStorage?.setItem(key, String(density));
    }
  };
}
