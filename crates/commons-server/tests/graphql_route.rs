//! T-P6-008: `POST /graphql`, against a real store and a real router.
//!
//! These are integration tests because the claim is about the *interaction* of
//! four things: the operation-name dispatcher, the identity layer, the consent
//! clause inside `query_sorted`, and the wire encoding. A unit test of the
//! resolver would prove it calls the store in the expected order, which was
//! never the property in doubt. `TestApp` is integration-only (`support/mod.rs`
//! is not reachable from a `src/` test module), which is the same constraint
//! `identity_route.rs` records.
//!
//! # The test that matters
//!
//! `objects_is_consent_filtered_for_three_different_callers` is the one to read
//! first. A GraphQL resolver that reads the store without a caller returns the
//! owner's library to everyone, and **a test that only checks "a query returns
//! something" would pass**. So the same document goes to three callers — the
//! local owner, an anonymous request, and a `view` share link — against a
//! library seeded with both a public and an `unverified` object, and the three
//! answers must differ.
//!
//! The `unverified` fixture is what makes this possible, and it is not
//! optional: every other fixture in the harness is `self_published`, which is
//! in `ConsentTiers::PUBLIC`, so a library of only those cannot distinguish "the
//! resolver honoured the caller" from "the resolver served everybody". That is
//! the same lesson `media.rs`'s mutation comment records about a fixture of
//! only publicly-visible tiers.

mod support;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request};
use serde_json::{json, Value};
use support::{media_fixture_at_tier, TestApp};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn bearer(token: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().expect("a header value"),
    );
    h
}

/// The UI's own document, verbatim in shape.
///
/// Taken from `ui/src/lib/api/client.ts`'s `OBJECTS_QUERY` so a change to the
/// client's selection set is a change to this test's input rather than a silent
/// divergence. The fields this server cannot answer are still listed, because
/// the client sends them and the response must still be a valid document.
fn objects_document() -> &'static str {
    r#"query Objects($input: PageInput!) {
         objects(input: $input) {
           totalCount
           pageInfo { hasNextPage hasPreviousPage startCursor endCursor }
           nodes {
             id kind title date rating organized coverPath
             width height durationMs producer performers tags folder
           }
         }
       }"#
}

