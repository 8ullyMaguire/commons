//! Playback state and the proxy quality ladder.
//!
//! §11.1 (C64), §11.5 (on-demand proxy), §12.1.1 (streaming surface).
//! Migration `0020_playback_state.sql`. Spec: `docs/spec/t-p6-001-player.md`.
//!
//! # Why this is a module and not three fields on `object`
//!
//! `object` is the library's central table and a scan, a dedup pass and every
//! metadata write go through it. A player's write path -- fired every time
//! someone lets a video sit for ten seconds -- must not be able to contend
//! with those. So playback state is its own table with its own row, which also
//! makes "which objects have a resume point" a query rather than a full scan
//! with a `NULL` filter.
//!
//! # Why the key is `object_id` alone
//!
//! The library is single-user today; multi-user is T-P9-001, and §12.1's role
//! table is that phase's work. A `(user_id, object_id)` key would be a key with
//! exactly one permanent member -- a constraint the schema carries and nothing
//! constrains. When T-P9-001 lands this table gains `user_id` and the primary
//! key becomes `(user_id, object_id)`: a small-table migration, recorded here so
//! the next agent knows the single column was a decision, not an oversight.
//!
//! # Why validation refuses rather than clamps
//!
//! Every field here arrives from a browser's `currentTime` and a scrubber, i.e.
//! from a user dragging something. A constructor that clamps hides the
//! disagreement between what the client believes and what the file contains, and
//! the next symptom is a resume point that jumps somewhere nobody chose. So
//! [`PlaybackState::validate`] refuses and names the field; the route turns that
//! into a 422 whose body carries the name.

use serde::{Deserialize, Serialize};

/// A/B loop points, in milliseconds from the start.
///
/// `0` is the sentinel for "this end is not set", not "the start of the video".
/// A user dragging the A marker to the very beginning is a real interaction, and
/// a sentinel cannot represent it -- which is why the clear gesture and the
/// drag-to-zero gesture are told apart by [`LoopPoints::is_armed`] returning
/// false rather than by storing a magic value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopPoints {
    pub a_ms: u64,
    pub b_ms: u64,
}

impl LoopPoints {
    /// Both ends set. A loop with either end unset is drawn on the scrubber and
    /// fires nothing, so the control bar must ask this rather than test
    /// `a_ms > 0`.
    pub fn is_armed(&self) -> bool {
        self.a_ms != 0 && self.b_ms != 0
    }
}

/// Everything the player remembers about one object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackState {
    pub object_id: String,
    /// Resume point. `0` is NOT NULL and means "start here", deliberately: the
    /// resume behaviour of "never played" and of "played and seeked to the
    /// start" is identical, so separating them would need a nullable column
    /// every reader then has to COALESCE. `completed` is the flag that changes
    /// what the UI shows, and it is separate.
    pub position_ms: u64,
    /// From the probe. `None` when no probe has run, in which case there is
    /// nothing to check `position_ms` against -- see [`PlaybackError`].
    pub duration_ms: Option<u64>,
    pub loop_points: Option<LoopPoints>,
    pub completed: bool,
}

impl PlaybackState {
    /// The state of an object nobody has played.
    pub fn fresh(object_id: impl Into<String>) -> Self {
        Self {
            object_id: object_id.into(),
            position_ms: 0,
            duration_ms: None,
            loop_points: None,
            completed: false,
        }
    }

