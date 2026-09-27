//! Playback state: the pure half, tested without a database.
//!
//! The persistence lives in `store/playback.rs`; this file is the part that
//! decides what a valid playback state IS, which is where the decisions in
//! `docs/spec/t-p6-001-player.md` actually live. A table can hold anything;
//! these functions are what make "a zero-length A/B loop" and "a resume point
//! past the end" refusals rather than rows.
//!
//! # Why `PlaybackState` validation is not a constructor detail
//!
//! The tempting shape is a `new()` that clamps and returns. That is wrong for
//! this type, because every field here arrives from a browser's `currentTime`
//! and a scrubber, i.e. from a user dragging something. Clamping hides the
//! disagreement between what the client thinks and what the file contains, and
//! the next thing that happens is a resume point that jumps somewhere nobody
//! chose. So `PlaybackState::validate` REFUSES and names the field, and the
//! route maps that to 422 with the field name in the body.

use commons_store::playback::{LoopPoints, PlaybackError, PlaybackState, Rung};

/// The default state for an object nobody has played.
///
/// `position_ms = 0` and NOT NULL is deliberate: "never played" and "played and
/// seeked to the start" are the same resume behaviour, and separating them
/// would need a nullable column that every reader then has to COALESCE. What
/// *is* worth distinguishing is `completed`, because that changes what the UI
/// shows.
fn fresh() -> PlaybackState {
    PlaybackState {
        object_id: "obj_1".to_string(),
        position_ms: 0,
        duration_ms: None,
        loop_points: None,
        completed: false,
    }
}

#[test]
fn a_fresh_state_is_valid() {
    let s = fresh();
    assert!(
        s.validate().is_ok(),
        "the default state must be accepted: {:?}",
        s.validate()
    );
}

#[test]
fn a_position_past_the_known_duration_is_refused_by_name() {
    // A duration is only known once a probe has run, so this is the case where
    // the client and the file actually disagree.
    let s = PlaybackState {
        duration_ms: Some(10_000),
        position_ms: 10_001,
        ..fresh()
    };
    let e = s
        .validate()
        .expect_err("a position past the end must be refused");
    assert_eq!(
        e,
        PlaybackError::PositionPastEnd {
            position_ms: 10_001,
            duration_ms: 10_000
        }
    );
    // The message must name the field, because this reaches a 422 body.
    assert!(
        e.to_string().contains("position_ms"),
        "message must name the field: {e}"
    );
}

#[test]
fn a_position_exactly_at_the_duration_is_allowed() {
    // Off-by-one in the wrong direction is a real bug here: "played to the end"
    // is the one position that must always be storable, or the last frame of
    // every video is unresumable.
    let s = PlaybackState {
        duration_ms: Some(10_000),
        position_ms: 10_000,
        ..fresh()
    };
    assert!(s.validate().is_ok(), "the end position itself is valid");
}

#[test]
fn an_unknown_duration_permits_any_position() {
    // Without a probe there is nothing to compare against, and refusing here
    // would make the player unusable on exactly the files that most need it.
    let s = PlaybackState {
        position_ms: 9_999_999,
        duration_ms: None,
        ..fresh()
    };
    assert!(s.validate().is_ok());
}

#[test]
fn a_zero_length_loop_is_refused_rather_than_sitting_there_armed() {
    // a == b would be a loop that never fires, which the control bar would
    // still draw as armed. The migration's CHECK forbids it too; this is the
    // early, named refusal.
    let l = LoopPoints {
        a_ms: 5_000,
        b_ms: 5_000,
    };
    let s = PlaybackState {
        loop_points: Some(l),
        ..fresh()
    };
    let e = s
        .validate()
        .expect_err("a zero-length loop must be refused");
    assert_eq!(e, PlaybackError::EmptyLoop);
    assert!(
        e.to_string().contains("loop"),
        "message must name the field: {e}"
    );
}

#[test]
fn an_inverted_loop_is_refused() {
    let l = LoopPoints {
        a_ms: 9_000,
        b_ms: 2_000,
    };
    let s = PlaybackState {
        loop_points: Some(l),
        ..fresh()
    };
    assert_eq!(
        s.validate().unwrap_err(),
        PlaybackError::InvertedLoop {
            a_ms: 9_000,
            b_ms: 2_000
        }
    );
}