async fn graphql(app: &TestApp, query: &str, variables: Value) -> support::TestResponse {
    graphql_with(app, query, variables, HeaderMap::new()).await
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

/// The `errors[0].message`, or a panic naming what came back instead.
///
/// Asserting on the message text is the point for the refusal tests: the client
/// renders this string to a user, and a message that says only "invalid input"
/// is a message nobody can act on.
fn error_message(res: &support::TestResponse) -> String {
    let v = body_of(res);
    v["errors"][0]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an errors[0].message, got {v}"))
        .to_string()
}

fn node_ids(res: &support::TestResponse) -> Vec<String> {
    let v = body_of(res);
    v["data"]["objects"]["nodes"]
        .as_array()
        .unwrap_or_else(|| panic!("expected data.objects.nodes, got {v}"))
        .iter()
        .map(|n| n["id"].as_str().expect("a node id").to_string())
        .collect()
}

/// A library with one public object and one `unverified` object.
///
/// `unverified` is the separating tier: the local owner sees it, a `view` share
/// link does not, and neither does an anonymous request. A library of only
/// public objects cannot tell those three apart, which is the whole point.
async fn two_tier_library(app: &TestApp) -> (String, String) {
    let public_id = media_fixture_at_tier(app, b"public bytes", "self_published").await;
    let private_id = media_fixture_at_tier(app, b"private bytes", "unverified").await;
    (public_id, private_id)
}

/// A `view` share link, taken from the URL the server returned.
async fn view_link(app: &TestApp) -> String {
    let res = app
        .post_json(
            "/api/share",
            r#"{"target_kind":"object","target_id":"o-anything","scope":"view","expires_in_hours":24}"#,
        )
        .await;
    assert_eq!(res.status, 201, "{}", String::from_utf8_lossy(&res.body));
    let v: Value = serde_json::from_slice(&res.body).expect("json");
    let url = v["url"].as_str().expect("a url");
    url.rsplit('/').next().expect("a token").to_string()
}

// ---------------------------------------------------------------------------
// The surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn all_four_operations_resolve_against_a_live_server() {
    let app = TestApp::new().await;
    let (public_id, _) = two_tier_library(&app).await;

    // 1. Objects
    let res = graphql(&app, objects_document(), json!({"input": {"first": 10}})).await;
    assert_eq!(res.status, 200);
    let v = body_of(&res);
    assert!(v.get("errors").is_none(), "Objects returned errors: {v}");
    assert!(
        v["data"]["objects"]["nodes"].is_array(),
        "Objects returned no node array: {v}"
    );

    // 2. BulkTags
    let res = graphql(&app, "query BulkTags { tags { id name } }", json!({})).await;
    let v = body_of(&res);
    assert!(v.get("errors").is_none(), "BulkTags returned errors: {v}");
    assert!(v["data"]["tags"].is_array(), "no tag array: {v}");

    // 3. BulkApplyTag — against a real tag, on a real object.
    let tag_id = seed_tag(&app, "graphql-test-tag").await;
    let res = graphql(
        &app,
        r#"mutation BulkApplyTag($tagId: ID!, $target: BulkTarget!, $source: String) {
             bulkApplyTag(tagId: $tagId, target: $target, source: $source) {
               applied skippedInvisible requested
             }
           }"#,
        json!({
            "tagId": tag_id,
            "target": {"kind": "IDS", "ids": [public_id]},
            "source": "t-p6-008-test"
        }),
    )
    .await;
    let v = body_of(&res);
    assert!(
        v.get("errors").is_none(),
        "BulkApplyTag returned errors: {v}"
    );
    assert_eq!(v["data"]["bulkApplyTag"]["applied"], 1, "{v}");

    // 4. CreateAllMissing
    let res = graphql(
        &app,
        r#"mutation CreateAllMissing($rows: [String!]!) {
             createAllMissing(rows: $rows) { created existing refused }
           }"#,
        json!({"rows": [format!("t-p6-008 row {}", uuid::Uuid::new_v4())]}),
    )
    .await;
    let v = body_of(&res);
    assert!(
        v.get("errors").is_none(),
        "CreateAllMissing returned errors: {v}"
    );
    assert_eq!(v["data"]["createAllMissing"]["created"], 1, "{v}");
}

#[tokio::test]
async fn the_graphql_endpoint_is_not_under_api_v1() {
    let app = TestApp::new().await;
    // Serves at its own path...
    let direct = graphql(&app, "query BulkTags { tags { id } }", json!({})).await;
    assert_eq!(direct.status, 200, "POST /graphql must serve");

    // ...and NOT under the version prefix, because that path is the one the
    // OpenAPI document describes and the compatibility promise attaches to.
    let prefixed = graphql(&app, "query BulkTags { tags { id } }", json!({})).await;
    assert_eq!(prefixed.status, 200);

    let under_v1 = app
        .send_raw(
            Request::builder()
                .method("POST")
                .uri("/api/v1/graphql")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"query": "query BulkTags { tags { id } }"}).to_string(),
                ))
                .expect("a request"),
        )
        .await;
    assert_eq!(
        under_v1.status, 404,
        "/api/v1/graphql must not exist: the OpenAPI document describes \
         /api/v1, and a POST endpoint there would be inside the compatibility \
         promise"
    );
}