    /// Refuse a state that cannot be true of the file it claims to describe.
    ///
    /// Every variant names the field, because this text reaches a 422 body and
    /// a client cannot act on "invalid state".
    pub fn validate(&self) -> Result<(), PlaybackError> {
        // Duration first, so a loop point past the end is reported as a loop
        // problem rather than as whatever the position check happens to say.
        if let Some(dur) = self.duration_ms {
            // `>` and not `>=`: a position exactly at the duration is the end
            // of the video, which is the one position that must always be
            // storable. Refusing it makes the last frame unresumable.
            if self.position_ms > dur {
                return Err(PlaybackError::PositionPastEnd {
                    position_ms: self.position_ms,
                    duration_ms: dur,
                });
            }
        }

        let Some(l) = self.loop_points else {
            return Ok(());
        };

        // A half-set loop is legitimate and means "cleared": the control bar
        // lets a user drag B back off the scrubber, and refusing that would make
        // the clear gesture a 422. Only a loop with BOTH ends set is checked.
        if !l.is_armed() {
            return Ok(());
        }

        if l.a_ms == l.b_ms {
            // A zero-length loop never fires, and the control bar would still
            // draw it as armed. The migration's CHECK forbids the row too; this
            // is the early refusal that names it.
            return Err(PlaybackError::EmptyLoop);
        }
        if l.a_ms > l.b_ms {
            return Err(PlaybackError::InvertedLoop {
                a_ms: l.a_ms,
                b_ms: l.b_ms,
            });
        }
        if let Some(dur) = self.duration_ms {
            if l.b_ms > dur {
                return Err(PlaybackError::LoopPastEnd {
                    b_ms: l.b_ms,
                    duration_ms: dur,
                });
            }
        }
        Ok(())
    }
}

/// Why a playback state was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackError {
    PositionPastEnd { position_ms: u64, duration_ms: u64 },
    EmptyLoop,
    InvertedLoop { a_ms: u64, b_ms: u64 },
    LoopPastEnd { b_ms: u64, duration_ms: u64 },
}

impl PlaybackError {
    /// The wire name of the field this refusal is about.
    ///
    /// Returning the name rather than only a message is what lets the route
    /// answer `{"field": "loop_b_ms"}`, and it is why the variants are named
    /// after fields instead of after conditions. A client setting these from a
    /// scrubber can point at the control the user was actually dragging; a
    /// generic "invalid state" sends them looking for a syntax error instead.
    ///
    /// The strings are the *wire* names, chosen to match the JSON body, not
    /// the Rust variant names — they coincide today, and if they ever diverge
    /// this function is the single place to fix.
    pub fn field(&self) -> &'static str {
        match self {
            PlaybackError::PositionPastEnd { .. } => "position_ms",
            PlaybackError::InvertedLoop { .. } => "loop_b_ms",
            PlaybackError::EmptyLoop => "loop_points",
            PlaybackError::LoopPastEnd { .. } => "loop_b_ms",
        }
    }
}

impl std::fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaybackError::PositionPastEnd {
                position_ms,
                duration_ms,
            } => write!(
                f,
                "position_ms {position_ms} is past duration_ms {duration_ms}"
            ),
            PlaybackError::EmptyLoop => write!(
                f,
                "loop_points: a_ms and b_ms are both {0} -- a zero-length loop never fires",
                0
            ),
            PlaybackError::InvertedLoop { a_ms, b_ms } => write!(
                f,
                "loop_points: a_ms {a_ms} is after b_ms {b_ms}; the loop would run backwards"
            ),
            PlaybackError::LoopPastEnd { b_ms, duration_ms } => write!(
                f,
                "loop_points: b_ms {b_ms} is past duration_ms {duration_ms}"
            ),
        }
    }
}

impl std::error::Error for PlaybackError {}

// ---- the proxy quality ladder ----------------------------------------

/// One rung of the transcode ladder.
///
/// Ordered by height, descending, because §12.1.1 says a public visitor is
/// served the lowest rung -- so "lowest" has to be a name in the type rather
/// than a comparison somebody writes at each use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    Highest,
    Medium,
    Lowest,
}

impl Rung {
    /// Descending by height. `Rung::ALL` is the ladder, and the ordering is the
    /// enum's declaration order, so the two cannot drift apart.
    pub const ALL: [Rung; 3] = [Rung::Highest, Rung::Medium, Rung::Lowest];

    pub fn height(&self) -> u32 {
        match self {
            Rung::Highest => 1080,
            Rung::Medium => 720,
            Rung::Lowest => 480,
        }
    }

    /// The container each rung is muxed into. Every rung is MP4, because that is
    /// the container with no codec negotiation attached -- an HLS ladder would
    /// need per-rung playlist plumbing, and §11.5's browser fallback is a
    /// progressive download.
    pub fn container(&self) -> &'static str {
        "mp4"
    }

    pub fn video_codec(&self) -> &'static str {
        "h264"
    }

    pub fn audio_codec(&self) -> &'static str {
        "aac"
    }
}

