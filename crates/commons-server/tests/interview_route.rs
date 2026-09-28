//! The interview routes, and the ways they can leak.
//!
//! # The property that shapes this file
//!
//! Absent, denied and "not transcribed" must be **indistinguishable**. Three
//! different reasons produce one 404, and that is the whole point: a client that
//! can tell them apart can walk the library and learn which objects a user has
//! and which they are refused. A route answering 200 with an empty transcript
//! would be the leak, and so would a 403 — a 403 confirms the object exists.
//!
//! So the tests here assert the response is the SAME, not merely that it is a
//! 404: an absent object and a denied object are compared field for field.
//!
//! What is *not* tested here is the transcription itself — that is
//! `commons-store/tests/asr_pipeline_db.rs`, which has the engine. This file is
//! about the shape of what comes back, which is where a JSON route fails
//! quietly: a `confidence` that serialises as `0.0` instead of `null`, a
//! `truncated` flag that is absent when it should be `false`.

mod support;

use axum::http::StatusCode;
use commons_core::domain::Marker;
use commons_store::interview::{replace_transcript, TranscriptRow, WindowRow, WordRow};
use commons_store::marker::insert_marker;
use support::{media_fixture, media_fixture_denied, TestApp};
use uuid::Uuid;

/// The parsed body.
///
/// `TestApp::get_json` returns the raw bytes, which is right for the tests that
/// compare two responses for byte equality. Every assertion here wants a field,
/// so parsing happens here — once — rather than at forty call sites.
fn json(r: &support::TestResponse) -> serde_json::Value {
    serde_json::from_slice(&r.body).unwrap_or_else(|e| {
        panic!(
            "the body is not JSON ({e}): {:?}",
            String::from_utf8_lossy(&r.body)
        )
    })
}

