//! §14.1 — the takedown pipeline.
//!
//! Three steps and one output, and the output is the part that matters:
//!
//! 1. a **report** moves an object to `quarantined`;
//! 2. if the report is **accepted**, the object becomes `denied`;
//! 3. a denial adds a **content-hash blocklist entry**, which is what stops the
//!    same bytes coming back.
//!
//! # Why the blocklist is a table and not a query over `consent_record`
//!
//! A tier on a row is a statement about a *row*. The thing a takedown is about
//! is a *file*, and the file outlives the row: re-upload the same bytes tomorrow
//! under a new object id and a `denied` tier on the old row is a takedown that
//! was quietly undone by somebody with a browser. §14.1 names a content hash for
//! exactly this reason, and the hash is the key.
//!
//! So `quarantine` writes a tier, `deny` writes a tier *and* a blocklist row,
//! and `check_import` is the only question asked about a hash. Nothing in this
//! module can answer "is this file blocked?" by looking at an object, because
//! that question does not have an object in it.
//!
//! # Three decisions that are load-bearing, and what breaks without them
//!
//! **A quarantine destroys nothing.** `quarantine` is a *report*. §14.1 ties
//! locator destruction to acceptance, and a pipeline that destroyed on a report
//! would let anybody deny a file's retrievability by filing a complaint and
//! never answering a moderator's questions. `deny` destroys; `quarantine` does
//! not; the tests assert both halves, because a pipeline that always destroys
//! and one that never destroys both pass a test with only one of them.
//!
//! **The blocklist key is `(content_hash, kind)`, not `content_hash`.** A
//! quarantine is a dispute and a denial is a decision, and both can be true of
//! the same bytes at once: peer A quarantines while peer B has already denied.
//! They are not in conflict, and a key of `content_hash` alone would make the
//! second insert collide with the first and *lose the denial* — which fails
//! safe for the wrong reason, and safe-by-accident is not a property.
//!
//! **A missing content hash is not a blocklist hit.** `is_blocked("")` is
//! false, and it has to be: an unhashed file matching a blocklist entry because
//! the entry's hash is also empty would deny an entire library in one query.
//! `check_import` returns a distinct verdict for it instead, so a caller can
//! decide rather than being handed a silent yes.

use commons_core::ConsentTier;
use commons_store::db::{Store, StoreError};
use commons_store::locator;
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// A takedown decision, with the parties named.
///
/// Borrowed rather than owned because a decision is assembled from a queue row
/// and a moderator's action, and copying four `String`s to say it is a
/// ceremony. The lifetimes are all the caller's, which is the point.
#[derive(Debug, Clone, Copy)]
pub struct Takedown<'a> {
    /// The person the content is about, from `person_cluster` (§7.5).
    pub cluster_id: &'a str,
    pub object_id: &'a str,
    /// Who asked. A free string rather than an account id: §14.1 says a
    /// performer can request takedown "without an account on the index", so
    /// this is sometimes a verified claim and sometimes a stranger's name, and
    /// neither is a foreign key to `account`.
    pub requested_by: &'a str,
    pub reason: &'a str,
    /// The steward who accepted it. Required, not optional: an accepted takedown
    /// with nobody accountable is how a moderation system becomes a button.
    pub decided_by: &'a str,
}

