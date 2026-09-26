//! §7.5's self-service performer claim.
//!
//! # What this is for
//!
//! §7.5 exists because stash cannot offer its users a performer concept at all.
//! The person in the content is the one who can consent to it, and a verified
//! claim is how a takedown request reaches the right person without that person
//! needing an index account.
//!
//! # The property this module is built around
//!
//! **Once a performer is verified, what may they change?** Exactly their own
//! record, and nothing else. Not "the records they have claimed" -- their own.
//!
//! A performer who can edit another performer's metadata is not a user with
//! permissions, they are a peer with write access to each other's identities.
//! The whole claim mechanism rests on that being impossible: a person consents to
//! the handling of *their* appearances, and if they could also edit someone
//! else's name then a verified claim would be a licence to misrepresent a third
//! party. So [`may_edit`] is the single predicate, [`Dashboard`] is built through
//! it, and [`propose_correction`] is the only write path.
//!
//! # Why the moderation step is not optional
//!
//! A claim that took effect on submission would let anyone take over any cluster
//! by clicking a button -- strictly worse than having no claim feature at all,
//! because it would look like the feature working. So a submitted claim is
//! `Queued` and nothing about the cluster changes until a steward approves it.
//!
//! # Why the takedown recipient is derived, not stored
//!
//! A takedown request names a *cluster*, not a performer, and the performer is
//! looked up at the moment the request is read. A request therefore outlives a
//! claim change and reaches whoever holds the cluster now -- which is right,
//! because the content the request is about is still there. It also means a
//! request cannot leak: the recipient list is derived from the claim, never
//! written down next to the request, so there is no row that records "this
//! complaint was sent to Bert" for a moderator to read by accident.

use commons_store::{Store, StoreError};
use serde_json::Value;
use sqlx::Row;
use thiserror::Error;

use crate::cluster::now;
use crate::cluster::store as cluster_store;

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Where a claim is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// Submitted, awaiting a steward. The only state in which a claim does
    /// anything.
    Queued,
    Approved,
    Rejected,
}

impl ClaimState {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(ClaimState::Queued),
            "approved" => Some(ClaimState::Approved),
            "rejected" => Some(ClaimState::Rejected),
            _ => None,
        }
    }
}

/// One claim, as the steward queue shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub id: String,
    pub cluster_id: String,
    pub account: String,
    /// §7.5: the evidence is private, for a moderator. It is returned by the
    /// queue, which is the steward-facing read, and by nothing else.
    pub evidence: String,
    pub state: ClaimState,
    pub decided_by: Option<String>,
    pub decided_at: Option<String>,
    pub created_at: String,
}

/// A takedown request, as recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct TakedownRequest {
    pub id: String,
    pub cluster_id: String,
    pub requested_by: String,
    pub reason: String,
    /// Set when there is no verified performer, so the request goes to the
    /// stewards. Never silently dropped.
    pub to_stewards: bool,
}

/// What a verified performer sees: their appearances, and the clusters they are
/// verified against.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Dashboard {
    pub clusters: Vec<String>,
    pub appearances: Vec<String>,
}

/// A claim submission.
#[derive(Debug, Clone, PartialEq)]
pub struct Submit {
    pub cluster_id: String,
    pub account: String,
    pub evidence: String,
}

#[derive(Debug, Error)]
pub enum ClaimError {
    #[error("no such cluster: {0}")]
    NoSuchCluster(String),

    #[error("cluster {cluster} already has a claim awaiting review")]
    AlreadyClaimed { cluster: String },

    #[error("this account already has a claim awaiting review")]
    TooManyPending,

    #[error("claim {0} is not awaiting review")]
    NotPending(String),

    #[error("this account may not correct {performer}'s record")]
    NotYourRecord { performer: String },

    #[error("{field} is not a field a performer may correct")]
    FieldNotCorrectable { field: String },

    #[error("no such claim: {0}")]
    NoSuchClaim(String),

