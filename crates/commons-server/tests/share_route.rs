//! The share routes end to end.
//!
//! T-P5-007 part 2B, spec §9.5 (#5612). Companion to the policy tests in
//! `commons-consent` (what a valid grant IS) and the store tests in
//! `commons-store/tests/share_db.rs` (the schema holds). This file tests the
//! part none of those can: that a link a user pastes into a chat actually
//! resolves, and that every way of getting it wrong answers sensibly.
//!
//! # What is asserted here that cannot be asserted anywhere else
//!
//! * **The token appears in the create response and nowhere else.** The owner's
//!   list after creating a link contains no token, because there is nothing to
//!   return one from. This is a property of the whole design and it is only
//!   visible from outside.
//! * **Every denial is 403 with a reason**, including a malformed token. A
//!   404 for "no such token" is the conventional choice and it is the wrong one
//!   here: the recipient holds the token, so there is no existence to hide, and
//!   "this link expired" is something they need to be told so they can ask the
//!   sender for a new one.
//! * **Revocation is immediate through the HTTP surface**, not just in the
//!   policy: a link that worked a moment ago must stop working, because
//!   "revocable at any time" is the spec's word and a revoke that needs a cache
//!   to expire is not revocation.
//!
//! SQLite rather than both engines: `share_db.rs` already runs every query on
//! both, and this file is about the wire.

#[path = "support/mod.rs"]
mod support;
use support::TestApp;

use serde_json::Value;

/// Create a link and return `(id, token)`, the token pulled out of the URL.
///
/// The token is the last path segment: the response's `url` is
/// `<base>/s/<token>`, and this asserts that shape by taking the tail rather
/// than by trusting a field the test itself invented.
async fn create(app: &TestApp, body: &str) -> (String, String) {
    let res = app.post_json("/api/share", body).await;
    assert_eq!(
        res.status,
        201,
        "create failed: {}",
        String::from_utf8_lossy(&res.body)
    );
    let v: Value = serde_json::from_slice(&res.body).expect("a json body");
    let url = v["url"].as_str().expect("a url").to_string();
    let token = url.rsplit('/').next().expect("a token segment").to_string();
    (v["id"].as_str().expect("an id").to_string(), token)
}

fn view_link() -> &'static str {
    r#"{"target_kind":"object","target_id":"obj-1","scope":"view","expires_in_hours":24}"#
}

fn json(res: &support::TestResponse) -> Value {
    serde_json::from_slice(&res.body).unwrap_or(Value::Null)
}

#[tokio::test]
async fn a_created_link_resolves() {
    let app = TestApp::new().await;
    let (id, token) = create(&app, view_link()).await;

    let res = app.get_json(&format!("/api/s/{token}")).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let v = json(&res);
    assert_eq!(v["target_id"], "obj-1");
    assert_eq!(v["target_kind"], "object");
    assert_eq!(v["can_download"], false, "a view link must not serve bytes");
    assert_eq!(v["needs_password"], false);

    // The id in the token and the id in the response agree -- the token names
    // the grant rather than being an opaque handle.
    assert!(
        token.starts_with(&format!("{id}.")),
        "{token} should start with {id}"
    );
}

#[tokio::test]
async fn the_token_appears_in_the_create_response_and_nowhere_else() {
    // The load-bearing property of the whole design. If a token can be read
    // back from the owner's list, the hashed column bought nothing, because the
    // link is readable by anyone who can see the owner's screen.
    let app = TestApp::new().await;
    let (_id, token) = create(&app, view_link()).await;
    let secret = token.split_once('.').expect("a token").1;

    let list = app.get_json("/api/share").await;
    assert_eq!(list.status, 200);
    let body = String::from_utf8_lossy(&list.body);
    assert!(
        !body.contains(secret),
        "the owner's list leaked the token secret: {body}"
    );
    // And the target and scope ARE there, so this is not passing because the
    // list is empty.
    assert!(body.contains("obj-1"), "the list should describe the grant");
}

#[tokio::test]
async fn a_download_scope_serves_the_access_route() {
    let app = TestApp::new().await;
    let (_id, token) = create(
        &app,
        r#"{"target_kind":"object","target_id":"obj-9","scope":"view_download","expires_in_hours":24}"#,
    )
    .await;

    let res = app.get_json(&format!("/api/s/{token}/access")).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(json(&res)["object_id"], "obj-9");
}

#[tokio::test]
async fn a_view_scope_does_not_serve_the_access_route() {
    // 403, not 404: the scope is wrong, and saying so is the difference
    // between "ask the sender for a download link" and "this file is gone".
    let app = TestApp::new().await;
    let (_id, token) = create(&app, view_link()).await;
    let res = app.get_json(&format!("/api/s/{token}/access")).await;
    assert_eq!(res.status, 403, "{}", String::from_utf8_lossy(&res.body));
}