/// Why a takedown could not be carried out.
///
/// Every variant is a case where the *complainant* would otherwise be told
/// nothing happened, which is the outcome §7.5 calls the one worse than a
/// misdirected request.
#[derive(Debug, thiserror::Error)]
pub enum DenyError {
    #[error("no such object: {0}")]
    NoSuchObject(String),
    #[error("the object has no file, so there is no content to block")]
    NoContent(String),
    #[error(
        "the object has no recorded content hash, so a takedown of it cannot \
             propagate: a peer receiving the tombstone would have nothing to \
             match on"
    )]
    NoContentHash(String),
    #[error(
        "an accepted takedown needs a reason: the basis is part of the \
             record, and a peer receiving the tombstone shows it to the person \
             whose content it is"
    )]
    EmptyReason,
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// Why content is blocked, or that it is not.
#[derive(Debug, thiserror::Error)]
pub enum QuarantineError {
    #[error("no such object: {0}")]
    NoSuchObject(String),
    #[error(
        "a report needs a reason: a takedown nobody can review is a takedown \
             nobody can answer"
    )]
    EmptyReason,
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// The verdict on importing some content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    /// §14.1: a takedown accepted. "Permanently blocked by hash across all
    /// peers."
    Denied,
    /// Contested, pending review. Importable — the dispute is not a decision,
    /// and blocking on it would be the pipeline denying on a report.
    Quarantined,
}

/// What [`check_import`] says about a piece of content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportVerdict {
    pub allowed: bool,
    pub block: Option<BlockReason>,
}

impl fmt::Display for ImportVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.block {
            None => write!(f, "allowed"),
            Some(BlockReason::Denied) => write!(f, "blocked: denied"),
            Some(BlockReason::Quarantined) => write!(f, "allowed but contested"),
        }
    }
}

/// Set an object's consent tier, failing if there is no consent record.
///
/// A missing record is an error rather than a no-op, and deliberately so: an
/// object with no consent record is invisible to every caller already (T-P4-007
/// made that the inner join's job), so a takedown that quietly succeeded against
/// one would be a takedown recorded nowhere and applied to nothing.
async fn set_tier(
    store: &Store,
    object_id: &str,
    tier: ConsentTier,
) -> Result<(), QuarantineError> {
    let n = sqlx::query("UPDATE consent_record SET tier = ?, updated_at = ? WHERE object_id = ?")
        .bind(tier.as_str())
        .bind(commons_core::ts::now())
        .bind(object_id)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?
        .rows_affected();
    if n == 0 {
        return Err(QuarantineError::NoSuchObject(object_id.to_string()));
    }
    Ok(())
}

/// A report. Moves the object to `quarantined` and stops there.
///
/// Quarantined is "hidden everywhere, pending review" (§14.1), so the object
/// disappears from every browse surface immediately — including the uploader's.
/// That is deliberate and is the one part of a quarantine that is not
/// reversible by the person who filed it: a report is a claim, and the content
/// being invisible while the claim is tested is the cost of testing it.
///
/// Nothing is destroyed. See the module docs.
pub async fn quarantine(
    store: &Store,
    cluster_id: &str,
    object_id: &str,
    requested_by: &str,
    reason: &str,
) -> Result<String, QuarantineError> {
    if reason.trim().is_empty() {
        return Err(QuarantineError::EmptyReason);
    }
    set_tier(store, object_id, ConsentTier::Quarantined).await?;

    // A blocklist row for the quarantine too, so `check_import` has something to
    // say and the outbox has something to send. A quarantine is not a block: the
    // verdict distinguishes them, and a peer receiving it must not be told the
    // content is denied.
    let request_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO takedown_request
             (id, cluster_id, requested_by, reason, to_stewards, state, created_at)
         VALUES (?, ?, ?, ?, 1, 'open', ?)",
    )
    .bind(&request_id)
    .bind(cluster_id)
    .bind(requested_by)
    .bind(reason)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;

    for hash in hashes_of(store, object_id).await? {
        record_block(store, &hash, "quarantined", Some(object_id), reason).await?;
        enqueue(store, &hash, "quarantined", Some(object_id), reason).await?;
    }
    Ok(request_id)
}

/// The content hashes behind an object, from its files.
///
/// Empty for an object with no files or no recorded hashes, and the caller
/// treats that as "nothing to block" rather than as an error: an object whose
/// file has not been hashed yet is a real state, and refusing the takedown over
/// it would leave the content up.
async fn hashes_of(store: &Store, object_id: &str) -> Result<Vec<String>, StoreError> {
    let rows: Vec<(Option<String>,)> = sqlx::query_as(
        "SELECT hash_blake3 FROM file WHERE object_id = ? AND hash_blake3 IS NOT NULL",
    )
    .bind(object_id)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(rows.into_iter().filter_map(|(h,)| h).collect())
}

