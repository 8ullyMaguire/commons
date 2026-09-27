//! `GET /media/:id/proxy.m3u8` — the on-demand proxy, §11.5.
//!
//! T-P6-001. Spec §4 and §6. The transcode itself is
//! [`commons_media::transcode`]; this file decides *whether* to transcode, under
//! the same consent gate `/media/:id` uses, and then serves the result with
//! range support so the client can seek.
//!
//! # Why the route is `.m3u8` but serves an MP4
//!
//! The ticket names `proxy.m3u8`, and the extension is kept because a client
//! configured for an HLS URL finds it. But the ladder in
//! [`commons_store::playback`] is a progressive ladder — one file per rung, not
//! segments — so the bytes are a plain MP4. Serving them under an `.m3u8` name
//! would make a strict HLS client try to parse an MP4 as a playlist and fail,
//! which is worse than a slightly wrong extension. So the route answers
//! `application/mp4` explicitly and the name is treated as a stable URL rather
//! than a promise about the format. Changing it to a real HLS ladder is
//! T-P9-002's per-role work, and §11.5's playlist plumbing is named there.
//!
//! # The two decisions that are not the store's
//!
//! 1. **A file the browser can already play is not proxied.** The proxy costs a
//!    transcode — minutes of CPU and a disk write — and §11.5 exists for codecs
//!    a browser cannot decode. Transcoding a playable mp4 to another mp4 is the
//!    expensive failure. So the route probes, asks
//!    [`commons_store::playback::rung_for`], and 404s rather than transcodes
//!    when the answer is `None`: a client asking for a proxy of a file that needs
//!    no proxy is a client bug, and answering with the original would hide it.
//! 2. **A transcode failure is a 502 with the reason, not a 500.** The failure is
//!    in ffmpeg and the file, not in the server, and the distinction tells a user
//!    whether to retry or to report a broken file.

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use commons_media::probe;
use commons_media::transcode::{CacheKey, TranscodeError, Transcoder};
use commons_store::playback::{rung_for, Rung, SourceCaps};
use serde::Deserialize;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::media::{local_caller, not_found};
use crate::range::{content_range, resolve, RangeError, RangeSpec, ACCEPT_RANGES};
use crate::AppState;

/// The query string. `h` picks a rung, and its absence means the top one.
///
/// A parameter rather than a path segment so the URL is stable when the ladder
/// changes: a client that hard-codes `/proxy-720.m3u8` breaks when 720 is
/// renamed, while `?h=720` can be reinterpreted later. Unknown values fall back
/// to the highest rung rather than 400 — a stale client asking for a rung that
/// no longer exists should get video, not an error.
#[derive(Debug, Default, Deserialize)]
pub struct ProxyQuery {
    pub h: Option<u32>,
}

