//! Share grant storage. Migration `0022_share_grants.sql`.
//!
//! Spec §9.5 (#5612). Policy lives in `commons_consent::share`; this module is
//! only the queries, and the split is deliberate: every decision about *whether*
//! a link works is in the policy module with no database in sight, so the rules
//! are testable without one and there is exactly one place they can change.
//!
//! # Why this module has almost no logic
//!
//! Because a store layer with policy in it is a store layer where the policy
//! cannot be tested. Every function here either moves a row or counts one.
//!
//! # Timestamps are TEXT, and that is deliberate
//!
//! ISO-8601 UTC TEXT per plan §0.4, so comparison and sort need no timezone
//! function and both engines agree — the same rule the other 21 migrations
//! follow, and the reason `expires_at > created_at` is a CHECK that behaves
//! identically on SQLite and Postgres. `chrono::DateTime` is what the *caller*
//! sees; it is converted at this boundary and nowhere else, because a
//! `DateTime` in a row would make the CHECK string-compare on one engine and
//! timestamp-compare on the other.
//!
//! # The two-engine rules, restated because they bite on every query here
//!
//! * Placeholders are `?` on SQLite and `$1, $2` on Postgres. Every query is
//!   written once as a `const SQL` with `{p}` markers and expanded through
//!   [`placeholders`].
//! * `access_count` is `BIGINT` in Postgres, so it decodes as `i64`. An
//!   `INTEGER` column read as `i64` is a **read-time** type error — not a
//!   write-time one, and not on SQLite at all, so the parity test passes and
//!   only a round-trip test catches it.

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::db::{placeholder, placeholders, Store, StoreError};

/// The stored spelling of a timestamp. Plan §0.4: ISO-8601 UTC, millis, `Z`.
fn to_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Parse a stored timestamp.
///
/// A malformed one yields `None` rather than panicking. A corrupt timestamp in
/// a share row is bad, but taking the process down on a read is worse than
/// reporting an expired grant: the policy module treats an unparseable
/// `expires_at` as not-live, and a link that does not work is the safe failure.
fn from_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Hex-encode bytes for storage.
///
/// TEXT, not a binary column: Postgres spells it BYTEA and SQLite spells it
/// BLOB, and this migration is applied to both, so there is no portable binary
/// type. Every other hash in this schema is hex TEXT (`0021_subtitles.sha256`)
/// and matching that is worth more than saving 32 bytes.
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode hex storage text. `None` on anything malformed, for the same reason
/// `from_ts` is: a corrupt row should make a link fail closed, not panic.
fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// One row of `share_grant`, as stored.
///
/// A plain mirror rather than a conversion into `commons_consent::share::ShareGrant`:
/// the store crate does not depend on the consent crate (it is the other way
/// round), and a mirror of nine columns is cheaper than an inversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareGrantRow {
    pub id: String,
    pub token_hash: Vec<u8>,
    pub scope: String,
    pub target_kind: String,
    pub target_id: String,
    pub password_hash: Option<Vec<u8>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: Option<DateTime<Utc>>,
    pub access_count: i64,
    pub last_accessed_at: Option<DateTime<Utc>>,
}

impl ShareGrantRow {
    /// A row with the fields a test or a create call wants set.
    pub fn new(id: &str, token_hash: Vec<u8>, expires_at: DateTime<Utc>) -> Self {
        Self {
            id: id.to_string(),
            token_hash,
            scope: "view".to_string(),
            target_kind: "object".to_string(),
            target_id: "obj-1".to_string(),
            password_hash: None,
            expires_at: Some(expires_at),
            revoked_at: None,
            created_at: Some(to_ts_parse(Utc::now())),
            access_count: 0,
            last_accessed_at: None,
        }
    }
}

/// `Utc::now()` through the TEXT representation, so `new` cannot produce a
/// `created_at` in a different format from the one the queries write.
fn to_ts_parse(t: DateTime<Utc>) -> DateTime<Utc> {
    from_ts(&to_ts(t)).unwrap_or(t)
}

/// Decode a grant row. The `i64` on `access_count` is load-bearing; see the
/// module header.
macro_rules! grant_from_row {
    ($r:expr, $row:ty) => {{
        let r: &$row = $r;
        ShareGrantRow {
            id: r.get("id"),
            token_hash: from_hex(&r.get::<String, _>("token_hash")).unwrap_or_default(),
            scope: r.get("scope"),
            target_kind: r.get("target_kind"),
            target_id: r.get("target_id"),
            password_hash: r
                .get::<Option<String>, _>("password_hash")
                .and_then(|s| from_hex(&s)),
            expires_at: {
                let v = r.get::<String, _>("expires_at");
                from_ts(&v)
            },
            revoked_at: r
                .get::<Option<String>, _>("revoked_at")
                .and_then(|s| from_ts(&s)),
            created_at: r
                .get::<Option<String>, _>("created_at")
                .and_then(|s| from_ts(&s)),
            access_count: r.get("access_count"),
            last_accessed_at: r
                .get::<Option<String>, _>("last_accessed_at")
                .and_then(|s| from_ts(&s)),
        }
    }};
}