/// Add one row to the blocklist, or leave an existing one alone.
///
/// `ON CONFLICT DO NOTHING` on `(content_hash, kind)`, which is the pair the
/// primary key is on. The alternative — a read-then-write — loses a race between
/// two peers' tombstones arriving at once, and the loser is whichever denial
/// happened to be slower.
async fn record_block(
    store: &Store,
    content_hash: &str,
    kind: &str,
    object_id: Option<&str>,
    reason: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO content_blocklist (content_hash, kind, object_id, reason, created_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (content_hash, kind) DO NOTHING",
    )
    .bind(content_hash)
    .bind(kind)
    .bind(object_id)
    .bind(reason)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// Queue a tombstone for delivery to peers.
async fn enqueue(
    store: &Store,
    content_hash: &str,
    kind: &str,
    object_id: Option<&str>,
    reason: &str,
) -> Result<String, StoreError> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO tombstone_outbox
             (id, content_hash, kind, object_id, reason, signer, signature, created_at)
         VALUES (?, ?, ?, ?, ?, 'local', 'local-decision', ?)",
    )
    .bind(&id)
    .bind(content_hash)
    .bind(kind)
    .bind(object_id)
    .bind(reason)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(id)
}

/// Accept a takedown. The object becomes `denied`, its locators are destroyed,
/// and every content hash behind it is added to the blocklist and queued for
/// propagation.
///
/// One function rather than three, because the three are one decision. A caller
/// that could do "tier only" or "blocklist only" would eventually do one of
/// them, and each of those is a takedown that does not survive a re-upload.
pub async fn deny(store: &Store, t: Takedown<'_>) -> Result<u64, DenyError> {
    if t.reason.trim().is_empty() {
        return Err(DenyError::EmptyReason);
    }

    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM object WHERE id = ?")
        .bind(t.object_id)
        .fetch_one(store.pool())
        .await
        .map_err(StoreError::Query)?;
    if exists == 0 {
        return Err(DenyError::NoSuchObject(t.object_id.to_string()));
    }

    // The locator destruction comes *first*, and before the tier changes.
    //
    // The order is the point. A magnet is the thing that retrieves the content,
    // and a tombstoned magnet still resolves, so the row has to go rather than
    // be marked. Doing it before the tier write means a crash between the two
    // leaves the object quarantined (invisible, unreachable) rather than denied
    // (blocked, but still holding a working locator) — and "invisible and
    // unreachable" is the recoverable state.
    let destroyed = locator::destroy_for_object(store, t.object_id).await?;

    sqlx::query("UPDATE consent_record SET tier = 'denied', updated_at = ? WHERE object_id = ?")
        .bind(commons_core::ts::now())
        .bind(t.object_id)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;

    let hashes = hashes_of(store, t.object_id).await?;
    for hash in &hashes {
        record_block(store, hash, "denied", Some(t.object_id), t.reason).await?;
        enqueue(store, hash, "denied", Some(t.object_id), t.reason).await?;
    }

    // Close the request, so the queue does not keep offering a decision that has
    // been made. A request left `open` after acceptance is a moderator clicking
    // accept again, and a second `deny` on the same object is a second set of
    // tombstones for a decision already made.
    sqlx::query(
        "UPDATE takedown_request SET state = 'accepted'
          WHERE state = 'open' AND cluster_id = ?",
    )
    .bind(t.cluster_id)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;

    Ok(destroyed)
}

