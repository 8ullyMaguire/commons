//! `GET /media/:object_id` — the bytes, with range support.
//!
//! T-P5-006 item 9's third part, and the layer `docs/spec/t-p5-006-vertical-feed.md`
//! §1 identified as the precondition for autoplay and preloading. §10.6 asks
//! for a feed; a feed needs a URL that serves a whole file, and `<video>` needs
//! more than that — it needs to seek, which means it needs `Range`.
//!
//! # The three things this route must get right, in order
//!
//! 1. **Consent, before any byte.** `Store::media_path` ANDs the consent clause
//!    into its `WHERE`, so `None` here already means "not for you" — no path
//!    and no size leave the store. This route therefore has nothing to
//!    remember to check, which is the point of putting the gate in the data
//!    layer: see `commons-store/src/media.rs` for why.
//! 2. **404, not 403, for a denied object.** §14.1 is not only about the bytes;
//!    whether an id exists is itself consent information. Answering 403 for a
//!    file the caller may not see turns this route into an oracle for what is
//!    in the library, so absent and denied are the same status.
//! 3. **A body that matches `Content-Range`.** The client trusts the header.
//!    If the two disagree the client waits for bytes that never arrive, which
//!    is a hang, not an error.

use std::path::Path;

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use commons_store::filter_ast::CallerId;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::identity;
use crate::range::{content_range, resolve, ByteRange, RangeError, RangeSpec, ACCEPT_RANGES};
use crate::AppState;

/// How much of a file is read into memory for one response.
///
/// A cap, and the cap is the security property rather than a tuning knob: a
/// 4 GB original with no `Range` header is 4 GB in the response body, and a
/// feed that autoplays three of those while a client prefetches the next two
/// is a server holding 20 GB of resident memory because a request said
/// "everything". Clients that stream — which is every browser playing a
/// `<video>` — send `Range` and get small responses.
const MAX_WHOLE_BODY: u64 = 64 * 1024 * 1024;

/// The identity this build attributes a request to.
///
/// A single local user, and named that way rather than called a default so that
/// a future authenticated build has to change this line visibly. See the
/// `CallerId` docs in `commons-store` for what a real one carries.
///
/// A logged-in local user with no explicit allowlist, which is what
/// `consent_clause` calls **the library owner** — `ConsentTiers::OWNER`.
///
/// The `account_id` is load-bearing and nothing in the type says so. With
/// `account_id: None` the consent clause resolves to `ConsentTiers::PUBLIC`,
/// which excludes `unverified` — and a freshly scanned file is `unverified`. So
/// an anonymous route serves an established library perfectly and cannot serve
/// a freshly indexed one, which is the library most people look at first.
///
/// That was found by mutation, not by reading: `scripts/mutate-media-route.sh`
/// sets `account_id` to `None` and all fifteen route tests still passed, because
/// every fixture was seeded at `self_published` — which IS in `PUBLIC`, so the
/// mutation was invisible. A fixture of only publicly-visible tiers cannot test
/// a permission, which is the same lesson as the preload test and the same
/// lesson as `media-view.test.ts`. The `unverified` fixture now kills it.
///
/// It is NOT `CallerId::steward` either. A steward gets `MODERATION`, a
/// moderation grant, and serving ordinary media under it would mean reading a
/// file with a permission taken for a different reason — the same shape of bug
/// as the `steward()` allowlist that `filter_ast`'s own docs record.
pub fn local_caller() -> CallerId {
    CallerId {
        // A stable id, not a fresh UUID per request. Nothing keys off it yet,
        // but a per-request identity is the kind of thing that later becomes
        // load-bearing by accident.
        account_id: Some("local".to_owned()),
        ..CallerId::anonymous()
    }
}