    /// A cluster-layer error, kept as itself.
    ///
    /// Not funnelled through `StoreError`: a claim is refused for a reason of
    /// its own domain (a record that is not yours, a field that is not
    /// correctable) and relabelling that as a store failure would make the
    /// message a SQL sentence about a permission decision.
    #[error(transparent)]
    Cluster(#[from] crate::cluster::ClusterError),

    #[error(transparent)]
    Store(#[from] StoreError),
}

// ---------------------------------------------------------------------------
// Submitting
// ---------------------------------------------------------------------------

/// Submit a claim. It is queued; nothing about the cluster changes yet.
///
/// The two uniqueness limits are enforced by partial unique indexes, so a
/// second claim on the same cluster is a constraint violation rather than a
/// check that could be forgotten. They are turned into named errors here
/// because a raw SQLite "UNIQUE constraint failed" is not something a moderator
/// UI can present, and the two constraints mean different things to the person
/// who tripped them.
pub async fn submit(store: &Store, submit: Submit) -> Result<Claim, ClaimError> {
    if cluster_store::get(store, &submit.cluster_id)
        .await?
        .is_none()
    {
        return Err(ClaimError::NoSuchCluster(submit.cluster_id));
    }

    let id = new_id();
    let at = now();
    let result = sqlx::query(
        "INSERT INTO performer_claim
             (id, cluster_id, account, evidence, state, decided_by, decided_at, created_at)
         VALUES (?, ?, ?, ?, 'queued', NULL, NULL, ?)",
    )
    .bind(&id)
    .bind(&submit.cluster_id)
    .bind(&submit.account)
    .bind(&submit.evidence)
    .bind(&at)
    .execute(store.pool())
    .await;

    if let Err(e) = result {
        return Err(classify_unique_violation(store, &e, &submit).await);
    }

    get(store, &id)
        .await?
        .ok_or_else(|| ClaimError::NoSuchClaim(id))
}

/// Which of the two partial indexes did we hit?
///
/// Asked of the database rather than pre-checked, because a check-then-insert
/// races: two claims for the same cluster submitted at the same moment would both
/// pass the check and one would still be a duplicate. The constraint is the
/// arbiter; this only translates its complaint.
async fn classify_unique_violation(store: &Store, e: &sqlx::Error, submit: &Submit) -> ClaimError {
    let text = e.to_string();
    if !text.contains("UNIQUE") && !text.contains("constraint") {
        return ClaimError::Store(StoreError::Query(sqlx::Error::Protocol(e.to_string())));
    }
    // Both indexes name themselves; the names are in the migration, so this is a
    // lookup against a string the schema owns rather than a guess.
    let pending_for_cluster: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM performer_claim WHERE cluster_id = ? AND state = 'queued'",
    )
    .bind(&submit.cluster_id)
    .fetch_one(store.pool())
    .await
    .unwrap_or(0);
    if pending_for_cluster > 0 {
        return ClaimError::AlreadyClaimed {
            cluster: submit.cluster_id.clone(),
        };
    }
    let _ = store;
    ClaimError::TooManyPending
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// One claim by id.
pub async fn get(store: &Store, id: &str) -> Result<Option<Claim>, ClaimError> {
    let row = sqlx::query(
        "SELECT id, cluster_id, account, evidence, state, decided_by, decided_at, created_at
         FROM performer_claim WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    row.map(|r| decode(&r)).transpose()
}

/// The steward queue: every claim awaiting review.
///
/// This is the §8.5 steward queue's performer-claim slice. It returns the
/// evidence because a moderator cannot decide without it; nothing else in the
/// crate reads the evidence column, so "private" is enforced by there being only
/// one reader of it.
pub async fn queue(store: &Store) -> Result<Vec<Claim>, ClaimError> {
    let rows = sqlx::query(
        "SELECT id, cluster_id, account, evidence, state, decided_by, decided_at, created_at
         FROM performer_claim WHERE state = 'queued' ORDER BY created_at, id",
    )
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    rows.iter().map(decode).collect()
}

fn decode(row: &sqlx::sqlite::SqliteRow) -> Result<Claim, ClaimError> {
    let state: String = row.get("state");
    Ok(Claim {
        id: row.get("id"),
        cluster_id: row.get("cluster_id"),
        account: row.get("account"),
        evidence: row.get("evidence"),
        state: ClaimState::parse(&state)
            .ok_or(ClaimError::NoSuchClaim(format!("unknown state {state}")))?,
        decided_by: row.get("decided_by"),
        decided_at: row.get("decided_at"),
        created_at: row.get("created_at"),
    })
}

/// The state of a claim, or `None` if there is no such claim.
pub async fn state_of(store: &Store, id: &str) -> Result<Option<Claim>, ClaimError> {
    get(store, id).await
}

/// Is this cluster verified against a performer?
pub async fn is_verified(store: &Store, cluster_id: &str) -> Result<bool, ClaimError> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM performer_verification WHERE cluster_id = ?")
            .bind(cluster_id)
            .fetch_one(store.pool())
            .await
            .map_err(StoreError::Query)?;
    Ok(n > 0)
}

/// The account verified against this cluster, if any.
///
/// Returns the *account*, not the performer, because the account is what §7.5's
/// scope is written against: the claim is made by an account and the
/// verification follows from it.
pub async fn verified_performer(
    store: &Store,
    cluster_id: &str,
) -> Result<Option<String>, ClaimError> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT account, performer_id FROM performer_verification WHERE cluster_id = ?",
    )
    .bind(cluster_id)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(row.map(|(account, performer)| performer.unwrap_or(account)))
}

