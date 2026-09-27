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