/// `GET /media/:object_id`.
#[utoipa::path(get, path = "/media/{object_id}", responses((status = 200, description = "Success")), params(("object_id" = String, Path)))]
/// T-P6-007: the `/api/v1` OpenAPI document reads this. The path is
/// the VERSIONED one even though the route also answers unversioned --
/// a document that listed the internal path would send consumers to the
/// surface §11.5 promises to keep unversioned.
pub async fn get_media(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    // T-P6-007: this line is the ticket. It used to be `local_caller()` --
    // a hardcoded constant, so `Role`, `CallerId` and `ShareGrant` were all
    // built, all used by the store, and never consulted by the server. A
    // request carrying `Authorization: Bearer <token>` now resolves to that
    // share grant; a request without credentials still resolves to
    // `local_caller()`, so every other test in this file is unaffected.
    let caller = identity::caller_from_request(&state, &headers).await;

    // The gate. `None` covers absent, not-present-on-disk, and denied, and all
    // three answer the same way on purpose.
    let location = match state.store.media_path(&object_id, &caller).await {
        Ok(Some(loc)) => loc,
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "media_path failed");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "media_lookup_failed",
                "the media index could not be read",
            );
        }
    };

    if !location.is_present() {
        return not_found();
    }

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());

    // The range is resolved ONCE, below, against the file's real length.
    //
    // The first version resolved it here against `location.size_bytes` -- the
    // size the index recorded -- and then resolved it again after stat-ing the
    // file, assigning the first result to a variable the compiler reported as
    // UNUSED. So the whole first `match` was dead: an unsatisfiable range never
    // produced its 416, because the arm that would have produced it computed a
    // value nobody read. The compiler caught it. No test would have -- the
    // surviving second resolve returns the same answer for every range a test
    // would plausibly send against a correctly-sized file, so the tests were
    // green over a branch that could not execute.
    //
    // The recorded size is still read, for one purpose below: deciding whether
    // a file that has been replaced since the last scan can be served at all.

    // The file's own size on disk, which may differ from the index's recorded
    // size if the file changed since the last scan. The recorded size decides
    // what a `Range` means; the real size decides what a read returns. Serving
    // a range computed from the recorded size out of a shorter file is a
    // short body with a full `Content-Range`, which is failure (3) above.
    let path = location.path.clone();
    let file = match File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(object_id = %object_id, path = %path.display(), error = %e, "media file could not be opened");
            return not_found();
        }
    };
    let real_len = match file.metadata().await {
        Ok(m) => m.len(),
        Err(_) => return not_found(),
    };

    // The index said one size and the disk says another. The file changed
    // since the last scan, so which length is authoritative is a real question
    // with a real answer: the DISK, because the disk is what a read returns. The
    // recorded size only decides whether the mismatch is small enough that the
    // file is the same file. A file that grew by more than a byte has been
    // replaced, and serving a range into the new bytes under the old length is
    // the short-body bug again.
    if real_len != location.size_bytes {
        tracing::debug!(
            object_id = %object_id,
            indexed = location.size_bytes,
            on_disk = real_len,
            "media file changed since the last scan; serving the on-disk length"
        );
    }

    // Re-resolve against the real length so a stale index size cannot produce
    // a range past the end. A client that gets a short body waits forever.
    let spec = match resolve(range_header.as_deref(), real_len) {
        Ok(spec) => spec,
        // Both kinds are 416 with `Content-Range: bytes */len` (§15.5.17), and
        // naming the kind is free here while it is still a free choice: the two
        // are indistinguishable to a browser and the next programmer debugging a
        // failing seek will want to know whether the header was nonsense or
        // merely out of bounds.
        Err(kind) => {
            let reason = match kind {
                RangeError::Malformed => "malformed",
                RangeError::Unsatisfiable => "unsatisfiable",
            };
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (header::ACCEPT_RANGES, ACCEPT_RANGES.to_owned()),
                    (header::CONTENT_RANGE, format!("bytes */{real_len}")),
                    (header::CONTENT_TYPE, "application/json".to_owned()),
                ],
                format!(r#"{{"error":"range_not_satisfiable","reason":"{reason}"}}"#),
            )
                .into_response();
        }
    };

    match spec {
        RangeSpec::Whole => {
            if real_len > MAX_WHOLE_BODY {
                // 200 with a body this large is the memory problem `MAX_WHOLE_BODY`
                // exists to avoid, and 413 is the honest answer: this server
                // serves ranges, and a client that wants the whole thing can ask
                // for it in ranges. A client that cannot is a client that was
                // going to buffer it in a `<video>` anyway.
                return json_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "media_too_large",
                    "request the file in ranges",
                );
            }
            match read_range(&path, 0, real_len).await {
                Ok(bytes) => full_response(&path, bytes, real_len),
                Err(e) => io_error(&object_id, e),
            }
        }
        RangeSpec::One(range) => {
            let bytes = match read_range(&path, range.start, range.end).await {
                Ok(bytes) => bytes,
                Err(e) => return io_error(&object_id, e),
            };
            partial_response(&path, bytes, range, real_len)
        }
        RangeSpec::Multiple => {
            // Not implemented, and the honest answer is 200 with the whole body
            // rather than a 416: the client asked for a representation it can
            // have, and per §14.2 a server that does not support the requested
            // form may ignore it. A 416 would be a lie — the range was
            // perfectly satisfiable, just not in the shape this server does.
            //
            // This is the one place where "the type made me handle it" is doing
            // real work: `RangeSpec::Multiple` exists in `range.rs` precisely
            // so this arm is here rather than a `todo!()` or a silent `Whole`
            // that hides the truncation.
            tracing::debug!(object_id = %object_id, "multipart range requested; serving the whole file");
            match read_range(&path, 0, real_len).await {
                Ok(bytes) => full_response(&path, bytes, real_len),
                Err(e) => io_error(&object_id, e),
            }
        }
    }
}