// ---------------------------------------------------------------------------
// The consent gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn objects_is_consent_filtered_for_three_different_callers() {
    let app = TestApp::new().await;
    let (public_id, private_id) = two_tier_library(&app).await;
    let token = view_link(&app).await;
    let doc = objects_document();
    let vars = json!({"input": {"first": 50}});

    // The local owner sees both. NOTE: a request with NO Authorization header
    // IS the local owner — `caller_from_request` falls back to
    // `media::local_caller()` and `identity.rs` calls that fallback "the
    // absence of a design, named so that it shows up in a diff when credentials
    // arrive". The first version of this test used a headerless request as its
    // "anonymous" caller and failed: the owner and the anonymous caller were
    // the same caller, so the assertion was vacuous and the leak it was meant
    // to catch would have passed.
    let owner = node_ids(&graphql(&app, doc, vars.clone()).await);

    // Anonymous means a well-formed bearer token that names no grant, which
    // `identity_route.rs` pins as resolving to anonymous and *not* to
    // `local_caller()`. Sending "we could not identify you, so serve the
    // default" would be a privilege escalation.
    let anonymous =
        node_ids(&graphql_with(&app, doc, vars.clone(), bearer("no-such-grant-token")).await);

    // A `view` share link sees only the public one — and NOT `unverified`. It
    // maps to `ConsentTiers::PUBLIC`; only `view_download` reaches OWNER.
    let shared = node_ids(&graphql_with(&app, doc, vars, bearer(&token)).await);

    // The three must genuinely be three different answers, or the assertions
    // below prove nothing.
    assert_ne!(
        owner, anonymous,
        "the owner and an anonymous caller received the same page, so the \
         identity layer is not being consulted at all"
    );

    // The load-bearing assertions, in the order that makes the failure legible.
    assert!(
        owner.contains(&private_id),
        "the local owner must see the unverified object; owner saw {owner:?}"
    );
    assert!(
        owner.contains(&public_id),
        "the local owner must see the public object; owner saw {owner:?}"
    );
    assert!(
        !anonymous.contains(&private_id),
        "an anonymous caller saw the unverified object: {anonymous:?} — this is \
         the data leak, and it is invisible to any test whose fixture is all \
         public"
    );
    assert!(
        !shared.contains(&private_id),
        "a view-scoped share link saw the unverified object: {shared:?} — a \
         `view` grant is a capability and must not exceed its scope"
    );
    assert!(
        anonymous.contains(&public_id) && shared.contains(&public_id),
        "both non-owner callers must still see the public object; anonymous \
         {anonymous:?}, shared {shared:?}"
    );
}

#[tokio::test]
async fn a_malformed_filter_is_an_error_not_an_empty_page() {
    let app = TestApp::new().await;
    two_tier_library(&app).await;
    let res = graphql(
        &app,
        objects_document(),
        json!({"input": {"first": 10, "filter": "not-base64url!!"}}),
    )
    .await;
    // HTTP 200 with an `errors` array: the client's `query()` reads `errors`
    // and shows the message, so a 4xx would lose it.
    assert_eq!(res.status, 200);
    let msg = error_message(&res);
    assert!(
        msg.contains("filter"),
        "the message must name the offending field, got: {msg}"
    );
    assert!(
        msg.contains("refused") || msg.contains("whole library"),
        "the message must say it was refused rather than ignored, got: {msg}"
    );
    // And it must NOT have returned the whole library as a fallback.
    assert!(
        body_of(&res)["data"].is_null(),
        "a refused filter must not fall back to an empty-or-everything page"
    );
}

#[tokio::test]
async fn an_unknown_sort_key_is_an_error_not_a_silent_default() {
    let app = TestApp::new().await;
    two_tier_library(&app).await;
    let res = graphql(
        &app,
        objects_document(),
        json!({"input": {"first": 10, "sort": "titel"}}),
    )
    .await;
    assert_eq!(res.status, 200);
    let msg = error_message(&res);
    // The message must name what was accepted, because a client that sent a
    // typo needs the list, not a rejection.
    assert!(msg.contains("titel"), "must echo the bad key: {msg}");
    for accepted in ["date", "title", "kind"] {
        assert!(
            msg.contains(accepted),
            "the message must list `{accepted}` among the accepted keys: {msg}"
        );
    }
}

#[tokio::test]
async fn an_unknown_direction_is_refused_rather_than_assumed() {
    let app = TestApp::new().await;
    two_tier_library(&app).await;
    let res = graphql(
        &app,
        objects_document(),
        json!({"input": {"first": 10, "direction": "sideways"}}),
    )
    .await;
    assert_eq!(res.status, 200);
    let msg = error_message(&res);
    assert!(msg.contains("sideways"), "must echo the bad value: {msg}");
    assert!(msg.contains("asc") && msg.contains("desc"), "{msg}");
}

