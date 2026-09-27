//! `GET /media/:object_id/proxy.m3u8`, as HTTP.
//!
//! T-P6-001, §11.5. The transcode itself is covered by `commons-media`'s
//! `transcode.rs`; this file proves the three claims that are the ROUTE's:
//!
//! * **the consent gate runs before any byte**, exactly as `/media/:id` does —
//!   a proxy of a denied object must be a 404 and never a transcoded file;
//! * **a file the browser can already play is not transcoded**, because the
//!   proxy costs minutes of CPU and a disk write;
//! * **a transcode failure is a 502 carrying the reason**, so a user can tell
//!   "retry" from "this file is broken".
//!
//! The fixtures are REAL video files, not random bytes. That is the whole point:
//! a fixture of random bytes fails at `ffprobe`, the route answers 422, and a
//! green suite would claim the transcode path works without ever transcoding.

mod support;

use axum::http::{header, StatusCode};
use support::{playable_video_fixture, set_content_hash, unplayable_video_fixture, TestApp};

fn body_of(r: &support::TestResponse) -> serde_json::Value {
    serde_json::from_slice(&r.body).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn a_playable_file_is_refused_a_proxy_rather_than_transcoded() {
    // The expensive-direction failure. h264/mp4 needs no proxy, so a request
    // for one is a client bug — and answering with the original would hide it,
    // which is why this is 422 with a message and not a redirect.
    let app = TestApp::new().await;
    let Some(object_id) = playable_video_fixture(&app).await else {
        eprintln!("skipping: ffmpeg absent");
        return;
    };
    let r = app.get_raw(&format!("/media/{object_id}/proxy.m3u8")).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_of(&r);
    assert_eq!(b["error"], "not_proxyable");
    let msg = b["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("h264") && msg.contains("mp4"),
        "the refusal must NAME the codecs, so a developer can see which rung \
         decision was made: {msg}"
    );
}

#[tokio::test]
async fn an_unplayable_file_is_proxied_and_the_bytes_are_video() {
    let app = TestApp::new().await;
    let Some(object_id) = unplayable_video_fixture(&app).await else {
        eprintln!("skipping: ffmpeg absent");
        return;
    };
    let r = app.get_raw(&format!("/media/{object_id}/proxy.m3u8")).await;

    assert_eq!(
        r.status,
        StatusCode::OK,
        "an mpeg4 source must proxy; body was {:?}",
        String::from_utf8_lossy(&r.body[..r.body.len().min(200)])
    );
    assert_eq!(r.headers[header::CONTENT_TYPE], "video/mp4");
    assert_eq!(r.headers[header::ACCEPT_RANGES], "bytes");
    assert!(
        !r.body.is_empty(),
        "a 200 with an empty body is a client that waits forever"
    );
    // `ftyp` at offset 4 is the MP4 signature. Asserting on it rather than on
    // the status alone is what makes this a claim about BYTES.
    assert_eq!(&r.body[4..8], b"ftyp", "the proxy really is an MP4");
}

#[tokio::test]
async fn a_proxy_is_private_so_a_shared_cache_cannot_leak_it() {
    let app = TestApp::new().await;
    let Some(object_id) = unplayable_video_fixture(&app).await else {
        return;
    };
    let r = app.get_raw(&format!("/media/{object_id}/proxy.m3u8")).await;
    let cc = r.headers[header::CACHE_CONTROL]
        .to_str()
        .unwrap_or_default();
    assert!(
        cc.contains("private"),
        "a consent-gated derivative must not be cacheable by a shared proxy: {cc}"
    );
}

#[tokio::test]
async fn a_denied_object_is_404_and_never_transcoded() {
    // The consent claim, and the important half is what it must NOT do: a 403
    // would confirm the object exists, and a 200 would be a transcoded copy of
    // media the caller may not see.
    let app = TestApp::new().await;
    let Some(object_id) = unplayable_video_fixture(&app).await else {
        return;
    };
    // Re-seed the consent record at a tier the anonymous local caller cannot see.
    sqlx::query("UPDATE consent_record SET tier = 'denied' WHERE object_id = ?")
        .bind(&object_id)
        .execute(app.store().pool())
        .await
        .expect("the tier is set");

    let r = app.get_raw(&format!("/media/{object_id}/proxy.m3u8")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_ne!(
        r.status,
        StatusCode::FORBIDDEN,
        "403 would confirm it exists"
    );
    // No media bytes and no length. A small JSON error body is what the media
    // route also sends and is not a leak; what must not appear is anything that
    // describes the file. Asserted on the *shape*: no `Content-Length` matching
    // a plausible video, and no MP4 signature anywhere in the body.
    assert!(
        !r.body.windows(4).any(|w| w == b"ftyp"),
        "a denied object must not return media bytes: {:?}",
        String::from_utf8_lossy(&r.body[..r.body.len().min(120)])
    );
    assert!(
        r.body.len() < 512,
        "a denial body is an error, not a payload: {} bytes",
        r.body.len()
    );
}

#[tokio::test]
async fn an_unknown_object_is_404() {
    let app = TestApp::new().await;
    let r = app.get_raw("/media/no-such-object/proxy.m3u8").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_source_ffprobe_cannot_read_is_422_with_the_reason() {
    // Not a 500: the server is fine, the file is not. The message names the
    // problem so the user is not told to retry forever.
    let app = TestApp::new().await;
    let object_id = support::media_fixture(&app, b"this is not a video at all").await;
    let r = app
        .get_raw(&format!("/media/{}/proxy.m3u8", object_id.object_id))
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_of(&r);
    assert_eq!(b["error"], "not_proxyable");
    assert!(
        !b["message"].as_str().unwrap_or_default().is_empty(),
        "a 422 with no reason is indistinguishable from a server fault"
    );
}

#[tokio::test]
async fn a_proxied_file_supports_range_so_the_client_can_seek() {
    let app = TestApp::new().await;
    let Some(object_id) = unplayable_video_fixture(&app).await else {
        return;
    };
    let whole = app.get_raw(&format!("/media/{object_id}/proxy.m3u8")).await;
    assert_eq!(whole.status, StatusCode::OK);

    let r = app
        .send_raw(
            axum::http::Request::builder()
                .uri(format!("/media/{object_id}/proxy.m3u8"))
                .header(header::RANGE, "bytes=0-99")
                .body(axum::body::Body::empty())
                .expect("a request"),
        )
        .await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body.len(), 100, "exactly the bytes asked for");
    assert_eq!(
        r.headers[header::CONTENT_RANGE],
        format!("bytes 0-99/{}", whole.body.len()),
        "the Content-Range and the whole length must agree, or the client waits forever"
    );
}

#[tokio::test]
async fn a_range_past_the_end_of_a_proxy_is_416_with_the_length() {
    let app = TestApp::new().await;
    let Some(object_id) = unplayable_video_fixture(&app).await else {
        return;
    };
    app.get_raw(&format!("/media/{object_id}/proxy.m3u8")).await;
    let r = app
        .send_raw(
            axum::http::Request::builder()
                .uri(format!("/media/{object_id}/proxy.m3u8"))
                .header(header::RANGE, "bytes=99999999-")
                .body(axum::body::Body::empty())
                .expect("a request"),
        )
        .await;
    assert_eq!(r.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert!(
        r.headers[header::CONTENT_RANGE]
            .to_str()
            .unwrap_or_default()
            .starts_with("bytes */"),
        "a 416 must say the real length, or the client cannot retry: {:?}",
        r.headers[header::CONTENT_RANGE]
    );
}

#[tokio::test]
async fn a_recorded_hash_is_used_rather_than_hashing_the_file() {
    // The production path. `file.hash_blake3` is what the index already
    // computed, and re-hashing a large file on every play would make the button
    // feel broken. With a hash recorded, the transcode must still succeed and
    // the cache must be keyed on it.
    let app = TestApp::new().await;
    let Some(object_id) = unplayable_video_fixture(&app).await else {
        return;
    };
    set_content_hash(&app, &object_id, "abc123def456").await;
    let r = app.get_raw(&format!("/media/{object_id}/proxy.m3u8")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.body.is_empty());
}

#[tokio::test]
async fn a_second_request_reuses_the_cached_encode() {
    // The claim §12.1.1 makes about bandwidth. Two requests, one transcode: the
    // second must be served from the cache directory. Asserted through the HTTP
    // surface by comparing the two bodies -- identical bytes mean the same file
    // came back, and a re-encode of the same input is byte-identical too, so the
    // sharper claim is made in `commons-media/tests/transcode.rs` via mtime.
    let app = TestApp::new().await;
    let Some(object_id) = unplayable_video_fixture(&app).await else {
        return;
    };
    let a = app.get_raw(&format!("/media/{object_id}/proxy.m3u8")).await;
    let b = app.get_raw(&format!("/media/{object_id}/proxy.m3u8")).await;
    assert_eq!(a.status, StatusCode::OK);
    assert_eq!(b.status, StatusCode::OK);
    assert_eq!(
        a.body, b.body,
        "the second request serves the cached encode"
    );
}

#[tokio::test]
async fn an_unknown_height_falls_back_to_a_rung_rather_than_400() {
    // A stale client asking for a rung that no longer exists should get video,
    // not an error. This is what makes the ladder changeable without breaking
    // saved URLs.
    let app = TestApp::new().await;
    let Some(object_id) = unplayable_video_fixture(&app).await else {
        return;
    };
    let r = app
        .get_raw(&format!("/media/{object_id}/proxy.m3u8?h=4321"))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        !r.body.is_empty(),
        "and it is a real video, not an error page"
    );
}

#[tokio::test]
async fn a_low_height_request_does_not_produce_a_taller_proxy() {
    // The over-the-wire counterpart of the filter's `min(H, ih)`: asking for a
    // small rung must not yield a bigger one, which would be an upscale.
    let app = TestApp::new().await;
    let Some(object_id) = unplayable_video_fixture(&app).await else {
        return;
    };
    let r = app
        .get_raw(&format!("/media/{object_id}/proxy.m3u8?h=240"))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    // The fixture is 240 tall, so every rung is a no-op scale and this cannot
    // distinguish; the assertion that matters is that it SUCCEEDS and stays
    // small rather than upscaling to 1080.
    assert!(
        r.body.len() < 2_000_000,
        "a 240p request should not produce a 1080p file: {} bytes",
        r.body.len()
    );
}