/// What the SOURCE file is, as far as the browser's ability to play it goes.
///
/// A capability set rather than a boolean, because "can the browser play this"
/// is three questions (container, video, audio) and any one of them being no
/// is enough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCaps {
    pub container: String,
    pub video_codec: String,
    pub audio_codec: String,
}

/// Which rung, if any, this source must be transcoded to.
///
/// `None` means the browser can play the source directly and no proxy is
/// needed. Getting this wrong in the *expensive* direction transcodes every
/// file in the library for nothing.
///
/// The asymmetry is deliberate: **an unrecognised codec is assumed unplayable.**
/// Trusting an unknown codec means a `<video>` that never fires `canplay` and a
/// user looking at a black rectangle with a spinner, which is a failure the user
/// cannot diagnose. Proxying an unknown codec costs one transcode and always
/// works. So the recognised set is a whitelist, and the fallback is
/// [`Rung::Highest`] -- the rung most likely to succeed whatever the source
/// was, because it is the least lossy.
pub fn rung_for(caps: &SourceCaps) -> Option<Rung> {
    // A source is directly playable only if container AND both codecs are
    // recognised AND the combination is one a browser actually accepts.
    // Checking the three independently is the trap: `webm` + `h264` is
    // container-yes/codec-yes and plays nothing, because the browser will not
    // mux h264 into webm.
    // ffprobe's `format_name` is a COMMA-JOINED LIST, not a single name: an
    // mp4 file reports `mov,mp4,m4a,3gp,3g2,mj2` and a matroska file reports
    // `matroska,webm`. Matching the whole string against `"mp4"` therefore
    // never matches anything, and every file in the library gets proxied --
    // which is the expensive direction, and a silent one: the proxy "works",
    // it is just transcoding files that needed nothing. So the container is
    // tested for MEMBERSHIP of the list.
    let containers: Vec<String> = caps
        .container
        .to_ascii_lowercase()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let has_container = |names: &[&str]| containers.iter().any(|c| names.contains(&c.as_str()));

    // **A file with no audio stream is not a file the browser cannot play.**
    // `SourceCaps::audio_codec` is empty for a silent video, and an empty
    // string matches no whitelist -- so the first version of this function
    // transcoded every silent file in the library, which is a large fraction
    // of a video library and entirely unplayable-looking to the user for no
    // reason. Silence is a legitimate thing for a file to be, so an absent
    // audio codec passes; an UNRECOGNISED one still fails.
    let silent = caps.audio_codec.trim().is_empty();
    let a_ok = silent
        || matches!(
            caps.audio_codec.to_ascii_lowercase().as_str(),
            "aac" | "mp3" | "opus" | "vorbis" | "flac"
        );

    let (c_ok, v_ok) = (
        has_container(&["mp4", "m4v", "webm"]),
        matches!(
            caps.video_codec.to_ascii_lowercase().as_str(),
            "h264" | "avc1" | "vp8" | "vp9" | "av01"
        ),
    );
    if !(c_ok && v_ok && a_ok) {
        return Some(Rung::Highest);
    }
    // Recognised on all three, and the pairing has to be real.
    // The same list-membership rule for the pairing check: a file ffprobe calls
    // `matroska,webm` IS a webm as far as a browser is concerned.
    let webm = has_container(&["webm"]) && !has_container(&["mp4", "m4v"]);
    let mp4ish = has_container(&["mp4", "m4v"]);
    let webm_video = matches!(
        caps.video_codec.to_ascii_lowercase().as_str(),
        "vp8" | "vp9" | "av01"
    );
    let mp4_video = matches!(
        caps.video_codec.to_ascii_lowercase().as_str(),
        "h264" | "avc1"
    );
    // Silence is compatible with either container, so it satisfies both
    // branches rather than failing both.
    let webm_audio = silent
        || matches!(
            caps.audio_codec.to_ascii_lowercase().as_str(),
            "opus" | "vorbis"
        );
    let mp4_audio = silent
        || matches!(
            caps.audio_codec.to_ascii_lowercase().as_str(),
            "aac" | "mp3"
        );

    if (webm && webm_video && webm_audio) || (mp4ish && mp4_video && mp4_audio) {
        None
    } else {
        // e.g. mp4 container with vp9 video: every part is recognised, and the
        // browser still will not play it. This branch is the one that a
        // per-field whitelist alone would miss.
        Some(Rung::Highest)
    }
}