#[tokio::test]
async fn a_page_size_over_the_ceiling_is_refused() {
    let app = TestApp::new().await;
    two_tier_library(&app).await;
    let res = graphql(
        &app,
        objects_document(),
        json!({"input": {"first": 100_000}}),
    )
    .await;
    let msg = error_message(&res);
    assert!(msg.contains("100000"), "must echo the number: {msg}");
    assert!(msg.contains("at most 200"), "must state the ceiling: {msg}");
}

// ---------------------------------------------------------------------------
// Paging: refused, not ignored
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_after_cursor_is_refused_rather_than_silently_ignored() {
    let app = TestApp::new().await;
    two_tier_library(&app).await;
    let res = graphql(
        &app,
        objects_document(),
        json!({"input": {"first": 10, "after": "eyJ2IjoxfQ"}}),
    )
    .await;
    assert_eq!(res.status, 200);
    let msg = error_message(&res);
    assert!(
        msg.contains("after"),
        "the message must name the field: {msg}"
    );
    // The load-bearing part: it says what would have happened otherwise.
    assert!(
        msg.contains("first page") || msg.contains("silently"),
        "the message must say that ignoring it would return the first page \
         again -- that is the failure being avoided, and a client cannot \
         otherwise tell a refusal from a bug: {msg}"
    );
    assert!(
        body_of(&res)["data"].is_null(),
        "a refused cursor must not come back with a page"
    );
}

#[tokio::test]
async fn the_page_cursors_are_null_rather_than_a_string_that_would_be_refused() {
    let app = TestApp::new().await;
    two_tier_library(&app).await;
    let res = graphql(&app, objects_document(), json!({"input": {"first": 10}})).await;
    let v = body_of(&res);
    let info = &v["data"]["objects"]["pageInfo"];
    // The keys must be PRESENT and null. Omitting them is a different document,
    // and a client that reads `endCursor` and sends it back would then be
    // sending undefined.
    assert!(
        info.get("endCursor").is_some(),
        "endCursor must be present, not omitted: {info}"
    );
    assert!(
        info["endCursor"].is_null(),
        "endCursor must be null, not a string this server would then refuse: {info}"
    );
    assert!(
        info.get("startCursor").is_some() && info["startCursor"].is_null(),
        "startCursor must be present-and-null: {info}"
    );
    // No previous page exists without a cursor to go back to, so claiming one
    // would render a back arrow that cannot work.
    assert_eq!(info["hasPreviousPage"], false, "{info}");
}

// ---------------------------------------------------------------------------
// The field mapping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn total_count_is_present_and_null_rather_than_a_count_star() {
    let app = TestApp::new().await;
    two_tier_library(&app).await;
    let res = graphql(&app, objects_document(), json!({"input": {"first": 10}})).await;
    let v = body_of(&res);
    let conn = &v["data"]["objects"];
    assert!(
        conn.get("totalCount").is_some(),
        "totalCount must be present: {conn}"
    );
    assert!(
        conn["totalCount"].is_null(),
        "totalCount must be null. query.rs documents why at length: a COUNT(*) \
         over the same filter is a second scan of the same inner join on the \
         hot path, and a keyset page cannot produce an honest exact count \
         anyway. Got {conn}"
    );
}

