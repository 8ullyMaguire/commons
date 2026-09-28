//! T-P6-010: `after:` paging on the `objects` resolver, end to end.
//!
//! A separate file from `graphql_route.rs` for two reasons, and the second is
//! the one that matters: `graphql_route.rs` is about the *surface* — does each
//! operation resolve, is each input field handled — while this is about one
//! field's behaviour *across a sequence of requests*. A paging bug produces no
//! wrong single response. It produces a correct first page, a correct second
//! page, and a walk that never ends or skips a row.
//!
//! **Which is why the load-bearing test here is a WALK, not a round trip.** A
//! round trip proves a cursor encodes and decodes; it says nothing about whether
//! paging terminates, repeats a boundary row, or drops one. Those are the three
//! ways keyset paging fails, and none of them is visible in any single request.
//!
//! The harness is shared and `#![allow(dead_code)]`, so a second file costs
//! nothing — the alternative is a second copy of the fixture builders, and two
//! copies drift.

mod support;

use std::collections::HashSet;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request};
use serde_json::{json, Value};
use support::{media_fixture_at_tier, TestApp};

// ---------------------------------------------------------------------------
// Fixtures and helpers
// ---------------------------------------------------------------------------

/// The same document `graphql_route.rs` uses, so a difference in what these
/// tests see is a difference in paging and not in the selection set.
fn objects_document() -> &'static str {
    r#"query Objects($input: PageInput!) {
         objects(input: $input) {
           totalCount
           pageInfo { hasNextPage hasPreviousPage startCursor endCursor }
           nodes { id kind title }
         }
       }"#
}

/// A request with NO Authorization header — **which is the local OWNER**, not an
/// anonymous caller. `caller_from_request` falls back to `media::local_caller()`
/// and `identity.rs` calls that fallback "the absence of a design".
///
/// This is written down because the first version of
/// `resuming_does_not_widen_visibility` walked every page with a headerless
/// request and called it "anonymous". It is the owner, so the test asserted the
/// owner does not see the owner's own `unverified` objects and failed — and
/// T-P6-008 had already been bitten by exactly this, in the same repo, for the
/// same reason. The fix there was a `bearer()` token naming no grant; this is
/// the same fix applied one layer down.
async fn graphql(app: &TestApp, query: &str, variables: Value) -> support::TestResponse {
    graphql_with(app, query, variables, HeaderMap::new()).await
}

/// Well-formed bearer token naming no grant, which `identity_route.rs` pins as
/// resolving to anonymous and NOT to `local_caller()`.
fn anonymous() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        header::AUTHORIZATION,
        "Bearer no-such-grant-token".parse().expect("a header"),
    );
    h
}

async fn graphql_with(
    app: &TestApp,
    query: &str,
    variables: Value,
    headers: HeaderMap,
) -> support::TestResponse {
    let body = json!({"query": query, "variables": variables}).to_string();
    let mut builder = Request::builder()
        .method("POST")
        .uri("/graphql")
        .header(header::CONTENT_TYPE, "application/json");
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    app.send_raw(builder.body(Body::from(body)).expect("a request"))
        .await
}

fn body_of(res: &support::TestResponse) -> Value {
    serde_json::from_slice(&res.body).unwrap_or_else(|e| {
        panic!(
            "the response is not JSON ({e}): {}",
            String::from_utf8_lossy(&res.body)
        )
    })
}

fn error_message(res: &support::TestResponse) -> String {
    let v = body_of(res);
    v["errors"][0]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an errors[0].message, got {v}"))
        .to_string()
}

/// One page, or a panic naming what came back.
///
/// Every walk step goes through here, so a page that is an *error* cannot be
/// mistaken for a page that is *empty* — the two look identical to a client
/// that ignores `errors`, which is the failure this file exists to rule out.
async fn page(
    app: &TestApp,
    first: usize,
    after: Option<&str>,
    sort: &str,
) -> (Vec<String>, Value) {
    let input = json!({ "first": first, "sort": sort, "after": after });
    let res = graphql(app, objects_document(), json!({ "input": input })).await;
    assert_eq!(res.status, 200, "GraphQL errors are 200 with a body");
    let v = body_of(&res);
    assert!(v.get("errors").is_none(), "a page request errored: {v}");
    let ids = v["data"]["objects"]["nodes"]
        .as_array()
        .unwrap_or_else(|| panic!("expected data.objects.nodes, got {v}"))
        .iter()
        .map(|n| n["id"].as_str().expect("a node id").to_string())
        .collect();
    (ids, v["data"]["objects"]["pageInfo"].clone())
}

