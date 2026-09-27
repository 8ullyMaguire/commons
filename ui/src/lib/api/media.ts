/**
 * The unified media view: which rows are in the Media tab, and the places a
 * scene and an image genuinely differ. Spec §5.17, §10.4, §10.5; plan T-P5-006
 * item 17; #1030, #7068, #1508.
 *
 * # The model is already unified, and that is the point
 *
 * §5.3 says "Image is an Object, so it votes like one", and `Object.kind` is a
 * `TEXT` column on the one `object` table with an index on it. So there is no
 * join, no union table and no second identity to reconcile: a media view is
 * `kind IN (...)` over one table, and everything that already works on a scene
 * works on an image.
 *
 * Which is why this file is small, and mostly about *membership and ordering*.
 * The interesting content of #1030 is not "show two kinds together" — that is
 * one `In` — it is the few places where the two cannot be treated identically,
 * and where getting it wrong produces a view that looks right and is a lie.
 *
 * # What this does NOT do
 *
 * It does not decide a tile's shape. `media-view.ts` (item 8) does, and its
 * header says so: one question per row, what shape is this tile and is there
 * anything to play. This file asks the question *before* that one — is this row
 * in the view, and where does it sort — and hands over. Duplicating the ratio
 * logic here would give two places to change when `ASPECT_LIMITS` moves, and the
 * two would disagree within a release.
 *
 * # The one deliberate disagreement with item 8
 *
 * Item 8's tile-level `isPlayable` requires a strictly positive `durationMs`,
 * and its header gives the reason: a failed probe reports `0`, and rendering that
 * as a playable zero-length video produces a tile that opens onto nothing.
 * That is the right call *for a tile*, whose only evidence is the duration.
 *
 * It is the wrong call *for membership*. This module is deciding whether the
 * row can be opened in the player, and it has a fact item 8 does not: the kind.
 * A scene whose probe failed is still a video, and refusing to play it because
 * its length is unknown would be a lie in the other direction. The two
 * functions answer different questions, and `tests/media.test.ts` says so
 * explicitly, because "why does this say playable and that one say no" is
 * otherwise a contradiction to whoever reads them next.
 *
 * # Why `kind` is parsed and reported rather than trusted
 *
 * `ObjectRow.kind` is a bare `string`, because the GraphQL schema is T-P6-007
 * and a generated enum would need the schema to exist. Worse, `object.kind` is
 * unconstrained `TEXT` in `0001_core.sql` — an enum in `commons-core`, a
 * string in the database, and nothing at the boundary keeps them in sync. So an
 * unknown kind is a *normal* event, not corruption, and the only honest
 * responses are to report it (`auditMedia`) and to keep its wire spelling as a
 * label (`sectionLabel`) rather than guessing.
 *
 * `KINDS` is asserted equal to `ObjectKind::as_str` by reading the Rust file, so
 * a kind added in Rust and not here fails a test instead of rendering every row
 * of that kind as a photograph.
 */

import type { ObjectRow } from './client.js';

/** The kinds the model knows, mirroring `ObjectKind::as_str` in `enums.rs`. */
export const KINDS = [
  'scene',
  'image',
  'gallery',
  'audio',
  'comic',
  'text',
  'interview'
] as const;

export type Kind = (typeof KINDS)[number];

/** The subset this module needs, so a test can build a row without a client. */
export type MediaRow = Pick<ObjectRow, 'id' | 'kind' | 'durationMs' | 'width' | 'height'>;

/**
 * A kind, or `null` if this build does not know it.
 *
 * Membership by value, never `startsWith`: a `startsWith('scene')` check calls
 * `scene-cut` a scene, and a kind added upstream with a compound name would then
 * be placed as playable video.
 */
export function parseKind(s: string): Kind | null {
  return (KINDS as readonly string[]).includes(s) ? (s as Kind) : null;
}

// ---------------------------------------------------------------------------
// What is in the Media tab
// ---------------------------------------------------------------------------

/**
 * The kinds the Media tab shows, in facet order.
 *
 * The list is **not** "every kind". #1030 asks for scenes and images
 * specifically, and a Media tab containing text objects and audio is a Library
 * tab that lost the plot.
 *
 * `gallery` is excluded for a different reason: a gallery is a *container*, so a
 * Media tab that lists galleries lists things twice — once as the container, and
 * once as the images inside it. That is the specific way "unified" goes wrong,
 * and it is why this is an explicit list rather than `KINDS.filter(media)`.
 *
 * This constant is the single place that decides what the tab means. The route
 * reads it for the query rather than repeating the list.
 */