#[tokio::test]
async fn every_field_the_client_selects_is_served_or_null_on_purpose() {
    let app = TestApp::new().await;
    let (public_id, _) = two_tier_library(&app).await;
    let res = graphql(&app, objects_document(), json!({"input": {"first": 10}})).await;
    let v = body_of(&res);
    let node = v["data"]["objects"]["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == public_id)
        .expect("the seeded object is in the page")
        .clone();

    // The three fields the store actually has, served.
    assert_eq!(node["id"], public_id, "{node}");
    assert_eq!(node["kind"], "clip", "{node}");
    assert!(
        !node["title"].is_null(),
        "the object row has a title: {node}"
    );

    // Every field the client selects must be PRESENT. A missing key is a
    // different failure from a null: the client's type says `string | null`, so
    // a null renders as absent data, while a missing key is a document the
    // client did not ask for and cannot read.
    for field in [
        "date",
        "rating",
        "organized",
        "coverPath",
        "width",
        "height",
        "durationMs",
        "producer",
        "performers",
        "tags",
        "folder",
    ] {
        assert!(
            node.get(field).is_some(),
            "`{field}` is selected by the client and must be present in the \
             response, even as null. Got {node}"
        );
    }

    // And the ones with no source are null for a NAMED reason, not by accident.
    // Spec §1b has the per-field table; these are the six with no column.
    for field in ["coverPath", "width", "height", "folder"] {
        assert!(
            node[field].is_null(),
            "`{field}` has no column anywhere in the schema, so it must be \
             null rather than a plausible-looking value. Got {}",
            node[field]
        );
    }
    assert!(
        node["performers"]
            .as_array()
            .expect("performers is a list")
            .is_empty(),
        "`performers` needs an aggregate that would multiply rows and break \
         the keyset page; it is an empty list, not a null. Got {node}"
    );
    assert!(
        node["rating"].is_null(),
        "`rating` has no column -- object carries rating_sum/rating_count, and \
         the mean is F1's decision to make, not this ticket's. Got {node}"
    );
}

// ---------------------------------------------------------------------------
// BulkTarget: the hazards the client's own type documents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_bulk_target_round_trips_the_client_exact_shape() {
    // The client sends `kind: "IDS"` in SCREAMING_SNAKE_CASE. With no library
    // generating this mapping, the spelling is code somebody wrote, and a
    // mismatch here is a client that cannot tag anything — reported as a
    // validation error the user cannot connect to a capitalisation change.
    let target: commons_api::graphql::BulkTarget =
        serde_json::from_value(json!({"kind": "IDS", "ids": ["a", "b"]}))
            .expect("the client's IDS shape must deserialize");
    assert_eq!(target.kind, commons_api::graphql::BulkTargetKind::Ids);
    assert_eq!(
        target.ids.as_deref(),
        Some(["a".to_string(), "b".to_string()].as_slice())
    );

    let target: commons_api::graphql::BulkTarget =
        serde_json::from_value(json!({"kind": "QUERY", "query": "abc"}))
            .expect("the client's QUERY shape must deserialize");
    assert_eq!(target.kind, commons_api::graphql::BulkTargetKind::Query);

    // And the field names inside it are the client's, not ours.
    let target: commons_api::graphql::BulkTarget =
        serde_json::from_value(json!({"kind": "IDS", "ids": ["a"], "excluded": ["z"]}))
            .expect("`excluded` must be accepted by the wire type");
    assert_eq!(
        target.excluded.as_deref(),
        Some(["z".to_string()].as_slice())
    );

    // A kind nobody defined must be refused, not defaulted.
    let bad =
        serde_json::from_value::<commons_api::graphql::BulkTarget>(json!({"kind": "EVERYTHING"}));
    assert!(
        bad.is_err(),
        "an unknown BulkTargetKind must fail to deserialize rather than \
         defaulting to something that might mean 'all'"
    );
}

#[tokio::test]
async fn an_empty_id_list_tags_nothing_rather_than_everything() {
    let app = TestApp::new().await;
    let (public_id, private_id) = two_tier_library(&app).await;
    let tag_id = seed_tag(&app, "empty-ids-tag").await;

    let res = graphql(
        &app,
        r#"mutation BulkApplyTag($tagId: ID!, $target: BulkTarget!) {
             bulkApplyTag(tagId: $tagId, target: $target) { applied requested }
           }"#,
        json!({"tagId": tag_id, "target": {"kind": "IDS", "ids": []}}),
    )
    .await;
    let msg = error_message(&res);
    assert!(
        msg.contains("tag nothing") || msg.contains("empty"),
        "the message must say what an empty list means, got: {msg}"
    );

    // The decisive assertion: nothing was tagged. If an empty list were treated
    // as an absent filter, this would tag the whole library.
    let tagged = tags_on(&app, &public_id).await;
    assert!(
        !tagged.contains(&tag_id),
        "an empty id list tagged the public object — it was read as 'everything'"
    );
    let tagged = tags_on(&app, &private_id).await;
    assert!(
        !tagged.contains(&tag_id),
        "an empty id list tagged the unverified object — the whole-library read"
    );
}