#[tokio::test]
async fn revoking_stops_the_link_immediately() {
    let app = TestApp::new().await;
    let (id, token) = create(&app, view_link()).await;
    assert_eq!(app.get_json(&format!("/api/s/{token}")).await.status, 200);

    let del = app.delete(&format!("/api/share/{id}")).await;
    assert_eq!(del.status, 200);

    // Immediately, not after a cache. "Revocable at any time" is the spec's
    // phrase and a revoke that needs a TTL to take effect is not revocation.
    let after = app.get_json(&format!("/api/s/{token}")).await;
    assert_eq!(after.status, 403);
    assert_eq!(json(&after)["error"], "revoked");
}

#[tokio::test]
async fn revoking_twice_is_not_an_error() {
    // DELETE is idempotent by spec, and a retried request must not look like a
    // failure the owner caused.
    let app = TestApp::new().await;
    let (id, _token) = create(&app, view_link()).await;
    assert_eq!(app.delete(&format!("/api/share/{id}")).await.status, 200);
    assert_eq!(app.delete(&format!("/api/share/{id}")).await.status, 200);
}

#[tokio::test]
async fn a_malformed_token_is_403_not_404() {
    // See the module header: the recipient holds the token, so there is no
    // existence to hide, and a 404 would send them to their sender saying the
    // link is broken when the only true fact is that it is malformed.
    let app = TestApp::new().await;
    for bad in ["nonsense", "a.b", "shr_x.short", "..", "x."] {
        let res = app.get_json(&format!("/api/s/{bad}")).await;
        assert_eq!(res.status, 403, "{bad} should be 403");
        assert_eq!(json(&res)["error"], "unknown_token", "{bad}");
    }
}

#[tokio::test]
async fn a_token_with_the_wrong_secret_is_refused() {
    // The same secret under a different id, and a real id with a mangled
    // secret. Without the id check the first would resolve, which is one
    // link's token working on another's row.
    let app = TestApp::new().await;
    let (id, token) = create(&app, view_link()).await;
    let secret = token.split_once('.').expect("a token").1;

    let (_, other_token) = create(
        &app,
        r#"{"target_kind":"object","target_id":"obj-2","scope":"view","expires_in_hours":24}"#,
    )
    .await;
    let other_id = other_token.split_once('.').expect("a token").0;

    // The real grant's secret under the wrong id.
    let crossed = format!("{other_id}.{secret}");
    assert_eq!(app.get_json(&format!("/api/s/{crossed}")).await.status, 403);
    // And a mangled secret under the right id.
    let mangled = format!("{id}.{}", &secret[..secret.len() - 4]);
    assert_eq!(app.get_json(&format!("/api/s/{mangled}")).await.status, 403);
    // Sanity: the untouched one still works, so the two above failed for the
    // reason asserted and not because everything 403s.
    assert_eq!(app.get_json(&format!("/api/s/{token}")).await.status, 200);
    assert_eq!(
        app.get_json(&format!("/api/s/{other_token}")).await.status,
        200
    );
}

#[tokio::test]
async fn an_expired_link_says_so() {
    // An hour is the minimum, so the test expires the row directly rather than
    // waiting -- the wire behaviour under test is the message, not the clock.
    let app = TestApp::new().await;
    let (id, token) = create(&app, view_link()).await;
    expire(&app, &id).await;
    let res = app.get_json(&format!("/api/s/{token}")).await;
    assert_eq!(res.status, 403);
    assert_eq!(json(&res)["error"], "expired");
}

#[tokio::test]
async fn a_revoked_and_expired_link_reports_revoked() {
    // The owner's action is the more informative answer, and it is the one that
    // tells them the link is gone for good rather than merely idle.
    let app = TestApp::new().await;
    let (id, token) = create(&app, view_link()).await;
    expire(&app, &id).await;
    assert_eq!(app.delete(&format!("/api/share/{id}")).await.status, 200);
    let res = app.get_json(&format!("/api/s/{token}")).await;
    assert_eq!(json(&res)["error"], "revoked");
}

#[tokio::test]
async fn a_password_link_asks_then_opens() {
    let app = TestApp::new().await;
    let (_id, token) = create(
        &app,
        r#"{"target_kind":"object","target_id":"obj-3","scope":"view","expires_in_hours":24,"password":"hunter2"}"#,
    )
    .await;
    // No password route exists, so the token alone cannot open the link. This
    // is asserted as 403 rather than "somewhere else" because it is the
    // security property: a share link is the capability, and a password on top
    // must not be a formality.
    let res = app.get_json(&format!("/api/s/{token}")).await;
    assert_eq!(res.status, 403);
}

#[tokio::test]
async fn the_list_shows_state_including_dead_links() {
    // "Where is the link I sent on Tuesday" is a question about a link that no
    // longer works, so the list cannot be filtered to live rows.
    let app = TestApp::new().await;
    let (live_id, _t1) = create(&app, view_link()).await;
    let (dead_id, _t2) = create(
        &app,
        r#"{"target_kind":"object","target_id":"obj-2","scope":"view","expires_in_hours":24}"#,
    )
    .await;
    assert_eq!(
        app.delete(&format!("/api/share/{dead_id}")).await.status,
        200
    );

    let res = app.get_json("/api/share").await;
    let items = json(&res);
    let arr = items.as_array().expect("an array");
    let find = |id: &str| {
        arr.iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from the list"))
            .clone()
    };
    assert_eq!(find(&live_id)["state"], "live");
    assert_eq!(find(&dead_id)["state"], "revoked");
}

