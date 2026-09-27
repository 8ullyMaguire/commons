# T‑P5-007 (part 2) — Time-limited share links, and player position in the URL

**Spec:** §15.10 (deep links, C90), §9.5 (streaming surface, time-limited
share links, #5612)
**Depends on:** T‑P5-007 part 1 (theming, focus, popovers) — done.

## Why this is a separate part

Part 1 built the theme tokens, focus management and viewport-constrained
popovers, and fixed the deep-link bug the ticket's own Accept criterion found
(a view did not survive a reload). Two items on the ticket's list were still
not built when that was tagged, and an audit of the codebase confirmed both
rather than assuming them:

* **Time-limited share links (#5612)** — no share feature exists. Every
  occurrence of the word "share" in `ui/src` and `crates/` is the word in a
  comment. There is no token, no grant, no expiry, no access log.
* **Player position in the URL** — `/play` reads exactly one query parameter,
  `o` (the object id). `page.url.searchParams.get('o')` is the only URL read
  in the route.

So this part builds both. The first is a real feature with a database table
and an HTTP surface; the second is small and is done first because it is the
same shape as the work part 1 already did.

## Part A — Player position in the URL

**Spec:** §15.10 — "Every view state is a resolvable URL: filters, sorts,
view mode, tagger selection, **player position**."

**Shape.** `/play?o=<id>&t=<seconds>`. The position is written as a
`timeupdate` is throttled (once per ~2 s, and only when it moves by ≥1 s), and
read on load as a seek. Two properties matter and both are testable:

1. **A reload resumes where you were.** The Accept criterion for the whole
   ticket is that a view survives a reload, and a video's position is a view
   state by the ticket's own sentence.
2. **The URL does not thrash.** A `history.replaceState` per frame is a
   history entry per frame and makes the back button useless — the exact
   failure the lightbox's `popstate` handling exists to avoid. So position
   updates `replace`, never `push`, and are throttled.

**Why seconds and not a frame or a percent.** Seconds are what a person
quotes ("send me the part at 4:12"), they survive a re-encode, and they are
what the spec's sibling items (filter, sort) already are — plain readable
values in a readable URL. A frame number is meaningless after a transcode and a
percentage is meaningless without a duration.

**Files**

| File | What |
|---|---|
| `ui/src/lib/player/position.ts` | The throttle and the read/write, pure |
| `ui/tests/player-position.test.ts` | Table tests for the throttle |
| `ui/src/routes/play/+page.svelte` | Wire the throttled writer |
| `ui/e2e/deeplink.spec.ts` | Reload resumes at the URL's position |

**Accept**

* `ui/tests/player-position.test.ts` — writing the same position twice within
  the interval emits once; a jump of ≥1 s emits; `replace` and never `push`.
* `ui/e2e/deeplink.spec.ts` — open `/play?o=id&t=42`, let it play, reload, and
  assert `currentTime` is ≥ 42. (The media stub needs `accept-ranges: bytes`
  before a seek can be asserted at all — see the player lessons.)

## Part B — Time-limited share links

**Spec:** §9.5 — "a signed, expiring, optionally password-protected URL grants
exactly one capability — view, or view-and-download — on one item or one smart
collection, revocable at any time, with an access log. It is a capability
grant, not a second account system."

### The model

A **grant** is a row. A **token** is the URL-safe string that references it.

```
share_grants(
  id            uuid primary key,
  token_hash    bytea not null unique,   -- BLAKE3 of the token, never the token
  scope         text not null,           -- 'view' | 'view_download'
  target_kind   text not null,           -- 'object' | 'smart_collection'
  target_id     uuid not null,
  password_hash bytea,                   -- null = no password
  expires_at    timestamptz not null,
  revoked_at    timestamptz,
  created_at    timestamptz not null,
  access_count  bigint not null default 0,
  last_accessed_at timestamptz
)
```

**The token is stored hashed, not stored.** A database dump of `share_grants`
must not yield working links. That is the whole difference between a capability
grant and a bearer token in a table, and it is a one-line decision that is very
expensive to reverse.

**Signing.** The token is `<id>.<secret>`; the secret is 32 random bytes,
base64url. Verification recomputes `BLAKE3(token)` and compares, then checks
`expires_at > now()` and `revoked_at IS NULL` in that order. The expiry check
is on the *row*, not in the token, because revocation has to be immediate —
an expiry baked into a signed token cannot be revoked before it expires without
rotating the signing key.

**Why a row and not a pure self-contained token.** A self-contained signed
token (HMAC over the payload) needs no table and needs no revocation. The spec
asks for *revocable at any time* and *with an access log*, and both of those
are rows. So the table is the design, and the signature is defence in depth:
the hash means a leaked row is not a leaked link, and the signature means a
forged id is rejected before the row is even read.

**Password.** Optional, and verified against `password_hash` with the same
BLAKE3-keyed construction. Not a real KDF — deliberately: this is a capability
grant with an expiry measured in hours, not a user credential with a lifetime
measured in years, and a slow KDF on a path a share link hits is a slow KDF a
share recipient waits on. Stated here so the choice is a decision rather than
an oversight.

### The access log

`share_access(id, grant_id, at, ip, user_agent, granted, denied_reason)`.
Written on **every** attempt, granted or denied. A log of only successes is
useless for the one question it gets asked — "is this link being tried by
someone who should not have it?" — because the answer is in the failures.

### The surface

| Route | What |
|---|---|
| `POST /api/share` | Create a grant. Auth: an owner. Returns the URL once. |
| `GET /api/share` | List the caller's grants. |
| `DELETE /api/share/:id` | Revoke. Immediate. |
| `GET /s/:token` | The page a recipient opens. |
| `GET /api/s/:token` | Resolve the token → target, enforcing scope + expiry. |
| `GET /api/s/:token/access` | The bytes, if scope is `view_download`. |
| `POST /api/s/:token/unlock` | Exchange the password for a session cookie. |

The token appears **once**, in the create response. There is no endpoint that
returns it again, because there is nothing to return it from.

### Files

| File | What |
|---|---|
| `crates/commons-consent/src/share.rs` | The model, signing, verification |
| `crates/commons-consent/tests/share.rs` | Model tests |
| `crates/commons-store/src/share.rs` | Queries |
| `crates/commons-store/migrations/NNN_share_grants.sql` | The two tables |
| `crates/commons-server/src/share.rs` | The routes |
| `ui/src/routes/s/[token]/+page.svelte` | The recipient's page |
| `ui/src/routes/s/+page.svelte` | The owner's management page |

**Accept**

* A token verifies, a wrong secret does not, an expired grant does not, a
  revoked grant does not — four tests that are the feature.
* A leaked `share_grants` row yields no working link (the hash test).
* The access log records a denied attempt with its reason.

## Order

Part A first: it is small, it is the same shape as work already done, and it
completes the sentence in §15.10. Part B after, because it is a new table, a
new HTTP surface and a new page, and a new table in a 72-migration chain is
worth doing on its own with a clean milestone boundary.