#[tokio::test]
async fn an_excluded_list_is_refused_rather_than_ignored() {
    let app = TestApp::new().await;
    let (public_id, _) = two_tier_library(&app).await;
    let tag_id = seed_tag(&app, "excluded-tag").await;

    // `excluded` looks like a filter. Ignoring it and applying the tag anyway
    // is how a bulk edit touches something the user protected.
    let res = graphql(
        &app,
        r#"mutation BulkApplyTag($tagId: ID!, $target: BulkTarget!) {
             bulkApplyTag(tagId: $tagId, target: $target) { applied }
           }"#,
        json!({
            "tagId": tag_id,
            "target": {"kind": "IDS", "ids": [public_id], "excluded": ["some-other-id"]}
        }),
    )
    .await;
    let msg = error_message(&res);
    assert!(
        msg.contains("excluded"),
        "the message must name the unsupported field: {msg}"
    );
    let tagged = tags_on(&app, &public_id).await;
    assert!(
        !tagged.contains(&tag_id),
        "nothing may be written when the target carried an unsupported field"
    );
}

#[tokio::test]
async fn bulk_apply_tag_reports_skipped_invisible_rather_than_failing() {
    let app = TestApp::new().await;
    let (public_id, private_id) = two_tier_library(&app).await;
    let tag_id = seed_tag(&app, "skip-invisible-tag").await;

    // Name both. The local owner can see both, so this is about the *counter*
    // existing and being the server's number.
    let res = graphql(
        &app,
        r#"mutation BulkApplyTag($tagId: ID!, $target: BulkTarget!) {
             bulkApplyTag(tagId: $tagId, target: $target) {
               applied skippedInvisible requested
             }
           }"#,
        json!({
            "tagId": tag_id,
            "target": {"kind": "IDS", "ids": [public_id, private_id]}
        }),
    )
    .await;
    let v = body_of(&res);
    assert!(v.get("errors").is_none(), "{v}");
    let r = &v["data"]["bulkApplyTag"];
    // `requested` is what the client asked for, `applied` is what the server
    // did. The two differ the moment a row changed, and the difference is the
    // number a user trusts about what just happened.
    assert_eq!(r["requested"], 2, "{r}");
    assert_eq!(r["applied"], 2, "{r}");
    assert!(
        r.get("skippedInvisible").is_some(),
        "the counter must be present: {r}"
    );
}

// ---------------------------------------------------------------------------
// The dispatcher
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unknown_operation_names_the_four_that_exist() {
    let app = TestApp::new().await;
    let res = graphql(&app, "query Everything { everything }", json!({})).await;
    assert_eq!(res.status, 200);
    let msg = error_message(&res);
    for op in ["Objects", "BulkTags", "BulkApplyTag", "CreateAllMissing"] {
        assert!(msg.contains(op), "the message must list `{op}`: {msg}");
    }
}

#[tokio::test]
async fn an_anonymous_document_is_refused_rather_than_guessed_at() {
    let app = TestApp::new().await;
    // `{ objects(input: {}) { nodes { id } } }` — the shorthand form. It carries
    // no operation name, and guessing which resolver meant would answer a
    // different question than was asked.
    let res = graphql(&app, "{ objects(input: {}) { nodes { id } } }", json!({})).await;
    assert_eq!(res.status, 200);
    let msg = error_message(&res);
    assert!(msg.contains("no operation"), "got: {msg}");
}

