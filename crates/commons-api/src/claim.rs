//! HTTP surface for §7.5's performer claim.
//!
//! # Why this file is thin
//!
//! Every rule lives in [`commons_identity::claim`]; this is the request/response
//! translation and nothing else. The reason for the split is that the rules are
//! the part that has to be right, and a rule reachable only through a router is a
//! rule that will be duplicated when the second router arrives (the CLI, the
//! federation receiver, a maintenance tool). So the router holds no policy: it
//! parses, it calls, and it maps [`ClaimError`] onto a status code.
//!
//! # The status codes, and why they are not uniform
//!
//! Most of these map to 4xx, and the two that do not are the interesting ones.
//!
//! * `NotYourRecord` is **403**, not 404. The caller is authenticated and asking
//!   about a record that exists; answering 404 would hide the existence of a
//!   performer they are not allowed to see, which is a different promise, and one
//!   this module should not be making on §7.5's behalf.
//! * `FieldNotCorrectable` is **422**, not 403. The caller *may* edit the record
//!   and *this field* is not one of them. Collapsing it into 403 would tell a
//!   performer that they have no permission on their own name, which is the one
//!   thing §7.5 promises them.
//! * `NoSuchCluster` is 404 and `NoSuchClaim` is 404, because a claim id in a
//!   URL that resolves to nothing is a missing resource and no better answer
//!   exists.
//!
//! [`ClaimError`]: commons_identity::claim::ClaimError

use commons_identity::claim::{self, ClaimError};
use serde::{Deserialize, Serialize};

/// POST body for submitting a claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitClaim {
    /// The cluster the claimant says is them.
    pub cluster_id: String,
    /// The account making the claim -- the person who is in the content.
    pub account: String,
    /// Why the claimant believes this. Private to the steward queue (§7.5).
    pub evidence: String,
}

impl From<SubmitClaim> for claim::Submit {
    fn from(s: SubmitClaim) -> Self {
        claim::Submit {
            cluster_id: s.cluster_id,
            account: s.account,
            evidence: s.evidence,
        }
    }
}

/// The wire form of a claim.
///
/// `evidence` is included because this is what a steward needs to decide, and the
/// endpoint that returns it is the steward queue. A claim returned to its own
/// submitter does not need it, and the route that does that (`GET /claim/mine`)
/// is not defined here for that reason: there is no caller for it yet, and a
/// route with no caller is a route that ships by accident.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimView {
    pub id: String,
    pub cluster_id: String,
    pub account: String,
    pub evidence: String,
    /// `queued` | `approved` | `rejected`.
    pub state: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<String>,
    pub created_at: String,
}

impl From<claim::Claim> for ClaimView {
    fn from(c: claim::Claim) -> Self {
        ClaimView {
            id: c.id,
            cluster_id: c.cluster_id,
            account: c.account,
            evidence: c.evidence,
            state: match c.state {
                claim::ClaimState::Queued => "queued",
                claim::ClaimState::Approved => "approved",
                claim::ClaimState::Rejected => "rejected",
            }
            .into(),
            decided_by: c.decided_by,
            decided_at: c.decided_at,
            created_at: c.created_at,
        }
    }
}

/// POST body for a takedown request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenTakedown {
    pub cluster_id: String,
    /// The account complaining. Not necessarily the person in the content.
    pub requested_by: String,
    pub reason: String,
}

/// The response to a takedown request.
///
/// `to_stewards` is in the response rather than being an internal detail,
/// because the complainant has a right to know whether anybody is going to read
/// their request. A takedown that silently goes nowhere is the one outcome
/// §7.5's third clause exists to prevent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TakedownView {
    pub id: String,
    pub cluster_id: String,
    /// The verified account, or empty when the request went to the stewards.
    pub recipients: Vec<String>,
    pub to_stewards: bool,
}

/// A status code and a message, ready to return.
///
/// Kept as a struct rather than a bare `u16` so a caller cannot return a status
/// without the explanation, and so the mapping is testable without an HTTP
/// harness.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

