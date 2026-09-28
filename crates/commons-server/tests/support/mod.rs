//! Fixtures for the HTTP route tests.
//!
//! # Why a real store and a real file
//!
//! The claims in `media_route.rs` are about the *interaction* of three things:
//! the consent query, the `stat` of a file on disk, and a read at an offset.
//! A mocked store proves none of them — it proves the route calls the mock in
//! the order the test expects, which is the property that was already correct
//! and is the one that breaks least often. So: a real store on SQLite, a real
//! `tempfile`, and real bytes.
//!
//! SQLite rather than Postgres because this file is about the route, and
//! `media_db.rs` already runs the store half on both engines. Running both here
//! would test the same query twice and the route zero extra times.
//!
//! # Why every fixture gets its own directory
//!
//! Each test writes a file with a UUID-derived name into a per-test tempdir and
//! cleans up after itself. A shared directory would make `content_type_for` --
//! which reads the extension -- depend on which test ran first, and the
//! "content type is derived from the extension" test would then be testing
//! whatever extension the previous test happened to leave behind.

// Each test binary links this module whole but uses a part of it: the media
// tests use the file fixtures, the playback tests need only `TestApp` and the
// JSON helpers. Dead-code warnings for the unused remainder are the cost of one
// shared harness rather than a sign of dead code — duplicating the fixture
// builders per test file to silence them would be the worse trade, because the
// two copies would then drift.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use commons_server::{router, AppState, Config};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A response, already collected.
///
/// Collecting in the helper rather than in each test means a test that forgets
/// to check the body still reads the bytes, so `content_length_matches_the_body`
/// cannot rot into a header-only assertion.
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Vec<u8>,
}

impl TestResponse {
    /// The length the `Content-Length` header claims, or 0 if absent.
    pub fn declared_len(&self) -> u64 {
        self.headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }
}

/// A running server with a tempdir and a store behind it.
pub struct TestApp {
    state: Arc<AppState>,
    /// Kept alive for the length of the test: dropping it removes the files.
    _dir: tempfile::TempDir,
}

impl TestApp {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().expect("a tempdir");
        let data_dir = dir.path().to_path_buf();

        // An explicit `Config` rather than a test constructor that does not
        // exist. `RunMode::Library` is the mode that opens SQLite in the data
        // directory, which is what makes a per-test tempdir the right isolation
        // here: the database file and the media files live together, and
        // dropping the tempdir removes both.
        let config = Config {
            public_base_url: "http://127.0.0.1:9999".to_string(),
            mode: commons_server::RunMode::Library,
            data_dir,
            bind: "127.0.0.1:0".to_owned(),
            metrics: false,
            // T-P6-005: off, as it is everywhere else. A test app that turned
            // DLNA on would bind a UDP socket per test, and a suite that binds
            // a fixed port is a suite that fails on the second run.
            dlna: commons_server::config::DlnaConfig {
                enabled: false,
                bind: "127.0.0.1:0".to_owned(),
                location_base: "http://127.0.0.1:9999".to_string(),
                friendly_name: "commons-test".to_owned(),
            },
        };
        let state = Arc::new(AppState::open(config).await.expect("the store opens"));

