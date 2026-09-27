//! `GET`/`PUT /media/:object_id/playback` — where the player is, and where it got to.
//!
//! T-P6-001. Spec §4 and §7 of `docs/spec/t-p6-001-player.md`. The store layer
//! (`commons-store/src/playback.rs`) owns validation and persistence; this file
//! owns the wire shape and nothing else, so there is exactly one place where a
//! rule lives.
//!
//! # Three decisions that are not forced by the store
//!
//! 1. **`GET` on an unknown object answers 200 with a fresh state, not 404.**
//!    The player asks for the state of every object it opens, and "nobody has
//!    played this" is the common case. A 404 would make every freshly-opened
//!    object look broken, and — worse — would be indistinguishable from an
//!    object that does not exist, so the player would need two requests to
//!    learn one thing. §14.1's concern (not leaking whether an id exists)
//!    does *not* apply here, and it is worth being explicit about why: the
//!    response is the caller's own playback position, so there is nothing to
//!    disclose. The bytes route is the opposite case and does 404, because
//!    there the answer is a fact about the library.
//! 2. **An invalid write is 422, not 400.** The request is well-formed JSON of
//!    the right shape; what is wrong is a *value* — a position past the end, an
//!    inverted loop. 400 says "I could not parse this", which is false and
//!    sends a client looking for a syntax error that is not there.
//! 3. **A refusal names the field in the body.** "loop_b_ms must be after
//!    loop_a_ms", not "invalid state". The client sets these fields from a
//!    scrubber, and a message naming the field is the difference between a
//!    user-visible bug report and a fix.

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use commons_store::playback::{get_playback, put_playback, PlaybackState};
use serde::{Deserialize, Serialize};

use crate::AppState;

/// The wire shape of a playback state.
///
/// Deliberately the struct's fields and nothing more: no `user_id` (single-user
/// until T-P9-001), no `is_armed`. `is_armed` is a *derived* fact — it is true
/// exactly when both loop ends are non-zero — and sending it would put a
/// redundancy in the API that a client could cache and then disagree with.
/// Deriving it on read is one comparison and cannot go stale.
///
/// ## Why `deny_unknown_fields`
///
/// Because the failure without it is a **silently dropped write**, which is the
/// worst kind: the server answers 200, the client's loop never takes effect, and
/// nothing anywhere says so.
///
/// It is not hypothetical here — it was found by writing a test with `"loop"`
/// instead of `"loop_points"`. The response came back 200, and the body was
/// `{"position_ms":1000,"duration_ms":90000,"completed":false}`: the loop
/// vanished, with no error on either side. A client that misspells a field, or
/// that is a version behind or ahead of the server, gets a success and a wrong
/// result, and will report "the loop marker doesn't stick" rather than "you sent
/// the wrong field name". Rejecting the request turns that into a 400 that names
/// the unknown field, and it is the difference between a one-line client fix
/// and an afternoon of reading the wrong code.
///
/// The cost is a redeploy for a field rename, which is the point: a rename that
/// cannot be deployed in step with every client is better left until the
/// alternative is a write that disappears.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackBody {
    pub position_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_points: Option<LoopBody>,
    #[serde(default)]
    pub completed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoopBody {
    pub a_ms: u64,
    pub b_ms: u64,
}

impl From<PlaybackState> for PlaybackBody {
    fn from(s: PlaybackState) -> Self {
        PlaybackBody {
            position_ms: s.position_ms,
            duration_ms: s.duration_ms,
            loop_points: s.loop_points.map(|l| LoopBody {
                a_ms: l.a_ms,
                b_ms: l.b_ms,
            }),
            completed: s.completed,
        }
    }
}

/// The body of a refusal: a human-readable reason and the field it names.
#[derive(Debug, Serialize)]
struct Refusal {
    error: &'static str,
    field: &'static str,
    message: String,
}

impl Refusal {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Refusal {
            error: "invalid_playback_state",
            field,
            message: message.into(),
        }
    }
}

/// `GET /media/:object_id/playback`.
pub async fn get_playback_route(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
) -> Response {
    match get_playback(&state.store, &object_id).await {
        Ok(p) => (StatusCode::OK, Json(PlaybackBody::from(p))).into_response(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "playback read failed");
            internal_error()
        }
    }
}

/// `PUT /media/:object_id/playback`.
pub async fn put_playback_route(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
    body: Result<Json<PlaybackBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    // A body that will not parse is a 400 and genuinely so — this is the one
    // case where "I could not parse this" is true.
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "malformed_body",
                    "message": e.body_text(),
                })),
            )
                .into_response()
        }
    };

    // The route *also* validates, even though `put_playback` will. Two reasons,
    // and the second is the one that matters: the store's error names a field
    // as a Rust identifier (`loop_b_ms`), which happens to be the wire name
    // too, but mapping the error to a status is a route's job and doing it
    // once, here, keeps the store free of HTTP.
    let candidate = PlaybackState {
        object_id: object_id.clone(),
        position_ms: body.position_ms,
        duration_ms: body.duration_ms,
        loop_points: body
            .loop_points
            .map(|l| commons_store::playback::LoopPoints {
                a_ms: l.a_ms,
                b_ms: l.b_ms,
            }),
        completed: body.completed,
    };

    if let Err(e) = candidate.validate() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(Refusal::new(e.field(), e.to_string())),
        )
            .into_response();
    }

    match put_playback(&state.store, &candidate).await {
        Ok(()) => Json(PlaybackBody::from(candidate)).into_response(),
        Err(commons_store::playback::PlaybackStoreError::Invalid(e)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(Refusal::new(e.field(), e.to_string())),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "playback write failed");
            internal_error()
        }
    }
}

/// A 500 with no detail in the body.
///
/// Internal, and the same function in one place rather than two call sites
/// writing their own — a 500 whose body says which table failed is a leak, and
/// the reason this is a shared helper is so the detail stays in the log and not
/// in the response.
fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal" })),
    )
        .into_response()
}