/// `GET /media/:id/proxy.m3u8`.
pub async fn get_proxy(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
    Query(q): Query<ProxyQuery>,
    headers: HeaderMap,
) -> Response {
    // The gate. Identical to `/media/:id` and for the same reason: `None` covers
    // absent, not-on-disk, and denied, and all three answer 404 so this route
    // cannot be used to ask what is in the library.
    let location = match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(loc)) => loc,
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "proxy: media_path failed");
            return internal_error();
        }
    };

    // Probe before transcoding. A proxy of a file that needs no proxy is a
    // client bug, and answering with the original would hide it.
    let info = match probe::probe(&location.path) {
        Ok(i) => i,
        Err(e) => {
            // A file the index has and ffprobe cannot read is a real, common
            // state (a partial copy, a codec the build lacks). It is named
            // rather than reported as a server fault.
            tracing::warn!(object_id = %object_id, error = %e, "proxy: probe failed");
            return unprocessable(format!("cannot read the source file: {e}"));
        }
    };

    let caps = source_caps(&info);
    let Some(rung) = rung_for(&caps) else {
        return unprocessable(format!(
            "this file needs no proxy: it is {} / {} / {}, which a browser plays directly",
            caps.container, caps.video_codec, caps.audio_codec
        ));
    };

    // The requested height selects within the ladder, and never above it: a
    // client asking for 1080 of a source that only needs a 480 proxy gets 480,
    // because a rung the source does not need is an upscale.
    let rung = match q.h {
        Some(h) => rung_at_or_below(rung, h),
        None => rung,
    };

    // The cache key is the content hash the index already recorded, so the route
    // does not re-hash a multi-gigabyte file on every request. When the column
    // is NULL — a file indexed before hashing, or one whose hash failed — the
    // file's size and mtime stand in, which is a weaker key but still stable for
    // a file that is not being rewritten.
    let digest = match stored_hash(&state, &object_id, &location.path).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "proxy: hash lookup failed");
            return internal_error();
        }
    };

    let cache_dir = state.config.cache_dir().join("proxy");
    let transcoder = Transcoder::new(cache_dir);

    // ffmpeg is a blocking process and a multi-minute one. It runs on
    // `spawn_blocking` so it does not occupy an async worker: a single request
    // would otherwise stall every other request the server is handling, which
    // for a "play this video" click is the whole feature.
    let key = CacheKey::new(digest, rung);
    let path = location.path.clone();
    let t = transcoder.clone();
    let produced =
        tokio::task::spawn_blocking(move || t.transcode(&path, &key.content_hash, rung)).await;

    let out_path = match produced {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return transcode_failed(&e),
        Err(e) => {
            // The blocking task itself was cancelled or panicked. Distinct from
            // an ffmpeg failure, and saying so is the difference between "retry"
            // and "this file is broken".
            tracing::error!(object_id = %object_id, error = %e, "proxy: transcode task failed");
            return transcode_failed(&TranscodeError::FfmpegFailed {
                status: None,
                detail: format!("the transcode task did not complete: {e}"),
            });
        }
    };

    serve_file(out_path, headers).await
}

/// `GET /media/:id/caps` -- what the server knows about this file's playability.
///
/// # Why this exists, and why it is a separate call
///
/// The player has to know whether to point a `<video>` at the file or at the
/// proxy, and it has to know BEFORE the element is constructed: a source the
/// browser cannot decode produces a `<video>` that never fires `canplay`, and
/// the user gets a black rectangle with a play button that does nothing. So the
/// decision cannot be made after an error.
///
/// The alternative -- deriving it from the object row -- does not work, and the
/// reason is worth recording: `ObjectRow` has no container, no codecs and no
/// frame rate, because nothing in the library views needs them and the database
/// has no columns for them either. So a client guessing from a row is guessing,
/// and `needsProxy({})` is permanently `true`: every file proxied, a transcode
/// per file, for files that needed none. Which is the same silent cost as the
/// `format_name` bug in the ladder, one layer up.
///
/// So the client asks, and the answer comes from the same probe and the same
/// [`rung_for`] the proxy itself uses. One decision, one implementation, and a
/// client that gets the real answer instead of a conservative guess.
///
/// The cost is one ffprobe per open, which is milliseconds on a small file and
/// tens on a large one. That is a real cost, and the reason a future cache is
/// keyed on the file's hash rather than its path: this endpoint is correct now
/// and expensive, and the fix is to remember the answer, not to stop asking.
///
/// `rung` is `null` when the file needs no proxy, which is the case the caller
/// exists to detect. A 200 with `rung: null` rather than a 404, because "this
/// file is fine" is an answer and not a failure.
pub async fn get_caps(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
) -> Response {
    // The same gate as `/media/:id` and `/proxy.m3u8`, for the same reason: one
    // consent decision, made in one place, rather than three that can disagree.
    let location = match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(loc)) => loc,
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "caps: media_path failed");
            return internal_error();
        }
    };
    if !location.is_present() {
        return not_found();
    }

    // A probe failure is NOT a 404 and NOT a 500. It means the file is indexed
    // and unreadable, which is a state a user can act on differently from either.
    let info = match probe::probe(&location.path) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(object_id = %object_id, error = %e, "caps: probe failed");
            return unprocessable(format!("cannot read the source file: {e}"));
        }
    };

    let caps = source_caps(&info);
    let rung = rung_for(&caps);
    // Width, height, fps and rotation come off the FIRST video stream, which is
    // the same stream `source_caps` reads the codec from -- so the shape a client
    // is told about and the shape the ladder judged are the same file's. A file
    // with no video stream reports nulls rather than zeros, because "0x0" reads
    // as a real measurement and `fps: 0` is what a failed probe would look like.
    let video = info.video_streams.first();
    let json = serde_json::json!({
        "container": caps.container,
        "video_codec": caps.video_codec,
        "audio_codec": caps.audio_codec,
        "width": video.map(|v| v.width),
        "height": video.map(|v| v.height),
        "fps": video.map(|v| v.fps),
        "rotation": video.map(|v| v.rotation),
        "duration_ms": info.duration_ms,
        "rung": rung.map(|r| r.height()),
    });
    (StatusCode::OK, axum::Json(json)).into_response()
}

