//! `GET`/`PUT /media/:object_id/playback`, as HTTP.
//!
//! T-P6-001. `playback_store.rs` proves the store; this proves the wire, and
//! there are two claims it can make that the store test cannot:
//!
//! * **The status codes are the ones a client actually handles.** A store that
//!   returns a typed error and a route that answers 400 are different bugs to
//!   the user: one is a bad value they can fix, the other sends them hunting
//!   for a JSON syntax error that is not there.
//! * **A `PUT` for an object that does not exist still succeeds.** This is the
//!   surprising one, and it is deliberate — see the route's own docs. The
//!   player saves position on a timer, and it can fire after the object was
//!   removed, or before the row exists. Refusing it would lose the position of
//!   every file the user is watching, in exchange for protecting a row that
//!   costs nothing and is already keyed by an id nothing else will read.

mod support;

use axum::http::StatusCode;
use support::TestApp;

/// The shape `/playback` answers with.
fn body_of(r: &support::TestResponse) -> serde_json::Value {
    serde_json::from_slice(&r.body).expect("the body is JSON")
}

#[tokio::test]
async fn an_unplayed_object_gets_a_fresh_state_rather_than_404() {
    let app = TestApp::new().await;
    let r = app.get_json("/media/never_played/playback").await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "an object with no saved position is the common case, not a failure"
    );
    let b = body_of(&r);
    assert_eq!(b["position_ms"], 0);
    assert_eq!(b["completed"], false);
    // A fresh state must not invent a loop, and must not claim a duration it
    // cannot know -- `null` means "unknown", and a client that treated it as 0
    // would compute a zero-length timeline and call the video broken.
    assert!(b["loop_points"].is_null(), "a fresh state has no loop");
    assert!(b["duration_ms"].is_null(), "a fresh state has no duration");
}

#[tokio::test]
async fn a_written_position_reads_back_over_http() {
    let app = TestApp::new().await;
    let put = app
        .put_json(
            "/media/obj_http_1/playback",
            r#"{"position_ms":4200,"duration_ms":90000,"completed":false}"#,
        )
        .await;
    assert_eq!(put.status, StatusCode::OK);

    let r = app.get_json("/media/obj_http_1/playback").await;
    let b = body_of(&r);
    assert_eq!(b["position_ms"], 4200);
    assert_eq!(b["duration_ms"], 90000);
}

#[tokio::test]
async fn a_loop_survives_the_wire_in_both_ends() {
    let app = TestApp::new().await;
    app.put_json(
        "/media/obj_http_loop/playback",
        r#"{"position_ms":500,"duration_ms":90000,"loop_points":{"a_ms":1000,"b_ms":4000}}"#,
    )
    .await;
    let b = body_of(&app.get_json("/media/obj_http_loop/playback").await);
    assert_eq!(b["loop_points"]["a_ms"], 1000);
    assert_eq!(b["loop_points"]["b_ms"], 4000);
}