/// Seed `n` visible objects at one tier, and return their ids.
///
/// One tier, deliberately: `self_published` is in `ConsentTiers::PUBLIC`, so
/// every seeded object is visible to the local owner and the walk is testing
/// paging rather than consent. `resuming_does_not_widen_visibility` below is
/// where the tiers are mixed on purpose.
async fn seed_visible(app: &TestApp, n: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        // Distinct bytes per row so nothing dedupes them into one object: two
        // objects with identical content collapse to a single row, and a walk
        // over 7 objects that silently holds 4 is a test that passes for the
        // wrong reason.
        ids.push(
            media_fixture_at_tier(app, format!("row {i} bytes").as_bytes(), "self_published").await,
        );
    }
    ids
}

/// Walk every object, `limit` at a time, and return the ids in the order the
/// server gave them.
///
/// A hard cap on the number of pages so a server that never stops offering a
/// `hasNextPage` fails the test instead of hanging it. That is a real
/// termination bug — a client would loop forever — and a hanging test reports
/// it far less clearly than "walked 50 pages and gave up".
async fn walk(app: &TestApp, limit: usize, sort: &str) -> (Vec<String>, usize) {
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let (ids, info) = page(app, limit, cursor.as_deref(), sort).await;
        seen.extend(ids);
        pages += 1;
        if pages > 50 {
            panic!("paging did not terminate after 50 pages; {seen:?}");
        }
        if info["hasNextPage"] == false {
            // And the last page must not hand back a cursor. A server that keeps
            // emitting one past the end gives a client an infinite loop that
            // looks like a very slow query.
            assert!(
                info["endCursor"].is_null(),
                "the last page must have a null endCursor: {info}"
            );
            return (seen, pages);
        }
        let next = info["endCursor"].as_str().unwrap_or_else(|| {
            panic!("hasNextPage is true but endCursor is null -- a client cannot resume: {info}")
        });
        assert_ne!(
            Some(next.to_string()),
            cursor,
            "the server returned the same cursor twice; the walk is not advancing"
        );
        cursor = Some(next.to_string());
    }
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_walk_covers_every_row_exactly_once() {
    // 7 at a page of 3: three pages, and the last is SHORT. A divisor (6 at 3)
    // would give two full pages and never exercise the short-final-page case,
    // which is where `has_more` has to go false.
    let app = TestApp::new().await;
    let seeded = seed_visible(&app, 7).await;

    let (seen, pages) = walk(&app, 3, "date").await;

    assert_eq!(pages, 3, "7 rows at 3 per page is 3 pages: 3+3+1");
    assert_eq!(
        seen.len(),
        seeded.len(),
        "paging lost rows: expected {}, saw {}\n{seen:?}",
        seeded.len(),
        seen.len()
    );
    // The duplicate check is SEPARATE and not redundant with the count. A bug
    // that repeats one boundary row AND drops another leaves the count
    // unchanged, and a count-only assertion passes it.
    let unique: HashSet<&String> = seen.iter().collect();
    assert_eq!(
        unique.len(),
        seeded.len(),
        "a row came back more than once, so another was skipped:\n{seen:?}"
    );
    // And the same SET, not just the same count.
    let expected: HashSet<&String> = seeded.iter().collect();
    assert_eq!(unique, expected, "paging returned a different set of rows");
}

#[tokio::test]
async fn a_walk_in_the_other_direction_covers_every_row_too() {
    // The sort is part of the cursor's identity, so ASC and DESC are separate
    // walks with separate cursors. Testing only DESC would leave the ascending
    // keyset predicate — the one whose comparison operator is the other way
    // round — entirely unexercised.
    let app = TestApp::new().await;
    let seeded = seed_visible(&app, 5).await;
    let (seen, pages) = walk(&app, 2, "title").await;
    assert_eq!(pages, 3, "5 rows at 2 per page is 3 pages");
    assert_eq!(seen.len(), seeded.len(), "{seen:?}");
    let unique: HashSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), seeded.len(), "a row repeated: {seen:?}");
}

