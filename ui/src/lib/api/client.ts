/**
 * The single GraphQL client. Every request the UI makes goes through `query`.
 *
 * # Why one module and not a generated client
 *
 * `commons-api` is still an empty placeholder crate, so there is no schema to
 * generate a typed client from yet. When there is, the fix is to generate the
 * TYPES here and keep this module as the transport. What must not happen is a
 * second transport: spec 3.3 says the desktop shell is a browser pointed at a
 * host, so if the UI grows a private REST side-channel for the shell, the shell
 * and the browser become two clients with two auth stories and two cache
 * stories, and the "no second private API" rule is gone.
 *
 * The invariant is testable, so it is tested: `no-direct-fetch.test.ts` greps
 * the whole source tree for `fetch(` outside this file.
 *
 * # Keyset cursors, and why `offset` is not a parameter
 *
 * Spec rule 2 (stash#6455, #6390): never OFFSET in a paginated query. OFFSET
 * makes the database walk and discard every skipped row, so page 400 of a
 * 100k library is 100k rows of work, and worse, an item inserted above the
 * cursor shifts every subsequent page and the user sees a duplicate. A keyset
 * cursor says "the last row I saw was (sort, id)" and the next page starts
 * there, which is an index seek regardless of depth.
 *
 * That is why `PageInput` has no `offset` field. It is a type-level guarantee,
 * which is stronger than a convention: a component that wants to skip to page
 * 400 cannot express it.
 */

/** The endpoint, relative, so the dev proxy and the Tauri shell share it. */
export const ENDPOINT = '/graphql';

/** A page request. Note the absence of `offset` — see the module comment. */
export interface PageInput {
  /** Rows per page. Bounded server-side; the client clamps anyway. */
  readonly first: number;
  /**
   * Opaque cursor from a previous page's `pageInfo.endCursor`, or null for
   * the first page. Opaque because the server is free to encode a keyset
   * however it likes; the client must not parse it, and must never construct
   * one by hand.
   */
  readonly after: string | null;
  /** The serialized filter, as a JSON string (spec 5.16: filters live in the URL). */
  readonly filter?: string | null;
  /** Sort field, e.g. `date`, `title`, `rating`. */
  readonly sort?: string | null;
  /** Sort direction. */
  readonly direction?: 'ASC' | 'DESC' | null;
  /** The caller's consent tiers (spec 14.1). Absent means "whatever the caller may see". */
  readonly tiers?: readonly string[] | null;
}

/** Relay-shaped page info. The client uses `hasNextPage` and nothing else. */
export interface PageInfo {
  readonly hasNextPage: boolean;
  readonly hasPreviousPage: boolean;
  readonly startCursor: string | null;
  readonly endCursor: string | null;
}

/** A connection: nodes plus page info. */
export interface Connection<T> {
  readonly nodes: readonly T[];
  readonly pageInfo: PageInfo;
  /** Total count when the server can answer it cheaply. `null` when it cannot. */
  readonly totalCount: number | null;
}

/** An error from the server, in GraphQL's own shape. */
export class GraphQLError extends Error {
  constructor(
    message: string,
    readonly extensions?: Record<string, unknown>
  ) {
    super(message);
    this.name = 'GraphQLError';
  }
}

/**
 * A transport. Injected so tests can run without a server, and so the desktop
 * shell can supply a different one without the components knowing.
 */
export type Transport = (body: string, signal?: AbortSignal) => Promise<Response>;

const defaultTransport: Transport = (body, signal) =>
  fetch(ENDPOINT, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body,
    signal
  });

let transport: Transport = defaultTransport;

/** Replace the transport. Tests only; returns a function that restores it. */
export function setTransport(t: Transport): () => void {
  const prev = transport;
  transport = t;
  return () => {
    transport = prev;
  };
}

/**
 * The one request function. Everything else in the UI calls this.
 *
 * GraphQL returns 200 with an `errors` array for a query that partially
 * failed, so checking `res.ok` is not enough: a 200 carrying errors has to
 * throw, or every caller needs its own error check and half of them will
 * forget. The FIRST error is thrown with its `path`, which is what makes a
 * failure debuggable — "cannot read property title of undefined" is not a
 * diagnosis, "objects.title: expected a string" is.
 */