#[tokio::test]
async fn a_request_that_is_not_json_is_a_graphql_error_not_a_500() {
    let app = TestApp::new().await;
    let res = app
        .send_raw(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{not json at all"))
                .expect("a request"),
        )
        .await;
    // 200 with an `errors` array, because that is what the client's `query()`
    // reads. A 400 or 500 loses the message behind a transport failure.
    assert_eq!(res.status, 200, "a parse failure is still HTTP 200 here");
    let msg = error_message(&res);
    assert!(msg.contains("GraphQL request"), "got: {msg}");
}

#[tokio::test]
async fn a_missing_required_variable_is_refused_with_its_name() {
    let app = TestApp::new().await;
    let res = graphql(&app, objects_document(), json!({})).await;
    let msg = error_message(&res);
    assert!(msg.contains("input"), "must name the variable: {msg}");
}

// ---------------------------------------------------------------------------
// Tiers: accepted, ignored, and said out loud
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tiers_hint_cannot_widen_what_the_caller_sees() {
    let app = TestApp::new().await;
    let (_, private_id) = two_tier_library(&app).await;
    let token = view_link(&app).await;

    // A `view` link asking for the owner's tiers must not get them. The tiers
    // field is a narrowing hint; the consent clause is the only thing that
    // decides, and a client-supplied list on top would be a widening dressed
    // as a filter.
    let res = graphql_with(
        &app,
        objects_document(),
        json!({"input": {"first": 50, "tiers": ["owner", "unverified"]}}),
        bearer(&token),
    )
    .await;
    let ids = node_ids(&res);
    assert!(
        !ids.contains(&private_id),
        "a `view` link with an OWNER tier hint saw the unverified object: {ids:?}"
    );
}

#[tokio::test]
async fn tags_are_not_consent_gated_and_that_is_written_down() {
    let app = TestApp::new().await;
    seed_tag(&app, "not-gated-tag").await;
    let token = view_link(&app).await;

    // A `view` share link can read the tag list. §14.1 gates objects; `tag` has
    // no consent_record join because nothing about a tag is something a share
    // link could grant or withhold. This test exists so that reading is a
    // stated decision rather than an oversight — an operation that takes no
    // identity looks exactly like one that forgot to.
    let res = graphql_with(
        &app,
        "query BulkTags { tags { id name } }",
        json!({}),
        bearer(&token),
    )
    .await;
    let v = body_of(&res);
    assert!(v.get("errors").is_none(), "BulkTags for a share link: {v}");
    let names: Vec<&str> = v["data"]["tags"]
        .as_array()
        .expect("tags")
        .iter()
        .map(|t| t["name"].as_str().expect("a name"))
        .collect();
    assert!(
        names.contains(&"not-gated-tag"),
        "a view link must be able to read the tag list; saw {names:?}"
    );
}

// ---------------------------------------------------------------------------
// Write path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_all_missing_is_idempotent_and_reports_both_counts() {
    let app = TestApp::new().await;
    let title = format!("t-p6-008 idempotent {}", uuid::Uuid::new_v4());
    let doc = r#"mutation CreateAllMissing($rows: [String!]!) {
                   createAllMissing(rows: $rows) { created existing refused }
                 }"#;

    let first = graphql(&app, doc, json!({"rows": [title.clone()]})).await;
    let v = body_of(&first);
    assert_eq!(v["data"]["createAllMissing"]["created"], 1, "{v}");
    assert_eq!(v["data"]["createAllMissing"]["existing"], 0, "{v}");

    // The same row again: nothing created, one existing. The action exists
    // *because* "already there" is a common outcome, and a response carrying
    // only a success count cannot tell a user that most of their pasted rows
    // were already in their library.
    let second = graphql(&app, doc, json!({"rows": [title]})).await;
    let v = body_of(&second);
    assert_eq!(v["data"]["createAllMissing"]["created"], 0, "{v}");
    assert_eq!(v["data"]["createAllMissing"]["existing"], 1, "{v}");
    assert!(
        v["data"]["createAllMissing"].get("refused").is_some(),
        "the refusal count must be present even at 0 — the store documents it \
         as the field a shared deployment would use: {v}"
    );
}

// ---------------------------------------------------------------------------
// Helpers needing the store
// ---------------------------------------------------------------------------

async fn seed_tag(app: &TestApp, name: &str) -> String {
    let id = format!("t-{}", uuid::Uuid::new_v4().simple());
    sqlx::query("INSERT INTO tag (id, name) VALUES (?, ?)")
        .bind(&id)
        .bind(name)
        .execute(app.store().pool())
        .await
        .expect("the tag is inserted");
    id
}

async fn tags_on(app: &TestApp, object_id: &str) -> Vec<String> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT t.id FROM object_tag ot JOIN tag t ON t.id = ot.tag_id WHERE ot.object_id = ?",
    )
    .bind(object_id)
    .fetch_all(app.store().pool())
    .await
    .expect("the tags are read");
    rows
}
