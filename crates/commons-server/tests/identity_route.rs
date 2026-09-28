//! T-P6-007: request → identity, at the only layer that has a store.
//!
//! The unit table for the header parser lives in `src/identity.rs` because it
//! needs no server. These cases cannot: each one needs a real `AppState` and a
//! real grant row, and this crate's only harness is `TestApp` in
//! `tests/support/`, which is integration-only — `TestApp`'s `state` field is
//! private and there is no accessor, so a `#[cfg(test)]` module in `src/` could
//! not reach it even if it wanted to. The split is a property of the harness.
//!
//! The invariant under all of these: **a share token is an identity with a
//! scope, never a role.** `Scope` exists with exactly two variants because a
//! capability grant is the right shape for a link; turning one into a `Role`
//! would throw that away and let a read-only link write.

mod support;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request};
use commons_server::identity::caller_from_request;
use commons_store::filter_ast::CallerId;
use support::TestApp;

fn bearer(token: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().expect("a header value"),
    );
    h
}

/// Create a link and return `(id, token)`, both pulled from the response.
///
/// The same shape as `share_route.rs`'s helper, and taken the same way — from
/// the `url` the server actually returned rather than from a token this test
/// assembled, so it cannot pass against a broken `assemble_token`.
async fn create_view_link(app: &TestApp) -> (String, String) {
    let res = app
        .post_json(
            "/api/share",
            r#"{"target_kind":"object","target_id":"obj-1","scope":"view","expires_in_hours":24}"#,
        )
        .await;
    assert_eq!(res.status, 201, "{}", String::from_utf8_lossy(&res.body));
    let v: serde_json::Value = serde_json::from_slice(&res.body).expect("json");
    let url = v["url"].as_str().expect("a url");
    let token = url.rsplit('/').next().expect("a token segment").to_string();
    (v["id"].as_str().expect("an id").to_string(), token)
}

/// A bearer token that is well-formed as a header and names no grant.
///
/// The load-bearing assertion is the *negative*: anonymous, not
/// `local_caller()`. A caller who sends a wrong token must not receive **more**
/// than a caller who sends nothing, and the tempting wrong answer here is the
/// local owner, because "we could not identify you, so serve the default"
/// sounds reasonable. It is a privilege escalation.
#[tokio::test]
async fn a_bearer_token_that_names_no_grant_is_anonymous() {
    let app = TestApp::new().await;
    let state = app.state();
    let caller = caller_from_request(state, &bearer("not-a-real-token")).await;
    assert_eq!(caller, CallerId::anonymous());
    assert_eq!(caller.role, commons_core::Role::Public);
    assert!(!caller.role.may_curate());
}

/// A resolved grant is `Public` and carries the grant's id — never a role with
/// permissions. The one assertion in this file that must never be relaxed.
#[tokio::test]
async fn a_resolved_grant_is_public_and_never_privileged() {
    let app = TestApp::new().await;
    let (_id, token) = create_view_link(&app).await;
    let state = app.state();
    let caller = caller_from_request(state, &bearer(&token)).await;

    assert_eq!(
        caller.role,
        commons_core::Role::Public,
        "a View link must never be able to do what Contributor can"
    );
    assert!(!caller.role.may_curate(), "a share grant does not curate");
    assert!(caller.account_id.is_some(), "a grant is still an identity");
}

/// A revoked grant stops resolving. The token is unchanged; the grant is not;
/// and the caller must fall to anonymous rather than to the owner.
#[tokio::test]
async fn a_revoked_grant_stops_resolving() {
    let app = TestApp::new().await;
    let (id, token) = create_view_link(&app).await;
    let state = app.state();
    assert_ne!(
        caller_from_request(state, &bearer(&token)).await,
        CallerId::anonymous(),
        "a live link is not anonymous, or this test proves nothing"
    );

    assert_eq!(app.delete(&format!("/api/share/{id}")).await.status, 200);

    let state = app.state();
    let caller = caller_from_request(state, &bearer(&token)).await;
    assert_eq!(caller, CallerId::anonymous());
    assert!(
        !caller.role.may_curate(),
        "a revoked link is anonymous, not the owner"
    );
}

/// No credentials is the old behaviour, exactly.
///
/// This is what makes the design safe to land in one commit: 22 existing route
/// tests depend on `local_caller()` being the fallback. If it ever stops being,
/// they all fail at once and the failure names this.
///
/// **The load-bearing assertion is `account_id`, not the role** — and that took
/// a failing test to learn. `local_caller()` is `Role::Public` with
/// `account_id: Some("local")`: it is the *library owner* by **consent tier**,
/// which `media_path` derives from a present account, not by role. So an
/// assertion of the shape "the fallback may curate" is simply false, and
/// writing one would have encoded a wrong mental model of the auth model into
/// the file whose whole job is to pin it. The pair that actually distinguishes
/// local from anonymous is `account_id`: `Some("local")` vs `None`. That pair
/// is also the `unverified`-visibility subtlety `local_caller`'s own doc
/// comment explains at length.
#[tokio::test]
async fn no_credentials_is_still_the_local_owner() {
    let app = TestApp::new().await;
    let state = app.state();
    let caller = caller_from_request(state, &HeaderMap::new()).await;
    assert_eq!(caller, commons_server::media::local_caller());
    assert_eq!(
        caller.account_id.as_deref(),
        Some("local"),
        "owner-ness here is a CONSENT TIER from a present account, not a role"
    );
    // And the counterpart, because the pair is the whole distinction: a
    // credential that does not resolve has no account at all.
    let state = app.state();
    let anonymous = caller_from_request(state, &bearer("nope")).await;
    assert_eq!(anonymous.account_id, None);
}

/// A revoked token on a real media request does not reach the bytes.
///
/// The unit-level tests prove the function returns an anonymous `CallerId`;
/// this proves the router uses that answer for a serving decision — the part
/// that `grep -c Role commons-server/` returning 0 could never show.
#[tokio::test]
async fn a_revoked_bearer_token_does_not_reach_the_media_bytes() {
    let app = TestApp::new().await;
    let (id, token) = create_view_link(&app).await;
    assert_eq!(app.delete(&format!("/api/share/{id}")).await.status, 200);

    let response = app
        .send_raw(
            Request::builder()
                .uri("/media/obj-1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("a request"),
        )
        .await;
    // Not the status code that matters — that 200 is *rejected* is. A revoked
    // link with the owner's consent would be a 200 here.
    assert_ne!(
        response.status, 200,
        "a revoked grant must not reach the media bytes"
    );
}