export async function query<T>(
  document: string,
  variables: Record<string, unknown> = {},
  signal?: AbortSignal
): Promise<T> {
  let res: Response;
  try {
    res = await transport(JSON.stringify({ query: document, variables }), signal);
  } catch (e) {
    // A network failure is not a GraphQL error and must say so, because the
    // two need different responses: one is "the server said no", the other is
    // "the server is not there".
    throw new GraphQLError(
      e instanceof Error ? `network: ${e.message}` : 'network: unknown failure'
    );
  }

  if (!res.ok) {
    throw new GraphQLError(`http ${res.status} ${res.statusText}`.trim());
  }

  const body = (await res.json()) as {
    data?: T;
    errors?: { message: string; path?: readonly (string | number)[]; extensions?: Record<string, unknown> }[];
  };

  if (body.errors?.length) {
    const first = body.errors[0];
    const where = first.path?.length ? ` at ${first.path.join('.')}` : '';
    throw new GraphQLError(`${first.message}${where}`, first.extensions);
  }
  if (body.data === undefined || body.data === null) {
    throw new GraphQLError('the server returned neither data nor errors');
  }
  return body.data;
}

/**
 * The objects query. One document, used by every browse surface.
 *
 * `first` and `after` are the only pagination inputs, and the selection set
 * is deliberately small: a virtualized grid renders ~30 rows at a time out of
 * 100k, and every field in this selection set is paid for 30 times per window
 * change. Detail fields belong to a separate query, fetched when a row is
 * opened, precisely so the grid query stays cheap.
 */
export const OBJECTS_QUERY = /* GraphQL */ `
  query Objects($input: PageInput!) {
    objects(input: $input) {
      totalCount
      pageInfo {
        hasNextPage
        hasPreviousPage
        startCursor
        endCursor
      }
      nodes {
        id
        kind
        title
        date
        rating
        organized
        coverPath
        width
        height
        durationMs
        producer
        performers
        tags
        folder
      }
    }
  }
`;

/** The row shape the grid needs. Mirrors the `nodes` selection above. */
export interface ObjectRow {
  readonly id: string;
  readonly kind: string;
  readonly title: string | null;
  readonly date: string | null;
  readonly rating: number | null;
  readonly organized: string | null;
  readonly coverPath: string | null;
  readonly width: number | null;
  readonly height: number | null;
  readonly durationMs: number | null;
  // The grouping fields, for the wall's group-by (#6544). A `null` here is
  // meaningful and is not normalised away: a row with no studio is grouped
  // under nothing, and "Unknown" as a section title is the thing a user has to
  // click through to find the three rows they wanted.
  readonly producer: string | null;
  readonly performers: readonly string[];
  readonly tags: readonly string[];
  readonly folder: string | null;
}

export interface ObjectsResult {
  readonly objects: Connection<ObjectRow>;
}

/** Fetch one page of objects. */
export function fetchObjects(
  input: PageInput,
  signal?: AbortSignal
): Promise<ObjectsResult> {
  return query<ObjectsResult>(OBJECTS_QUERY, { input }, signal);
}

// ---------------------------------------------------------------------------
// Bulk writes (T-P5-006 item 3)
// ---------------------------------------------------------------------------

/**
 * The bulk-tag mutation.
 *
 * A mutation and not a REST endpoint, and the reason is the invariant this file
 * exists to hold: `tests/invariants.test.ts` asserts that no module outside the
 * transport names `fetch`, because a second HTTP path is a second place for a
 * base URL, a header, an auth token and an error shape to disagree. A route
 * that grew its own `fetch('/api/bulk/tag')` passed every functional test and
 * failed that one -- correctly.
 *
 * The `target` is a GraphQL input object rather than a loose pair of `ids` and
 * `filter` arguments, so "neither was given" and "both were given" are states
 * the server rejects by schema rather than states it has to notice at runtime.
 */
