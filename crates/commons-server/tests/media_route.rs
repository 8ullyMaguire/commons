//! `GET /media/:object_id`, as HTTP.
//!
//! `tests/range.rs` proves the parser. This proves the route: that the consent
//! gate runs before a byte, that a 404 is a 404 and not a 403, and — the claim
//! no unit test can make — that the bytes in the body are the bytes the
//! `Content-Range` header claims. A route can have a perfect parser and a
//! short body, and the client hangs.
//!
//! The store is a real one on a real engine with a real file, because the
//! claims are about the interaction of the query, the `stat` and the read. A
//! mock store would let all of them pass while the route served the wrong thing.

mod support;

use axum::http::{header, StatusCode};
use support::{
    media_fixture, media_fixture_at_tier, media_fixture_denied, media_fixture_named,
    media_fixture_size_mismatch, media_fixture_then_remove, TestApp,
};

/// The whole body, and the header that says how long it is.
#[tokio::test]
async fn a_present_file_serves_its_whole_body() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app.get(&fixture.object_id, None).await;
    assert_eq!(r.status, StatusCode::OK);
    // `Accept-Ranges` on the 200 too, and the reason is the assertion: a
    // client that cannot see the token will not send `Range` at all, and then
    // never seeks — so a 200 without it is a scrub bar that silently does
    // nothing.
    assert_eq!(r.headers[header::ACCEPT_RANGES], "bytes");
    assert_eq!(r.declared_len(), 10);
    assert_eq!(r.body, b"0123456789");
    // The two must agree, which is the property rather than any particular
    // pair of values: `Content-Length` equals the number of bytes in the body.
    assert_eq!(
        r.declared_len(),
        r.body.len() as u64,
        "Content-Length and the body must describe the same bytes"
    );
}

/// The claim that needs a real test: the header and the body are written in
/// different places, and a client that trusts the header waits forever.
#[tokio::test]
async fn a_206_body_is_exactly_the_bytes_the_content_range_claims() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    // A three-byte window from the middle: neither starting at 0 nor running to
    // the end, because those two are where an off-by-one is least visible.
    let r = app.get(&fixture.object_id, Some("bytes=3-5")).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.headers[header::CONTENT_RANGE], "bytes 3-5/10");
    assert_eq!(r.body, b"345", "the body must be the claimed three bytes");
    assert_eq!(r.declared_len(), 3);
}

/// A suffix range, because `-N` has its own arithmetic and its own bug.
#[tokio::test]
async fn a_suffix_range_serves_the_tail() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app.get(&fixture.object_id, Some("bytes=-3")).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.headers[header::CONTENT_RANGE], "bytes 7-9/10");
    assert_eq!(r.body, b"789");
}

/// An open-ended range, which is what a `<video>` sends when it starts playing:
/// `bytes=0-`.
#[tokio::test]
async fn an_open_ended_range_serves_to_the_end() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app.get(&fixture.object_id, Some("bytes=7-")).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.headers[header::CONTENT_RANGE], "bytes 7-9/10");
    assert_eq!(r.body, b"789");
}

/// The last byte alone: the boundary where an inclusive/half-open confusion
/// serves zero bytes or two.
#[tokio::test]
async fn the_last_byte_alone_is_one_byte() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app.get(&fixture.object_id, Some("bytes=9-9")).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.headers[header::CONTENT_RANGE], "bytes 9-9/10");
    assert_eq!(r.body, b"9", "one byte, not none and not two");
    assert_eq!(r.declared_len(), 1);
}

/// A range past the end is 416 with the length, not a short 206.
#[tokio::test]
async fn a_range_past_the_end_is_416_with_the_length() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app.get(&fixture.object_id, Some("bytes=50-60")).await;
    assert_eq!(r.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(r.headers[header::CONTENT_RANGE], "bytes */10");
    assert_eq!(r.headers[header::ACCEPT_RANGES], "bytes");
}