/// Map a claim-layer error onto HTTP.
///
/// The one mapping worth arguing about is `NotYourRecord` -> 403, and the
/// reasoning is in the module docs: 404 would promise the caller that the record
/// does not exist, and §7.5 has no business making that promise about another
/// person's identity.
pub fn status_for(e: &ClaimError) -> ApiError {
    let (status, message) = match e {
        ClaimError::NoSuchCluster(_) => (404, "no such cluster"),
        ClaimError::NoSuchClaim(_) => (404, "no such claim"),
        ClaimError::AlreadyClaimed { .. } => {
            (409, "this cluster already has a claim awaiting review")
        }
        ClaimError::TooManyPending => (409, "this account already has a claim awaiting review"),
        ClaimError::NotPending(_) => (409, "this claim has already been decided"),
        ClaimError::NotYourRecord { .. } => (403, "you may not correct this record"),
        ClaimError::FieldNotCorrectable { .. } => {
            (422, "that field is not one a performer may correct")
        }
        // A cluster-layer error surfacing through a claim call. 409 is the
        // honest choice: the claim's own rules were satisfied and something
        // about the cluster was not, and the caller cannot fix it by changing
        // the claim.
        ClaimError::Cluster(_) => (409, "the cluster is not in a state that can be claimed"),
        ClaimError::Store(_) => (500, "storage error"),
    };
    ApiError {
        status,
        message: message.into(),
    }
}

/// The steward queue, as the route returns it.
pub async fn steward_queue(store: &commons_store::Store) -> Result<Vec<ClaimView>, ApiError> {
    claim::queue(store)
        .await
        .map(|claims| claims.into_iter().map(ClaimView::from).collect())
        .map_err(|e| status_for(&e))
}

/// Submit a claim.
pub async fn submit_claim(
    store: &commons_store::Store,
    body: SubmitClaim,
) -> Result<ClaimView, ApiError> {
    claim::submit(store, body.into())
        .await
        .map(ClaimView::from)
        .map_err(|e| status_for(&e))
}

/// Approve or reject a claim, as a steward.
pub async fn decide_claim(
    store: &commons_store::Store,
    claim_id: &str,
    moderator: &str,
    approve: bool,
    reason: &str,
) -> Result<ClaimView, ApiError> {
    if approve {
        claim::approve(store, claim_id, moderator)
            .await
            .map_err(|e| status_for(&e))?;
    } else {
        claim::reject(store, claim_id, moderator, reason)
            .await
            .map_err(|e| status_for(&e))?;
    }
    claim::get(store, claim_id)
        .await
        .map_err(|e| status_for(&e))?
        .map(ClaimView::from)
        .ok_or_else(|| ApiError {
            status: 404,
            message: "no such claim".into(),
        })
}

/// Open a takedown request and report where it went.
pub async fn open_takedown(
    store: &commons_store::Store,
    body: OpenTakedown,
) -> Result<TakedownView, ApiError> {
    let request = claim::open_takedown(store, &body.cluster_id, &body.requested_by, &body.reason)
        .await
        .map_err(|e| status_for(&e))?;
    let recipients = claim::takedown_recipients(store, &request.id)
        .await
        .map_err(|e| status_for(&e))?;
    Ok(TakedownView {
        id: request.id,
        cluster_id: request.cluster_id,
        recipients,
        to_stewards: request.to_stewards,
    })
}

/// The dashboard for an account.
pub async fn dashboard(
    store: &commons_store::Store,
    account: &str,
) -> Result<DashboardView, ApiError> {
    let dash = claim::dashboard(store, account)
        .await
        .map_err(|e| status_for(&e))?;
    Ok(DashboardView {
        clusters: dash.clusters,
        appearances: dash.appearances,
    })
}

/// The dashboard as the route returns it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DashboardView {
    /// The clusters this account is verified against. Never anyone else's.
    pub clusters: Vec<String>,
    pub appearances: Vec<String>,
}