export const MEDIA_KINDS: readonly Kind[] = ['scene', 'image'];

/**
 * The facet for a media view, as filter values for `kind IN (...)`.
 *
 * `CmpOp::In` exists in `filter_ast.rs` and takes a value list, so this is the
 * whole server-side cost of the tab: one facet, no join, and it composes with
 * every other filter the user has.
 */
export function mediaFilterKinds(kinds: readonly Kind[] = MEDIA_KINDS): string[] {
  // A copy. A caller that pushes to the result must not change what the tab is.
  return [...kinds];
}

/** Whether a row belongs in the Media tab. */
export function isMediaKind(kind: string): boolean {
  const k = parseKind(kind);
  return k !== null && MEDIA_KINDS.includes(k);
}

// ---------------------------------------------------------------------------
// Ordering across kinds
// ---------------------------------------------------------------------------

/** Where a kind sits in the order; anything outside it sorts last. */
export function kindRank(kind: string, order: readonly Kind[] = MEDIA_KINDS): number {
  const k = parseKind(kind);
  if (k === null) return order.length;
  const i = order.indexOf(k);
  return i === -1 ? order.length : i;
}

/**
 * A total order over rows of mixed kinds.
 *
 * #7068 and #1508 are both about a secondary sort that is only defined *within*
 * a kind, which interleaves two kinds in an arrangement that changes as rows
 * arrive. The rule here: compare the kind in a **fixed** order, then the id.
 *
 * The id tiebreak is not decoration — without it the comparator is not a total
 * order, and rows that tie on the kind exchange places depending on which page
 * boundary they landed on. With it, adding a row cannot move the others.
 *
 * This comparator is used *after* the server's sort has already put the primary
 * field in order, so the kind and id only ever break ties. That is the
 * division of labour that makes the cross-kind tiebreak safe: it is not
 * re-sorting, it is making a tie total.
 */