/// An end past the end is CLAMPED and served, per §14.1.2. A client asking for
/// `0-999999` of a 10-byte file wants the file.
#[tokio::test]
async fn an_end_past_the_end_is_clamped_and_served() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app.get(&fixture.object_id, Some("bytes=0-999999")).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.headers[header::CONTENT_RANGE], "bytes 0-9/10");
    assert_eq!(r.body, b"0123456789", "the whole file, not a truncated one");
}

/// A garbage header is 416, not a 200 and not a 500, and it says which kind of
/// 416 it is.
#[tokio::test]
async fn a_malformed_range_is_416_naming_the_reason() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app.get(&fixture.object_id, Some("bytes=abc")).await;
    assert_eq!(r.status, StatusCode::RANGE_NOT_SATISFIABLE);
    let json = String::from_utf8(r.body.clone()).expect("a json body");
    assert!(
        json.contains("malformed"),
        "the body should say which kind of 416 this is, got {json}"
    );
}

/// The security claim, in HTTP terms. Absent, denied and not-on-disk are the
/// same answer, and that answer is 404 rather than 403 — a 403 confirms the
/// object exists, which on a consent-first platform is itself the information
/// §14.1 is withholding.
#[tokio::test]
async fn an_unknown_object_is_404_and_never_403() {
    let app = TestApp::new().await;

    let r = app.get("no-such-object", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // The whole point: not 403, not 401, not a 200 carrying an error body.
    assert_ne!(r.status, StatusCode::FORBIDDEN);
    assert_ne!(r.status, StatusCode::UNAUTHORIZED);
}

/// A denied object is 404 too, and this is the test that says the gate is in
/// the query rather than at the route: the route has no consent code to forget.
#[tokio::test]
async fn a_denied_object_is_404_and_never_403() {
    let app = TestApp::new().await;
    let object_id = media_fixture_denied(&app, b"secret").await;

    let r = app.get(&object_id, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_ne!(r.status, StatusCode::FORBIDDEN);
    // And nothing leaked: the body must not contain the bytes, and the headers
    // must not contain the length either — a `Content-Length` on a denied object
    // is a way to learn the file's size without consent for it.
    assert!(
        !r.body.windows(6).any(|w| w == b"secret"),
        "a denied object's bytes must not appear in the response"
    );
    //
    // The size claim, stated properly. The obvious assertion -- "no
    // Content-Length" -- is wrong, because axum sets a Content-Length for the
    // JSON error body itself and the 404 here carries `21`. What must not
    // appear is the FILE's length, so the assertion is about the file's bytes:
    // the denied file is 6 bytes long, and a response whose declared length
    // equals 6 would be disclosing it. Comparing against the error body's own
    // length would pass on a route that leaked the size in a header.
    assert_ne!(
        r.declared_len(),
        6,
        "a denied object must not disclose its size in a header"
    );
    assert!(
        !String::from_utf8_lossy(&r.body).contains("6"),
        "a denied object's size must not appear in the body either"
    );
}

/// A row that exists but whose file is gone is 404, not a 500. A library
/// mid-rescan is full of these, and a route that 500s on one fails every page
/// load during a scan.
#[tokio::test]
async fn a_file_missing_from_disk_is_404_not_500() {
    let app = TestApp::new().await;
    let object_id = media_fixture_then_remove(&app, b"gone").await;

    let r = app.get(&object_id, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // And a range against a file that is not there is 404 too, not 416: the
    // 416 arm would confirm that the object exists and is merely out of range.
    let r = app.get(&object_id, Some("bytes=0-3")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// Health and metrics still answer after the router gained a second state.
#[tokio::test]
async fn the_health_routes_still_answer() {
    let app = TestApp::new().await;

    assert_eq!(app.get_raw("/healthz").await.status, StatusCode::OK);
    assert_eq!(app.get_raw("/livez").await.status, StatusCode::OK);
    // And an unknown path is still the Phase 0 JSON, not a media error.
    assert_eq!(app.get_raw("/nope").await.status, StatusCode::NOT_FOUND);
}

/// The content type is what makes a `<video>` play rather than download, and a
/// route that serves `application/octet-stream` for an mp4 gives a feed a
/// silent, empty slide.
#[tokio::test]
async fn the_content_type_is_derived_from_the_extension() {
    let app = TestApp::new().await;
    let object_id = media_fixture_named(&app, b"clip", "mp4").await;

    let r = app.get(&object_id, None).await;
    assert_eq!(r.headers[header::CONTENT_TYPE], "video/mp4");
}

/// `Content-Length` for a 206 is the LENGTH of the range, not the file's — the
/// bug where a partial response announces the whole file and the client waits
/// for bytes that were never sent.
#[tokio::test]
async fn a_206_content_length_is_the_range_not_the_file() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app.get(&fixture.object_id, Some("bytes=2-4")).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.declared_len(), 3, "three bytes, not the ten-byte file");
    assert_eq!(r.body, b"234");
}

/// Several ranges: 200 with the whole body, not a 206 carrying only the first.
///
/// The alternative is worse than it looks — a 206 with one range answers a
/// question with a body the client cannot align with its request, and the
/// browser's media stack retries forever.
#[tokio::test]
async fn several_ranges_get_the_whole_file_not_a_truncated_206() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app.get(&fixture.object_id, Some("bytes=0-2,5-7")).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "multipart is not implemented, so the whole representation is the honest answer"
    );
    assert_eq!(r.body, b"0123456789");
}

/// The operator's own freshly-scanned file, which nobody else may see.
///
/// Found by mutation, and it is the load-bearing identity test. `account_id:
/// None` in the route makes the consent clause resolve to
/// `ConsentTiers::PUBLIC`, which excludes `unverified` — so a route that asked
/// as a visitor would 404 here. With every other fixture at
/// `self_published` (which IS in PUBLIC) that mutation passed all fifteen tests.
///
/// A freshly scanned file is `unverified`, so this is not an exotic tier: it is
/// what the library looks like the moment a scan finishes, and it is the state
/// every new user meets first.
#[tokio::test]
async fn the_operator_serves_their_own_unverified_file() {
    let app = TestApp::new().await;
    let object_id = media_fixture_at_tier(&app, b"fresh", "unverified").await;

    let r = app.get(&object_id, None).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "the library owner must be able to see a freshly scanned file"
    );
    assert_eq!(r.body, b"fresh");
}

