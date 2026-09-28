//! T-P6-007 step 3: the `/api/v1` prefix.
//!
//! The routing tests live in `src/lib.rs`'s own module, where the local
//! `app(config(..))` harness can reach the router directly. This file exists for
//! the one assertion that harness cannot make: a real media object served
//! **byte for byte identically** at the old path and the versioned one. It
//! needs `TestApp` and a real fixture, and `TestApp` lives in `tests/`, so a
//! `src/` test cannot reach it.
//!
//! The split is the harness, not the subject — the same reason
//! `src/identity.rs` has a parser table and the behavioural cases are here.

mod support;

use axum::body::Body;
use axum::http::Request;
use support::TestApp;

/// A real object is served identically at both paths.
///
/// The src/ tests only prove both paths 404 for an *absent* object, which a
/// router that dropped every `/api/v1` route would also satisfy. This one
/// proves a 200 at both, with the same bytes.
///
/// **Byte comparison, not status comparison.** Two handlers could both answer
/// 200 and disagree about the body, and a consumer experiences that as
/// corrupted media rather than as a version problem — which is why a
/// status-only assertion here would be measuring the wrong thing.
#[tokio::test]
async fn a_real_object_is_served_identically_at_both_paths() {
    let app = TestApp::new().await;
    let object_id = support::media_fixture_named(&app, b"hello bytes", "mp4").await;

    let old = fetch(&app, format!("/media/{object_id}")).await;
    let v1 = fetch(&app, format!("/api/v1/media/{object_id}")).await;

    assert_eq!(old.status, 200, "the unversioned path must serve");
    assert_eq!(v1.status, 200, "the versioned path must serve too");
    assert_eq!(old.body, v1.body, "the same bytes at both paths");
}

/// A plain `async fn` rather than an `async move` closure: the closure form
/// needs boxing to satisfy the borrow checker and reads as though the capture
/// were the interesting part.
async fn fetch(app: &TestApp, path: String) -> support::TestResponse {
    app.send_raw(
        Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("a request"),
    )
    .await
}

/// A JSON route at both paths, where the body is a document rather than bytes.
///
/// The media route is the one a consumer notices first, but the JSON routes are
/// the ones an OpenAPI-generated client will bind to, and a shape difference
/// there is what breaks a generated SDK rather than a player.
#[tokio::test]
async fn a_json_route_answers_the_same_at_both_paths() {
    let app = TestApp::new().await;
    let object_id = support::media_fixture_named(&app, b"bytes", "mp4").await;

    let old = app.get_json(&format!("/media/{object_id}/chapters")).await;
    let v1 = app
        .get_json(&format!("/api/v1/media/{object_id}/chapters"))
        .await;
    assert_eq!(old.status, v1.status);
    assert_eq!(old.body, v1.body, "the same document at both paths");
}

/// The share routes are public, and they live at a doubled prefix.
///
/// `/api/v1/api/share` reads like a mistake and is not: the handlers parse
/// their own `/api` prefix, and re-rooting them to `/api/v1/share` would mean a
/// second set of handlers. Asserting the doubled form is what stops a future
/// reader "fixing" it — and a reader who does fix it will break every script
/// that has already discovered the real path.
#[tokio::test]
async fn a_share_can_be_created_through_the_versioned_api() {
    let app = TestApp::new().await;
    let res = app
        .post_json(
            "/api/v1/api/share",
            r#"{"target_kind":"object","target_id":"obj-1","scope":"view","expires_in_hours":24}"#,
        )
        .await;
    assert_eq!(
        res.status,
        201,
        "the versioned share route must work: {}",
        String::from_utf8_lossy(&res.body)
    );
}

/// `GET /api/v1/openapi.json` serves the document.
///
/// The unit tests check the document as a value; this checks it is REACHABLE.
/// Those are different failures — a document built and never served is a
/// document no consumer can find, and it is the failure mode a unit test cannot
/// see.
#[tokio::test]
async fn the_document_is_served_at_the_versioned_path() {
    let app = TestApp::new().await;
    let res = app.get_json("/api/v1/openapi.json").await;
    assert_eq!(res.status, 200, "the document must be served");
    let body = String::from_utf8_lossy(&res.body);
    assert!(
        body.contains("\"openapi\""),
        "the response is not an OpenAPI document: {}",
        &body[..body.len().min(200)]
    );
    assert!(
        body.contains("/media/{object_id}"),
        "the document has no paths -- the `paths(...)` list is not wired up"
    );
    // The PATH KEYS must be OpenAPI's `{param}` form, not axum's `:param`. A
    // generated client reads these keys and sends the names from them verbatim,
    // so a colon here becomes a wrong parameter name at runtime.
    //
    // The check is on the keys specifically, not on the whole body: utoipa copies
    // each handler's doc comment into `summary`/`description`, and those
    // comments legitimately quote the axum form (`GET /media/:object_id`) when
    // they are talking about the Rust route. Searching the whole document
    // flagged that prose and would have driven someone to rewrite accurate
    // documentation to satisfy a test.
    let doc: serde_json::Value = serde_json::from_str(&body).expect("the document is valid JSON");
    for (path, _item) in doc["paths"].as_object().expect("paths is an object") {
        assert!(
            !path.contains(':'),
            "path key {path} uses axum's `:param` form; OpenAPI requires `{{param}}`, \
             and a generated client would send the wrong parameter name"
        );
    }
}

/// And it is NOT served unversioned, because the versioned mount is the public
/// one and an unversioned copy invites a consumer onto the path §11.5 says
/// carries no compatibility promise.
#[tokio::test]
async fn the_document_is_only_served_under_the_version_prefix() {
    let app = TestApp::new().await;
    assert_eq!(app.get_json("/openapi.json").await.status, 404);
}
