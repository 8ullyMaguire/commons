/**
 * The tile-shape function. Spec 10.4, plan T-P5-006 item 8.
 *
 * # What this decides, and what it deliberately does not
 *
 * One question per row: what shape is this tile, and is there anything to play?
 * Everything else about a media view — the window, the scroll math, the lightbox
 * — is downstream of the answer and gets it from here.
 *
 * It is a pure function of a row and a target ratio, with no DOM, no fetch, and
 * no store. That is not tidiness. The wrong answers this function can give are
 * all invisible in a screenshot: a row at zero height, a strip that makes the
 * viewport enormous, a ratio inverted so every tile letterboxes. Each is a
 * *number*, so the tests belong here and the DOM is left to draw it.
 *
 * # Why proportional rather than cropped
 *
 * A square tile with `object-fit: cover` is the cheap option and the wrong one.
 * A person scanning a wall of tiles is choosing what to open, and a tile that
 * has silently removed a third of the frame cannot be scanned. The cost of
 * proportional tiles is real and it is paid in exactly one function — this one —
 * rather than in every tile.
 *
 * The cost is that a row's height depends on its contents, and a virtualized
 * grid's window depends on row heights. Which is why `ViewState` carries a
 * *declared* target ratio and a row's height is that ratio applied to the row's
 * own width: the height depends only on a number the virtualizer already has,
 * and the window stays computable without loading the rows inside it.
 */

import type { ObjectRow } from './client.js';

/**
 * The bounds a tile's aspect may take.
 *
 * Not decoration. An unclamped strip — a 10000×1 banner scan, or `height: 0`
 * from a probe that returned nothing — becomes a row that is either
 * infinitely tall or undefined, and one such row makes the whole viewport
 * unusable rather than that one tile looking odd.
 */
export const ASPECT_LIMITS = { min: 0.25, max: 4 } as const;

/**
 * What the row holds, decided only from facts the row carries.
 *
 * `audio` is deliberately unreachable. The first version of this function
 * branched on `durationMs === 0` for audio, which is wrong twice over: a probe
 * that FAILED also reports 0, so a failed video scan rendered as playable
 * audio; and a genuine 0 is then not playable at all. Nothing in `ObjectRow`
 * distinguishes an audio file from a video one — the kind string is a Scene
 * label, not a container — so guessing here would be a guess. `audio` stays in
 * the type because the lightbox will need it the moment the row carries a
 * container, and adding a variant then is cheaper than removing one now.
 */
export type TileKind = 'image' | 'video' | 'audio' | 'unknown';

export interface TileShape {
  /**
   * Width ÷ height, so a tile `w` wide is `w / aspect` tall.
   *
   * Portrait rows are therefore *below* 1. `VirtualGrid` documents getting
   * this backwards once, and the wrong direction is invisible to any test that
   * only checks the number is positive.
   */
  readonly aspect: number;
  readonly kind: TileKind;
  /** Whether there is something to play. A still is not playable. */
  readonly playable: boolean;
}

/**
 * The target ratio: the 2:3 poster shape most libraries are full of.
 *
 * Named rather than inlined at each call site, because "the default tile shape"
 * appearing as `2 / 3` in four places is four places to change.
 */
export const POSTER_RATIO = 2 / 3;

/**
 * What a row's tile looks like.
 *
 * @param row    One grid row. Only `width`, `height`, `kind` and `durationMs`
 *               are read; the rest of `ObjectRow` is here because the caller
 *               already has a row and should not have to build a second one.
 * @param target The declared ratio for a row whose own media is unknown or
 *               unusable. Not "the fallback" — the layout's intent, which is
 *               what keeps the grid regular where the data cannot.
 */
export function tileShape(row: ObjectRow, target: number = POSTER_RATIO): TileShape {
  const aspect = clamp(aspectOf(row.width, row.height) ?? safeTarget(target));
  return { aspect, kind: kindOf(row), playable: isPlayable(row) };
}

/**
 * The row's own ratio, or `undefined` when it cannot be known.
 *
 * `undefined` rather than a number so the caller can tell "no dimensions" from
 * "dimensions that happen to equal the target" — a row whose width is genuinely
 * absent is a row nobody has probed yet, and it is worth a different badge in
 * the UI than a row that measured square.
 */
function aspectOf(width: number | null, height: number | null): number | undefined {
  if (width === null || height === null) return undefined;
  // Non-finite and non-positive are both "no measurement", not "a ratio of
  // zero". `height: 0` is a probe that returned nothing, and treating it as a
  // real number is how a division by zero reaches the layout.
  if (!Number.isFinite(width) || !Number.isFinite(height)) return undefined;
  if (width <= 0 || height <= 0) return undefined;
  return width / height;
}

/** The declared target, itself made safe. */
function safeTarget(target: number): number {
  return Number.isFinite(target) && target > 0 ? target : POSTER_RATIO;
}

function clamp(aspect: number): number {
  return Math.min(ASPECT_LIMITS.max, Math.max(ASPECT_LIMITS.min, aspect));
}

/**
 * What the row holds, from facts rather than from a filename.
 *
 * A duration is a fact about the content; an extension is a claim about a name,
 * and the row carries no extension — which is the point. A `.mp4` that failed to
 * probe reports no duration and lands in `unknown`, not in `video`, so nothing
 * offers to play a file that will not.
 */
function kindOf(row: ObjectRow): TileKind {
  if (isPlayable(row)) return 'video';
  // A still is an image on EITHER piece of evidence: a cover to draw, or
  // dimensions, which are themselves the product of a probe. The first version
  // of this checked `coverPath` alone, so an image with measured dimensions but
  // no thumbnail — a cover that has not been generated, or a format the
  // thumbnaller skipped — came out `unknown`, and the tile fell back to the
  // declared ratio as though nothing were known. `unknown` is reserved for a row
  // with no duration, no cover, and no dimensions: nothing has been measured.
  if (row.coverPath !== null) return 'image';
  if (aspectOf(row.width, row.height) !== undefined) return 'image';
  return 'unknown';
}

/**
 * Whether this row can be played.
 *
 * Strictly positive: a probe that failed can report `0`, and rendering that as
 * a playable zero-length video produces a tile that opens onto nothing.
 */
function isPlayable(row: ObjectRow): boolean {
  return typeof row.durationMs === 'number' && Number.isFinite(row.durationMs) && row.durationMs > 0;
}