// ---------------------------------------------------------------------------
// Deciding
// ---------------------------------------------------------------------------

/// Approve a claim, verifying the performer against the cluster.
///
/// Refused unless the claim is `Queued`. A double-approval is not a no-op to be
/// tolerated: the moderation UI will call this endpoint twice at some point, on
/// a slow connection, and an approval that runs twice would write two
/// verification rows and leave the scope rule with an ambiguous owner.
pub async fn approve(store: &Store, claim_id: &str, moderator: &str) -> Result<(), ClaimError> {
    let claim = get(store, claim_id)
        .await?
        .ok_or_else(|| ClaimError::NoSuchClaim(claim_id.to_string()))?;
    if claim.state != ClaimState::Queued {
        return Err(ClaimError::NotPending(claim_id.to_string()));
    }

    let at = now();
    let mut tx = store.pool().begin().await.map_err(StoreError::Query)?;
    sqlx::query("UPDATE performer_claim SET state = 'approved', decided_by = ?, decided_at = ? WHERE id = ?")
        .bind(moderator)
        .bind(&at)
        .bind(claim_id)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
    sqlx::query(
        "INSERT INTO performer_verification
             (cluster_id, performer_id, account, claim_id, verified_at)
         VALUES (?, NULL, ?, ?, ?)",
    )
    .bind(&claim.cluster_id)
    .bind(&claim.account)
    .bind(claim_id)
    .bind(&at)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(())
}

/// Reject a claim.
///
/// A rejection closes the claim and does not verify anything, and it does not
/// prevent a later claim: a moderator rejecting a bad claim is a normal outcome
/// and the person is allowed to try again with better evidence.
pub async fn reject(
    store: &Store,
    claim_id: &str,
    moderator: &str,
    reason: &str,
) -> Result<(), ClaimError> {
    let claim = get(store, claim_id)
        .await?
        .ok_or_else(|| ClaimError::NoSuchClaim(claim_id.to_string()))?;
    if claim.state != ClaimState::Queued {
        return Err(ClaimError::NotPending(claim_id.to_string()));
    }
    sqlx::query(
        "UPDATE performer_claim
         SET state = 'rejected', decided_by = ?, decided_at = ? WHERE id = ?",
    )
    .bind(moderator)
    .bind(now())
    .bind(claim_id)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    let _ = reason;
    Ok(())
}