/// The rung for a requested height, never above the one the source needs.
///
/// A request for 1080 of a source that only needs a 480 proxy gets 480: the
/// alternative is upscaling, which is what the transcode filter's `min(H, ih)`
/// exists to prevent, and asking over HTTP must not be a way around it.
///
/// Returns `needed` unchanged when the request is lower than `needed` too,
/// because `needed` is already the least work that makes the file playable --
/// serving 480 to a client that asked for 360 would transcode less than the
/// decision requires, and the ladder has no rung below `Lowest`.
fn rung_at_or_below(needed: Rung, requested: u32) -> Rung {
    // Ascending by height: Lowest (480) < Medium (720) < Highest (1080).
    let ladder = [Rung::Lowest, Rung::Medium, Rung::Highest];
    ladder
        .iter()
        .copied()
        .filter(|r| r.height() <= requested && r.height() <= needed.height())
        .max_by_key(|r| r.height())
        // `.max_by_key` over a filtered list can be empty only if `needed` is
        // taller than every rung at or below the request, which cannot happen --
        // but returning `needed` keeps the total function honest rather than
        // making the caller handle an Option the type system says is impossible.
        .unwrap_or(needed)
}

/// The source's container and first streams, as the ladder wants them.
fn source_caps(info: &probe::MediaInfo) -> SourceCaps {
    SourceCaps {
        container: info.format_name.clone(),
        video_codec: info
            .video_streams
            .first()
            .map(|s| s.codec_name.clone())
            .unwrap_or_default(),
        audio_codec: info
            .audio_streams
            .first()
            .map(|s| s.codec_name.clone())
            .unwrap_or_default(),
    }
}

/// The cache digest for this object: the recorded hash, or a size+mtime stand-in.
async fn stored_hash(
    state: &AppState,
    object_id: &str,
    path: &std::path::Path,
) -> Result<String, commons_store::db::StoreError> {
    use commons_store::db::Store;
    use sqlx::Row;

    macro_rules! q {
        ($p:expr, $ph:literal) => {
            sqlx::query(&format!(
                "SELECT f.hash_blake3, f.size_bytes, f.mtime_ns FROM file f \
                 INNER JOIN object o ON o.id = f.object_id \
                 WHERE f.object_id = {} AND f.state = 'present' \
                 ORDER BY f.path",
                if $ph { "$1" } else { "?" }
            ))
            .bind(object_id)
            .fetch_optional($p)
            .await
        };
    }

    // Map to a plain `Option<String>` inside each arm: `SqliteRow` and `PgRow`
    // are unrelated Rust types, and letting either escape the `match` is E0308.
    // `store::playback::get_playback` carries the same note for the same reason.
    let stored: Option<String> = match &state.store {
        Store::Sqlite(p) => q!(p, false)
            .map_err(commons_store::db::StoreError::Query)?
            .and_then(|r| r.get::<Option<String>, _>("hash_blake3")),
        Store::Postgres(p) => q!(p, true)
            .map_err(commons_store::db::StoreError::Query)?
            .and_then(|r| r.get::<Option<String>, _>("hash_blake3")),
    };

    // `media_path` already found the file, so an absent row here should be
    // unreachable. If it is not, the size+mtime stand-in is slower to trust but
    // always available, which beats a 500 for a row that demonstrably exists.
    match stored.filter(|s| !s.is_empty()) {
        Some(h) => Ok(h),
        None => Ok(fallback_digest(path)),
    }
}

