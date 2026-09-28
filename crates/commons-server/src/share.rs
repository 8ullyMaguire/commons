//! `/api/share` and `/api/s/:token` — issuing a link, and using one.
//!
//! T-P5-007 part 2B. Spec §9.5 (#5612) and §15.10; design in
//! `docs/spec/t-p5-007-share-links.md`.
//!
//! # Three decisions that are not forced by the store
//!
//! 1. **The create response is the only place the token ever appears.** There is
//!    no endpoint that returns it again, because there is nothing to return it
//!    from: the secret is stored as a hash (migration 0022). A client that
//!    reloads its management page has a list of *links it already made*, not
//!    links it can re-copy — which is the point. A token the owner cannot
//!    re-read is a token that cannot leak from the owner's own screen.
//!
//! 2. **Every denial is 403 with the reason in the body — except a malformed
//!    token, which is also 403.** The tempting alternative is 404 for "no such
//!    token", which is more polite about not confirming existence. It is wrong
//!    here: the recipient already holds the token, so there is no existence to
//!    hide, and the recipient genuinely needs to be told "this link expired" so
//!    they can ask the sender for a new one. A share page that says "not found"
//!    for an expired link sends people to their sender saying the link is
//!    broken, when the only true fact is that it is old.
//!
//!    `unknown_token` still covers both "no such id" and "wrong secret", so the
//!    response does not confirm which — only the *owner* sees a grant id, and
//!    they can already see their own grants.
//!
//! 3. **`view` does not serve bytes, and says so with 403 rather than 404.** A
//!    404 would tell a download-scoped guesser that the route does not exist
//!    rather than that their scope is wrong. §9.5's two scopes differ in exactly
//!    one behaviour, and a 403 names it.
//!
//! # Why there is no `POST /api/s/:token/unlock`
//!
//! The spec's surface sketch had one. It is not needed and it is a liability: a
//! password-protected link would need a cookie/session to carry the unlock, and
//! then the *download* route would have to decide whether that cookie is
//! sufficient — two routes trusting a session, and one of them handing out file
//! bytes on the strength of it. Instead the password is a field on the access
//! request, and both the view and the download route run the same
//! `commons_consent::share::resolve`. There is exactly one thing that can grant
//! access, and it is called at both.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use commons_consent::share::{
    assemble_token, split_token, DeniedReason, GrantState, Scope, ShareGrant, ShareSecret,
    TargetKind,
};
use commons_store::share::{
    get_grant_by_id, insert_grant, list_grants, record_access, record_granted, revoke_grant,
    ShareAccessRow, ShareGrantRow,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

/// The longest lifetime a grant may be issued for.
///
/// A cap, and a generous one, because the shape of the bug it prevents is a
/// client sending `expires_in_hours: 0` or `i64::MAX` and getting a link that
/// never expires. §9.5 says "time-limited"; a grant with no upper bound is not
/// time-limited, and a permanent share link is the same object as a permanent
/// public URL with an audit trail.
const MAX_TTL_HOURS: i64 = 24 * 90;

/// The shortest, because a link that is already dead when it is created is a
/// link the owner will send to somebody before they notice.
const MIN_TTL_HOURS: i64 = 1;

/// `POST /api/share` — issue a link.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateShare {
    pub target_kind: String,
    pub target_id: String,
    /// `view` or `view_download`. Required rather than defaulted: a link's
    /// scope is the one thing about it that cannot be widened later, and a
    /// default here would be a default *silently widening* a capability.
    pub scope: String,
    #[serde(default)]
    pub expires_in_hours: Option<i64>,
    #[serde(default)]
    pub password: Option<String>,
}

/// The create response. The token is here and nowhere else, ever.
#[derive(Debug, Serialize)]
pub struct CreatedShare {
    pub id: String,
    /// The full link, ready to paste. Built from `config.public_base_url` so a
    /// deployment behind a reverse proxy does not hand out a link with the
    /// wrong host in it.
    pub url: String,
    pub expires_at: String,
}

