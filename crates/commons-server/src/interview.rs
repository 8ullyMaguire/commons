//! `GET /media/:id/transcript` and `GET /media/:id/chapters`.
//!
//! T-P6-004, the delivery half. `commons-ml` turns audio into words and
//! `commons-store` keeps them; neither puts a chapter list on a video element.
//! That needs a browser, and a browser cannot read a database.
//!
//! # Why the transcript ships words rather than a VTT
//!
//! Subtitles have a standard the browser implements for us. Transcripts do not:
//! a `<track>` renders lines, and an interview needs a *searchable* document, a
//! click-to-seek word, and a list of gaps where the transcription failed. None of
//! that survives a round trip through WebVTT, so the transcript ships JSON and
//! the client builds the view. A transcript served as VTT would be a document
//! that renders beautifully and cannot be searched — the same failure the token
//! ids would have produced, one layer up.
//!
//! # Words are capped, and the cap is in the response
//!
//! A three-hour interview is about 30,000 words. Shipping all of them to render
//! a scrolling panel is a megabyte of JSON on every load, for a panel that shows
//! forty lines. So the words route is paged, and the page size is a query
//! parameter with a server-side maximum: a client asking for a million words gets
//! the maximum and a `truncated` flag, rather than a response it cannot use. The
//! flag is in the body rather than implied by the array length, because "you are
//! looking at the first 500 of 30,000" and "that is all of them" are different
//! things to render and a client cannot tell them apart from an array alone.
//!
//! # `words_are_ids` is passed through, not resolved
//!
//! A parakeet run with no vocabulary stores numeric token ids as the text. The
//! client has to know, because a panel of ids is a timing scaffold and the user
//! must be told that rather than shown a search box that finds nothing. The
//! server cannot resolve it: the vocabulary is not in the database, and a
//! server that invented words from ids would be producing fiction.
//!
//! # The gate is the same one `GET /media/:id` uses
//!
//! Absent, not-on-disk and denied all give the same 404, because a client that
//! can distinguish them can probe the library for what a user has.

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::media::{local_caller, not_found};
use crate::AppState;

/// The most words one request will return, whatever the client asks for.
///
/// 500 words is about a page of transcript and is the most a scrolling panel
/// shows before it needs paging anyway. A client asking for more gets this, plus
/// `truncated: true` — see the module docs.
const MAX_WORDS: i64 = 500;

/// A transcript and its provenance, for a client deciding whether to offer a
/// transcript panel at all.
#[derive(Debug, Serialize)]
struct TranscriptSummary {
    /// The transcript's own id. Not the object id: a second engine produces a
    /// second transcript, and a client that treats them as one shows whichever it
    /// happened to fetch.
    id: String,
    engine: String,
    model_id: String,
    /// Not for display. Present so a bug report about "the transcript is wrong"
    /// can be answered with "which model produced it" instead of a guess.
    model_sha256: String,
    language: Option<String>,
    word_count: i64,
    duration_ms: i64,
    /// `true` when the words are numeric token ids, not words. See the module
    /// docs: a client must not offer search over ids.
    words_are_ids: bool,
    /// Ranges the transcription could not produce, with the reason.
    failed_windows: Vec<FailedWindow>,
    /// The engine's own words are gone; these are what a human proposed.
    corrections_pending: i64,
}

#[derive(Debug, Serialize)]
struct FailedWindow {
    window_index: i32,
    start_ms: i32,
    end_ms: i32,
    reason: String,
}