#[tokio::test]
async fn a_position_past_the_end_is_422_and_names_the_field() {
    // 422, not 400: the JSON parsed fine and the shape was right. 400 would
    // say "I could not parse this", which is false.
    let app = TestApp::new().await;
    let r = app
        .put_json(
            "/media/obj_http_bad/playback",
            r#"{"position_ms":50000,"duration_ms":10000}"#,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_of(&r);
    assert_eq!(
        b["field"], "position_ms",
        "the refusal must name the field so the client can point at the control"
    );
    assert!(
        b["message"].as_str().unwrap_or_default().len() > 4,
        "a message, not a code"
    );
}

#[tokio::test]
async fn an_inverted_loop_is_422_and_names_the_loop_field() {
    let app = TestApp::new().await;
    let r = app
        .put_json(
            "/media/obj_http_inv/playback",
            r#"{"position_ms":0,"duration_ms":90000,"loop_points":{"a_ms":4000,"b_ms":1000}}"#,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_of(&r)["field"], "loop_b_ms");
}

#[tokio::test]
async fn a_loop_past_the_end_is_refused() {
    let app = TestApp::new().await;
    let r = app
        .put_json(
            "/media/obj_http_past/playback",
            r#"{"position_ms":0,"duration_ms":90000,"loop_points":{"a_ms":1000,"b_ms":99000}}"#,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_malformed_body_is_400_and_says_so() {
    // The one case where 400 is honest: the bytes were not the expected shape.
    let app = TestApp::new().await;
    let r = app
        .put_json("/media/obj_http_malformed/playback", "{not json")
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(body_of(&r)["error"], "malformed_body");
}

#[tokio::test]
async fn missing_fields_default_rather_than_failing() {
    // A player that has only just loaded a file knows its position and nothing
    // else. Requiring every field would make the first save -- the one that
    // happens most -- the one that fails.
    let app = TestApp::new().await;
    let r = app
        .put_json("/media/obj_http_min/playback", r#"{"position_ms":10}"#)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let b = body_of(&app.get_json("/media/obj_http_min/playback").await);
    assert_eq!(b["position_ms"], 10);
    assert!(b["duration_ms"].is_null());
    assert!(b["loop_points"].is_null());
    assert_eq!(b["completed"], false);
}

#[tokio::test]
async fn saving_twice_replaces_rather_than_appending() {
    let app = TestApp::new().await;
    app.put_json("/media/obj_http_up/playback", r#"{"position_ms":1000}"#)
        .await;
    app.put_json(
        "/media/obj_http_up/playback",
        r#"{"position_ms":7000,"completed":true}"#,
    )
    .await;
    let b = body_of(&app.get_json("/media/obj_http_up/playback").await);
    assert_eq!(b["position_ms"], 7000, "the last save must win");
    assert_eq!(b["completed"], true);
}

#[tokio::test]
async fn a_refused_save_leaves_the_previous_position_intact() {
    // The order matters: a bad save must not clear a good one. If validation
    // happened after the write, the user's resume point would be lost by the
    // exact request that reported the problem.
    let app = TestApp::new().await;
    app.put_json(
        "/media/obj_http_keep/playback",
        r#"{"position_ms":4242,"duration_ms":90000}"#,
    )
    .await;
    let bad = app
        .put_json(
            "/media/obj_http_keep/playback",
            r#"{"position_ms":99999,"duration_ms":90000}"#,
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_of(&app.get_json("/media/obj_http_keep/playback").await);
    assert_eq!(
        b["position_ms"], 4242,
        "a refused save must not disturb the good one"
    );
}

#[tokio::test]
async fn two_objects_keep_independent_positions_over_http() {
    let app = TestApp::new().await;
    app.put_json("/media/obj_http_a/playback", r#"{"position_ms":111}"#)
        .await;
    app.put_json("/media/obj_http_b/playback", r#"{"position_ms":222}"#)
        .await;
    assert_eq!(
        body_of(&app.get_json("/media/obj_http_a/playback").await)["position_ms"],
        111
    );
    assert_eq!(
        body_of(&app.get_json("/media/obj_http_b/playback").await)["position_ms"],
        222
    );
}

#[tokio::test]
async fn the_response_omits_absent_optional_fields_rather_than_sending_nulls() {
    // `skip_serializing_if` is a real decision, not tidiness: a client that
    // does `if ("loop_points" in body)` must see a key absent when there is no
    // loop, and `"loop_points": null` would make that test true. The GET test
    // above asserts the value is null; this asserts the key is not there.
    let app = TestApp::new().await;
    let r = app.get_json("/media/obj_http_shape/playback").await;
    let v: serde_json::Value = serde_json::from_slice(&r.body).expect("JSON");
    let obj = v.as_object().expect("an object");
    assert!(
        !obj.contains_key("loop_points"),
        "an absent loop omits the key entirely"
    );
    assert!(
        obj.contains_key("position_ms"),
        "a required field is always present"
    );
}

#[tokio::test]
async fn a_three_hour_position_is_not_truncated() {
    // 10_800_000 ms. A `u32` of *seconds* would be fine, but a `u32` of
    // milliseconds overflows at ~49 days and an `i32` of seconds overflows at
    // 68 years -- the range is not the interesting part. What matters is that
    // the value a client PUTs is the value a client GETs, for a long file,
    // because a truncation shows up as a video resuming at the wrong place.
    let app = TestApp::new().await;
    app.put_json(
        "/media/obj_http_long/playback",
        r#"{"position_ms":10800000}"#,
    )
    .await;
    assert_eq!(
        body_of(&app.get_json("/media/obj_http_long/playback").await)["position_ms"],
        10_800_000
    );
}
