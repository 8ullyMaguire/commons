-- Migration: 0022 share grants
--
-- §9.5 (streaming surface, time-limited share links, #5612) and §15.10 (deep
-- links). Spec: docs/spec/t-p5-007-share-links.md. Module:
-- crates/commons-consent/src/share.rs.
--
-- # What a grant is
--
-- A capability, not an account. §9.5 asks for "a signed, expiring, optionally
-- password-protected URL [that] grants exactly one capability -- view, or
-- view-and-download -- on one item or one smart collection, revocable at any
-- time, with an access log."
--
-- Four properties fall out of that sentence and each is a column below:
--
--   * *exactly one capability* -- `scope`, and a CHECK, because a capability
--     that can be widened by a typo is not a capability.
--   * *one item or one smart collection* -- `target_kind` + `target_id`. One
--     target, never a filter: §9.5 says "one item", and a filter target is
--     explicitly unbounded at request time, which is the same unbounded-ness
--     that makes `undo_record` hard to replay.
--   * *expiring* -- `expires_at`, NOT NULL, so a grant that forgot to expire
--     is unrepresentable rather than merely unlikely.
--   * *revocable at any time* -- `revoked_at`. Nullability is the state, the
--     same rule as `undo_record.undone_at` and `playback_state`: "not revoked"
--     and "revoked" are different states and the difference is the whole
--     feature.
--
-- # Why the token is stored HASHED
--
-- The one decision in this migration that is expensive to reverse.
--
-- `token_hash` is BLAKE3 of the token, and the token itself exists only in the
-- create response. A database dump, a backup, a log line, or an `EXPLAIN` of
-- the wrong query then yields no working links. Storing the token would make
-- every copy of the database a copy of every link ever issued, with an expiry
-- that does not revoke the copy.
--
-- The cost is that a lost token cannot be recovered, only reissued. That is the
-- intended trade: the owner can revoke and reissue, and nobody can recover a
-- link from a backup.
--
-- # Why there is a row at all, rather than a self-contained signed token
--
-- An HMAC over `(id, expiry, scope)` needs no table, and is the smaller design.
-- It is the wrong one, because the spec asks for *revocable at any time* and
-- *with an access log*, and both are rows. An expiry baked into a signed token
-- cannot be revoked before it arrives without rotating the signing key for
-- every other link in existence. So the table is the design, and the signature
-- is defence in depth: the hash means a leaked row is not a leaked link, and
-- the signature means a forged id is rejected before the row is read at all.
--
-- # Why `password_hash` is not a real KDF
--
-- Stated so it reads as a decision. §9.5 calls this an optional password on a
-- capability with an expiry measured in hours, not a user credential with a
-- lifetime measured in years. The cost of a slow KDF lands on the share
-- recipient, who is waiting on a page load, and the threat it defends against
-- -- someone who already has the link and wants to stop the recipient reading
-- the file -- is not the threat a slow KDF is for. If this ever becomes a
-- credential, this column is the thing that has to change.
--
-- # The access log is a separate table
--
-- Because a log of only successes cannot answer the one question it gets asked
-- -- "is this link being tried by someone who should not have it?" -- and that
-- question lives entirely in the failures. So every attempt is recorded, with
-- `granted` and, when it is not, why.

CREATE TABLE share_grant (
    id            TEXT PRIMARY KEY,
    -- BLAKE3 of the token. See the header: never the token itself.
    token_hash    BLOB NOT NULL UNIQUE,
    -- 'view' or 'view_download'. A CHECK rather than a convention, because
    -- 'download' read as 'view_download' is the failure this feature must not
    -- have, and a convention is not a constraint.
    scope         TEXT NOT NULL,
    -- 'object' or 'smart_collection'.
    target_kind   TEXT NOT NULL,
    target_id     TEXT NOT NULL,
    -- NULL means no password. NOT hashed with a KDF; see the header.
    password_hash BLOB,
    expires_at    TEXT NOT NULL,
    -- NULL = live. The state, not a boolean: see the header.
    revoked_at    TEXT,
    created_at    TEXT NOT NULL,
    -- Denormalised counters rather than a count over share_access. Two
    -- reasons: the owner's list view sorts by "most used", which would
    -- otherwise be a join over the log for every row shown; and a count that is
    -- wrong by one because the log write failed is better than a list view
    -- that fails. `last_accessed_at` is the same fact for display.
    access_count  INTEGER NOT NULL DEFAULT 0,
    last_accessed_at TEXT,
    CONSTRAINT share_scope_valid CHECK (scope IN ('view', 'view_download')),
    CONSTRAINT share_target_kind_valid
        CHECK (target_kind IN ('object', 'smart_collection')),
    -- A grant that expires at or before it was created is a bug, and a CHECK is
    -- where it is caught once instead of at every read site.
    CONSTRAINT share_expiry_after_creation CHECK (expires_at > created_at)
);

-- The lookup every request makes: BLAKE3(token) -> row. Covered by the UNIQUE
-- on `token_hash`, so this index is the one that serves it and the constraint
-- is free rather than a second structure over the same column.
--
-- Deliberately NOT indexed on `target_id`: there is no "everything shared for
-- this object" query in this ticket, and share grants are low-cardinality
-- compared with the media table. An index for a query nobody runs is a write
-- cost on every grant created, which is rare -- but the read path above is hit
-- by every request, so that is the one worth the space.
--
-- `revoked_at IS NULL` is applied after the token hash has already narrowed to
-- exactly one row, so it needs no index of its own. Indexing a boolean that is
-- NULL for every live row stores nothing useful.
CREATE INDEX share_grant_target ON share_grant(target_kind, target_id);

-- Every attempt, granted or denied.
--
-- `grant_id` is deliberately NOT a foreign key. A log row that outlived its
-- grant is the correct record: the grant is deleted when the library is
-- deleted, and "we deleted the link and the log of who used it" is a worse
-- outcome than a dangling id. ON DELETE SET NULL would need the column
-- nullable, and the nullable version cannot distinguish "the grant was deleted"
-- from "the grant id was never recorded".
CREATE TABLE share_access (
    id            TEXT PRIMARY KEY,
    grant_id      TEXT NOT NULL,
    at            TEXT NOT NULL,
    -- The client address, as seen. Recorded rather than derived later because
    -- there is no later: a log that computes the address on read has no record
    -- of it if the request context changes shape.
    ip            TEXT,
    user_agent    TEXT,
    -- 1 when the request was allowed. 0 with a `denied_reason` when it was not.
    granted       INTEGER NOT NULL,
    -- NULL when granted. One of 'unknown_token', 'expired', 'revoked',
    -- 'password_required', 'wrong_password' otherwise. A free string for the
    -- same reason `undo_record.action` is: this is a log the owner reads, and
    -- adding a reason should not need a migration.
    denied_reason TEXT,
    CONSTRAINT share_access_denial_has_reason
        CHECK (granted = 1 OR denied_reason IS NOT NULL)
);

-- The owner's list: "my links, newest first".
CREATE INDEX share_access_grant_at ON share_access(grant_id, at);