const BULK_APPLY_TAG = `
  mutation BulkApplyTag($tagId: ID!, $target: BulkTarget!, $source: String) {
    bulkApplyTag(tagId: $tagId, target: $target, source: $source) {
      applied
      skippedInvisible
      requested
    }
  }
`;

/**
 * The target, as the schema takes it.
 *
 * `kind` is required and named rather than inferred from which field is set,
 * because `ids: []` and "no ids given" are different things and an
 * inferred-from-presence schema cannot tell them apart. `ids: []` is the
 * dangerous one: it is a request to tag nothing, and a server that treated it
 * as an absent filter would tag the whole library.
 */
export type BulkTarget =
  | { kind: 'IDS'; ids: readonly string[]; excluded?: readonly string[] }
  | { kind: 'QUERY'; query: string; excluded?: readonly string[] };

/** What the server did, mirroring Rust's `BulkOutcome`. */
export interface BulkApplyTagResult {
  readonly bulkApplyTag: {
    readonly applied: number;
    readonly skippedInvisible: number;
    readonly requested: number;
  };
}

/**
 * Apply a tag to everything in `target` the caller can see.
 *
 * `query` is the serialized filter string, matching `PageInput.filter` -- the
 * same serialization `view.ts` already produces and puts in the URL, so a
 * select-all over the current view is a select-all over exactly the rows the
 * user is looking at and no others.
 */
export function bulkApplyTag(
  tagId: string,
  target: BulkTarget,
  source = 'bulk-edit'
): Promise<BulkApplyTagResult> {
  return query<BulkApplyTagResult>(BULK_APPLY_TAG, { tagId, target, source });
}

/** The tags a user may apply, for a bulk edit's target list. */
const TAGS_QUERY = `
  query BulkTags {
    tags {
      id
      name
    }
  }
`;

export interface TagsResult {
  readonly tags: readonly { id: string; name: string }[];
}

/**
 * Every tag, for a bulk edit.
 *
 * Loaded with the page rather than when the modal opens: a modal that opens
 * empty and fills in a moment later is a modal whose confirm button flickers
 * from disabled to enabled while the user is reading the scope line above it.
 */
export function fetchTags(signal?: AbortSignal): Promise<TagsResult> {
  return query<TagsResult>(TAGS_QUERY, {}, signal);
}

// ---------------------------------------------------------------------------
// The create actions (T-P5-006 item 10, spec 10.10)
// ---------------------------------------------------------------------------

/**
 * Create every row that is not already in the library, and report both counts.
 *
 * The response asks for `created` AND `existing` because the action exists
 * *because* "already there" is a common outcome, and a response carrying only a
 * success count cannot tell a user that 35 of their 40 pasted rows were already
 * in their library. `ui/src/lib/api/create.ts` renders the two separately, and
 * it cannot do that if the server does not send them.
 */
const CREATE_ALL_MISSING = `
  mutation CreateAllMissing($rows: [String!]!) {
    createAllMissing(rows: $rows) {
      created
      existing
      refused
    }
  }
`;

export interface CreateAllMissingResult {
  readonly createAllMissing: {
    readonly created: number;
    readonly existing: number;
    readonly refused: number;
  };
}

/**
 * Create the rows that are missing, skipping the ones already in the library.
 *
 * Idempotent: a second call with the same rows creates nothing and reports them
 * all as `existing`. The caller does not have to know which they were.
 *
 * `rows` are titles, not ids, and deliberately so — the id is derived from the
 * row's content on the server, so a client that computed it would have a second
 * implementation of the derivation to keep in step. A title containing a quote
 * or a newline is data, and the transport's escaping is the transport's job.
 */
export function createAllMissing(rows: readonly string[]): Promise<CreateAllMissingResult> {
  return query<CreateAllMissingResult>(CREATE_ALL_MISSING, { rows });
}

// ---------------------------------------------------------------------------
// Playback state (T-P6-001)
// ---------------------------------------------------------------------------

/**
 * A file's saved position, as the server holds it.
 *
 * Both loop markers are nullable *independently*, and that is the shape the
 * database has: a half-set loop is a real state a user can leave behind by
 * setting marker A and not marker B, and collapsing it to "no loop" would make
 * the UI claim marker A does not exist.
 */
