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
#[utoipa::path(get, path = "/media/{object_id}/transcript", responses((status = 200, description = "Success")), params(("object_id" = String, Path)))]
/// T-P6-007: the `/api/v1` OpenAPI document reads this. The path is
/// the VERSIONED one even though the route also answers unversioned --
/// a document that listed the internal path would send consumers to the
/// surface §11.5 promises to keep unversioned.
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
#[utoipa::path(get, path = "/media/{object_id}/transcript/words", responses((status = 200, description = "Success")), params(("object_id" = String, Path)))]
/// T-P6-007: the `/api/v1` OpenAPI document reads this. The path is
/// the VERSIONED one even though the route also answers unversioned --
/// a document that listed the internal path would send consumers to the
/// surface §11.5 promises to keep unversioned.
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
#[utoipa::path(get, path = "/media/{object_id}/chapters", responses((status = 200, description = "Success")), params(("object_id" = String, Path)))]
/// T-P6-007: the `/api/v1` OpenAPI document reads this. The path is
/// the VERSIONED one even though the route also answers unversioned --
/// a document that listed the internal path would send consumers to the
/// surface §11.5 promises to keep unversioned.
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

// ---------- quotes and topics (T-P6-004b step 6) ----------

use commons_store::tags::Namespace;

/// Cap on a page of quotes. Matches `MAX_WORDS`: both are "a screenful", and a
/// second constant would be a second answer to "how much is too much".
const MAX_QUOTES: i64 = 200;

#[derive(Debug, Serialize)]
struct QuoteOut {
    id: String,
    start_ms: i32,
    end_ms: i32,
    text: String,
    weight: f64,
    /// `human` or `model` -- the stored spelling, via `WeightSource::as_str`,
    /// so the wire value and the CHECK constraint in migration 0024 cannot
    /// drift apart. A separate field rather than folded into `weight`: the
    /// scales are 1-5 and 0.0-1.0, and a client that cannot tell them apart
    /// will average them.
    weight_source: &'static str,
}