#[tokio::test]
async fn the_cursor_a_page_returns_is_accepted_as_the_next_after() {
    // The round trip, on its own. It is NOT the load-bearing test — `a_walk_*`
    // is — but a cursor that is emitted and then refused is a distinct failure
    // from one that is never emitted, and it would pass a test that only read
    // `endCursor` without sending it back.
    let app = TestApp::new().await;
    seed_visible(&app, 4).await;

    let (first_ids, first_info) = page(&app, 2, None, "date").await;
    assert_eq!(first_ids.len(), 2);
    assert_eq!(first_info["hasNextPage"], true);
    let cursor = first_info["endCursor"].as_str().expect("a cursor");

    let (second_ids, _) = page(&app, 2, Some(cursor), "date").await;
    assert!(
        !second_ids.is_empty(),
        "the cursor this server emitted must resume to rows, not to nothing"
    );
    // And the resumed page is not the first page again. Without this, a server
    // that ignores the cursor and always returns page 1 would pass every
    // "the cursor decodes" assertion while paging forever.
    assert_ne!(
        first_ids, second_ids,
        "the second page is the first page: the cursor was ignored"
    );
}

#[tokio::test]
async fn the_last_page_has_no_cursor_and_the_first_page_has_no_previous() {
    let app = TestApp::new().await;
    seed_visible(&app, 2).await;
    let (_, info) = page(&app, 10, None, "date").await;
    assert_eq!(info["hasNextPage"], false, "{info}");
    assert!(info["endCursor"].is_null(), "{info}");
    // Unchanged by forward paging, and worth pinning: a cursor FORWARD is not a
    // cursor BACKWARD, so claiming a previous page would render a back arrow
    // that cannot work.
    assert_eq!(info["hasPreviousPage"], false, "{info}");
    assert!(info["startCursor"].is_null(), "{info}");
}

// ---------------------------------------------------------------------------
// Consent: the test a paging feature can silently break
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resuming_does_not_widen_visibility() {
    // **This is the test most likely to be the only one that catches a real
    // bug**, and it is worth saying why: every other test in this file uses a
    // single-tier fixture or reads a single page. A cursor that cached the first
    // page's filter — or that was treated as a capability rather than a
    // position — would satisfy all of them.
    //
    // A cursor carries a sort fingerprint and NO caller. If the resolver ever
    // stopped passing the caller through to `query_sorted` on the resumed page,
    // page 1 would still look right and page 2 would leak.
    let app = TestApp::new().await;
    // 3 public, 2 unverified. The unverified ones are visible to the LOCAL
    // OWNER, so this walk as the owner must return all 5 — and the point is
    // made from the other side by the anonymous walk below.
    let public_ids = seed_visible(&app, 3).await;
    let mut hidden_ids = Vec::new();
    for i in 0..2 {
        hidden_ids.push(
            media_fixture_at_tier(&app, format!("secret {i}").as_bytes(), "unverified").await,
        );
    }

    let (owner_saw, pages) = walk(&app, 2, "date").await;
    assert!(
        pages >= 2,
        "the walk must cross a page boundary to mean anything"
    );
    assert_eq!(owner_saw.len(), 5, "the owner sees all five: {owner_saw:?}");
    for id in &hidden_ids {
        assert!(
            owner_saw.contains(id),
            "the owner must see their own unverified object {id}: {owner_saw:?}"
        );
    }

    // Now the same walk as an ANONYMOUS caller, page by page. The unverified
    // ids must not appear on ANY page -- and "any page" is the assertion that
    // matters, because a leak confined to page 2 is exactly the bug.
    let mut anon_saw: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let input = json!({ "first": 2, "sort": "date", "after": cursor });
        // The token on EVERY page, not just the first: a cursor carries no
        // caller, so the identity layer has to be consulted per request, and a
        // test that authenticated once and then paged anonymously would be
        // asserting the opposite of what it says.
        let res = graphql_with(
            &app,
            objects_document(),
            json!({ "input": input }),
            anonymous(),
        )
        .await;
        let v = body_of(&res);
        assert!(v.get("errors").is_none(), "{v}");
        let info = &v["data"]["objects"]["pageInfo"];
        for n in v["data"]["objects"]["nodes"].as_array().expect("nodes") {
            let id = n["id"].as_str().expect("an id");
            assert!(
                !hidden_ids.iter().any(|h| h == id),
                "an anonymous caller saw the unverified object {id} -- a cursor \
                 must not be a capability"
            );
            anon_saw.push(id.to_string());
        }
        if info["hasNextPage"] == false {
            break;
        }
        cursor = info["endCursor"].as_str().map(|s| s.to_string());
    }
    assert_eq!(
        anon_saw.len(),
        public_ids.len(),
        "the anonymous walk should see exactly the three public objects: {anon_saw:?}"
    );
}

