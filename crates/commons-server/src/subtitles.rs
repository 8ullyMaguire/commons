//! `GET /media/:id/subtitles` and `GET /media/:id/subtitles/:doc_id.vtt`.
//!
//! T-P6-002, the delivery half. `commons-media` extracts cues and
//! `commons-store` keeps them, and neither of those puts a `<track>` on a video
//! element: that needs a byte stream a browser can fetch on its own, and a
//! browser cannot read a database. So this module is the two routes that close
//! the loop, and both are deliberately boring.
//!
//! # Why the list and the VTT are separate routes
//!
//! A client needs the track list to build `<track kind="subtitles">` elements
//! and needs the VTT bytes to render one. They have different failure modes and
//! different caching, so they are different requests: the list changes when a
//! re-extract happens, the bytes do not. Serving the list as a redirect to the
//! first track would hide every track but one.
//!
//! # The gate, and it is the same one
//!
//! `GET /media/:id` 404s for absent, not-on-disk, and denied alike, and this
//! route uses the same `media_path` call for the same reason: a client that can
//! enumerate subtitle tracks can otherwise probe the library for what a user
//! has. **A 200 with an empty list would be the leak** — the caller cannot tell
//! "no subtitles" from "not allowed to know", so absence and denial must give
//! the same bytes, which is the 404.
//!
//! # Content type, and why it is not `application/octet-stream`
//!
//! `text/vtt`. Chromium and Firefox both sniff WebVTT far more reliably when the
//! type is right, and a `<track>` served as a generic type renders nothing at
//! all with no error anywhere — the video plays and the subtitles are silently
//! absent, which is the single worst failure this feature can have.
//!
//! # Cache-Control
//!
//! `no-cache`, not `no-store` and not a long max-age. The list is cheap to
//! revalidate and must reflect a re-extract. The VTT body is immutable for a
//! given document id — a new extraction is a *new row* with a new id, because
//! the id is what makes the URL unique — so the bytes could carry a long
//! max-age, and this deliberately does not give them one, because the
//! extraction that produces a new row can also be a re-extraction of the same
//! track with different cues.

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use commons_store::subtitles as store_sub;
use serde::Serialize;

use crate::media::{local_caller, not_found};
use crate::AppState;

/// One track in the list, as the player needs it.
///
/// Not the `Document` row. The row carries `sha256`, `byte_size` and
/// `extracted_at`, which are for the extractor and for debugging, and shipping
/// them to a browser tells a user nothing they can act on. The one field that
/// looks internal and is not: `label` is already computed by the store, because
/// "which of these is English" is a question the store can answer from the row
/// and a client cannot answer from a number.
#[derive(Debug, Serialize)]
struct TrackSummary {
    /// The document id, and the whole of the VTT URL. The player does not
    /// compose this path itself — one place decides its shape.
    id: String,
    label: String,
    /// `None` when the file declares no language, which is not the same as an
    /// empty string. Serialised as `null` rather than `""` for that reason, and
    /// the client must not coalesce them.
    language: Option<String>,
    is_default: bool,
    is_forced: bool,
    is_hearing_impaired: bool,
    /// Cue count, so a client can tell a populated track from an empty one
    /// before fetching it.
    cue_count: i64,
    /// The stored format, as `subrip` / `webvtt` / `ass` / `mov_text`.
    ///
    /// Not decoration. A browser renders ASS as plain text and silently drops
    /// the positioning, colour and karaoke, so a client that cannot see the
    /// format cannot warn the user that the track they picked is not being shown
    /// the way it was authored -- it just looks like a plain subtitle file with
    /// the styling missing. `format` is what makes that warning possible at all.
    format: String,
}

