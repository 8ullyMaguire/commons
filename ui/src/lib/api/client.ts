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