        Self { state, _dir: dir }
    }

    pub fn store(&self) -> &commons_store::Store {
        &self.state.store
    }

    /// `GET /media/{object_id}`, with an optional `Range` header.
    pub async fn get(&self, object_id: &str, range: Option<&str>) -> TestResponse {
        let mut builder = Request::builder().uri(format!("/media/{object_id}"));
        if let Some(r) = range {
            builder = builder.header(header::RANGE, r);
        }
        self.send(builder.body(Body::empty()).expect("a request"))
            .await
    }

    /// A JSON `GET`, for the routes that answer with an object rather than
    /// bytes. `/media` needed `get`; the playback pair needs the parsed body
    /// and nothing else, so this returns the raw bytes and lets the test
    /// deserialize -- a helper that handed back a typed struct would hide the
    /// wire format, which is the thing a client actually depends on.
    pub async fn get_json(&self, path: &str) -> TestResponse {
        self.send(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("a request"),
        )
        .await
    }

    /// A JSON `PUT` with the given body, for the routes that take one.
    pub async fn put_json(&self, path: &str, body: &str) -> TestResponse {
        self.send(
            Request::builder()
                .method("PUT")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_owned()))
                .expect("a request"),
        )
        .await
    }

    /// A JSON `POST` with the given body. T-P5-007 part 2B needs it for
    /// `/api/share`; `put_json` is the same call with a different verb.
    pub async fn post_json(&self, path: &str, body: &str) -> TestResponse {
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_owned()))
                .expect("a request"),
        )
        .await
    }

    /// A `DELETE` at a path, for `/api/share/:id`.
    pub async fn delete(&self, path: &str) -> TestResponse {
        self.send(
            Request::builder()
                .method("DELETE")
                .uri(path)
                .body(Body::empty())
                .expect("a request"),
        )
        .await
    }

    /// Any path, for the health-route tests.
    pub async fn get_raw(&self, path: &str) -> TestResponse {
        let request = Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("a request");
        self.send(request).await
    }

    /// An arbitrary request, for the tests that need their own headers.
    ///
    /// `get` and `put_json` cover most cases; this exists for `Range`, which
    /// needs a header on a path that is not `/media/:id` and a body assertion
    /// the convenience helpers do not return.
    pub async fn send_raw(&self, request: Request<Body>) -> TestResponse {
        self.send(request).await
    }

    async fn send(&self, request: Request<Body>) -> TestResponse {
        let response = router(self.state.clone())
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("a body")
            .to_bytes()
            .to_vec();
        TestResponse {
            status,
            headers,
            body,
        }
    }

    /// Write a file into this app's directory and return its path.
    pub fn write_file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self._dir.path().join(name);
        std::fs::write(&path, bytes).expect("a file is written");
        path
    }

    pub fn remove_file(&self, path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// An object seeded with a real file at a real path.
///
/// Only the id. An earlier version also returned the path "for a caller that
/// needs to delete the file", and clippy was right that nothing reads it: the
/// one test that removes a file gets a helper that removes it. A struct field
/// kept for a hypothetical future test is a field nobody has checked.
pub struct MediaFixture {
    pub object_id: String,
}

/// Seed an object whose file has `bytes` in it, consented as `tier`.
pub async fn media_fixture(app: &TestApp, bytes: &[u8]) -> MediaFixture {
    let name = format!("{}.mp4", uuid::Uuid::new_v4().simple());
    let path = app.write_file(&name, bytes);
    let object_id = seed(app, &path, bytes.len() as i64, "self_published", "present").await;
    MediaFixture { object_id }
}

/// The same, with a chosen extension, for the content-type test.
pub async fn media_fixture_named(app: &TestApp, bytes: &[u8], ext: &str) -> String {
    let name = format!("{}.{ext}", uuid::Uuid::new_v4().simple());
    let path = app.write_file(&name, bytes);
    seed(app, &path, bytes.len() as i64, "self_published", "present").await
}

/// An object consented at `denied`, so the gate refuses it.
///
/// Returns the object id, not a fixture: there is no path to hand back,
/// because a test that holds the denied path could use it to check the route
/// directly and would then not be testing the gate at all.
pub async fn media_fixture_denied(app: &TestApp, bytes: &[u8]) -> String {
    let name = format!("{}.mp4", uuid::Uuid::new_v4().simple());
    let path = app.write_file(&name, bytes);
    seed(app, &path, bytes.len() as i64, "denied", "present").await
}

/// An object at an arbitrary consent tier, so a test can pin a permission the
/// other fixtures cannot see.
///
/// The reason this exists: every other fixture is `self_published`, which is in
/// `ConsentTiers::PUBLIC`, so a fixture of only those cannot distinguish "the
/// route serves this because the caller is allowed" from "the route serves this
/// to anybody". `unverified` is the tier that separates the two — the operator
/// sees it and a visitor does not — so it is the one a mutation to the caller's
/// identity would change.
pub async fn media_fixture_at_tier(app: &TestApp, bytes: &[u8], tier: &str) -> String {
    let name = format!("{}.mp4", uuid::Uuid::new_v4().simple());
    let path = app.write_file(&name, bytes);
    seed(app, &path, bytes.len() as i64, tier, "present").await
}

/// A file whose bytes on disk do NOT match the size the index recorded.
///
/// This is the stale-index case: the library was rescanned, the file was
/// replaced or truncated since, and the row still says the old length. The
/// route resolves a `Range` against ONE length and reads from the other, and
/// this is the fixture that makes which one matter observable — every other
/// fixture writes a file whose size matches the row exactly, so the two lengths
/// are equal and a mutation between them is invisible.
///
/// `recorded` is what the index says, `on_disk` is what is actually written.
pub async fn media_fixture_size_mismatch(app: &TestApp, on_disk: &[u8], recorded: i64) -> String {
    let name = format!("{}.mp4", uuid::Uuid::new_v4().simple());
    let path = app.write_file(&name, on_disk);
    seed(app, &path, recorded, "self_published", "present").await
}

/// A file that exists in the index and is gone from disk.
pub async fn media_fixture_then_remove(app: &TestApp, bytes: &[u8]) -> String {
    let name = format!("{}.mp4", uuid::Uuid::new_v4().simple());
    let path = app.write_file(&name, bytes);
    let object_id = seed(app, &path, bytes.len() as i64, "self_published", "present").await;
    app.remove_file(&path);
    object_id
}

/// Insert the object, the file row and the consent row.
///
/// Async rather than wrapped in `block_on`, which is what the first version did
/// and which would have deadlocked: `block_on` inside a `#[tokio::test]` parks
/// the current thread's executor waiting for a future that needs the very
/// runtime that is parked. Three deadlocked tests and no error message is a
/// worse afternoon than one `async`.
async fn seed(app: &TestApp, path: &Path, size: i64, tier: &str, state: &str) -> String {
    let seed = uuid::Uuid::new_v4().simple().to_string();
    let object_id = format!("o-{seed}");
    let t = commons_core::ts::now();
    let store = app.store();

    {
        // `file.size_bytes` is nullable in the schema and the route reads it as
        // a `u64`, so it is seeded explicitly here. Left NULL it becomes 0 and
        // every range in the route resolves against an empty body.
        sqlx::query(
            "INSERT INTO object (id, kind, title, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&object_id)
        .bind("clip")
        .bind(format!("{seed}-obj"))
        .bind(t.clone())
        .bind(t.clone())
        .execute(store.pool())
        .await
        .expect("the object is inserted");

        sqlx::query(
            "INSERT INTO file (id, object_id, path, state, size_bytes) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(format!("f-{seed}"))
        .bind(&object_id)
        .bind(path.to_string_lossy().to_string())
        .bind(state)
        .bind(size)
        .execute(store.pool())
        .await
        .expect("the file is inserted");

        sqlx::query(
            "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(format!("cr-{seed}"))
        .bind(&object_id)
        .bind(tier)
        .bind(0i32)
        .bind(t)
        .execute(store.pool())
        .await
        .expect("the consent record is inserted");
    }

    object_id
}

/// Write a real video a browser CANNOT play, and seed it as an object.
///
/// The proxy's whole reason to exist is a codec no browser decodes, so a
/// fixture of random bytes would pass every route test while exercising
/// nothing: ffprobe would fail on it, the route would answer 422 "cannot read
/// the source", and a green suite would say the transcode path works. So this
/// generates a genuine Matroska file with an MPEG-4 Part 2 video stream --
/// a codec that exists, that ffmpeg reads, and that no browser plays.
///
/// Returns the object id. Skips (returning `None`) when ffmpeg is absent, so a
/// machine without it does not fail the suite; the tests that need it assert on
/// the `None` case rather than silently passing.
pub async fn unplayable_video_fixture(app: &TestApp) -> Option<String> {
    if !ffmpeg_available() {
        return None;
    }
    let name = format!("{}.mkv", uuid::Uuid::new_v4().simple());
    let path = app._dir.path().join(&name);
    let made = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=10",
            "-c:v",
            "mpeg4",
            "-an",
        ])
        .arg(&path)
        .output()
        .expect("ffmpeg runs");
    if !made.status.success() {
        return None;
    }
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as i64;
    Some(seed(app, &path, size, "self_published", "present").await)
}

/// A browser-playable video: h264 in mp4, which the ladder passes through.
pub async fn playable_video_fixture(app: &TestApp) -> Option<String> {
    if !ffmpeg_available() {
        return None;
    }
    let name = format!("{}.mp4", uuid::Uuid::new_v4().simple());
    let path = app._dir.path().join(&name);
    let made = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=10",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-an",
        ])
        .arg(&path)
        .output()
        .expect("ffmpeg runs");
    if !made.status.success() {
        return None;
    }
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as i64;
    Some(seed(app, &path, size, "self_published", "present").await)
}

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Record a content hash on an object's file, as the index would.
///
/// Without this the proxy falls back to a `size-mtime` digest, which works and
/// is tested -- but the *hash* path is the one that runs in production, and a
/// test that only ever exercises the fallback leaves the real path unproven.
pub async fn set_content_hash(app: &TestApp, object_id: &str, hash: &str) {
    sqlx::query("UPDATE file SET hash_blake3 = ? WHERE object_id = ?")
        .bind(hash)
        .bind(object_id)
        .execute(app.store().pool())
        .await
        .expect("the hash is recorded");
}