export interface PlaybackState {
  readonly position_ms: number;
  readonly duration_ms: number | null;
  readonly loop_a_ms: number | null;
  readonly loop_b_ms: number | null;
  readonly completed: boolean;
  readonly updated_at: string;
}

function mediaPath(objectId: string, suffix = ''): string {
  return `/media/${encodeURIComponent(objectId)}${suffix}`;
}

/**
 * What the server knows about a file's shape and playability.
 *
 * `rung` is the load-bearing field: null means a browser plays this directly,
 * a number is the proxy height to ask for. The client cannot work this out
 * itself -- `ObjectRow` has no container, no codecs and no frame rate, and the
 * database has no columns for them -- so a client that guessed would guess
 * "proxied" every time and pay a transcode per file. The same failure as the
 * `format_name` bug in the ladder, one layer up.
 */
export interface MediaCaps {
  readonly container: string;
  readonly video_codec: string;
  /** `""` for a silent file, which is NOT the same as a missing field. */
  readonly audio_codec: string;
  readonly width: number | null;
  readonly height: number | null;
  readonly fps: number | null;
  /** Display-matrix rotation, 0/90/180/270. */
  readonly rotation: number | null;
  readonly duration_ms: number | null;
  /** The proxy height this file needs, or null when it needs none. */
  readonly rung: number | null;
}

/**
 * Ask what a file is.
 *
 * A 404 becomes null, like `fetchPlayback`: "no such file, or none you may see"
 * is an answer the caller already has to handle, and a thrown error here would
 * make "the file is gone" and "the server is down" the same message to a user.
 * A 422 means ffprobe could not read the file, which is a *different* failure
 * and does throw -- the caller says "this file cannot be read" rather than
 * playing nothing.
 */
export async function fetchMediaCaps(
  objectId: string,
  signal?: AbortSignal
): Promise<MediaCaps | null> {
  const res = await fetch(mediaPath(objectId, '/caps'), { signal });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error(`caps: ${res.status}`);
  return (await res.json()) as MediaCaps;
}

/**
 * One track, as `GET /media/:id/subtitles` sends it.
 *
 * `language` is `string | null` and the null is load-bearing: `null` is a file
 * that declares no language, and `""` is a file that says it has none. The
 * server refuses to coalesce them, so a client that maps null to `""` shows
 * "Unknown" for a track the extractor never saw a tag for and "None" for one the
 * tagger deliberately cleared.
 *
 * The id is the whole of the VTT URL, which is why this type has no `url`
 * field: composing one in two places is how a route change breaks silently.
 */
export interface SubtitleTrack {
  readonly id: string;
  readonly label: string;
  readonly language: string | null;
  readonly is_default: boolean;
  readonly is_forced: boolean;
  readonly is_hearing_impaired: boolean;
  /** 0 is possible and means "a real, empty track", not "unknown". */
  readonly cue_count: number;
  /**
   * `subrip` / `webvtt` / `ass` / `mov_text`, as the store names it.
   *
   * A browser renders ASS as plain text and drops the styling silently, so this
   * is what lets the player say so rather than showing a track that looks
   * merely plain.
   */
  readonly format: string;
}

export interface FunscriptSummary {
  id: string;
  path: string;
  axis_count: number;
  metadata?: FunscriptMetadata | null;
}

export interface FunscriptMetadata {
  title?: string | null;
  author?: string | null;
  version?: string | null;
  source?: string | null;
}

export interface FunscriptActionWire {
  at_ms: number;
  position: number;
}

export interface FunscriptAxisWire {
  name: string;
  actions: FunscriptActionWire[];
}

export interface FunscriptTimeline {
  axes: FunscriptAxisWire[];
  source: string;
  warnings: string[];
  span_ms: number;
}

export interface SubtitleTrackList {
  readonly tracks: readonly SubtitleTrack[];
}