/// `GET /api/share` — the owner's links, dead ones included.
#[derive(Debug, Serialize)]
pub struct ShareListItem {
    pub id: String,
    pub target_kind: String,
    pub target_id: String,
    pub scope: String,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
    pub created_at: Option<String>,
    pub access_count: i64,
    pub last_accessed_at: Option<String>,
    /// `live`, `expired` or `revoked`. Sent rather than left to the client to
    /// work out, because the rule is "revoked outranks expired" and a client
    /// that implements it differently shows an owner the wrong state on a link
    /// they deliberately killed.
    pub state: String,
}

pub async fn create_share(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateShare>,
) -> Response {
    let Some(kind) = TargetKind::from_str_opt(&body.target_kind) else {
        return bad_request("target_kind must be 'object' or 'smart_collection'");
    };
    let Some(scope) = Scope::from_str_opt(&body.scope) else {
        return bad_request("scope must be 'view' or 'view_download'");
    };
    if body.target_id.trim().is_empty() {
        return bad_request("target_id must not be empty");
    }
    // An empty password is worse than none: `password_hash` becomes Some, the
    // recipient is shown a prompt, and anything at all unlocks it.
    if body.password.as_deref() == Some("") {
        return bad_request("password must not be empty; omit it for no password");
    }
    let hours = body.expires_in_hours.unwrap_or(24);
    if !(MIN_TTL_HOURS..=MAX_TTL_HOURS).contains(&hours) {
        return bad_request(&format!(
            "expires_in_hours must be between {MIN_TTL_HOURS} and {MAX_TTL_HOURS}"
        ));
    }

    let now = Utc::now();
    let id = format!("shr_{}", Uuid::new_v4());
    let secret = ShareSecret::mint();
    let token = assemble_token(&id, &secret);
    let row = ShareGrantRow {
        id: id.clone(),
        token_hash: secret.hash().to_vec(),
        scope: scope.as_str().to_string(),
        target_kind: kind.as_str().to_string(),
        target_id: body.target_id.clone(),
        password_hash: body
            .password
            .as_deref()
            .map(ShareSecret::hash_password)
            .map(|h| h.to_vec()),
        expires_at: Some(now + Duration::hours(hours)),
        revoked_at: None,
        created_at: Some(now),
        access_count: 0,
        last_accessed_at: None,
    };
    if let Err(e) = insert_grant(&state.store, &row).await {
        return internal(e);
    }
    let url = format!(
        "{}/s/{}",
        state.config.public_base_url.trim_end_matches('/'),
        token
    );
    (
        StatusCode::CREATED,
        Json(CreatedShare {
            id,
            url,
            expires_at: row.expires_at.map(ts).unwrap_or_default(),
        }),
    )
        .into_response()
}

pub async fn list_share(State(state): State<Arc<AppState>>) -> Response {
    match list_grants(&state.store).await {
        Ok(rows) => {
            let now = Utc::now();
            let items: Vec<ShareListItem> = rows
                .iter()
                .map(|r| ShareListItem {
                    id: r.id.clone(),
                    target_kind: r.target_kind.clone(),
                    target_id: r.target_id.clone(),
                    scope: r.scope.clone(),
                    expires_at: r.expires_at.map(ts),
                    revoked_at: r.revoked_at.map(ts),
                    created_at: r.created_at.map(ts),
                    access_count: r.access_count,
                    last_accessed_at: r.last_accessed_at.map(ts),
                    state: state_of(r.revoked_at, r.expires_at, now).to_string(),
                })
                .collect();
            (StatusCode::OK, Json(items)).into_response()
        }
        Err(e) => internal(e),
    }
}

pub async fn revoke_share(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match revoke_grant(&state.store, &id, Utc::now()).await {
        // 200 whether or not it was already revoked: DELETE is idempotent by
        // spec, and a 404 on the second call would make a retried request look
        // like a failure the owner caused.
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "revoked": true }))).into_response(),
        Err(e) => internal(e),
    }
}

/// `GET /api/s/:token` — resolve a token to its target.
#[derive(Debug, Serialize)]
pub struct ResolvedShare {
    pub target_kind: String,
    pub target_id: String,
    /// Whether the bytes are fetchable with this token. A client that gets
    /// `false` should not show a download button that will 403.
    pub can_download: bool,
    /// `true` when a password is still needed. The client shows a field; it
    /// does not get a second chance to guess, because `resolve` is called
    /// again with the password.
    pub needs_password: bool,
}