// ---------------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------------

/// Assert that a cursor is refused, and that the message names the CAUSE.
///
/// Asserting merely "there is an error" would pass if the resolver were broken
/// for every input — a 500-shaped refusal is still a refusal. The client renders
/// this string, and "bad request" is a string nobody can act on.
fn assert_refused(res: &support::TestResponse, must_name: &str, what: &str) {
    let v = body_of(res);
    assert!(
        v.get("errors").is_some(),
        "{what} was accepted; expected a refusal: {v}"
    );
    // A refused cursor must not come back with a page, or a client that reads
    // `data` before `errors` gets the first page and pages forever.
    assert!(
        v["data"].is_null(),
        "{what} came back with data as well as an error: {v}"
    );
    let msg = error_message(res);
    assert!(
        msg.contains(must_name),
        "{what}: the message must name {must_name:?} so the client can act on \
         it, got: {msg}"
    );
}

#[tokio::test]
async fn a_cursor_from_another_text_sort_is_refused_rather_than_seeking() {
    // **Two TEXT sorts, and that is the whole point.** `Date` and `Title` are
    // both `Value::Str` and both arity 2, and `after_binds` checks length only,
    // so a date cursor seeked in a title sort binds cleanly, runs, and pages
    // with the wrong column -- returning rows and erroring nowhere.
    //
    // A text/int pair (`date` vs `rating`) would be refused by the *type* check
    // whether or not the fingerprint existed, so it would pass with the
    // fingerprint deleted and prove nothing about it.
    let app = TestApp::new().await;
    seed_visible(&app, 4).await;

    let (_, date_info) = page(&app, 2, None, "date").await;
    let date_cursor = date_info["endCursor"]
        .as_str()
        .expect("a cursor")
        .to_string();

    let input = json!({ "first": 2, "sort": "title", "after": date_cursor });
    let res = graphql(&app, objects_document(), json!({ "input": input })).await;
    assert_refused(
        &res,
        "different sort",
        "a date cursor used with a title sort",
    );

    // The negative control, and it belongs in the same test: the SAME request
    // without the cursor must succeed on the SAME fixture. Without it, a
    // resolver that failed for every reason would pass this test.
    let (ids, _) = page(&app, 2, None, "title").await;
    assert!(!ids.is_empty(), "the control must return rows");
}

#[tokio::test]
async fn a_malformed_cursor_is_refused_not_ignored() {
    // The input the DELETED `an_after_cursor_is_refused_rather_than_silently_
    // ignored` test used: `{"v":1}` with no fingerprint. Still an error, now for
    // a stated reason rather than "not implemented" — and the difference matters,
    // because the two call for different client behaviour.
    let app = TestApp::new().await;
    seed_visible(&app, 2).await;
    let input = json!({ "first": 10, "after": "eyJ2IjoxfQ" });
    let res = graphql(&app, objects_document(), json!({ "input": input })).await;
    assert_refused(&res, "cursor", "a cursor with no fingerprint");
}

#[tokio::test]
async fn a_cursor_that_is_not_base64url_is_refused() {
    let app = TestApp::new().await;
    seed_visible(&app, 2).await;
    let input = json!({ "first": 10, "after": "not base64 !!" });
    let res = graphql(&app, objects_document(), json!({ "input": input })).await;
    assert_refused(&res, "base64url", "a cursor that is not base64url");
}