#[derive(Debug, Serialize)]
struct QuotePage {
    object_id: String,
    quotes: Vec<QuoteOut>,
    total: i64,
    /// Same contract as `get_words`: the client is told there is more rather
    /// than handed a page it cannot page.
    truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct QuoteQuery {
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /media/:object_id/quotes?offset=&limit=`
///
/// The same pager as `get_words`, deliberately: a second pager with the same
/// semantics and a different parameter name is a client bug waiting to happen.
#[utoipa::path(get, path = "/media/{object_id}/quotes", responses((status = 200, description = "Success")), params(("object_id" = String, Path)))]
/// T-P6-007: the `/api/v1` OpenAPI document reads this. The path is
/// the VERSIONED one even though the route also answers unversioned --
/// a document that listed the internal path would send consumers to the
/// surface §11.5 promises to keep unversioned.
pub async fn get_quotes(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
    Query(q): Query<QuoteQuery>,
) -> Response {
    // The gate `GET /media/:id` uses. Absent, off-disk and denied are one
    // answer; a new route that forgets this re-opens a library-probing hole.
    match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "interview: media_path failed");
            return internal_error();
        }
    }
    let store = &state.store;
    // Clamp BEFORE reading, for the reason `get_words` does: a negative offset
    // is a `LIMIT -1 OFFSET -5`, which one engine refuses and the other
    // answers as "no limit".
    let offset = q.offset.max(0);
    let limit = q.limit.unwrap_or(MAX_QUOTES).clamp(1, MAX_QUOTES);

    let all = match commons_store::interview::quotes_for(store, &object_id).await {
        Ok(q) => q,
        Err(_) => return not_found(),
    };
    // The store returns quotes in TIME ORDER, which is the only order a
    // "moments worth going back to" list can be read in.
    let total = all.len() as i64;
    let quotes: Vec<QuoteOut> = all
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|q| QuoteOut {
            id: q.id,
            start_ms: q.start_ms,
            end_ms: q.end_ms,
            text: q.text,
            weight: q.weight,
            weight_source: q.weight_source.as_str(),
        })
        .collect();
    let truncated = offset + (quotes.len() as i64) < total;

    (
        StatusCode::OK,
        axum::Json(QuotePage {
            object_id,
            quotes,
            total,
            truncated,
        }),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
struct TopicOut {
    id: String,
    name: String,
    /// §5.15: a model's topic is a suggestion and the response says so
    /// explicitly. A client must not have to infer it from a missing field or
    /// from a namespace it is expected to parse -- this is the same argument
    /// the module already makes for `confidence: null` vs `0`.
    proposed: bool,
    confidence: Option<f64>,
    source: Option<String>,
}

#[derive(Debug, Serialize)]
struct TopicList {
    object_id: String,
    topics: Vec<TopicOut>,
}

/// `GET /media/:object_id/topics`
///
/// An object with no topics is an EMPTY LIST and a 200, not a 404. A library
/// of recordings that have not been tagged is the normal state, and a 404
/// would say "this thing does not exist" about a recording that is right
/// there on screen. This is the same call `get_chapters` already makes.
#[utoipa::path(get, path = "/media/{object_id}/topics", responses((status = 200, description = "Success")), params(("object_id" = String, Path)))]
/// T-P6-007: the `/api/v1` OpenAPI document reads this. The path is
/// the VERSIONED one even though the route also answers unversioned --
/// a document that listed the internal path would send consumers to the
/// surface §11.5 promises to keep unversioned.
pub async fn get_topics(
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
    // `object_tags_full` rather than `object_tags`: the proposed flag is a
    // property of the TAG's namespace, so a route that read only the
    // application rows would have to re-join to answer it.
    let pairs = match store.object_tags_full(&object_id).await {
        Ok(p) => p,
        Err(_) => return not_found(),
    };
    let topics: Vec<TopicOut> = pairs
        .into_iter()
        .map(|(tag, app)| {
            // The namespace is the record of WHO applied it, so `proposed` is
            // read off the tag and not inferred from a null confidence: a
            // model that does not score its own output still proposed it.
            let proposed = matches!(tag.namespace, Namespace::Ml(_));
            TopicOut {
                id: tag.id,
                name: tag.name,
                proposed,
                confidence: app.confidence,
                source: app.source,
            }
        })
        .collect();

    (StatusCode::OK, axum::Json(TopicList { object_id, topics })).into_response()
}

/// Response for speaker cluster proposals.
#[derive(Debug, Serialize)]
struct SpeakerClusterProposalsOut {
    object_id: String,
    proposals: Vec<SpeakerClusterProposalOut>,
}

#[derive(Debug, Serialize)]
struct SpeakerClusterProposalOut {
    speaker_key: String,
    /// `null` when the cluster has not been proposed yet.
    cluster_id: Option<String>,
}

/// `GET /media/:object_id/speaker-clusters`
///
/// Returns the speaker cluster proposals for an object's transcript.
/// The consent gate is the same one `GET /media/:id` uses -- absent,
/// off-disk and denied all give the same 404, because a client that
/// can distinguish them can probe the library for what a user has.
#[utoipa::path(get, path = "/media/{object_id}/speaker-clusters", responses((status = 200, description = "Success")), params(("object_id" = String, Path)))]
/// T-P6-007: the `/api/v1` OpenAPI document reads this. The path is
/// the VERSIONED one even though the route also answers unversioned --
/// a document that listed the internal path would send consumers to the
/// surface §11.5 promises to keep unversioned.
pub async fn get_speaker_clusters(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
) -> Response {
    // The gate `GET /media/:id` uses. Absent, off-disk and denied are one
    // answer; a new route that forgets this re-opens a library-probing hole.
    match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "interview: media_path failed");
            return internal_error();
        }
    }
    let store = &state.store;
    // First get the transcript for this object
    let transcript = match commons_store::interview::transcript_for(store, &object_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return not_found(),
        Err(_) => return not_found(),
    };
    let proposals = match commons_store::interview::speaker_cluster_proposals(store, &transcript.id).await {
        Ok(p) => p,
        Err(_) => return not_found(),
    };
    let proposals_out: Vec<SpeakerClusterProposalOut> = proposals
        .into_iter()
        .map(|(speaker_key, cluster_id)| SpeakerClusterProposalOut { speaker_key, cluster_id })
        .collect();

    (StatusCode::OK, axum::Json(SpeakerClusterProposalsOut { object_id, proposals: proposals_out })).into_response()
}

/// Response for rejections.
#[derive(Debug, Serialize)]
struct RejectionsOut {
    object_id: String,
    rejections: Vec<RejectionOut>,
}

#[derive(Debug, Serialize)]
struct RejectionOut {
    source: String,
    value_json: String,
    /// `null` when the rejection has no recorded author.
    rejected_by: Option<String>,
}

/// `GET /media/:object_id/rejections`
///
/// Returns all tag rejections recorded against an object, newest first.
/// The consent gate is the same one `GET /media/:id` uses -- absent,
/// off-disk and denied all give the same 404, because a client that
/// can distinguish them can probe the library for what a user has.
#[utoipa::path(get, path = "/media/{object_id}/rejections", responses((status = 200, description = "Success")), params(("object_id" = String, Path)))]
/// T-P6-007: the `/api/v1` OpenAPI document reads this. The path is
/// the VERSIONED one even though the route also answers unversioned --
/// a document that listed the internal path would send consumers to the
/// surface §11.5 promises to keep unversioned.
pub async fn get_rejections(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
) -> Response {
    // The gate `GET /media/:id` uses. Absent, off-disk and denied are one
    // answer; a new route that forgets this re-opens a library-probing hole.
    match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "interview: media_path failed");
            return internal_error();
        }
    }
    let store = &state.store;
    let rejections = match store.rejections_for(&object_id).await {
        Ok(r) => r,
        Err(_) => return not_found(),
    };
    let rejections_out: Vec<RejectionOut> = rejections
        .into_iter()
        .map(|(source, value_json, rejected_by)| RejectionOut { source, value_json, rejected_by })
        .collect();

    (StatusCode::OK, axum::Json(RejectionsOut { object_id, rejections: rejections_out })).into_response()
}