#[test]
fn a_loop_past_the_duration_is_refused() {
    let l = LoopPoints {
        a_ms: 1_000,
        b_ms: 20_000,
    };
    let s = PlaybackState {
        duration_ms: Some(10_000),
        loop_points: Some(l),
        ..fresh()
    };
    let e = s
        .validate()
        .expect_err("a loop point past the end must be refused");
    assert_eq!(
        e,
        PlaybackError::LoopPastEnd {
            b_ms: 20_000,
            duration_ms: 10_000
        }
    );
}

#[test]
fn a_half_set_loop_is_allowed_because_it_means_cleared() {
    // A user clearing only B is a real interaction (drag B back off the bar).
    // Refusing it would make the control bar's clear gesture a 422.
    let l = LoopPoints {
        a_ms: 1_000,
        b_ms: 0,
    };
    let s = PlaybackState {
        loop_points: Some(l),
        ..fresh()
    };
    assert!(
        s.validate().is_ok(),
        "a half-set loop is how the UI says 'cleared'"
    );
}

#[test]
fn the_loop_is_inert_when_either_end_is_missing() {
    // The property the control bar depends on: it may draw points on the
    // scrubber, but nothing fires unless BOTH ends are set.
    let only_a = LoopPoints {
        a_ms: 1_000,
        b_ms: 0,
    };
    let only_b = LoopPoints {
        a_ms: 0,
        b_ms: 4_000,
    };
    assert!(!only_a.is_armed(), "A alone is not a loop");
    assert!(!only_b.is_armed(), "B alone is not a loop");
    let both = LoopPoints {
        a_ms: 1_000,
        b_ms: 4_000,
    };
    assert!(both.is_armed());
}

// ---- the proxy ladder -------------------------------------------------

