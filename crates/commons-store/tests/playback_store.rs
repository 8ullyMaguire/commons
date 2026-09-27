//! Playback state against a real store, through the API the player uses.
//!
//! T-P6-001. Spec §4. Companion to `playback_db.rs` (the schema) and
//! `playback_pure.rs` (what a valid state is). This file is the middle: does
//! `get_playback` return what `put_playback` was given, and does `get_playback`
//! return a default rather than an error for an object nobody has played.
//!
//! # The GET-200-for-unknown behaviour is the thing being tested
//!
//! The player asks for the playback state of every object it opens, and
//! "nobody has played this" is the common case rather than an error. A 404 here
//! would make every freshly-opened object look broken, and would be
//! indistinguishable from an object that does not exist at all. The store's
//! contract is therefore "a missing row is a fresh state", and that is a
//! decision the tests pin rather than a behaviour nobody chose.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_store::db::Store;
use commons_store::playback::{LoopPoints, PlaybackError, PlaybackState, PlaybackStoreError};

macro_rules! on_each_store {
    (|$store:ident| $body:block) => {{
        for $store in [sqlite_store().await, postgres_store().await] {
            $body
        }
    }};
}

#[tokio::test]
async fn an_unknown_object_gets_a_fresh_state_rather_than_an_error() {
    on_each_store!(|s| {
        let got = commons_store::playback::get_playback(&s, "obj_never_played")
            .await
            .expect("an unknown object must not be an error");
        assert_eq!(got, PlaybackState::fresh("obj_never_played"));
    });
}

#[tokio::test]
async fn a_written_state_reads_back_exactly() {
    on_each_store!(|s| {
        let want = PlaybackState {
            object_id: "obj_rt_1".into(),
            position_ms: 61_500,
            duration_ms: Some(600_000),
            loop_points: Some(LoopPoints {
                a_ms: 1_000,
                b_ms: 2_000,
            }),
            completed: true,
        };
        commons_store::playback::put_playback(&s, &want)
            .await
            .expect("a valid state must write");

        let got = commons_store::playback::get_playback(&s, "obj_rt_1")
            .await
            .expect("readable");
        assert_eq!(got, want, "a round trip must be lossless on every field");
    });
}

#[tokio::test]
async fn a_null_loop_round_trips_as_no_loop() {
    // `loop_points: None` in Rust is two NULL columns, and reading it back must
    // be `None` again rather than `Some({a_ms: 0, b_ms: 0})`. The second is the
    // interesting failure: it is a half-set loop, which `is_armed` reports as
    // false, so the control bar would behave correctly while the stored state
    // disagreed with the API's.
    on_each_store!(|s| {
        let want = PlaybackState {
            object_id: "obj_rt_2".into(),
            position_ms: 10,
            duration_ms: Some(100),
            loop_points: None,
            completed: false,
        };
        commons_store::playback::put_playback(&s, &want)
            .await
            .expect("write");
        let got = commons_store::playback::get_playback(&s, "obj_rt_2")
            .await
            .expect("read");
        assert_eq!(
            got.loop_points, None,
            "absent loop must read back as absent"
        );
    });
}

#[tokio::test]
async fn a_half_set_loop_survives_the_round_trip_as_half_set() {
    on_each_store!(|s| {
        let want = PlaybackState {
            object_id: "obj_rt_3".into(),
            position_ms: 0,
            duration_ms: Some(100),
            loop_points: Some(LoopPoints {
                a_ms: 1_500,
                b_ms: 0,
            }),
            completed: false,
        };
        commons_store::playback::put_playback(&s, &want)
            .await
            .expect("write");
        let got = commons_store::playback::get_playback(&s, "obj_rt_3")
            .await
            .expect("read");
        assert_eq!(
            got.loop_points,
            Some(LoopPoints {
                a_ms: 1_500,
                b_ms: 0
            })
        );
        assert!(
            !got.loop_points.unwrap().is_armed(),
            "a half-set loop must still read as disarmed after a round trip"
        );
    });
}