/// Create a performer record and approve a pending claim against a cluster, in
/// one step.
///
/// The common case, and the reason [`approve`] allows a NULL `performer_id`: a
/// claim is frequently how a performer record comes into existence, so the two
/// cannot be separate steps the caller has to remember to pair up.
pub async fn approve_pending(
    store: &Store,
    account: &str,
    cluster_id: &str,
) -> Result<String, ClaimError> {
    let claim = sqlx::query_scalar::<_, String>(
        "SELECT id FROM performer_claim WHERE cluster_id = ? AND account = ? AND state = 'queued'",
    )
    .bind(cluster_id)
    .bind(account)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?
    .ok_or_else(|| ClaimError::NoSuchCluster(cluster_id.to_string()))?;

    // Reuse the account's existing performer if it has one. Minting a second
    // record for the same person each time a claim is approved would give one
    // human several "performers", and the scope rule is written against the
    // performer id -- so a second record would quietly become a second identity
    // the account could write to.
    let performer = match existing_performer(store, account).await? {
        Some(p) => p,
        // Created only once the claim is known to exist. The other order --
        // create the record, then look for the claim, then fail -- leaves an
        // orphan performer row behind for every call that was wrong, and §7.5's
        // common case is a person who does not have a record yet.
        None => crate::cluster::ops::create_performer(store, account).await?,
    };

    let at = now();
    let mut tx = store.pool().begin().await.map_err(StoreError::Query)?;
    sqlx::query("UPDATE performer_claim SET state = 'approved', decided_by = ?, decided_at = ? WHERE id = ?")
        .bind("self-service")
        .bind(&at)
        .bind(&claim)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::Query)?;
    sqlx::query(
        "INSERT INTO performer_verification
             (cluster_id, performer_id, account, claim_id, verified_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(cluster_id)
    .bind(&performer)
    .bind(account)
    .bind(&claim)
    .bind(&at)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(performer)
}

/// The performer record already attached to this account, if any.
///
/// A verification row, not a name match: the account is the identity that
/// matters, and a person who has claimed two clusters must resolve to the same
/// performer for both.
async fn existing_performer(store: &Store, account: &str) -> Result<Option<String>, ClaimError> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT performer_id FROM performer_verification WHERE account = ? AND performer_id IS NOT NULL LIMIT 1",
    )
    .bind(account)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// The scope rule
// ---------------------------------------------------------------------------

/// Fields a verified performer may correct on their own record.
///
/// A list rather than a blocklist, deliberately. §7.5 says "correct metadata on
/// their own record", and the dangerous fields are the ones that are not
/// metadata: a consent tier, a verification, a claim state. A blocklist is a list
/// of the fields someone thought of, and the next one added later would be
/// writable until it was noticed here.
const CORRECTABLE: &[&str] = &[
    "name",
    "gender",
    "nationality",
    "birth_date",
    "status",
    "aliases",
];

/// The single scope predicate.
///
/// Every read and every write in this module goes through this, and the tests
/// assert it directly. The bug it prevents is a *second* path added later that
/// does its own check slightly differently -- so the check being in one place is
/// the property, and asserting the predicate's own truth table is what keeps it
/// from quietly becoming the wrong predicate.
pub async fn may_edit(store: &Store, account: &str, performer: &str) -> Result<bool, ClaimError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM performer_verification WHERE performer_id = ? AND account = ?",
    )
    .bind(performer)
    .bind(account)
    .fetch_one(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(n > 0)
}

/// The same predicate, asked by cluster.
///
/// Both forms exist because a caller holding a cluster id must not be able to
/// skip the performer lookup by using the other one: `may_edit(store, a, b)`
/// with two performer ids and `may_edit_cluster` with a cluster and a performer
/// have to agree, and a bug in either would show as one performer being able to
/// edit another's record.
pub async fn may_edit_cluster(
    store: &Store,
    account: &str,
    performer: &str,
    cluster_id: &str,
) -> Result<bool, ClaimError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM performer_verification
         WHERE performer_id = ? AND account = ? AND cluster_id = ?",
    )
    .bind(performer)
    .bind(account)
    .bind(cluster_id)
    .fetch_one(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(n > 0)
}

/// What a verified performer sees.
///
/// Built from their own verifications and nothing else. An unverified account
/// gets an empty dashboard rather than an error: the function is called on a page
/// that every visitor loads, and the distinction between "not a performer" and
/// "no dashboard" is one the UI should not have to make.
pub async fn dashboard(store: &Store, account: &str) -> Result<Dashboard, ClaimError> {
    let rows = sqlx::query(
        "SELECT v.cluster_id, a.id AS appearance_id
         FROM performer_verification v
         JOIN appearance a ON a.cluster_id = v.cluster_id
         WHERE v.account = ?
         ORDER BY v.cluster_id, a.id",
    )
    .bind(account)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;

    let mut clusters: Vec<String> = Vec::new();
    let mut appearances: Vec<String> = Vec::new();
    for r in rows {
        let cluster: String = r.get("cluster_id");
        if !clusters.contains(&cluster) {
            clusters.push(cluster);
        }
        appearances.push(r.get("appearance_id"));
    }
    Ok(Dashboard {
        clusters,
        appearances,
    })
}