pub async fn resolve_share(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    let (_row, grant) = match load_and_resolve(&state, &token, None, &headers).await {
        Ok(v) => v,
        Err(response) => return *response,
    };
    (
        StatusCode::OK,
        Json(ResolvedShare {
            target_kind: grant.target_kind.as_str().to_string(),
            target_id: grant.target_id,
            can_download: grant.scope.can_download(),
            // A `PasswordRequired` denial never reaches here, so this is only
            // ever `false` on a granted resolve -- but it is sent rather than
            // omitted so the client has one shape to render.
            needs_password: false,
        }),
    )
        .into_response()
}

/// `GET /api/s/:token/access` — the object row, if the scope allows it.
///
/// Named `access` rather than `object` because it is not a general object read:
/// it is a capability-scoped view, and a name that sounds like a normal resource
/// invites a future change that treats it as one.
pub async fn share_access(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    let (row, grant) = match load_and_resolve(&state, &token, None, &headers).await {
        Ok(v) => v,
        Err(response) => return *response,
    };
    if !grant.scope.can_download() {
        // 403, not 404: the scope is wrong, and saying so is the whole
        // difference between "ask the sender for a download link" and "this
        // file is gone". The reason is `unknown_token` rather than a new
        // variant: from the recipient's side a too-narrow scope and a wrong
        // token are the same situation -- this link does not open that -- and
        // distinguishing them would confirm that the token is real.
        return denied_response(DeniedReason::UnknownToken);
    }
    // The counter is best-effort. A failed bump must not fail the request: the
    // recipient has a valid link, and answering 500 for a counter is a worse
    // outcome than a count that is one low.
    if let Err(e) = record_granted(&state.store, &row, Utc::now()).await {
        tracing::warn!(grant = %row.id, error = %e, "share counter update failed");
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "object_id": grant.target_id,
            "scope": grant.scope.as_str(),
        })),
    )
        .into_response()
}

/// Load the grant for a token and apply the whole policy to it.
///
/// The one place the three steps happen in order, so a route cannot get the
/// order wrong:
///
/// 1. **Split** the token. A malformed one never reaches the database.
/// 2. **Look up** by id. Unfiltered — the policy needs the revoked and expired
///    rows to report *why*.
/// 3. **Resolve**: signature, then revocation, then expiry, then password.
///
/// Returns the row as well as the outcome, because the granted arm needs the
/// stored `access_count` to bump it and re-reading would be a race against the
/// very counter being written.
/// Resolve a token to its grant, or to the response explaining why not.
///
/// `pub(crate)` since T-P6-007: `identity::caller_from_request` needs the whole
/// policy -- signature, revocation, expiry, password -- as one call rather than
/// re-implementing four of them to get a `CallerId`. It stays crate-private
/// because it is not an HTTP surface, and the `Err` arm is an axum `Response`,
/// which no external caller could do anything useful with.
pub(crate) async fn load_and_resolve(
    state: &Arc<AppState>,
    token: &str,
    password: Option<&str>,
    headers: &HeaderMap,
) -> Result<(ShareGrantRow, ShareGrant), Box<Response>> {
    let now = Utc::now();
    // 1. Split. A malformed token never reaches the database -- there is no id
    // to look up, and a string that cannot parse is not worth a query.
    let Some((id, _secret)) = split_token(token) else {
        log_attempt(
            state,
            None,
            token,
            false,
            Some(DeniedReason::UnknownToken),
            headers,
        )
        .await;
        return Err(Box::new(denied_response(DeniedReason::UnknownToken)));
    };
    // 2. Look up, unfiltered: the policy needs the revoked and expired rows to
    //    report WHY, and a query that filtered here would make every denial
    //    look like a bad link.
    let row = match get_grant_by_id(&state.store, id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            log_attempt(
                state,
                None,
                token,
                false,
                Some(DeniedReason::UnknownToken),
                headers,
            )
            .await;
            return Err(Box::new(denied_response(DeniedReason::UnknownToken)));
        }
        Err(e) => return Err(Box::new(internal(e))),
    };

    // 3. The whole policy, in one call: signature, then revocation, then
    //    expiry, then password. The ORDER is the policy, so it lives in
    //    `commons-consent::share::resolve` and not here.
    let grant = to_policy(&row);
    match commons_consent::share::resolve(token, &grant, password, now) {
        commons_consent::share::Resolution::Granted { .. } => {
            log_attempt(state, Some(&row.id), token, true, None, headers).await;
            Ok((row, grant))
        }
        commons_consent::share::Resolution::Denied { reason } => {
            log_attempt(state, Some(&row.id), token, false, Some(reason), headers).await;
            Err(Box::new(denied_response(reason)))
        }
    }
}

