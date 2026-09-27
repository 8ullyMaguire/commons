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
    let (c_ok, v_ok, a_ok) = (
        matches!(
            caps.container.to_ascii_lowercase().as_str(),
            "mp4" | "m4v" | "webm"
        ),
        matches!(
            caps.video_codec.to_ascii_lowercase().as_str(),
            "h264" | "avc1" | "vp8" | "vp9" | "av01"
        ),
        matches!(
            caps.audio_codec.to_ascii_lowercase().as_str(),
            "aac" | "mp3" | "opus" | "vorbis" | "flac"
        ),
    );
    if !(c_ok && v_ok && a_ok) {
        return Some(Rung::Highest);
    }
    // Recognised on all three, and the pairing has to be real.
    let webm = caps.container.eq_ignore_ascii_case("webm");
    let mp4ish = matches!(caps.container.to_ascii_lowercase().as_str(), "mp4" | "m4v");
    let webm_video = matches!(
        caps.video_codec.to_ascii_lowercase().as_str(),
        "vp8" | "vp9" | "av01"
    );
    let mp4_video = matches!(
        caps.video_codec.to_ascii_lowercase().as_str(),
        "h264" | "avc1"
    );
    let webm_audio = matches!(
        caps.audio_codec.to_ascii_lowercase().as_str(),
        "opus" | "vorbis"
    );
    let mp4_audio = matches!(
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