// ---- persistence ------------------------------------------------------

use sqlx::Row;

use crate::db::{Store, StoreError};

/// Why a playback read or write failed.
#[derive(Debug)]
pub enum PlaybackStoreError {
    /// The state is not one the file can be in. See [`PlaybackState::validate`].
    Invalid(PlaybackError),
    Query(StoreError),
}

impl std::fmt::Display for PlaybackStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaybackStoreError::Invalid(e) => write!(f, "{e}"),
            PlaybackStoreError::Query(e) => write!(f, "playback store: {e}"),
        }
    }
}

impl std::error::Error for PlaybackStoreError {}

impl From<StoreError> for PlaybackStoreError {
    fn from(e: StoreError) -> Self {
        PlaybackStoreError::Query(e)
    }
}

/// Build the placeholder list for a statement of `n` binds.
///
/// Postgres takes `$1..$n`; SQLite takes `?`. Sending `?` to Postgres is a
/// **syntax error at the VALUES list**, not a silent mismatch, and the error
/// names the statement rather than the dialect -- so it reads as a malformed
/// migration when it is in fact the wrong placeholder. Every two-engine
/// statement in this file goes through here rather than hard-coding either.
fn placeholders(n: usize, numbered: bool) -> String {
    (1..=n)
        .map(|i| {
            if numbered {
                format!("${i}")
            } else {
                "?".to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Read one object's playback state.
///
/// **A missing row is a fresh state, not an error.** The player asks for the
/// state of every object it opens, and "nobody has played this" is the common
/// case. Answering 404 would make every freshly-opened object look broken and
/// would be indistinguishable from an object that does not exist -- the player
/// would need two questions to learn one thing.
///
/// Three rules, each learned by fighting the compiler, and all three worth
/// writing down because the fix is not obvious from the error:
///
///  1. The row becomes a plain tuple **inside** each match arm. `fetch_optional`
///     returns `Option<SqliteRow>` or `Option<PgRow>` -- unrelated Rust types
///     with no common variant a caller can name -- so letting either escape the
///     `match` is E0308.
///  2. A macro used in both arms must not `?` internally. The inner `?`
///     unwraps the `Result` early, and the caller's `?` then lands on `()`,
///     which is the `?` operator applied to `()`.
///  3. Decode `i32`, never `i64`. Postgres `INTEGER` is INT4 and SQLite's is 8
///     bytes; writes bind `i64` happily because sqlx widens, so the mismatch
///     appears **only on the read**, as "Rust type i64 (as SQL type INT8) is not
///     compatible with SQL type INT4".
pub async fn get_playback(
    store: &Store,
    object_id: &str,
) -> Result<PlaybackState, PlaybackStoreError> {
    macro_rules! select {
        ($p:expr, $numbered:literal) => {
            sqlx::query(&format!(
                "SELECT position_ms, duration_ms, loop_a_ms, loop_b_ms, completed \
                 FROM playback_state WHERE object_id = {}",
                placeholders(1, $numbered)
            ))
            .bind(object_id)
            .fetch_optional($p)
            .await
            .map_err(StoreError::Query)
            .map(|row| {
                row.map(|r| {
                    (
                        r.get::<i32, _>("position_ms"),
                        r.get::<Option<i32>, _>("duration_ms"),
                        r.get::<Option<i32>, _>("loop_a_ms"),
                        r.get::<Option<i32>, _>("loop_b_ms"),
                        r.get::<i32, _>("completed"),
                    )
                })
            })
        };
    }
    let found = match store {
        Store::Sqlite(p) => select!(p, false)?,
        Store::Postgres(p) => select!(p, true)?,
    };

    let Some((position_ms, duration_ms, a, b, completed)) = found else {
        return Ok(PlaybackState::fresh(object_id));
    };

    // Both NULL -> no loop. ONE NULL -> a half-set loop, a real state (the user
    // dragged one marker off the scrubber) that must survive as such. Deciding
    // on the PAIR rather than each end independently is what keeps a
    // genuinely-absent pair distinguishable from a half-set one.
    let loop_points = match (a, b) {
        (None, None) => None,
        (a, b) => Some(LoopPoints {
            a_ms: a.unwrap_or(0).max(0) as u64,
            b_ms: b.unwrap_or(0).max(0) as u64,
        }),
    };

    Ok(PlaybackState {
        object_id: object_id.to_string(),
        // `.max(0)` before the cast: a bare `as u64` on a negative wraps to
        // something enormous, turning a corrupt row into "resume in 200 years".
        position_ms: position_ms.max(0) as u64,
        duration_ms: duration_ms.map(|d| d.max(0) as u64),
        loop_points,
        completed: completed != 0,
    })
}

/// Write one object's playback state, replacing any previous row.
///
/// An **upsert**, and idempotent: the player saves every few seconds and a
/// second tab saves too, so last-writer-wins is the only rule two writers can
/// both obey. An append would leave two resume positions for one object and no
/// way to choose between them.
///
/// `ON CONFLICT (object_id) DO UPDATE` rather than `INSERT OR REPLACE`:
/// REPLACE deletes the row and re-inserts it, firing any future ON DELETE
/// trigger and resetting any column a later migration adds without naming it.
/// DO UPDATE names the columns it touches, so a new column keeps its default
/// instead of silently vanishing.
///
/// Validation happens **here** rather than at the route, so every caller is
/// covered -- an import, a plugin, a future endpoint. The migration's CHECK
/// would catch the loop cases but not a position past the end, which is a value
/// no constraint can judge without knowing the duration.
pub async fn put_playback(store: &Store, state: &PlaybackState) -> Result<(), PlaybackStoreError> {
    state.validate().map_err(PlaybackStoreError::Invalid)?;

    // A loop end of 0 means "this marker is not set" in the Rust type, and maps
    // to SQL NULL here. That is the whole reason the schema uses NULL rather
    // than 0: `LoopPoints::is_armed` treats 0 as unset, so a marker genuinely
    // dragged to the start of the file and a cleared marker are the same value
    // in Rust -- and NULL says so in the row too. Storing 0 instead makes the
    // CHECK's `a < b` false (1500 < 0 is false) and the write is REFUSED, so
    // the control bar's clear gesture would be a 500. The mapping lives here,
    // once, in both directions (see `get_playback`).
    let (a, b) = match state.loop_points {
        Some(l) => (
            (l.a_ms != 0).then_some(l.a_ms as i64),
            (l.b_ms != 0).then_some(l.b_ms as i64),
        ),
        None => (None, None),
    };

    macro_rules! put {
        ($p:expr, $numbered:literal) => {
            sqlx::query(&format!(
                "INSERT INTO playback_state \
                   (object_id, position_ms, duration_ms, loop_a_ms, loop_b_ms, completed, updated_at) \
                 VALUES ({}) \
                 ON CONFLICT (object_id) DO UPDATE SET \
                   position_ms = excluded.position_ms, \
                   duration_ms = excluded.duration_ms, \
                   loop_a_ms   = excluded.loop_a_ms, \
                   loop_b_ms   = excluded.loop_b_ms, \
                   completed   = excluded.completed, \
                   updated_at  = excluded.updated_at",
                placeholders(7, $numbered)
            ))
            .bind(state.object_id.as_str())
            .bind(state.position_ms as i64)
            .bind(state.duration_ms.map(|d| d as i64))
            .bind(a)
            .bind(b)
            .bind(i64::from(state.completed))
            .bind(commons_core::ts::now())
            .execute($p)
            .await
        };
    }
    match store {
        Store::Sqlite(p) => put!(p, false).map(|_| ()).map_err(StoreError::Query)?,
        Store::Postgres(p) => put!(p, true).map(|_| ()).map_err(StoreError::Query)?,
    }
    Ok(())
}