/**
 * The subtitle tracks the server holds for one object.
 *
 * A 404 is `null`, exactly as for `fetchMediaCaps` and `fetchPlayback`, and for
 * the same reason: absent, not-on-disk and denied all mean "this object is not
 * available to you", and a client cannot tell them apart -- nor should it try.
 * Every other failure throws, because a server that is down is worth telling a
 * user about and a file with no subtitles is not.
 *
 * The empty case is the common one, and it is a 200 with `tracks: []` rather
 * than a 404: a library of files with no subtitles is a working library.
 */
export async function fetchSubtitleTracks(
  objectId: string,
  signal?: AbortSignal
): Promise<SubtitleTrackList | null> {
  const res = await fetch(mediaPath(objectId, '/subtitles'), { signal });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error(`subtitles: ${res.status}`);
  return (await res.json()) as SubtitleTrackList;
}

/**
 * The funscripts recorded for an object, or null when it has none.
 *
 * A 404 is `null` for the same reason it is for subtitles: absent and denied
 * answer identically (the server folds them together deliberately), and a
 * player must not be able to tell them apart.
 */
export async function fetchFunscripts(
  objectId: string,
  signal?: AbortSignal
): Promise<FunscriptSummary[] | null> {
  const res = await fetch(mediaPath(objectId, '/funscripts'), { signal });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error(`funscripts: ${res.status}`);
  return (await res.json()) as FunscriptSummary[];
}

/**
 * One script's timeline: every axis, every action.
 *
 * `interpolation` and `durationMs` are query parameters rather than client-side
 * options on purpose. The server already has the parsed script and already
 * knows how to read the space between two actions, and a client that
 * re-interpolated would be a second implementation of the same rule that could
 * disagree with the first at an action -- the one place where the two answers
 * are required to be identical.
 *
 * A 422 is thrown rather than returned: the file is there and will not parse,
 * which is a different thing from there being no script, and the detail says
 * why.
 */
export async function fetchFunscriptTimeline(
  objectId: string,
  funscriptId: string,
  opts: { interpolation?: Interpolation; durationMs?: number; signal?: AbortSignal } = {}
): Promise<FunscriptTimeline> {
  const q = new URLSearchParams();
  if (opts.interpolation) q.set('interpolation', opts.interpolation);
  // Absent rather than zero: the server reads a zero duration as "not asked
  // for" and clamping to it would erase every action.
  if (opts.durationMs) q.set('duration_ms', String(opts.durationMs));
  const qs = q.toString();
  const res = await fetch(
    mediaPath(objectId, `/funscripts/${funscriptId}`) + (qs ? `?${qs}` : ''),
    { signal: opts.signal }
  );
  if (!res.ok) throw new Error(`funscript: ${res.status}`);
  return (await res.json()) as FunscriptTimeline;
}

/**
 * Read a file's saved position.
 *
 * A file with no saved state is not an error: it is a file nobody has played,
 * which is the normal case for most of a library. So a 404 becomes `null` and
 * every other failure throws -- "this file has never been played" and "the
 * server is down" need different responses from a player, and only the second
 * is worth telling a user about.
 */
export async function fetchPlayback(
  objectId: string,
  signal?: AbortSignal
): Promise<PlaybackState | null> {
  const res = await fetch(mediaPath(objectId, '/playback'), { signal });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error(`playback: ${res.status}`);
  return (await res.json()) as PlaybackState;
}

/**
 * Save a position.
 *
 * `keepalive` is set because the most important call is the one made as the page
 * goes away, and a `fetch` cancelled by navigation is a position lost -- which
 * is the whole reason the endpoint exists. One header on every call is cheaper
 * than a second code path for the one call that matters most.
 *
 * The loop markers are sent as null rather than 0 when unset. The server stores
 * NULL for "not set"; sending 0 would be a real position at the start of the
 * file, and a loop whose A is 0 is one the server's CHECK rejects as not
 * ordered.
 */
export function savePlayback(
  objectId: string,
  state: {
    position_ms: number;
    duration_ms: number | null;
    loop_a_ms: number | null;
    loop_b_ms: number | null;
  }
): Promise<void> {
  return fetch(mediaPath(objectId, '/playback'), {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(state),
    keepalive: true
  }).then((res) => {
    if (!res.ok) throw new Error(`playback: ${res.status}`);
  });
}