/// A digest for a file with no recorded hash.
///
/// `size-mtime`, NOT a content hash. Hashing a 40 GB file on the first play
/// would take minutes and is the right thing to do in the *scanner*, where the
/// cost is paid once at index time; doing it here would make the button feel
/// broken. The stand-in is weaker — a file edited in place with the same size
/// and mtime reuses a stale proxy — and that is the honest trade at this layer.
fn fallback_digest(path: &std::path::Path) -> String {
    let (size, mtime) = std::fs::metadata(path)
        .map(|m| (m.len(), m.modified().ok()))
        .unwrap_or((0, None));
    let mtime = mtime
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("s{size}-{mtime}")
}

/// Serve a finished proxy with range support.
///
/// The range handling is `commons-server`'s own `range` module rather than a
/// second implementation, for the reason the module's own docs give: a second
/// parser is a second set of answers to "what does `bytes=0-` mean", and the
/// media route and this one disagreeing is a bug that only shows up as a seek
/// that fails on proxied files and works on originals.
async fn serve_file(path: PathBuf, headers: HeaderMap) -> Response {
    let meta = match tokio::fs::metadata(&path).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "proxy: output vanished before it could be served");
            return internal_error();
        }
    };
    let size = meta.len();
    let range_header = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    let spec = match resolve(range_header, size) {
        Ok(s) => s,
        Err(kind) => {
            let reason = match kind {
                RangeError::Malformed => "malformed",
                RangeError::Unsatisfiable => "unsatisfiable",
            };
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (header::ACCEPT_RANGES, ACCEPT_RANGES.to_owned()),
                    (header::CONTENT_RANGE, format!("bytes */{size}")),
                ],
                format!(r#"{{"error":"range_not_satisfiable","reason":"{reason}"}}"#),
            )
                .into_response();
        }
    };

    let (start, len, status) = match spec {
        RangeSpec::Whole => (0, size, StatusCode::OK),
        RangeSpec::One(r) => (r.start, r.end - r.start, StatusCode::PARTIAL_CONTENT),
        // Multipart is unimplemented in the shared module, and the media route
        // already answers it. Mirroring that answer rather than inventing a
        // third one: a client that asked for two ranges gets the whole body,
        // which is what a browser does with a single-range seek anyway.
        RangeSpec::Multiple => (0, size, StatusCode::OK),
    };

    let mut builder = Response::builder()
        .header(header::ACCEPT_RANGES, ACCEPT_RANGES)
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::CACHE_CONTROL, "private, max-age=3600")
        .header(header::CONTENT_LENGTH, len);
    if let RangeSpec::One(r) = spec {
        builder = builder.header(header::CONTENT_RANGE, content_range(r, size));
    }

    match tokio::fs::File::open(&path).await {
        Ok(mut f) => {
            if start > 0 && f.seek(std::io::SeekFrom::Start(start)).await.is_err() {
                return internal_error();
            }
            let mut buf = vec![0u8; len as usize];
            match f.read_exact(&mut buf).await {
                Ok(_) => builder
                    .status(status)
                    .body(axum::body::Body::from(buf))
                    .expect("a valid response"),
                Err(e) => {
                    tracing::error!(error = %e, "proxy: read of the finished encode failed");
                    internal_error()
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "proxy: finished encode could not be opened");
            internal_error()
        }
    }
}

fn unprocessable(message: String) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        axum::Json(serde_json::json!({ "error": "not_proxyable", "message": message })),
    )
        .into_response()
}

fn transcode_failed(e: &TranscodeError) -> Response {
    tracing::error!(error = %e, "proxy: transcode failed");
    (
        StatusCode::BAD_GATEWAY,
        axum::Json(serde_json::json!({
            "error": "transcode_failed",
            // The reason goes in the body. "proxy failed" is undiagnosable from
            // a browser, and the ffmpeg stderr line is what tells a user whether
            // to retry or to report a broken file.
            "message": e.to_string(),
        })),
    )
        .into_response()
}

fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({ "error": "internal" })),
    )
        .into_response()
}
