/**
 * Share links, as the UI sees them. Spec §9.5 (#5612), plan T-P5-007 part 2B.
 *
 * # Why the client does not decide anything
 *
 * The server decides whether a link works — signature, then revocation, then
 * expiry, then password, in that order — and this file turns the answer into a
 * state the view can switch on. A client that re-derived any of it would be a
 * second implementation of the policy, and the two would disagree exactly when
 * it mattered: on a revoked link that has also expired.
 *
 * So `ShareDenial` is the server's `DeniedReason` and nothing is guessed. In
 * particular there is no 'not-found' state, because the server does not send
 * one: every denial is a 403, and a 403 that means "this link expired" is
 * something the recipient must be told so they can go and ask for a new one.
 *
 * # Why the token is not re-readable
 *
 * `createShare` returns the URL and `listShares` does not. That is not an
 * omission in the client; the server stores only a hash (migration 0022), so
 * there is nothing to send back. A UI that showed a "copy link" button on a row
 * from the list would be showing a button that cannot work, and the user would
 * learn that by clicking it.
 */

/**
 * The two closed sets the server defines.
 *
 * Duplicated as literal unions rather than imported from a shared types module
 * because they are a *contract with the server's enums*, and the test that
 * matters is the one that fails when the server's spelling changes. A shared
 * `string` type would make both sides compile while the values disagreed.
 */
export type Scope = 'view' | 'view_download';
export type TargetKind = 'object' | 'smart_collection';

import { deleteShare, fetchShare, fetchShares, postShare } from './client.js';

/** Why a link did not open. Mirrors Rust's `DeniedReason`. */
export type ShareDenial =
  | 'unknown_token'
  | 'expired'
  | 'revoked'
  | 'password_required'
  | 'wrong_password';

/** A denial, with the sentence the recipient should read. */
export interface ShareRefusal {
  readonly kind: 'refused';
  readonly reason: ShareDenial;
  /**
   * What to say to the person holding the link.
   *
   * `expired` and `revoked` are told apart deliberately, even though both stop
   * the link working: "this link has expired, ask for a new one" sends the
   * recipient to their sender with something actionable, and "this link is not
   * available" sends them nowhere. A revocation is still worth saying plainly
   * — somebody deliberately turned this link off, and telling the recipient so
   * is more honest than implying it merely aged out.
   */
  readonly message: string;
}

/** A link that opened. */
export interface ShareOpened {
  readonly kind: 'opened';
  readonly targetKind: TargetKind;
  readonly targetId: string;
  /** Whether the bytes are fetchable with this token. */
  readonly canDownload: boolean;
  /** Always false on an open: `password_required` is a refusal, not a flag. */
  readonly needsPassword: boolean;
}

/** The result of opening a link. */
export type ShareResult = ShareOpened | ShareRefusal;

/** One of the owner's links, as `GET /api/share` sends it. */
export interface ShareListItem {
  readonly id: string;
  readonly target_kind: TargetKind;
  readonly target_id: string;
  readonly scope: Scope;
  readonly expires_at: string | null;
  readonly revoked_at: string | null;
  readonly created_at: string | null;
  readonly access_count: number;
  readonly last_accessed_at: string | null;
  /** `live`, `expired` or `revoked`, decided by the server. */
  readonly state: 'live' | 'expired' | 'revoked';
}

/** What creating a link returns. The URL appears here and nowhere else. */
export interface CreatedShare {
  readonly id: string;
  readonly url: string;
  readonly expires_at: string;
}

/** What to create. */
export interface CreateShareInput {
  readonly targetKind: TargetKind;
  readonly targetId: string;
  readonly scope: Scope;
  /** Hours. The server clamps this and refuses anything outside 1..2160. */
  readonly expiresInHours?: number;
  /** Omit for no password. An empty string is refused by the server. */
  readonly password?: string;
}

const DENIAL_MESSAGE: Record<ShareDenial, string> = {
  unknown_token: 'This link is not valid. Check that you copied all of it.',
  expired: 'This link has expired. Ask whoever sent it for a new one.',
  revoked: 'This link was turned off by whoever shared it.',
  password_required: 'This link is password-protected.',
  wrong_password: 'That password is not right.'
};

/**
 * Turn a 403 body into a refusal.
 *
 * A reason the client does not recognise still produces a refusal — with the
 * unknown-token sentence, which is the safe one, since it claims less. A server
 * that grew a new denial reason must not make an old client show a blank page
 * or, worse, a success.
 */
export function refusalFrom(reason: string): ShareRefusal {
  const known = (Object.keys(DENIAL_MESSAGE) as ShareDenial[]).includes(
    reason as ShareDenial
  );
  const safe: ShareDenial = known ? (reason as ShareDenial) : 'unknown_token';
  return { kind: 'refused', reason: safe, message: DENIAL_MESSAGE[safe] };
}

/**
 * The word to show for a link's state.
 *
 * `Live`/`Expired`/`Turned off`, and `revoked` is deliberately not "Expired":
 * the owner needs to tell those apart to know whether the link can be replaced
 * or whether the sender turned it off.
 *
 * The default arm is for a state a client from before this list existed might
 * be handed. Returning a string rather than throwing is the point — one row with
 * a state this build does not know must not take the whole management page
 * down.
 */
export function stateLabel(state: string): string {
  switch (state) {
    case 'live':
      return 'Active';
    case 'expired':
      return 'Expired';
    case 'revoked':
      return 'Turned off';
    default:
      return 'Unknown';
  }
}

/**
 * The wire verbs live in `client.ts`, which is the only module allowed to call
 * `fetch` — see `tests/invariants.test.ts`. This file classifies their answers
 * and holds no transport of its own.
 */

export async function openShare(
  token: string,
  signal?: AbortSignal
): Promise<ShareResult> {
  const { status, body } = await fetchShare(token, signal);
  if (status === 403) {
    return refusalFrom(typeof body.error === 'string' ? body.error : 'unknown_token');
  }
  return {
    kind: 'opened',
    targetKind: body.target_kind as TargetKind,
    targetId: String(body.target_id ?? ''),
    canDownload: body.can_download === true,
    needsPassword: body.needs_password === true
  };
}

/** The owner's links, dead ones included. */
export async function listShares(signal?: AbortSignal): Promise<ShareListItem[]> {
  return (await fetchShares(signal)) as ShareListItem[];
}

/**
 * Create a link.
 *
 * The returned URL is the only copy that will ever exist: the server keeps a
 * hash. A caller that discards it has made a link nobody can reach, so the UI
 * shows it once and says so.
 */
export async function createShare(
  input: CreateShareInput,
  signal?: AbortSignal
): Promise<CreatedShare> {
  const body: Record<string, unknown> = {
    target_kind: input.targetKind,
    target_id: input.targetId,
    scope: input.scope
  };
  if (input.expiresInHours !== undefined) body.expires_in_hours = input.expiresInHours;
  if (input.password !== undefined) body.password = input.password;
  const created = await postShare(body, signal);
  return created.body as unknown as CreatedShare;
}

/** Turn a link off. Idempotent: a second call succeeds. */
export async function revokeShare(id: string, signal?: AbortSignal): Promise<void> {
  await deleteShare(id, signal);
}