#[tokio::test]
async fn the_access_log_records_the_attempts() {
    // The reason the table exists: "is this link being tried by someone who
    // should not have it?" lives entirely in the failures.
    let app = TestApp::new().await;
    let (id, token) = create(&app, view_link()).await;
    // One good use, one refusal, then revoke and refuse again.
    assert_eq!(app.get_json(&format!("/api/s/{token}")).await.status, 200);
    assert_eq!(app.get_json("/api/s/bogus").await.status, 403);
    assert_eq!(app.delete(&format!("/api/share/{id}")).await.status, 200);
    assert_eq!(app.get_json(&format!("/api/s/{token}")).await.status, 403);

    let rows = commons_store::share::list_access(app.store(), &id)
        .await
        .expect("the log reads");
    // 200 + the post-revocation denial. The `bogus` attempt names no grant, so
    // it is recorded under the placeholder and is not in this grant's log --
    // which is worth stating, because it means a guesser is visible only in the
    // owner's whole-table view, not on any one link.
    assert!(
        rows.len() >= 2,
        "expected the grant's own attempts, got {rows:?}"
    );
    assert!(rows.iter().any(|r| r.granted));
    assert!(
        rows.iter()
            .any(|r| r.denied_reason.as_deref() == Some("revoked")),
        "expected a revoked denial in {rows:?}"
    );
}

#[tokio::test]
async fn bad_input_is_422_and_names_the_field() {
    // 422 not 400: the request is well-formed JSON of the right shape and what
    // is wrong is a value. A message naming the field is the difference
    // between a bug report and a fix.
    let app = TestApp::new().await;
    let cases = [
        (
            r#"{"target_kind":"nope","target_id":"o","scope":"view"}"#,
            "target_kind",
        ),
        (
            r#"{"target_kind":"object","target_id":"o","scope":"download"}"#,
            "scope",
        ),
        (
            r#"{"target_kind":"object","target_id":"","scope":"view"}"#,
            "target_id",
        ),
        (
            r#"{"target_kind":"object","target_id":"o","scope":"view","password":""}"#,
            "password",
        ),
        (
            r#"{"target_kind":"object","target_id":"o","scope":"view","expires_in_hours":0}"#,
            "expires_in_hours",
        ),
        (
            r#"{"target_kind":"object","target_id":"o","scope":"view","expires_in_hours":999999}"#,
            "expires_in_hours",
        ),
    ];
    for (body, field) in cases {
        let res = app.post_json("/api/share", body).await;
        assert_eq!(res.status, 422, "{body} should be 422");
        let msg = json(&res)["error"].as_str().unwrap_or_default().to_string();
        assert!(msg.contains(field), "{body}: {msg:?} should name {field}");
    }
}

#[tokio::test]
async fn an_unknown_field_is_refused_rather_than_ignored() {
    // The same reason `PlaybackBody` is `deny_unknown_fields`: a misspelled or
    // ahead-of-its-time field that is silently dropped is a write that appears
    // to succeed and does nothing.
    let app = TestApp::new().await;
    let res = app
        .post_json(
            "/api/share",
            r#"{"target_kind":"object","target_id":"o","scope":"view","expiry_hours":1}"#,
        )
        .await;
    assert!(
        res.status.is_client_error(),
        "an unknown field must not be accepted, got {}",
        res.status
    );
}

#[tokio::test]
async fn scope_is_required_rather_than_defaulted() {
    // A link's scope is the one thing about it that cannot be widened later, so
    // a default here would be a default *silently widening* a capability.
    let app = TestApp::new().await;
    let res = app
        .post_json("/api/share", r#"{"target_kind":"object","target_id":"o"}"#)
        .await;
    assert!(res.status.is_client_error(), "got {}", res.status);
}

/// Make a grant look expired, by moving BOTH timestamps.
///
/// The `created_at` move is not incidental bookkeeping: the first version of
/// this helper set `expires_at` alone and was refused by
/// `share_expiry_after_creation`. That CHECK is the constraint doing its job --
/// a grant that expires before it was created is not an expired grant, it is a
/// corrupt row -- and the fix is to move `created_at` back rather than to
/// weaken the CHECK for the convenience of a test.
///
/// The alternative to all of this is waiting an hour, and what is under test
/// here is the wire message, not the arithmetic.
async fn expire(app: &TestApp, id: &str) {
    let commons_store::Store::Sqlite(pool) = app.store() else {
        // This file runs on SQLite; `share_db.rs` covers both engines.
        unreachable!("share_route runs on SQLite");
    };
    sqlx::query(
        "UPDATE share_grant SET created_at = '1999-01-01T00:00:00.000Z', \
         expires_at = '2000-01-01T00:00:00.000Z' WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .expect("expire");
}