export function compareMedia(
  a: MediaRow,
  b: MediaRow,
  kindOrder: readonly Kind[] = MEDIA_KINDS
): number {
  const ka = kindRank(a.kind, kindOrder);
  const kb = kindRank(b.kind, kindOrder);
  if (ka !== kb) return ka - kb;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

/** Sort a mixed list by `compareMedia`. Does not mutate the input. */
export function sortMedia<T extends MediaRow>(
  rows: readonly T[],
  kindOrder: readonly Kind[] = MEDIA_KINDS
): T[] {
  return [...rows].sort((a, b) => compareMedia(a, b, kindOrder));
}

// ---------------------------------------------------------------------------
// Playability — the one disagreement with item 8
// ---------------------------------------------------------------------------

/**
 * Whether this row can be opened in the player.
 *
 * True for `scene` and `audio`, **regardless of the duration**. Duration is
 * metadata; capability is the kind. A scene whose probe failed or has not run
 * reports `null` or `0`, and refusing to play it for that reason would hide a
 * video behind a broken measurement.
 *
 * The contrast with `media-view.ts`'s tile-level `isPlayable` is deliberate and
 * the two coexist: a tile refuses to offer playback on a zero duration because
 * that is the only evidence it has, while membership here has the kind.
 */
export function isPlayable(row: MediaRow): boolean {
  const k = parseKind(row.kind);
  return k === 'scene' || k === 'audio';
}

/** Whether the tile should show a play affordance. Follows `isPlayable`. */
export function hasPlayAffordance(row: MediaRow): boolean {
  return isPlayable(row);
}

// ---------------------------------------------------------------------------
// Duration
// ---------------------------------------------------------------------------

/** `m:ss` or `h:mm:ss`, or `null` when there is no usable duration. */
export function formatDuration(ms: number | null): string | null {
  // The guards, in order: a missing duration, a non-finite one, and a negative
  // one all mean "not measured". None of them is `0:00`.
  if (ms === null || !Number.isFinite(ms) || ms < 0) return null;
  const total = Math.floor(ms / 1000);
  const s = total % 60;
  const m = Math.floor(total / 60) % 60;
  const h = Math.floor(total / 3600);
  const pad = (n: number): string => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

/**
 * The duration badge, or `null`.
 *
 * `null` for a still, and `null` for a scene with no *positive* duration. A
 * zero-length scene is unmeasured, not zero seconds, and a badge of `0:00` next
 * to a photograph with the same badge merges two rows that are not the same.
 */
export function durationBadge(row: MediaRow): string | null {
  if (!isPlayable(row)) return null;
  const ms = row.durationMs;
  if (ms === null || !Number.isFinite(ms) || ms <= 0) return null;
  return formatDuration(ms);
}

// ---------------------------------------------------------------------------
// The audit a new kind forces
// ---------------------------------------------------------------------------

export interface MediaAudit {
  /** Rows in the tab. */
  readonly shown: number;
  /** Distinct kinds this build could not place. */
  readonly unknownKinds: readonly string[];
  /** Rows not shown, for any reason. */
  readonly excluded: number;
  /** Rows per kind, keyed in `kinds` order. */
  readonly byKind: Readonly<Record<string, number>>;
}

/**
 * Audit a batch against `MEDIA_KINDS`.
 *
 * Called by the view on every load, and asserted in the tests, for one reason:
 * when a kind is added to `ObjectKind` in Rust and not to `MEDIA_KINDS`, **this
 * is where it shows up**. A grid that silently drops a new kind is the failure
 * the whole function exists to prevent, and an unplaced row is the one failure a
 * user cannot diagnose from the screen they are looking at.
 *
 * `unknownKinds` is distinct from "excluded by a narrower tab" on purpose. One
 * is a build that cannot place a row at all; the other is a tab that was asked
 * for a subset. Only the first is a bug.
 */
export function auditMedia(rows: readonly MediaRow[], kinds: readonly Kind[] = MEDIA_KINDS): MediaAudit {
  const unknown: string[] = [];
  const counts = new Map<string, number>();
  let shown = 0;
  let excluded = 0;
  for (const r of rows) {
    const k = parseKind(r.kind);
    if (k === null) {
      // Once per distinct spelling, not once per row: 4,000 rows of one new
      // kind is one problem, not four thousand.
      if (!unknown.includes(r.kind)) unknown.push(r.kind);
      excluded += 1;
      continue;
    }
    if (!kinds.includes(k)) {
      excluded += 1;
      continue;
    }
    shown += 1;
    counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  // Keyed in `kinds` order, not in the order rows happened to arrive. Built by
  // assignment, `byKind`'s key order is the first-row order, so the same library
  // audited after a single new scene arrives reports its keys in a different
  // order -- and a legend built from it moves under the cursor.
  const byKind: Record<string, number> = {};
  for (const k of kinds) {
    const n = counts.get(k);
    if (n !== undefined) byKind[k] = n;
  }
  return { shown, unknownKinds: unknown, excluded, byKind };
}

// ---------------------------------------------------------------------------
// Grouping a media wall by kind
// ---------------------------------------------------------------------------

/** The section a row belongs under, or `null` if it is not in the tab. */
export function kindSection(row: MediaRow, kinds: readonly Kind[] = MEDIA_KINDS): string | null {
  const k = parseKind(row.kind);
  if (k === null || !kinds.includes(k)) return null;
  return k;
}

/** The heading for a kind section. */
export function sectionLabel(kind: string): string {
  switch (parseKind(kind)) {
    case 'scene':
      return 'Scenes';
    case 'image':
      return 'Images';
    case 'gallery':
      return 'Galleries';
    case 'audio':
      return 'Audio';
    case 'comic':
      return 'Comics';
    case 'text':
      return 'Text';
    case 'interview':
      return 'Interviews';
    default:
      // The wire spelling, not 'undefined'. A section titled "undefined" tells
      // the user nothing; the wire spelling tells a developer exactly what to go
      // and look at.
      return kind;
  }
}

/**
 * Order sections by kind order, not alphabetically.
 *
 * "Images" before "Scenes" is alphabetical and wrong: `MEDIA_KINDS` is the order
 * the facet lists them, and a wall whose sections move when a kind is added is
 * the same complaint as a folder tree that re-sorts on rename.
 */
export function orderSections(
  sections: readonly string[],
  kinds: readonly Kind[] = MEDIA_KINDS
): string[] {
  return [...sections].sort((a, b) => kindRank(a, kinds) - kindRank(b, kinds));
}

// ---------------------------------------------------------------------------
// The facet, as the wire actually spells it
// ---------------------------------------------------------------------------

/**
 * The serialized `Filter` AST, as `serde_json` writes it.
 *
 * The shape is not guessed. It was printed by `serde_json::to_string` on the
 * Rust `Filter` and copied here, because a filter that the UI builds and the
 * server cannot parse is a tab that silently shows the wrong rows — and a
 * silently wrong filter is the worst thing this page could ship:
 *
 *     {"and":[{"facet":{"kind":null,"field":{"builtin":"kind"},
 *       "op":"in","values":[{"str":"scene"},{"str":"image"}]}}]}
 *
 * Three details are load-bearing and all three are invisible if you write the
 * obvious thing:
 *
 *   - `field` is `{"builtin":"kind"}`, **not** the string `"kind"`. `FieldRef` is
 *     externally tagged, so a bare string is a shape the server rejects.
 *   - a value is `{"str":"scene"}`, not `"scene"`. `Value` is externally tagged
 *     for the same reason, and `{"str":"a"}` versus `{"list":[...]}` is
 *     disambiguated by structure rather than by convention.
 *   - `op` is lowercase: `in`, not `In`.
 */
export interface FacetNode {
  readonly facet: {
    readonly kind: null;
    readonly field: { readonly builtin: string };
    readonly op: string;
    readonly values: readonly { readonly str: string }[];
  };
}

/** A filter the UI can build. Only the shapes item 17 needs. */
export type BuiltFilter = FacetNode | { readonly and: readonly BuiltFilter[] };

/** The `kind IN (...)` facet, as the wire spells it. */
export function kindFacet(kinds: readonly Kind[] = MEDIA_KINDS): FacetNode {
  return {
    facet: {
      kind: null,
      field: { builtin: 'kind' },
      op: 'in',
      values: kinds.map((k) => ({ str: k }))
    }
  };
}

/**
 * AND a facet onto a base filter, returning the string the transport takes.
 *
 * # The base is opaque, and that decides the shape of this function
 *
 * `view.ts` says the filter is "one opaque, versioned, base64url-encoded
 * string... it is the thing the server parses", and no UI code parses it. So
 * there is no way to merge a media facet INTO a user's filter without decoding
 * the user's filter, which is exactly the thing the design forbids.
 *
 * Which leaves two honest options, and the choice matters:
 *
 *   (a) Replace. The tab shows scenes and images; the URL's `q` is ignored.
 *       Simple, and it is a lie: a user who typed `?q=cat` and then opened the
 *       Media tab would get everything, and the URL would still say `q=cat`, so
 *       a shared link would not describe what was shared.
 *
 *   (b) Keep the base and append the facet as a conjunction *at the transport*.
 *       `{"and":[<base>, <facet>]}` is not expressible, because `<base>` is a
 *       string and an `and` takes nodes.
 *
 * So this does (a) and says so — and the *route* is where the other half of the
 * fix lives: it puts the facet in the URL as a real, shareable value rather than
 * hiding it here. `mediaFilter` is the honest single-filter form; the tab's
 * composability with `?q=` is a transport concern, and pretending otherwise in
 * a helper is how a query that looks right returns the wrong rows.
 *
 * The tests pin the shape against what Rust printed, which is the only thing
 * that makes "the server will accept this" a fact rather than a hope.
 */
export function mediaFilter(kinds: readonly Kind[] = MEDIA_KINDS): string {
  return JSON.stringify(kindFacet(kinds));
}

/**
 * Whether a string looks like something `Filter::from_url` would accept.
 *
 * Not a parser — a *shape check*, used to decide whether to trust a hand-edited
 * URL. The Media tab replaces `q` rather than ANDing onto it (above), so a base
 * that does not parse is dropped rather than passed through broken, and this is
 * what "does not parse" means in a language that cannot ask the server.
 */
export function looksLikeFilter(s: string): boolean {
  if (s === '') return false;
  try {
    const parsed: unknown = JSON.parse(s);
    // A filter is an OBJECT keyed by a variant name. A bare array, a number, or
    // `"cat"` is not one, and `Filter::from_url` would reject it.
    return typeof parsed === 'object' && parsed !== null && !Array.isArray(parsed);
  } catch {
    return false;
  }
}