#[tokio::test]
async fn a_cursor_with_the_wrong_number_of_keys_is_refused() {
    // The right fingerprint, the wrong arity. This is the case that would
    // otherwise reach `after_binds`, which asserts with a PANIC — and a panic in
    // a request handler is a 500 for what is a client error. Reaching it from the
    // wire must be an error with a message instead.
    let app = TestApp::new().await;
    // FOUR objects at a page of TWO. The first version of this test seeded two
    // and asked for two, so `has_more` was false, `endCursor` was null, and the
    // `.expect("a cursor")` fired. The fixture had quietly removed the thing
    // under test: a page with no successor is exactly the case that emits no
    // cursor, and that is the case where the arity check is unreachable.
    seed_visible(&app, 4).await;

    // Take a real cursor -- and therefore a real fingerprint -- from a real
    // page, then hand back the same envelope with one key instead of two.
    let (_, info) = page(&app, 2, None, "date").await;
    assert_eq!(
        info["hasNextPage"], true,
        "the fixture must have a next page: {info}"
    );
    let real = info["endCursor"]
        .as_str()
        .expect("a cursor: a page with a successor must emit one");
    // Decode, rewrite `k` to a single entry, re-encode.
    //
    // **Using the crate's own codec, not a hand-rolled one.** The first version
    // of this test carried its own base64url encode/decode pair, and its
    // DECODER failed on the server's real cursor -- so the test was measuring my
    // codec's compatibility with the server rather than the server's arity
    // check. A helper that reimplements the thing under test is a second
    // source of truth, and here it was simply wrong.
    let json_bytes = commons_store::filter_ast::base64url_decode(real)
        .expect("the server's own cursor decodes with the crate's own decoder");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&json_bytes).expect("the server's own cursor is JSON");
    let original_keys = doc["k"].as_array().expect("a k array").len();
    assert!(
        original_keys >= 2,
        "expected at least two keys, got {original_keys}"
    );
    // Take the LAST key, not the first. `o.date` is nullable, so a page
    // beginning at the top of the sort can carry a NULL in slot 0 -- and
    // `doc["k"][0]` on a JSON array indexes fine, but the first version of this
    // line indexed with a *string* key and produced a bare string, so the
    // envelope deserialized as `Shape` rather than reaching the arity check at
    // all. The refusal was real; it just named the wrong cause, which is what
    // this assertion exists to prevent.
    let last = doc["k"]
        .as_array()
        .expect("a k array")
        .last()
        .expect("a key")
        .clone();
    doc["k"] = json!([last]);
    let tampered = commons_store::filter_ast::base64url_encode(doc.to_string().as_bytes());

    let input = json!({ "first": 10, "after": tampered });
    let res = graphql(&app, objects_document(), json!({ "input": input })).await;
    assert_refused(
        &res,
        "number of keys",
        "a cursor with one key instead of two",
    );
}

// ---------------------------------------------------------------------------
// The aggregate: no bad cursor ever produces a page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_bad_cursor_is_an_error_rather_than_a_page() {
    // The single assertion is the point: NONE of these may come back with data.
    // A `from_url` that "helpfully" defaulted would return the first page under
    // `hasNextPage: true`, which is the loop the old blanket refusal existed to
    // prevent -- and it would look like a working server.
    let app = TestApp::new().await;
    seed_visible(&app, 3).await;

    let bad: Vec<(&str, &str)> = vec![
        ("empty string", ""),
        ("not base64", "!!!"),
        ("valid base64, not json", "aGVsbG8"),
        ("json, not an object", "WzEsMiwzXQ"),
        ("no version", "eyJmIjoiZGVhZGJlZWYiLCJrIjpbXX0"),
    ];
    for (what, cursor) in bad {
        let input = json!({ "first": 2, "after": cursor });
        let res = graphql(&app, objects_document(), json!({ "input": input })).await;
        let v = body_of(&res);
        assert!(
            v.get("errors").is_some() && v["data"].is_null(),
            "{what:?} ({cursor:?}) produced a page instead of an error: {v}"
        );
    }

    // And the control: the same request with NO cursor is a page. Without this,
    // "every bad cursor errors" is also satisfied by a server that errors on
    // everything.
    let (ids, _) = page(&app, 2, None, "date").await;
    assert!(!ids.is_empty(), "the control must return rows");
}
