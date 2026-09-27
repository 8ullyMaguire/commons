//! The two subtitle routes, and the two ways they can leak.
//!
//! Everything here is about the parts that are wrong in a way no test of the
//! extractor or the store would catch: the gate (a route that answers 200 with
//! an empty list turns "denied" into "no subtitles", which is a library
//! oracle) and the ownership check on the document id (a document id from
//! another object must not be readable, and 403 would confirm it exists).
//!
//! The cue-rendering rules are unit-tested in `subtitles.rs` next to the code.
//! What is tested *here* is the route: status, content type, and the fact that
//! the bytes a browser gets are the bytes the store holds.

mod support;

use axum::http::{header, StatusCode};
use commons_store::subtitles as st;
use support::{media_fixture, media_fixture_denied, TestApp};

/// A document with one cue, stored through the real store API so the route
/// reads what the extractor would have written.
async fn seed_document(app: &TestApp, object_id: &str, text: &str) -> String {
    let doc = st::Document {
        id: format!("sub-{object_id}-1"),
        object_id: object_id.to_string(),
        origin: st::Origin::Sidecar,
        stream_index: None,
        path: Some("/tmp/whatever.srt".to_string()),
        format: st::DocFormat::Vtt,
        language: Some("en".to_string()),
        is_default: true,
        is_forced: false,
        is_hearing_impaired: false,
        sha256: "0".repeat(64),
        byte_size: 42,
        extracted_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let cues = st::Cues::new(vec![(0, 0, 1_500, text.to_string(), None)]);
    assert!(
        st::put_document(app.store(), &doc, &cues).await.unwrap(),
        "seeding a document must actually write it"
    );
    doc.id
}

#[tokio::test]
async fn an_object_with_no_subtitles_answers_an_empty_list_not_404() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app
        .get_raw(&format!("/media/{}/subtitles", fixture.object_id))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let json = serde_json::from_slice::<serde_json::Value>(&r.body).unwrap();
    assert_eq!(json["tracks"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_denied_object_answers_404_and_not_an_empty_list() {
    // The oracle. `media_fixture_denied` is a file the gate refuses; if this
    // answered 200 with `[]`, a client could walk the library and learn which
    // objects exist and are denied. It has to be byte-identical to "absent".
    let app = TestApp::new().await;
    let denied = media_fixture_denied(&app, b"0123456789").await;

    let r = app.get_json(&format!("/media/{denied}/subtitles")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_absent_object_answers_the_same_404_as_a_denied_one() {
    // Two states, one answer. If these ever differ, one of them is a leak.
    let app = TestApp::new().await;
    let denied = media_fixture_denied(&app, b"0123456789").await;

    let denied_r = app.get_raw(&format!("/media/{denied}/subtitles")).await;
    let absent_r = app.get_raw("/media/no-such-object/subtitles").await;
    assert_eq!(denied_r.status, absent_r.status);
    assert_eq!(denied_r.body, absent_r.body);
}

#[tokio::test]
async fn a_seeded_track_is_listed_with_its_label_and_count() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    seed_document(&app, &fixture.object_id, "hello").await;

    let r = app
        .get_raw(&format!("/media/{}/subtitles", fixture.object_id))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let json = serde_json::from_slice::<serde_json::Value>(&r.body).unwrap();
    let tracks = json["tracks"].as_array().unwrap();
    assert_eq!(1, tracks.len());
    assert_eq!(1, tracks[0]["cue_count"]);
    assert_eq!("en", tracks[0]["language"]);
    assert!(tracks[0]["label"].as_str().unwrap().contains("en"));
    // The format travels with the list, because a client cannot warn that ASS
    // styling is being dropped without knowing the format.
    assert_eq!("vtt", tracks[0]["format"]);
    // The id is the whole of the VTT URL, so the client never composes a path.
    assert_eq!(format!("sub-{}-1", fixture.object_id), tracks[0]["id"]);
}

#[tokio::test]
async fn a_track_serves_webvtt_with_the_right_content_type() {
    // The one that matters most: a `<track>` served as a generic type renders
    // nothing, logs nothing, and leaves the user watching a silent video.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let id = seed_document(&app, &fixture.object_id, "hello").await;

    let r = app
        .get_raw(&format!("/media/{}/subtitles/{id}.vtt", fixture.object_id))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers[header::CONTENT_TYPE], "text/vtt; charset=utf-8");
    let body = String::from_utf8(r.body.clone()).unwrap();
    assert!(body.starts_with("WEBVTT\n"), "{body}");
    assert!(body.contains("00:00:00.000 --> 00:00:01.500"), "{body}");
    assert!(body.contains("hello"), "{body}");
}

#[tokio::test]
async fn a_document_belonging_to_another_object_is_404_and_not_403() {
    // The ownership check. Two real objects, each with a real document, and a
    // request for one's document under the other's id.
    let app = TestApp::new().await;
    let a = media_fixture(&app, b"aaaaaaaaaaaaaaaa").await;
    let b = media_fixture(&app, b"bbbbbbbbbbbbbbbb").await;
    let a_doc = seed_document(&app, &a.object_id, "secret-a").await;
    seed_document(&app, &b.object_id, "public-b").await;

    // `a_doc` is genuine, and it is not `b`'s.
    let r = app
        .get_raw(&format!("/media/{}/subtitles/{a_doc}.vtt", b.object_id))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(
        !String::from_utf8_lossy(&r.body).contains("secret-a"),
        "another object's cue text leaked: {:?}",
        String::from_utf8_lossy(&r.body)
    );

    // And a 403 would be a leak too, by confirming the id exists.
    assert_ne!(StatusCode::FORBIDDEN, r.status);
}

#[tokio::test]
async fn the_vtt_of_a_denied_object_is_404_even_with_a_real_document_id() {
    // The gate, on the byte-serving route. A caller who has a document id from
    // a listing they used to have must not be able to keep reading it.
    let app = TestApp::new().await;
    let denied = media_fixture_denied(&app, b"0123456789").await;
    let doc = seed_document(&app, &denied, "should not be readable").await;

    let r = app
        .get_raw(&format!("/media/{denied}/subtitles/{doc}.vtt"))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(!String::from_utf8_lossy(&r.body).contains("should not be"));
}

#[tokio::test]
async fn an_unknown_document_id_is_404_rather_than_an_empty_file() {
    // 404 for "no such document" and 200 for "a document with no cues" are
    // different states, and conflating them makes a broken URL look like an
    // empty track.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let r = app
        .get_raw(&format!(
            "/media/{}/subtitles/no-such-doc.vtt",
            fixture.object_id
        ))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_document_with_no_cues_is_still_a_well_formed_vtt() {
    // A real, empty track. The browser must get a file it can parse, not a
    // 404 it will log as a load error.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let doc = st::Document {
        id: "empty-1".to_string(),
        object_id: fixture.object_id.clone(),
        origin: st::Origin::Sidecar,
        stream_index: None,
        path: Some("/tmp/empty.srt".to_string()),
        format: st::DocFormat::Vtt,
        language: None,
        is_default: false,
        is_forced: false,
        is_hearing_impaired: false,
        sha256: "1".repeat(64),
        byte_size: 0,
        extracted_at: "2026-01-01T00:00:00Z".to_string(),
    };
    st::put_document(app.store(), &doc, &st::Cues::new(vec![]))
        .await
        .unwrap();

    let r = app
        .get_raw(&format!(
            "/media/{}/subtitles/empty-1.vtt",
            fixture.object_id
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, b"WEBVTT\n\n");
}

/// The regression for the bug this route shipped with.
///
/// In `/x/:id.vtt`, axum captures the *whole* segment as the parameter name
/// `id.vtt` -- the suffix is part of the name, not a suffix to strip. So the
/// handler received `sub-1.vtt` and looked up a document with that literal id,
/// which does not exist, and answered a correct-looking 404 for a track that was
/// sitting right there. The browser logs a track load error and shows nothing.
///
/// Every other test in this file seeds a document and fetches it by URL, so
/// every other one of them failed on this too. What is worth pinning is the
/// direction: an id that happens to END in `.vtt` must be found, not 404, and a
/// path with no suffix at all must 404 rather than being guessed at.
#[tokio::test]
async fn the_vtt_suffix_is_part_of_the_url_and_not_part_of_the_id() {
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let id = seed_document(&app, &fixture.object_id, "hello").await;
    assert!(
        !id.ends_with(".vtt"),
        "the fixture id must not already end in .vtt"
    );

    // The URL the list route hands out.
    let r = app
        .get_raw(&format!("/media/{}/subtitles/{id}.vtt", fixture.object_id))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&r.body).contains("hello"));
}

#[tokio::test]
async fn a_url_with_no_vtt_suffix_is_404_rather_than_a_guess() {
    // Not a redirect: a second URL for one resource is how a cache ends up
    // holding two copies of the same track.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let id = seed_document(&app, &fixture.object_id, "hello").await;

    let r = app
        .get_raw(&format!("/media/{}/subtitles/{id}", fixture.object_id))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_document_whose_id_itself_ends_in_vtt_is_still_found() {
    // The other direction. If the handler stripped a suffix unconditionally by
    // searching for the FIRST `.vtt`, an id like `track.vtt.1` would be cut to
    // `track`. Ids are minted by the store, so this cannot happen today -- and
    // the test is here so that if the id scheme ever changes, this fails loudly
    // instead of silently serving the wrong track.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;
    let doc = st::Document {
        id: "track.vtt.1".to_string(),
        object_id: fixture.object_id.clone(),
        origin: st::Origin::Sidecar,
        stream_index: None,
        path: Some("/tmp/x.srt".to_string()),
        format: st::DocFormat::Vtt,
        language: None,
        is_default: false,
        is_forced: false,
        is_hearing_impaired: false,
        sha256: "2".repeat(64),
        byte_size: 1,
        extracted_at: "2026-01-01T00:00:00Z".to_string(),
    };
    st::put_document(
        app.store(),
        &doc,
        &st::Cues::new(vec![(0u32, 0i64, 1_000i64, "found-me".to_string(), None)]),
    )
    .await
    .unwrap();

    let r = app
        .get_raw(&format!(
            "/media/{}/subtitles/track.vtt.1.vtt",
            fixture.object_id
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&r.body).contains("found-me"));
}