/// Fetch a grant by its id.
///
/// The `id` half of the token, so this is the first half of every resolve. It
/// returns the row **unfiltered** — not `WHERE revoked_at IS NULL`, not
/// `WHERE expires_at > now()`. The caller hands it to
/// `commons_consent::share::resolve`, which needs the revoked and expired rows
/// too, because *why* a link failed is what goes in the access log and in the
/// owner's view of their own links.
///
/// A query that filtered here would make `Expired` and `Revoked`
/// indistinguishable from `UnknownToken` at the call site, and a link that
/// reports "not found" when its owner killed it is a confusing thing to debug.
pub async fn get_grant_by_id(store: &Store, id: &str) -> Result<Option<ShareGrantRow>, StoreError> {
    const SQL: &str = "SELECT id, token_hash, scope, target_kind, target_id, password_hash, \
         expires_at, revoked_at, created_at, access_count, last_accessed_at \
         FROM share_grant WHERE id = {p}";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query(&sql)
                .bind(id)
                .fetch_optional($p)
                .await
                .map_err(StoreError::Query)
                .map(|row| row.as_ref().map(|r| grant_from_row!(r, $row)))
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// Insert a grant.
///
/// The `token_hash` arrives already hashed from the consent crate; nothing here
/// hashes, so there is no second implementation of the construction to drift out
/// of agreement with the first.
pub async fn insert_grant(store: &Store, row: &ShareGrantRow) -> Result<(), StoreError> {
    const SQL: &str = "INSERT INTO share_grant (id, token_hash, scope, target_kind, target_id, \
         password_hash, expires_at, revoked_at, created_at, access_count, last_accessed_at) \
         VALUES ({p})";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{p}", &placeholders(11, $numbered));
            sqlx::query(&sql)
                .bind(&row.id)
                .bind(to_hex(&row.token_hash))
                .bind(&row.scope)
                .bind(&row.target_kind)
                .bind(&row.target_id)
                .bind(row.password_hash.as_deref().map(to_hex))
                .bind(row.expires_at.map(to_ts))
                .bind(row.revoked_at.map(to_ts))
                .bind(row.created_at.map(to_ts))
                .bind(row.access_count)
                .bind(row.last_accessed_at.map(to_ts))
                .execute($p)
                .await
                .map_err(StoreError::Query)
                .map(|_| ())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}

/// Revoke a grant. Idempotent, and immediate.
///
/// `revoked_at = COALESCE(revoked_at, now)`, so revoking twice does not move
/// the timestamp. That matters because the timestamp is the only record of
/// *when* the owner killed the link, and a second revoke from an impatient
/// double-click would otherwise rewrite it to a time the owner did not act at.
pub async fn revoke_grant(store: &Store, id: &str, at: DateTime<Utc>) -> Result<bool, StoreError> {
    const SQL: &str = "UPDATE share_grant SET revoked_at = COALESCE(revoked_at, {p0}) \
         WHERE id = {p1} AND revoked_at IS NULL";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL
                .replace("{p0}", &placeholder(1, $numbered))
                .replace("{p1}", &placeholder(2, $numbered));
            sqlx::query(&sql)
                .bind(to_ts(at))
                .bind(id)
                .execute($p)
                .await
                .map_err(StoreError::Query)
                .map(|done| done.rows_affected() > 0)
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}

/// Every grant, newest first, for the owner's management page.
///
/// Not filtered to live rows: the page shows revoked and expired links too, with
/// their state, because "where is the link I sent on Tuesday" is a question
/// about a link that no longer works.
pub async fn list_grants(store: &Store) -> Result<Vec<ShareGrantRow>, StoreError> {
    const SQL: &str = "SELECT id, token_hash, scope, target_kind, target_id, password_hash, \
         expires_at, revoked_at, created_at, access_count, last_accessed_at \
         FROM share_grant ORDER BY created_at DESC";
    macro_rules! go {
        ($p:expr, $row:ty) => {{
            sqlx::query(SQL)
                .fetch_all($p)
                .await
                .map_err(StoreError::Query)
                .map(|rows| rows.iter().map(|r| grant_from_row!(r, $row)).collect())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, sqlx::postgres::PgRow),
    }
}

/// Grants pointing at one target. The "which of my items is shared?" query.
pub async fn list_grants_for_target(
    store: &Store,
    target_kind: &str,
    target_id: &str,
) -> Result<Vec<ShareGrantRow>, StoreError> {
    const SQL: &str = "SELECT id, token_hash, scope, target_kind, target_id, password_hash, \
         expires_at, revoked_at, created_at, access_count, last_accessed_at \
         FROM share_grant WHERE target_kind = {p0} AND target_id = {p1} \
         ORDER BY created_at DESC";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL
                .replace("{p0}", &placeholder(1, $numbered))
                .replace("{p1}", &placeholder(2, $numbered));
            sqlx::query(&sql)
                .bind(target_kind)
                .bind(target_id)
                .fetch_all($p)
                .await
                .map_err(StoreError::Query)
                .map(|rows| rows.iter().map(|r| grant_from_row!(r, $row)).collect())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// One row of `share_access`, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareAccessRow {
    pub id: String,
    pub grant_id: String,
    pub at: DateTime<Utc>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub granted: bool,
    pub denied_reason: Option<String>,
}

macro_rules! access_from_row {
    ($r:expr, $row:ty) => {{
        let r: &$row = $r;
        let raw_at = r.get::<String, _>("at");
        let at = from_ts(&raw_at).unwrap_or_else(|| to_ts_parse(Utc::now()));
        ShareAccessRow {
            id: r.get("id"),
            grant_id: r.get("grant_id"),
            at,
            ip: r.get("ip"),
            user_agent: r.get("user_agent"),
            granted: r.get::<i32, _>("granted") != 0,
            denied_reason: r.get("denied_reason"),
        }
    }};
}

/// Record one access attempt — granted or denied, always.
///
/// `granted` and `denied_reason` are passed in rather than derived, and
/// `denied_reason` is required to be `Some` when `granted` is false, so the
/// CHECK in the migration is a backstop for a caller bug rather than the
/// mechanism. A log that records "denied" without a reason is not a log; it is
/// a count.
pub async fn record_access(store: &Store, entry: &ShareAccessRow) -> Result<(), StoreError> {
    // Deliberately NO `debug_assert` here. One was the first draft and it made
    // this function untestable for the property that matters: the assert fires
    // in the caller, so a test that writes a reason-less denial never reaches
    // the database, and the CHECK in migration 0022 is left unproven. The
    // database is the constraint that has to hold for callers that do not go
    // through Rust at all, so the check belongs there and only there.
    const SQL: &str = "INSERT INTO share_access (id, grant_id, at, ip, user_agent, granted, \
         denied_reason) VALUES ({p})";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{p}", &placeholders(7, $numbered));
            sqlx::query(&sql)
                .bind(&entry.id)
                .bind(&entry.grant_id)
                .bind(to_ts(entry.at))
                .bind(&entry.ip)
                .bind(&entry.user_agent)
                .bind(i32::from(entry.granted))
                .bind(&entry.denied_reason)
                .execute($p)
                .await
                .map_err(StoreError::Query)
                .map(|_| ())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}

/// The access log for one grant, newest first.
pub async fn list_access(store: &Store, grant_id: &str) -> Result<Vec<ShareAccessRow>, StoreError> {
    const SQL: &str = "SELECT id, grant_id, at, ip, user_agent, granted, denied_reason \
         FROM share_access WHERE grant_id = {p} ORDER BY at DESC";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query(&sql)
                .bind(grant_id)
                .fetch_all($p)
                .await
                .map_err(StoreError::Query)
                .map(|rows| rows.iter().map(|r| access_from_row!(r, $row)).collect())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// Bump the denormalised counters after a granted access.
///
/// The new count is computed in Rust rather than with `access_count =
/// access_count + 1`, and that looks like a mistake: `+ 1` is not portable SQL
/// (it is not on Postgres, and the plan's portable rules forbid
/// engine-specific expressions). The grant row is passed in because the caller
/// already has it from the resolve, and a second read would be a race against
/// the very counter being written.
pub async fn record_granted(
    store: &Store,
    grant: &ShareGrantRow,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    const SQL: &str = "UPDATE share_grant SET access_count = {p0}, last_accessed_at = {p1} \
         WHERE id = {p2}";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL
                .replace("{p0}", &placeholder(1, $numbered))
                .replace("{p1}", &placeholder(2, $numbered))
                .replace("{p2}", &placeholder(3, $numbered));
            sqlx::query(&sql)
                .bind(grant.access_count + 1)
                .bind(to_ts(at))
                .bind(&grant.id)
                .execute($p)
                .await
                .map_err(StoreError::Query)
                .map(|_| ())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}

/// Exposed for tests that need to write the exact text a query writes.
#[doc(hidden)]
pub fn timestamp_text(t: DateTime<Utc>) -> String {
    to_ts(t)
}