/// `GET /media/:object_id/subtitles`.
pub async fn list_tracks(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
) -> Response {
    // The gate, identical to `/media/:id` and `/media/:id/proxy.m3u8`.
    match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "subtitles: media_path failed");
            return internal_error();
        }
    }

    let docs = match store_sub::list_documents(&state.store, &object_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "subtitles: list_documents failed");
            return internal_error();
        }
    };

    // A per-document cue count is a query each, and the list is a dozen rows at
    // most, so this is N+1 against a table that is indexed on `document_id` and
    // holds a few hundred rows. Worth a comment because "just count them" is
    // the kind of thing that gets optimised wrongly under load: the alternative
    // is a `GROUP BY document_id` over every subtitle cue in the library on
    // every player open, and player open is the hot path, not this table.
    let mut tracks = Vec::with_capacity(docs.len());
    for doc in &docs {
        let cue_count = match store_sub::list_cues(&state.store, &doc.id).await {
            Ok(c) => c.len() as i64,
            Err(e) => {
                // A count that cannot be read is reported as 0 and the track is
                // still listed. Dropping the track would make a database blip
                // silently remove a user's subtitles from the list, and the VTT
                // route would still serve them — so the list would be lying in
                // the other direction. Logging is the part that matters.
                tracing::warn!(
                    object_id = %object_id,
                    document_id = %doc.id,
                    error = %e,
                    "subtitles: cue count failed, reporting zero"
                );
                0
            }
        };
        tracks.push(TrackSummary {
            id: doc.id.clone(),
            label: doc.label(),
            language: doc.language.clone(),
            is_default: doc.is_default,
            is_forced: doc.is_forced,
            is_hearing_impaired: doc.is_hearing_impaired,
            cue_count,
            format: doc.format.as_str().to_string(),
        });
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        serde_json::json!({ "tracks": tracks }).to_string(),
    )
        .into_response()
}

/// `GET /media/:object_id/subtitles/:document_id.vtt`.
///
/// The `:document_id` is an id, not a row number and not a path, and it is
/// checked against *this* object rather than trusted: a valid document id for
/// another object must 404, or the route is a way to read a track off an object
/// the caller has no gate-passing claim to.
pub async fn get_vtt(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((object_id, raw_id)): AxumPath<(String, String)>,
) -> Response {
    // **The `.vtt` is part of the captured parameter, not a stripped suffix.**
    // In a route pattern `/x/:id.vtt`, axum treats `id.vtt` as the parameter
    // *name* and captures the whole segment -- `empty-1.vtt` arrives here as the
    // id. So the suffix has to be removed by hand, and a request for
    // `.../sub-abc.vtt` otherwise looks up the document `sub-abc.vtt`, which
    // does not exist, and answers 404 for a track that is there.
    //
    // This is worth stating because the 404 is *correct-looking*: no error, no
    // panic, a well-formed response, and a browser that logs "track failed to
    // load" and moves on. Every test that seeded a document and then asked for
    // it by id failed on this, and the only reason it was found is that a test
    // went through the URL rather than calling the store directly.
    let Some(document_id) = raw_id.strip_suffix(".vtt") else {
        // No suffix: not a VTT URL, and not something to guess about. The
        // 404 is deliberate rather than a redirect, because a redirect from
        // `/sub-abc` to `/sub-abc.vtt` would be a second URL for one resource.
        return not_found();
    };
    // Same gate, same reason.
    match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return not_found();
        }
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "subtitles: media_path failed");
            return internal_error();
        }
    }

    let doc = match store_sub::get_document(&state.store, document_id).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return not_found();
        }
        Err(e) => {
            tracing::error!(document_id = %document_id, error = %e, "subtitles: get_document failed");
            return internal_error();
        }
    };

    // The ownership check. Not `doc.object_id == object_id` as a string
    // comparison, and the difference is a security boundary: `object_id` is
    // caller-supplied and unescaped, and a prefix or case difference would slip
    // past a loose compare and serve one user's track under another's id.
    if doc.object_id != object_id {
        // 404, not 403. A 403 here would confirm the document exists, which is
        // the leak the gate exists to prevent.
        return not_found();
    }

    let cues = match store_sub::list_cues(&state.store, document_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(document_id = %document_id, error = %e, "subtitles: list_cues failed");
            return internal_error();
        }
    };

    // An empty track is a valid answer with a valid body: a WebVTT file with
    // no cues. Returning 404 for a track that exists with no cues would make a
    // genuinely empty file indistinguishable from a wrong URL, and the browser
    // would log a load error for a file that is correct.
    let vtt = render_vtt(&cues);

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/vtt; charset=utf-8"),
            // See the module note: no max-age, because a re-extraction of the
            // same track can change the bytes under a stable id.
            (header::CACHE_CONTROL, "no-cache"),
        ],
        vtt,
    )
        .into_response()
}

