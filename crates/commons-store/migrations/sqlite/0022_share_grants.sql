-- 0022_share_grants.sql - SQLite mirror (Postgres: 0022_share_grants.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0022_share_grants.sql and re-run that script.
-- T-P0-007's parity test fails if the two files' table sets ever diverge.
--
-- Portable-SQL rules in force (plan section 0.4):
--   * ids are TEXT
--   * timestamps are ISO-8601 UTC TEXT, so comparison and sort need no
--     timezone function and both engines agree
--   * no vector columns; embeddings live in a sidecar ANN file
--   * booleans are INTEGER 0|1 here and BOOLEAN in Postgres; the store crate
--     hides the difference and no query writes a literal
--   * foreign keys need `PRAGMA foreign_keys = ON` per connection, which the
--     store crate sets at open time

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