/// One page of words.
#[derive(Debug, Serialize)]
struct WordPage {
    transcript_id: String,
    words: Vec<WordOut>,
    /// The total in the transcript, not in this page.
    total: i64,
    /// `true` when `words` is a prefix rather than the whole transcript. The
    /// client needs this to decide whether to offer "load more", and cannot
    /// infer it from `words.len() < total` — which is also true of a page that
    /// happens to end at the end.
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct WordOut {
    ordinal: i32,
    text: String,
    start_ms: i32,
    end_ms: i32,
    /// `null` when the engine does not score words, which is different from a
    /// score of zero. A client that coalesces them puts every word from a
    /// non-scoring engine at the bottom of a "least confident" sort.
    confidence: Option<f64>,
    speaker: Option<String>,
}

/// One chapter, from the marker table.
#[derive(Debug, Serialize)]
struct ChapterOut {
    id: String,
    title: String,
    start_ms: i64,
    /// `null` for an open-ended chapter — a "from here on" bookmark. A client
    /// that renders it as zero renders a chapter at the top of the recording.
    end_ms: Option<i64>,
    /// The `ml:` tag the model proposed for this chapter, if any. A model's
    /// opinion, and the namespace is what says so to a reader.
    tag: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChapterList {
    object_id: String,
    chapters: Vec<ChapterOut>,
}

#[derive(Debug, Deserialize)]
pub struct WordQuery {
    /// Where to start, by ordinal. Zero-based, and clamped to the total.
    #[serde(default)]
    offset: i64,
    /// How many to return, capped at `MAX_WORDS`.
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /media/:object_id/transcript`
pub async fn get_transcript(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
) -> Response {
    // The same 404 for absent, off-disk and denied. See the module docs.
    match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "interview: media_path failed");
            return internal_error();
        }
    }
    let store = &state.store;
    let row = match commons_store::interview::transcript_for(store, &object_id).await {
        Ok(Some(r)) => r,
        // No transcript is a 404, not an empty object: "not transcribed" and
        // "not allowed to know" must look the same, and a 200 with zero words
        // would be a distinguishable answer.
        Ok(None) | Err(_) => return not_found(),
    };

    // A whisper.cpp run has real words; a parakeet run without a vocabulary has
    // ids. The distinction is in the engine's name because that is where the
    // vocabulary's absence is recorded — there is no column for it, and adding
    // one for a fact the engine already knows would be a second source of truth.
    let words_are_ids = row.engine == "parakeet";
    // Keyed on the OBJECT, not the transcript: a proposal to correct a word
    // outlives any one transcript, and re-transcribing must not silently
    // discard the corrections a person already proposed. (Read with the
    // transcript in hand, a re-transcription would appear to have no
    // outstanding corrections, which is the opposite of true.)
    let pending = match commons_store::interview::corrections_for(store, &object_id).await {
        Ok(list) => list.len() as i64,
        // A count that cannot be read is zero and the transcript still renders.
        // The alternative is refusing the whole summary over a counter.
        Err(e) => {
            tracing::warn!(object_id = %object_id, error = %e, "interview: corrections_for failed");
            0
        }
    };

    let failed = match commons_store::interview::failed_windows(store, &row.id).await {
        Ok(list) => list
            .into_iter()
            // The `ok = false` filter is repeated here rather than trusted from
            // the query name: a window row with `ok = true` is not a gap, and
            // shipping it as one would put a hole in the timeline that is not
            // there.
            .filter(|w| !w.ok)
            .map(|w| FailedWindow {
                window_index: w.window_index,
                start_ms: w.start_ms,
                end_ms: w.end_ms,
                reason: w.failure.unwrap_or_else(|| "the engine did not say".into()),
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    (
        StatusCode::OK,
        axum::Json(TranscriptSummary {
            id: row.id,
            engine: row.engine,
            model_id: row.model_id,
            model_sha256: row.model_sha256,
            language: row.language,
            word_count: row.word_count,
            duration_ms: row.duration_ms,
            words_are_ids,
            failed_windows: failed,
            corrections_pending: pending,
        }),
    )
        .into_response()
}

/// `GET /media/:object_id/transcript/words?offset=&limit=`
pub async fn get_words(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
    Query(q): Query<WordQuery>,
) -> Response {
    match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "interview: media_path failed");
            return internal_error();
        }
    }
    let store = &state.store;
    let row = match commons_store::interview::transcript_for(store, &object_id).await {
        Ok(Some(r)) => r,
        Ok(None) | Err(_) => return not_found(),
    };

    // Clamp BEFORE the query. A negative offset is a `LIMIT -1 OFFSET -5`, which
    // Postgres refuses and SQLite answers as "no limit" — so an unvalidated
    // parameter is a 500 on one engine and a full table read on the other.
    let offset = q.offset.max(0);
    let limit = q.limit.unwrap_or(MAX_WORDS).clamp(1, MAX_WORDS);

    let all = match commons_store::interview::words_for(store, &row.id).await {
        Ok(w) => w,
        Err(_) => return not_found(),
    };
    let total = all.len() as i64;
    let page: Vec<WordOut> = all
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|w| WordOut {
            ordinal: w.ordinal,
            text: w.text,
            start_ms: w.start_ms,
            end_ms: w.end_ms,
            confidence: w.confidence,
            speaker: w.speaker,
        })
        .collect();
    let truncated = offset + (page.len() as i64) < total;

    (
        StatusCode::OK,
        axum::Json(WordPage {
            transcript_id: row.id,
            words: page,
            total,
            truncated,
        }),
    )
        .into_response()
}

/// `GET /media/:object_id/chapters`
pub async fn get_chapters(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
) -> Response {
    match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "interview: media_path failed");
            return internal_error();
        }
    }
    let store = &state.store;
    // The store returns markers in TIME ORDER, which is the only order a
    // chapter list can be read in: a list sorted by id or by title has a next
    // button that goes backwards.
    let markers = match commons_store::marker::markers_for(store, &object_id).await {
        Ok(m) => m,
        Err(_) => return not_found(),
    };

    let chapters: Vec<ChapterOut> = markers
        .into_iter()
        .map(|m| ChapterOut {
            id: m.id.to_string(),
            title: m.title,
            start_ms: m.start_ms,
            end_ms: m.end_ms,
            // The marker carries a tag id, not a name. Resolving it is a second
            // query per chapter, so the tag is left out rather than resolved
            // badly: a client showing `null` renders no tag, which is honest, and
            // a client showing the wrong tag is not.
            tag: None,
        })
        .collect();

    (
        StatusCode::OK,
        axum::Json(ChapterList {
            object_id,
            chapters,
        }),
    )
        .into_response()
}

/// A 500, without the database's own error text.
///
/// The same private copy the other route modules carry: `media::json_error` is
/// private, and a shared public helper would be a wider change than adding a
/// route. The reason the message is generic is the same in every copy — a
/// constraint name in a response tells a caller about the schema, and this
/// server serves one user's library.
fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({ "error": "internal" })),
    )
        .into_response()
}