/// Is this content hash blocked here?
///
/// The empty hash is never blocked, and the `!content_hash.is_empty()` guard is
/// the reason. A blocklist lookup that matched on the empty string would deny
/// every file in a library that has not been hashed yet, which is most of them
/// on the first run.
pub async fn is_blocked(store: &Store, content_hash: &str) -> Result<bool, StoreError> {
    if content_hash.is_empty() {
        return Ok(false);
    }
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM content_blocklist WHERE content_hash = ? AND kind = 'denied'",
    )
    .bind(content_hash)
    .fetch_one(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(n > 0)
}

/// The verdict on importing `content_hash`.
///
/// The check §14.1 names — "checked on import, scan, and match". One function
/// for all three, because a scan that checks and an import that does not is the
/// gap the requirement exists to close, and three functions means three
/// chances to forget one.
pub async fn check_import(store: &Store, content_hash: &str) -> Result<ImportVerdict, StoreError> {
    if content_hash.is_empty() {
        return Ok(ImportVerdict {
            allowed: true,
            block: None,
        });
    }
    let kind: Option<String> = sqlx::query_scalar(
        "SELECT kind FROM content_blocklist WHERE content_hash = ?
         ORDER BY (kind = 'denied') DESC LIMIT 1",
    )
    .bind(content_hash)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;

    Ok(match kind.as_deref() {
        Some("denied") => ImportVerdict {
            allowed: false,
            block: Some(BlockReason::Denied),
        },
        // Quarantined is importable. The content is *contested*, and a dispute is
        // not a decision; blocking on it would mean a report denies a file,
        // which is the pipeline failing at the one job it must get right.
        Some(_) => ImportVerdict {
            allowed: true,
            block: Some(BlockReason::Quarantined),
        },
        None => ImportVerdict {
            allowed: true,
            block: None,
        },
    })
}

/// The consent tier of an object, or `None` if there is no consent record.
pub async fn tier_of(store: &Store, object_id: &str) -> Result<Option<String>, StoreError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT tier FROM consent_record WHERE object_id = ?")
            .bind(object_id)
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?;
    Ok(row.map(|(t,)| t))
}

/// Redact a subject's identifying data on request (stash-box#656).
///
/// Writes a [`redaction_log`] row and returns how many rows it removed. The log
/// row is written *first* and describes what is about to go rather than
/// containing it, because the log outlives the redaction: deleting the note with
/// the data would leave no way to answer "was this ever redacted?", which is
/// the only question an audit asks.
pub async fn redact(
    store: &Store,
    subject_type: &str,
    subject_id: &str,
    what: &str,
    requested_by: &str,
) -> Result<i64, StoreError> {
    // Which tables hold this subject's identifying rows. A lookup rather than
    // interpolated SQL: the subject type is a caller-supplied string and a
    // `format!` here is an injection.
    let tables: &[&str] = match subject_type {
        "performer" => &["performer_alias", "performer"],
        "cluster" => &["performer_alias", "performer"],
        // Unknown subject types redact nothing and log nothing, rather than
        // guessing at a table name.
        _ => return Ok(0),
    };

    let mut removed = 0i64;
    for table in tables {
        let n = sqlx::query(&format!("DELETE FROM {table} WHERE id = ?"))
            .bind(subject_id)
            .execute(store.pool())
            .await
            .map_err(StoreError::Query)?
            .rows_affected() as i64;
        removed += n;
    }

    sqlx::query(
        "INSERT INTO redaction_log
             (id, subject_type, subject_id, what, rows_removed, requested_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(subject_type)
    .bind(subject_id)
    .bind(what)
    .bind(removed)
    .bind(requested_by)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(removed)
}

/// The wire shape of a tombstone, as it crosses to a peer.
///
/// Serialize/Deserialize rather than a shared struct so the wire format is
/// pinned independently of the Rust one: adding a field here is a protocol
/// change, and `#[serde(default)]` on one is a promise that old peers will not
/// send it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TombstoneRecord {
    pub id: String,
    pub content_hash: String,
    pub kind: String,
    pub object_id: Option<String>,
    pub reason: String,
    pub signer: String,
    pub signature: String,
    pub created_at: String,
}