#[tokio::test]
async fn put_is_an_upsert_so_a_second_save_replaces_rather_than_duplicating() {
    // The player saves every few seconds, and a second tab saves too. The
    // alternative is an append, which leaves two resume positions for one
    // object and no rule for choosing between them.
    on_each_store!(|s| {
        let first = PlaybackState {
            object_id: "obj_up".into(),
            position_ms: 1_000,
            duration_ms: Some(10_000),
            loop_points: None,
            completed: false,
        };
        let second = PlaybackState {
            position_ms: 7_500,
            completed: true,
            ..first.clone()
        };
        commons_store::playback::put_playback(&s, &first)
            .await
            .expect("first write");
        commons_store::playback::put_playback(&s, &second)
            .await
            .expect("second write");

        let got = commons_store::playback::get_playback(&s, "obj_up")
            .await
            .expect("read");
        assert_eq!(got.position_ms, 7_500, "the last write must win");
        assert!(got.completed, "the last write must win for completed too");

        let n: i64 = match &s {
            Store::Sqlite(db) => {
                sqlx::query_scalar("SELECT COUNT(*) FROM playback_state WHERE object_id='obj_up'")
                    .fetch_one(db)
                    .await
                    .expect("countable")
            }
            Store::Postgres(db) => {
                sqlx::query_scalar("SELECT COUNT(*) FROM playback_state WHERE object_id='obj_up'")
                    .fetch_one(db)
                    .await
                    .expect("countable")
            }
        };
        assert_eq!(n, 1, "an upsert must leave exactly one row");
    });
}

#[tokio::test]
async fn an_invalid_state_is_refused_before_it_reaches_the_database() {
    // The refusal is `validate`'s job, and the point of testing it HERE is that
    // the store must call `validate` rather than trusting its caller. A store
    // that only trusted the route would accept a bad state from any other
    // caller — an import, a plugin, a future endpoint — and the CHECK would
    // catch only the loop cases, not a position past the end.
    on_each_store!(|s| {
        let bad = PlaybackState {
            object_id: "obj_bad".into(),
            position_ms: 50_000,
            duration_ms: Some(10_000),
            loop_points: None,
            completed: false,
        };
        let err = commons_store::playback::put_playback(&s, &bad)
            .await
            .expect_err("a position past the end must be refused");
        assert!(
            matches!(
                err,
                PlaybackStoreError::Invalid(PlaybackError::PositionPastEnd { .. })
            ),
            "expected Invalid(PositionPastEnd), got {err:?}"
        );

        // And nothing was written.
        let got = commons_store::playback::get_playback(&s, "obj_bad")
            .await
            .expect("readable");
        assert_eq!(
            got,
            PlaybackState::fresh("obj_bad"),
            "a refused write must leave no row"
        );
    });
}

#[tokio::test]
async fn two_objects_keep_independent_state() {
    // Guards against a keyed-by-something-else bug: if the upsert ever keyed on
    // the wrong column, saving one object's position would move another's.
    on_each_store!(|s| {
        let a = PlaybackState {
            object_id: "obj_two_a".into(),
            position_ms: 1_000,
            duration_ms: Some(10_000),
            loop_points: None,
            completed: false,
        };
        let b = PlaybackState {
            object_id: "obj_two_b".into(),
            position_ms: 9_000,
            duration_ms: Some(10_000),
            loop_points: None,
            completed: false,
        };
        commons_store::playback::put_playback(&s, &a)
            .await
            .expect("write a");
        commons_store::playback::put_playback(&s, &b)
            .await
            .expect("write b");
        assert_eq!(
            commons_store::playback::get_playback(&s, "obj_two_a")
                .await
                .expect("read")
                .position_ms,
            1_000
        );
        assert_eq!(
            commons_store::playback::get_playback(&s, "obj_two_b")
                .await
                .expect("read")
                .position_ms,
            9_000
        );
    });
}

#[tokio::test]
async fn a_large_position_survives_the_round_trip() {
    // A long video's position exceeds what fits in a 32-bit millisecond count
    // only after ~35 minutes, so this is not far-fetched: a three-hour file is
    // 10.8 million ms, which fits, but a `u32` seconds field would not. The
    // assertion is here because the alternative -- discovering the truncation
    // from a video that resumes at the wrong place -- is exactly the failure
    // §6 of the spec warns about.
    on_each_store!(|s| {
        let want = PlaybackState {
            object_id: "obj_big".into(),
            position_ms: 10_800_000, // three hours
            duration_ms: Some(10_800_000),
            loop_points: None,
            completed: false,
        };
        commons_store::playback::put_playback(&s, &want)
            .await
            .expect("write");
        assert_eq!(
            commons_store::playback::get_playback(&s, "obj_big")
                .await
                .expect("read")
                .position_ms,
            10_800_000
        );
    });
}