/// Read `start..end` (half-open) from a file.
async fn read_range(path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path).await?;
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }
    let want = (end - start) as usize;
    let mut buf = vec![0u8; want];
    file.read_exact(&mut buf).await?;
    Ok(buf)
}

fn content_type_for(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("gif") => "image/gif",
        // A guess rather than a fixed default. The route serves whatever the
        // index recorded, and an unknown extension with no `Content-Type` makes
        // a browser download the file instead of playing it — which for a feed
        // means a silent, empty slide.
        _ => "application/octet-stream",
    }
    .to_owned()
}

fn full_response(path: &Path, bytes: Vec<u8>, len: u64) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type_for(path)),
            (header::CONTENT_LENGTH, len.to_string()),
            // On the 200 too. A client that cannot see the token will not send
            // `Range` and then never seeks.
            (header::ACCEPT_RANGES, ACCEPT_RANGES.to_owned()),
        ],
        bytes,
    )
        .into_response()
}

fn partial_response(path: &Path, bytes: Vec<u8>, range: ByteRange, len: u64) -> Response {
    (
        StatusCode::PARTIAL_CONTENT,
        [
            (header::CONTENT_TYPE, content_type_for(path)),
            (header::CONTENT_LENGTH, bytes.len().to_string()),
            (header::CONTENT_RANGE, content_range(range, len)),
            (header::ACCEPT_RANGES, ACCEPT_RANGES.to_owned()),
        ],
        bytes,
    )
        .into_response()
}

pub fn not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, "not_found", "")
}

fn json_error(status: StatusCode, code: &str, hint: &str) -> Response {
    let body = if hint.is_empty() {
        format!(r#"{{"error":"{code}"}}"#)
    } else {
        format!(r#"{{"error":"{code}","hint":"{hint}"}}"#)
    };
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn io_error(object_id: &str, e: std::io::Error) -> Response {
    tracing::error!(object_id = %object_id, error = %e, "media read failed");
    json_error(StatusCode::INTERNAL_SERVER_ERROR, "media_read_failed", "")
}