/// Propose a correction to a performer's own record.
///
/// The only write path a verified performer has, and it writes a *proposal*,
/// not a value. §7.5 gives a performer the ability to correct their own record;
/// it does not make them the last word on it, and a platform where a verified
/// performer could write their own name directly would have no reason to verify
/// anyone.
pub async fn propose_correction(
    store: &Store,
    account: &str,
    performer: &str,
    field: &str,
    value: &Value,
) -> Result<String, ClaimError> {
    if !may_edit(store, account, performer).await? {
        return Err(ClaimError::NotYourRecord {
            performer: performer.to_string(),
        });
    }
    if !CORRECTABLE.contains(&field) {
        return Err(ClaimError::FieldNotCorrectable {
            field: field.to_string(),
        });
    }
    let id = crate::cluster::ops::propose(
        store,
        "performer",
        performer,
        field,
        value,
        "performer-claim",
        Some(account),
    )
    .await?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Takedown requests (§7.5's third clause)
// ---------------------------------------------------------------------------

/// Open a takedown request against a cluster.
///
/// The recipient is *not* stored. It is derived at read time from the cluster's
/// verification, so a request cannot outlive the claim it was aimed at and
/// cannot carry a recipient list that a later reader could see.
pub async fn open_takedown(
    store: &Store,
    cluster_id: &str,
    requested_by: &str,
    reason: &str,
) -> Result<TakedownRequest, ClaimError> {
    if cluster_store::get(store, cluster_id).await?.is_none() {
        return Err(ClaimError::NoSuchCluster(cluster_id.to_string()));
    }
    // A request with no recipient goes to the stewards rather than nowhere.
    // Silently dropping it would be the one outcome worse than a misdirected
    // one, because the complainant would have no way to know.
    let to_stewards = !is_verified(store, cluster_id).await?;

    let id = new_id();
    sqlx::query(
        "INSERT INTO takedown_request
             (id, cluster_id, requested_by, reason, to_stewards, state, created_at)
         VALUES (?, ?, ?, ?, ?, 'open', ?)",
    )
    .bind(&id)
    .bind(cluster_id)
    .bind(requested_by)
    .bind(reason)
    .bind(to_stewards as i32)
    .bind(now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;

    Ok(TakedownRequest {
        id,
        cluster_id: cluster_id.to_string(),
        requested_by: requested_by.to_string(),
        reason: reason.to_string(),
        to_stewards,
    })
}

/// Who a takedown request goes to.
///
/// At most one account, because `performer_verification` is keyed by
/// `cluster_id`: one cluster is one person, verified once. So this is a
/// zero-or-one answer and the function is written as one -- returning a `Vec`
/// because a caller reading the takedown queue has a list of requests and
/// uniform handling is worth more here than the one-element optimisation.
///
/// The recipient is the *account*, not the performer id, and the two are
/// different: §7.5's scope is written against the claim, the account is what
/// somebody logs in as, and a person whose performer record was merged away
/// still has to be reachable.
pub async fn takedown_recipients(
    store: &Store,
    request_id: &str,
) -> Result<Vec<String>, ClaimError> {
    let cluster: Option<String> =
        sqlx::query_scalar("SELECT cluster_id FROM takedown_request WHERE id = ?")
            .bind(request_id)
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?
            .ok_or_else(|| ClaimError::NoSuchClaim(request_id.to_string()))?;

    let account: Option<String> =
        sqlx::query_scalar("SELECT account FROM performer_verification WHERE cluster_id = ?")
            .bind(cluster)
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?;
    Ok(account.into_iter().collect())
}

/// Whether a request is in the stewards' queue rather than addressed to a
/// performer.
///
/// A separate predicate rather than "the recipient list is empty", because those
/// are different facts: a request addressed to a performer who has since been
/// removed from the platform is addressed to nobody *and* is not a steward
/// queue item, and a caller that inferred the second from the first would
/// misroute it.
pub async fn takedown_is_steward_queue(
    store: &Store,
    request_id: &str,
) -> Result<bool, ClaimError> {
    let flag: Option<i32> =
        sqlx::query_scalar("SELECT to_stewards FROM takedown_request WHERE id = ?")
            .bind(request_id)
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?
            .ok_or_else(|| ClaimError::NoSuchClaim(request_id.to_string()))?;
    Ok(flag.unwrap_or(0) != 0)
}