/// Cue rows to a WebVTT document.
///
/// The store owns the cue model and the media crate owns the WebVTT grammar,
/// and this function is the only place the two meet, which is why it is here and
/// not in either. Timestamps are formatted in media-relative milliseconds and
/// zero-length cues are dropped, because a WebVTT cue with an end at or before
/// its start is not rendered by any browser and a track that renders nothing is
/// indistinguishable from no track.
fn render_vtt(cues: &[store_sub::CueRow]) -> String {
    let mut out = String::with_capacity(64 + cues.len() * 48);
    out.push_str("WEBVTT\n\n");
    for c in cues {
        if c.end_ms <= c.start_ms {
            continue;
        }
        out.push_str(&vtt_timestamp(c.start_ms));
        out.push_str(" --> ");
        out.push_str(&vtt_timestamp(c.end_ms));
        out.push('\n');
        // Cue text may be multi-line, and a blank line inside a cue is what
        // terminates it, so a cue whose text contains one cannot be emitted
        // verbatim. Replacing it is lossy but keeps the cue; dropping the cue
        // loses the user's subtitle. The extractor's parser rejects the blank
        // line earlier, so this is defence in depth rather than a live path.
        out.push_str(&c.text.replace("\r\n", "\n").replace("\n\n", "\n"));
        out.push_str("\n\n");
    }
    out
}

fn vtt_timestamp(ms: i64) -> String {
    // A negative cue timestamp is nonsense, and formatting one as `00:00:00.000`
    // would put a cue at the start of the video that the extractor says begins
    // before the video does. Clamped, not wrapped: wrapping puts a cue at
    // 23:59:59.998 and renders it for a quarter of a second at the end.
    let ms = ms.max(0);
    let (h, m, s, milli) = (
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1_000) % 60,
        ms % 1_000,
    );
    format!("{h:02}:{m:02}:{s:02}.{milli:03}")
}

fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":"internal"}"#,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(start: i64, end: i64, text: &str) -> store_sub::CueRow {
        store_sub::CueRow {
            seq: 0,
            start_ms: start,
            end_ms: end,
            text: text.to_string(),
            style: None,
        }
    }

    #[test]
    fn an_empty_document_is_still_a_valid_webvtt_file() {
        // A browser given an empty but well-formed VTT shows no cues and logs
        // nothing. Given 404 it logs a load error, and the user cannot tell an
        // empty track from a broken URL.
        let v = render_vtt(&[]);
        assert_eq!("WEBVTT\n\n", v);
    }

    #[test]
    fn a_cue_is_rendered_with_both_timestamps() {
        let v = render_vtt(&[row(0, 1500, "hello")]);
        assert_eq!("WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nhello\n\n", v);
    }

    #[test]
    fn a_zero_length_cue_is_dropped() {
        // Not rendered by any browser, and a track that renders nothing is
        // indistinguishable from no track at all.
        let v = render_vtt(&[row(1000, 1000, "nothing")]);
        assert_eq!("WEBVTT\n\n", v);
    }

    #[test]
    fn an_inverted_cue_is_dropped_too() {
        // `end < start` is as unrenderable as `end == start`, and the same
        // check covers both.
        let v = render_vtt(&[row(2000, 1000, "backwards")]);
        assert_eq!("WEBVTT\n\n", v);
    }

    #[test]
    fn a_good_cue_survives_alongside_a_dropped_one() {
        // The drop is per-cue. Dropping the document would lose a subtitle the
        // user can see is there.
        let v = render_vtt(&[row(0, 0, "gone"), row(1000, 2000, "kept")]);
        assert!(v.contains("kept"), "{v}");
        assert!(!v.contains("gone"), "{v}");
    }

    #[test]
    fn hours_are_not_wrapped() {
        assert_eq!("01:00:00.000", vtt_timestamp(3_600_000));
        assert_eq!("00:00:00.000", vtt_timestamp(0));
    }

    #[test]
    fn a_negative_timestamp_clamps_rather_than_wrapping() {
        // Wrapping would put the cue at 23:59:59.999 and render it for a
        // quarter of a second at the very end of the video.
        assert_eq!("00:00:00.000", vtt_timestamp(-1));
        assert_eq!("00:00:00.000", vtt_timestamp(-999_999));
    }

    #[test]
    fn a_blank_line_in_cue_text_does_not_terminate_the_cue_early() {
        // A blank line ends a cue in WebVTT, so a cue containing one rendered
        // verbatim would be cut in half and the remainder parsed as a new cue
        // with no timing line.
        let v = render_vtt(&[row(0, 1000, "first\n\nsecond")]);
        assert_eq!(
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nfirst\nsecond\n\n",
            v
        );
    }

    #[test]
    fn carriage_returns_are_normalised() {
        let v = render_vtt(&[row(0, 1000, "a\r\nb")]);
        assert!(!v.contains('\r'), "a CR in a VTT body: {v:?}");
        assert!(v.contains("a\nb"), "{v}");
    }
}