/// The store's row as the policy type.
///
/// A conversion rather than a re-decode of the row's strings, so the policy
/// module stays the only place that knows what `scope` means. `expires_at` and
/// `created_at` are `Option` in the store (a corrupt timestamp parses to `None`)
/// and a `None` expiry is treated as already expired — fail closed, because a
/// grant whose expiry cannot be read must not be a grant that never expires.
fn to_policy(row: &ShareGrantRow) -> ShareGrant {
    ShareGrant {
        id: row.id.clone(),
        token_hash: <[u8; 32]>::try_from(row.token_hash.as_slice()).unwrap_or([0u8; 32]),
        scope: Scope::from_str_opt(&row.scope).unwrap_or(Scope::View),
        target_kind: TargetKind::from_str_opt(&row.target_kind).unwrap_or(TargetKind::Object),
        target_id: row.target_id.clone(),
        password_hash: row
            .password_hash
            .as_deref()
            .and_then(|h| <[u8; 32]>::try_from(h).ok()),
        expires_at: row.expires_at.unwrap_or(Utc::now() - Duration::seconds(1)),
        revoked_at: row.revoked_at,
        created_at: row.created_at.unwrap_or_else(Utc::now),
        access_count: row.access_count,
        last_accessed_at: row.last_accessed_at,
    }
}

/// One access-log row.
///
/// Best-effort by design: a failure here is logged and swallowed, because the
/// recipient is waiting on a page load and a log write is not worth failing it
/// for. The one case that *is* worth a `tracing::warn!` is a write that fails
/// for a reason other than the grant being absent, because a silently empty log
/// is the failure mode this whole table exists to prevent.
async fn log_attempt(
    state: &Arc<AppState>,
    grant_id: Option<&str>,
    _token: &str,
    granted: bool,
    reason: Option<DeniedReason>,
    headers: &HeaderMap,
) {
    // The grant_id is a bare token id for a *malformed* token too, which does
    // not exist. Recorded anyway under a placeholder rather than skipped, so
    // "somebody is guessing at this library" is visible in the owner's log as
    // a run of unknown_token rows.
    let entry = ShareAccessRow {
        id: format!("sha_{}", Uuid::new_v4()),
        grant_id: grant_id.unwrap_or("-").to_string(),
        at: Utc::now(),
        ip: headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(',').next().unwrap_or(v).trim().to_string()),
        user_agent: headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        granted,
        denied_reason: reason.map(|r| r.as_str().to_string()),
    };
    if let Err(e) = record_access(&state.store, &entry).await {
        tracing::warn!(error = %e, "share access log write failed");
    }
}

fn denied_response(reason: DeniedReason) -> Response {
    let status = match reason {
        // Everything is 403. See the module header for why a malformed token
        // does not get a 404: the recipient holds the token, so there is no
        // existence to hide, and "this link expired" is something they need to
        // be told so they can ask for a new one.
        DeniedReason::Expired
        | DeniedReason::Revoked
        | DeniedReason::UnknownToken
        | DeniedReason::PasswordRequired
        | DeniedReason::WrongPassword => StatusCode::FORBIDDEN,
    };
    (
        status,
        Json(serde_json::json!({ "error": reason.as_str() })),
    )
        .into_response()
}

fn state_of(
    revoked_at: Option<chrono::DateTime<Utc>>,
    expires_at: Option<chrono::DateTime<Utc>>,
    now: chrono::DateTime<Utc>,
) -> &'static str {
    match GrantState::of(
        revoked_at,
        expires_at.unwrap_or(now - Duration::seconds(1)),
        now,
    ) {
        GrantState::Live => "live",
        GrantState::Expired => "expired",
        GrantState::Revoked => "revoked",
    }
}

fn ts(t: chrono::DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn bad_request(why: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({ "error": why })),
    )
        .into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "share request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal error" })),
    )
        .into_response()
}