/// A file replaced since the last scan, served at its ON-DISK length.
///
/// Found by mutation, and this is the one that hangs a client rather than
/// failing it. The index says the file is 10 bytes; the disk says 4. Resolve
/// the range against the recorded 10 and a request for `bytes=0-9` produces a
/// `Content-Range: bytes 0-9/10` over a four-byte body — the client waits for
/// six bytes that were never sent, forever.
///
/// The fixture is the whole point: every other test writes a file whose length
/// matches the row, so the two are equal and the mutation is invisible. This
/// one makes them disagree, and asserts the header and the body agree with EACH
/// OTHER rather than with either source, which is the property that matters.
#[tokio::test]
async fn a_file_replaced_since_the_scan_is_served_at_its_on_disk_length() {
    let app = TestApp::new().await;
    // Four bytes on disk, ten recorded.
    let object_id = media_fixture_size_mismatch(&app, b"0123", 10).await;

    let r = app.get(&object_id, Some("bytes=0-9")).await;

    // Whatever the answer, the body and the header must describe the SAME
    // bytes. A 416 here is correct and acceptable; a 206 over four bytes while
    // claiming ten is not.
    if r.status == StatusCode::PARTIAL_CONTENT {
        assert_eq!(
            r.declared_len(),
            r.body.len() as u64,
            "Content-Length and the body must agree even when the index is stale"
        );
        let claimed: u64 = r.headers[header::CONTENT_RANGE]
            .to_str()
            .expect("a content-range")
            .rsplit('/')
            .next()
            .expect("a length")
            .parse()
            .expect("a number");
        assert_eq!(
            claimed,
            r.body.len() as u64,
            "the total in Content-Range must be the real length, not the stale one"
        );
    } else {
        assert_eq!(
            r.status,
            StatusCode::RANGE_NOT_SATISFIABLE,
            "a range past the real end must be refused, not served short"
        );
    }
}