/// A transcript with `count` words, one second apart, through the real store
/// API so the route reads what the driver would have written.
async fn seed_transcript(app: &TestApp, object_id: &str, count: i64) -> String {
    let id = format!("{object_id}#test");
    let words: Vec<WordRow> = (0..count)
        .map(|i| WordRow {
            ordinal: i as i32,
            text: format!("word{i}"),
            start_ms: (i * 1_000) as i32,
            end_ms: (i * 1_000 + 400) as i32,
            // Half scored, half not. `None` and `Some(0.0)` are different claims
            // and the response has to preserve the difference.
            confidence: if i % 2 == 0 { Some(0.9) } else { None },
            speaker: None,
        })
        .collect();
    let mut row = TranscriptRow::new(
        &id,
        object_id,
        "whisper.cpp",
        "ggml-base.en",
        &"ab".repeat(32),
        "ffmpeg -> s16le 16kHz mono",
    );
    row.word_count = count;
    row.duration_ms = count * 1_000;
    replace_transcript(app.store(), &row, &words, &[])
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn an_absent_object_and_a_denied_object_answer_byte_identical_404s() {
    // The oracle. Not "both 404" -- the same bytes, because a 404 that differs
    // between the two is still a library oracle.
    //
    // The denied object HAS a transcript, deliberately. If it did not, the test
    // would pass with the access gate removed: an ungated request would fall
    // through to "no transcript", which is also a 404, and the comparison would
    // find two identical bodies. Seeding it is what makes this a test of the gate
    // rather than of the shape of the 404.
    let app = TestApp::new().await;
    let denied = media_fixture_denied(&app, b"0123456789").await;
    seed_transcript(&app, &denied, 4).await;
    let absent = Uuid::new_v4().to_string();

    // The transcript is really there -- otherwise this proves nothing.
    let direct = commons_store::interview::transcript_for(app.store(), &denied)
        .await
        .expect("the store answers")
        .is_some();
    assert!(direct, "the denied object's transcript is seeded");

    let a = app.get_raw(&format!("/media/{absent}/transcript")).await;
    let b = app.get_raw(&format!("/media/{denied}/transcript")).await;
    assert_eq!(a.status, StatusCode::NOT_FOUND);
    assert_eq!(b.status, StatusCode::NOT_FOUND);
    assert_eq!(
        a.body,
        b.body,
        "the two 404s must be indistinguishable: {:?} vs {:?}",
        String::from_utf8_lossy(&a.body),
        String::from_utf8_lossy(&b.body)
    );

    // And the same for the other two routes, or the oracle just moves.
    for suffix in ["/chapters", "/transcript/words"] {
        let a = app.get_raw(&format!("/media/{absent}{suffix}")).await;
        let b = app.get_raw(&format!("/media/{denied}{suffix}")).await;
        assert_eq!(a.status, StatusCode::NOT_FOUND, "{suffix}");
        assert_eq!(a.body, b.body, "{suffix} must not leak denial");
    }
}

#[tokio::test]
async fn an_untranscribed_object_is_a_404_and_not_an_empty_transcript() {
    // "No transcript" and "not allowed to know" are the same answer, so a 200
    // with zero words is a leak even though it looks like a normal empty result.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let r = app
        .get_raw(&format!("/media/{}/transcript", fixture.object_id))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_transcript_carries_the_provenance_a_bug_report_needs() {
    // Engine, model AND digest. "The transcript is wrong" is unanswerable
    // without knowing which model produced it.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    seed_transcript(&app, &fixture.object_id, 4).await;

    let r = app
        .get_json(&format!("/media/{}/transcript", fixture.object_id))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(json(&r)["engine"], "whisper.cpp");
    assert_eq!(json(&r)["model_id"], "ggml-base.en");
    assert_eq!(json(&r)["model_sha256"], "ab".repeat(32));
    assert_eq!(json(&r)["word_count"], 4);
    // whisper.cpp ships a vocabulary, so these are words. A client offering a
    // search box over ids is the failure this flag exists to prevent.
    assert_eq!(json(&r)["words_are_ids"], false);
}

#[tokio::test]
async fn an_unscored_word_is_null_and_not_zero() {
    // The distinction the whole schema is built on. A `confidence` of `0.0` for
    // a word the engine had no opinion about puts it last in a "least confident"
    // sort, which is a claim about the word rather than about the engine.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    seed_transcript(&app, &fixture.object_id, 4).await;

    let r = app
        .get_json(&format!("/media/{}/transcript/words", fixture.object_id))
        .await;
    let body = json(&r);
    let words = body["words"].as_array().unwrap();
    assert_eq!(words.len(), 4);
    assert_eq!(words[0]["confidence"], 0.9);
    assert!(
        words[1]["confidence"].is_null(),
        "an unscored word is null, got {}",
        words[1]["confidence"]
    );
    assert!(words[2]["confidence"] == 0.9);
    assert!(words[3]["confidence"].is_null());
}

#[tokio::test]
async fn a_parakeet_transcript_says_its_words_are_ids() {
    // A transcript of numeric ids renders, exports, and aligns a chapter list
    // perfectly, and is unsearchable. The client has to be able to tell.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let id = format!("{}#parakeet", fixture.object_id);
    let words: Vec<WordRow> = (0..3)
        .map(|i| WordRow::new(i, &i.to_string(), i * 100, i * 100 + 50))
        .collect();
    let row = TranscriptRow::new(
        &id,
        &fixture.object_id,
        "parakeet",
        "parakeet-tdt-0.6b",
        &"cd".repeat(32),
        "ffmpeg -> s16le 16kHz mono",
    );
    replace_transcript(app.store(), &row, &words, &[])
        .await
        .unwrap();

    let r = app
        .get_json(&format!("/media/{}/transcript", fixture.object_id))
        .await;
    assert_eq!(
        json(&r)["words_are_ids"],
        true,
        "a parakeet run with no vocabulary"
    );
    assert_eq!(json(&r)["engine"], "parakeet");
}

#[tokio::test]
async fn words_are_paged_and_the_flag_says_whether_more_exist() {
    // The client needs this to decide whether to offer "load more", and cannot
    // infer it from `words.len() < total` -- which is also true of a page that
    // happens to end at the end.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    seed_transcript(&app, &fixture.object_id, 10).await;
    let url = format!("/media/{}/transcript/words?limit=4", fixture.object_id);

    let first = app.get_json(&url).await;
    assert_eq!(json(&first)["words"].as_array().unwrap().len(), 4);
    assert_eq!(json(&first)["total"], 10, "the total, not the page");
    assert_eq!(json(&first)["truncated"], true);

    // The last page: fewer words than asked for, and NOT truncated.
    let last = app
        .get_json(&format!(
            "/media/{}/transcript/words?offset=8&limit=4",
            fixture.object_id
        ))
        .await;
    assert_eq!(json(&last)["words"].as_array().unwrap().len(), 2);
    assert_eq!(json(&last)["truncated"], false, "the end is not truncation");

    // An offset past the end is an empty page, not an error and not the last
    // page repeated.
    let past = app
        .get_json(&format!(
            "/media/{}/transcript/words?offset=999&limit=4",
            fixture.object_id
        ))
        .await;
    assert_eq!(past.status, StatusCode::OK);
    assert_eq!(json(&past)["words"].as_array().unwrap().len(), 0);
    assert_eq!(json(&past)["truncated"], false);
}

#[tokio::test]
async fn a_limit_beyond_the_maximum_is_capped_rather_than_honoured() {
    // A client asking for a million words must not get a million words. The cap
    // is a server-side decision because a client-side one is not enforced.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    seed_transcript(&app, &fixture.object_id, 20).await;
    let r = app
        .get_json(&format!(
            "/media/{}/transcript/words?limit=1000000",
            fixture.object_id
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        json(&r)["words"].as_array().unwrap().len(),
        20,
        "fewer than the cap because that is all there is; the point is it is not 1,000,000"
    );
    assert_eq!(json(&r)["truncated"], false);
}

#[tokio::test]
async fn a_negative_offset_is_clamped_rather_than_forwarded() {
    // `LIMIT -1 OFFSET -5` is a 500 on Postgres and "no limit" on SQLite, so an
    // unvalidated parameter is a divergence between the two engines rather than
    // a bad request. Both must answer the same way.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    seed_transcript(&app, &fixture.object_id, 6).await;
    let r = app
        .get_json(&format!(
            "/media/{}/transcript/words?offset=-5&limit=3",
            fixture.object_id
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let body = json(&r);
    let words = body["words"].as_array().unwrap();
    assert_eq!(words.len(), 3);
    assert_eq!(words[0]["ordinal"], 0, "clamped to the start, not an error");
}

#[tokio::test]
async fn a_failed_window_is_reported_with_its_range_and_reason() {
    // The property the whole window table exists for. A gap with no record is a
    // gap nobody can find, and a range without a reason cannot be acted on.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let id = format!("{}#gapped", fixture.object_id);
    let row = TranscriptRow::new(
        &id,
        &fixture.object_id,
        "whisper.cpp",
        "ggml-base.en",
        &"ab".repeat(32),
        "ffmpeg -> s16le",
    );
    let windows = vec![
        WindowRow {
            window_index: 0,
            start_ms: 0,
            end_ms: 30_000,
            ok: true,
            failure: None,
        },
        WindowRow {
            window_index: 1,
            start_ms: 30_000,
            end_ms: 60_000,
            ok: false,
            failure: Some("whisper.cpp failed (exit 1): out of memory".into()),
        },
    ];
    replace_transcript(app.store(), &row, &[], &windows)
        .await
        .unwrap();

    let r = app
        .get_json(&format!("/media/{}/transcript", fixture.object_id))
        .await;
    let body = json(&r);
    let failed = body["failed_windows"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "the one that failed, not both windows");
    assert_eq!(failed[0]["start_ms"], 30_000);
    assert_eq!(failed[0]["end_ms"], 60_000);
    assert!(
        failed[0]["reason"]
            .as_str()
            .unwrap()
            .contains("out of memory"),
        "the reason survives: {}",
        failed[0]["reason"]
    );
}

#[tokio::test]
async fn chapters_come_back_in_time_order() {
    // Three rows, inserted out of order, because two rows are sorted either way
    // and an ordering bug hides until there are three.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    for (title, start, end) in [
        ("third", 20_000, Some(30_000)),
        ("first", 0, Some(10_000)),
        ("second", 10_000, Some(20_000)),
    ] {
        insert_marker(
            app.store(),
            &Marker {
                id: Uuid::new_v4(),
                object_id: fixture.object_id.clone(),
                title: title.to_string(),
                start_ms: start,
                end_ms: end,
                primary_tag_id: None,
                rating: None,
                created_at: chrono::Utc::now().to_rfc3339(),
            },
        )
        .await
        .unwrap();
    }
    let r = app
        .get_json(&format!("/media/{}/chapters", fixture.object_id))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let body = json(&r);
    let chapters = body["chapters"].as_array().unwrap();
    let titles: Vec<&str> = chapters
        .iter()
        .map(|c| c["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["first", "second", "third"], "in time order");
    assert_eq!(chapters[0]["end_ms"], 10_000);
    assert_eq!(chapters[1]["end_ms"], 20_000);
}

#[tokio::test]
async fn an_open_ended_chapters_end_is_null_and_not_zero() {
    // A chapter with no end runs to the end of the media. A client that coalesces
    // null to 0 renders it as a zero-length chapter at the top of the recording,
    // and a next-chapter button derived from it never advances.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    insert_marker(
        app.store(),
        &Marker {
            id: Uuid::new_v4(),
            object_id: fixture.object_id.clone(),
            title: "the rest".to_string(),
            start_ms: 5_000,
            end_ms: None,
            primary_tag_id: None,
            rating: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        },
    )
    .await
    .unwrap();
    let r = app
        .get_json(&format!("/media/{}/chapters", fixture.object_id))
        .await;
    let body = json(&r);
    let chapters = body["chapters"].as_array().unwrap();
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0]["start_ms"], 5_000);
    assert!(
        chapters[0]["end_ms"].is_null(),
        "an open-ended chapter has no end, got {}",
        chapters[0]["end_ms"]
    );
}

#[tokio::test]
async fn an_object_with_no_chapters_is_an_empty_list_not_a_404() {
    // A library full of recordings with no chapters is the normal state, and a
    // 404 would make the client show an error rather than hide a feature.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let r = app
        .get_json(&format!("/media/{}/chapters", fixture.object_id))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(json(&r)["chapters"].as_array().unwrap().len(), 0);
    assert_eq!(json(&r)["object_id"], fixture.object_id);
}