#[test]
fn the_ladder_has_three_rungs_and_they_are_ordered_by_height() {
    // "Three rungs" is in the spec, and an ordered enum means the ordering is
    // the type's rather than a comment's.
    let rungs = Rung::ALL;
    assert_eq!(rungs.len(), 3);
    for pair in rungs.windows(2) {
        assert!(
            pair[0].height() > pair[1].height(),
            "rungs must descend: {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn the_ladder_rungs_have_browsers_that_can_actually_play_them() {
    // The whole point of a proxy is that the browser cannot play the source.
    // A rung the browser also cannot play is a rung that transcodes to
    // something still broken, so the container/codec pairs are asserted rather
    // than assumed.
    for r in Rung::ALL {
        assert!(!r.container().is_empty(), "{r:?} has no container");
        assert!(!r.video_codec().is_empty(), "{r:?} has no video codec");
        assert!(!r.audio_codec().is_empty(), "{r:?} has no audio codec");
    }
    // H.264 in an MP4 container is the one combination with near-universal
    // browser support; the lowest rung MUST be it, because the lowest rung is
    // what §12.1.1 says a public visitor gets.
    assert_eq!(Rung::Lowest.container(), "mp4");
    assert_eq!(Rung::Lowest.video_codec(), "h264");
}

#[test]
fn a_browser_can_play_a_source_without_any_proxy_at_all() {
    // The decision is a function of what the SOURCE is, and getting it wrong
    // in the expensive direction transcodes everything for nothing.
    let webm_vp9 = SourceCaps {
        container: "webm".into(),
        video_codec: "vp9".into(),
        audio_codec: "opus".into(),
    };
    assert_eq!(
        rung_for(&webm_vp9),
        None,
        "a format the browser plays needs no proxy"
    );

    let avi_mpeg4 = SourceCaps {
        container: "avi".into(),
        video_codec: "mpeg4".into(),
        audio_codec: "mp3".into(),
    };
    assert!(
        rung_for(&avi_mpeg4).is_some(),
        "AVI/mpeg4 is not browser-playable"
    );
}

#[test]
fn an_unknown_codec_is_proxied_rather_than_trusted() {
    // The asymmetry is the point: an unrecognised codec is assumed
    // unplayable. Trusting it means a black rectangle and a spinner, which is
    // the failure the spec's §6 calls out by name.
    let weird = SourceCaps {
        container: "mkv".into(),
        video_codec: "theora2015".into(),
        audio_codec: "pcm_s16le".into(),
    };
    assert_eq!(rung_for(&weird), Some(Rung::Highest),
               "unknown means unplayable, and the safest playable rung is the highest-quality one we can make");
}

use commons_store::playback::{rung_for, SourceCaps};

/// ffprobe's `format_name` is a comma-joined LIST, and treating it as one name
/// made every file in the library need a proxy.
///
/// This is worth its own test because the failure is silent in the worst way:
/// the proxy still works, so nothing is visibly broken — the server just
/// transcodes files that needed nothing, spending minutes of CPU and filling a
/// disk. A green suite said "the ladder decides correctly" throughout.
#[test]
fn a_comma_joined_container_list_is_matched_by_membership() {
    // What ffprobe actually prints for an mp4.
    let mp4 = SourceCaps {
        container: "mov,mp4,m4a,3gp,3g2,mj2".into(),
        video_codec: "h264".into(),
        audio_codec: "aac".into(),
    };
    assert_eq!(
        rung_for(&mp4),
        None,
        "a browser plays h264/aac in mp4, so no rung is needed"
    );

    // And for a matroska file, which is `matroska,webm` -- so it IS a webm as
    // far as a browser is concerned, and h264 in it is NOT playable.
    let mkv_h264 = SourceCaps {
        container: "matroska,webm".into(),
        video_codec: "h264".into(),
        audio_codec: "aac".into(),
    };
    assert_eq!(
        rung_for(&mkv_h264),
        Some(Rung::Highest),
        "h264 muxed into matroska plays in no browser, whatever the list says"
    );

    // A webm with a webm codec is fine, and the same list must not break it.
    let webm = SourceCaps {
        container: "matroska,webm".into(),
        video_codec: "vp9".into(),
        audio_codec: "opus".into(),
    };
    assert_eq!(rung_for(&webm), None, "vp9/opus in webm needs no proxy");

    // Surrounding whitespace in the list is ffprobe's, not ours, but trimming
    // costs nothing and an untrimmed " mp4" would miss.
    let spaced = SourceCaps {
        container: " mov, mp4 , m4a ".into(),
        video_codec: "h264".into(),
        audio_codec: "aac".into(),
    };
    assert_eq!(
        rung_for(&spaced),
        None,
        "the list is trimmed before matching"
    );
}

/// A list containing BOTH webm and mp4 is an mp4 to a browser, and the pairing
/// check must agree with the membership check.
#[test]
fn a_list_containing_both_containers_is_treated_as_mp4() {
    let both = SourceCaps {
        container: "matroska,webm,mp4".into(),
        video_codec: "h264".into(),
        audio_codec: "aac".into(),
    };
    assert_eq!(
        rung_for(&both),
        None,
        "mp4 is in the list, so h264/aac plays and no rung is needed"
    );
}

/// A file with NO audio stream needs no proxy.
///
/// Silence is common in a video library — a screen recording, a clip with the
/// audio stripped — and `SourceCaps::audio_codec` is the empty string for it. An
/// empty string matches no codec whitelist, so the first version of the ladder
/// transcoded every silent file in the library: minutes of CPU each, for a file
/// a browser plays perfectly well. The failure is invisible from the outside,
/// because the proxy works.
#[test]
fn a_silent_playable_file_needs_no_rung() {
    for container in ["mov,mp4,m4a,3gp,3g2,mj2", "matroska,webm"] {
        let silent = SourceCaps {
            container: container.into(),
            video_codec: "h264".into(),
            audio_codec: String::new(),
        };
        // h264 in a matroska/webm is not playable regardless of silence, so the
        // only container that can pass here is the mp4 one.
        let expected = if container.starts_with("mov") {
            None
        } else {
            Some(Rung::Highest)
        };
        assert_eq!(
            rung_for(&silent),
            expected,
            "container {container} with no audio stream"
        );
    }
}

/// Silence must not become a blanket pass: an unrecognised CODEC still needs a
/// proxy, and a silent file with an unplayable video still needs one.
#[test]
fn silence_does_not_excuse_an_unplayable_video() {
    let silent_bad_video = SourceCaps {
        container: "mov,mp4,m4a".into(),
        video_codec: "mpeg4".into(),
        audio_codec: String::new(),
    };
    assert_eq!(
        rung_for(&silent_bad_video),
        Some(Rung::Highest),
        "silence plus mpeg4 is still a file no browser plays"
    );
}
