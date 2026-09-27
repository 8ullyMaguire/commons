//! Playback state against a real database, on both engines.
//!
//! T-P6-001. Spec §3. Companion to `playback_pure.rs`, which tests what a valid
//! state IS. This file tests the *schema*, and the split matters: a correct
//! `validate` cannot prove the table exists, that the CHECK constraint is
//! actually installed, or that a write bypassing validation is still stopped.
//! A migration that silently fails to create its constraint passes every other
//! test in the repository.
//!
//! The load-bearing test here is `the_loop_check_fires_without_rust_validation`,
//! because it is the one that can only be checked against a real database: the
//! CHECK is the second line of defence, and a second line that does not fire is
//! not a second line.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_core::ts::now;
use commons_store::db::Store;
use commons_store::playback::LoopPoints;
use sqlx::Row;

/// Run a block against both engines. See `folders_db.rs` for why this is a
/// macro: `sqlx`'s two result types are unrelated Rust types.
macro_rules! both_engines {
    (|$s:ident| $body:block) => {{
        async {
            let $s = postgres_store().await;
            $body
        }
        .await;
        async {
            let $s = sqlite_store().await;
            $body
        }
        .await;
    }};
}

/// A valid state, with the fields the test wants varied.
fn state(
    object: &str,
    position_ms: i64,
    a: Option<i64>,
    b: Option<i64>,
) -> Vec<(&'static str, Value)> {
    vec![
        ("object_id", Value::Text(Some(object.to_string()))),
        ("position_ms", Value::Int(position_ms)),
        ("duration_ms", Value::Int(10_000)),
        // A missing end is a real NULL, not an empty string: the CHECK is
        // written with `IS NULL` arms precisely so a half-set loop inserts.
        // A half-set loop is Integer-or-NULL, so it needs its own typed Option
        // rather than the text one -- the type is what the driver sends.
        ("loop_a_ms", Value::OptInt(a)),
        ("loop_b_ms", Value::OptInt(b)),
        ("completed", Value::Int(0)),
        ("updated_at", Value::Text(Some(now()))),
    ]
}

/// Insert a row with raw SQL, bypassing `Store::put_playback` and therefore
/// bypassing `validate`. This is the shape of the test that matters.
///
/// Three things this helper got wrong before it worked, all of which presented
/// as *schema* defects rather than test bugs -- the most expensive kind of
/// confusion, because the obvious reading of "syntax error at or near ,," is
/// that the table is malformed:
///
///   1. Values were interpolated into the statement text, so a NULL could not
///      be expressed (an absent `loop_a_ms` became an empty string) and the
///      ISO timestamp became bare syntax errors. Now bound.
///   2. The query was built once and executed in both arms. `sqlx::Query`'s
///      backend is fixed at construction, so that does not compile -- the type
///      is a lie until the arm is chosen. Now built per arm.
///   3. Postgres needs NUMBERED placeholders (`$1`), SQLite wants `?`. The
///      statement is therefore assembled from a placeholder list passed in by
///      the caller, exactly as `folders_db.rs` does.
async fn raw_insert(store: &Store, row: &[(&str, Value)]) -> Result<(), String> {
    let cols: Vec<&str> = row.iter().map(|(k, _)| *k).collect();
    assert!(!cols.is_empty(), "nothing to insert");
    let uniq: std::collections::BTreeSet<&&str> = cols.iter().collect();
    assert_eq!(
        uniq.len(),
        cols.len(),
        "a column was listed twice: {cols:?}"
    );

    // Placeholders from the ACTUAL arity, and the whole query built INSIDE each
    // arm. Two earlier versions got this wrong in ways that read as schema
    // faults: a fixed seven `?` made the two-column insert fail with "more
    // expressions than target columns", and building the query once outside the
    // match does not compile because `sqlx::Query`'s backend is fixed at
    // construction. Postgres needs `$1`, SQLite wants `?`.
    macro_rules! insert {
        ($p:expr, $numbered:literal) => {{
            let ph: Vec<String> = (1..=cols.len())
                .map(|i| {
                    if $numbered {
                        format!("${i}")
                    } else {
                        "?".into()
                    }
                })
                .collect();
            let sql = format!(
                "INSERT INTO playback_state ({}) VALUES ({})",
                cols.join(", "),
                ph.join(", ")
            );
            let mut q = sqlx::query(&sql);
            for (_, v) in row {
                q = match v {
                    Value::Int(i) => q.bind(*i),
                    Value::OptInt(i) => q.bind(*i),
                    Value::Text(t) => q.bind(t.clone()),
                };
            }
            q.execute($p).await.map(|_| ()).map_err(|e| e.to_string())
        }};
    }
    match store {
        Store::Sqlite(p) => insert!(p, false),
        Store::Postgres(p) => insert!(p, true),
    }
}

/// One column value, typed.
///
/// The first version bound everything as `Option<String>`, so `position_ms`
/// arrived as text and Postgres refused it: "column is of type integer but
/// expression is of type text". The database was right and the test was wrong.
/// The error named the column, which is the only reason it was diagnosable in
/// one step rather than by inspection.
#[derive(Debug, Clone)]
enum Value {
    Int(i64),
    /// An integer that may be NULL, bound as a typed `Option<i64>`.
    OptInt(Option<i64>),
    /// `None` is a SQL NULL. Mirrors `undo.rs`'s own `P::Text(Option<String>)`
    /// rather than inventing a third variant: the first version had a separate
    /// `Null` that bound as `Option::<String>::None`, which types the parameter
    /// as text and Postgres then refuses an integer column. The Option has to
    /// be INSIDE the typed variant, or the driver never learns the type.
    Text(Option<String>),
}

#[tokio::test]
async fn the_table_exists_and_takes_a_row() {
    both_engines!(|s| {
        raw_insert(&s, &state("obj_play_1", 4_200, None, None))
            .await
            .expect("a valid state must insert");
    });
}

#[tokio::test]
async fn the_loop_check_fires_without_rust_validation() {
    // THE test. The Rust side refuses an inverted loop in `validate`, so this
    // can only be reached by writing SQL directly -- which is exactly what an
    // import, a future migration backfill, or a caller that forgets to validate
    // would do. If the CHECK is not installed, this succeeds and the row sits
    // there looking armed while never firing.
    both_engines!(|s| {
        let inverted = raw_insert(&s, &state("obj_play_inv", 0, Some(9_000), Some(2_000))).await;
        assert!(
            inverted.is_err(),
            "the schema must refuse loop_a > loop_b even when validate() is bypassed"
        );

        let empty = raw_insert(&s, &state("obj_play_empty", 0, Some(5_000), Some(5_000))).await;
        assert!(
            empty.is_err(),
            "the schema must refuse a zero-length loop even when validate() is bypassed"
        );
    });
}

#[tokio::test]
async fn a_half_set_loop_is_accepted_because_it_means_cleared() {
    // The CHECK is written with two `IS NULL` arms precisely so this passes: the
    // control bar's clear gesture writes one end as NULL, and a CHECK that
    // forbade it would make clearing a loop impossible while still looking
    // armed.
    both_engines!(|s| {
        raw_insert(&s, &state("obj_play_half", 0, Some(1_000), None))
            .await
            .expect("a loop with only A set must be storable: it means cleared");
        raw_insert(&s, &state("obj_play_half2", 0, None, Some(1_000)))
            .await
            .expect("a loop with only B set must be storable too");
    });
}

#[tokio::test]
async fn an_ordered_loop_is_accepted() {
    both_engines!(|s| {
        raw_insert(&s, &state("obj_play_ok", 0, Some(1_000), Some(4_000)))
            .await
            .expect("a well-formed loop must insert");
    });
}

#[tokio::test]
async fn the_primary_key_is_the_object_so_one_object_has_one_row() {
    // A second row for the same object must fail rather than coexist: two
    // resume positions for one video is not a state any reader can resolve.
    both_engines!(|s| {
        raw_insert(&s, &state("obj_play_dup", 1_000, None, None))
            .await
            .expect("first write");
        let second = raw_insert(&s, &state("obj_play_dup", 9_000, None, None)).await;
        assert!(second.is_err(), "object_id must be unique");
    });
}

#[tokio::test]
async fn the_defaults_match_what_rust_calls_a_fresh_state() {
    // A schema default that disagrees with `PlaybackState::fresh` is a defect
    // that only shows up as a resumed video starting at the wrong place, so the
    // default is read back rather than assumed.
    both_engines!(|s| {
        raw_insert(
            &s,
            &[
                ("object_id", Value::Text(Some("obj_play_def".into()))),
                ("updated_at", Value::Text(Some(now()))),
            ],
        )
        .await
        .expect("a row with only the required columns must insert");

        // `i32`, not `i64`: Postgres INTEGER is INT4 and SQLite's is 8 bytes,
        // so an `i64` decode fails on Postgres with "Rust type i64 (as SQL type
        // INT8) is not compatible with SQL type INT4". The write side bound
        // `i64` happily -- sqlx widens -- so the mismatch only appears on the
        // read, which is why it survived the first eight runs.
        let (pos, dur, a, b, done): (i32, Option<i32>, Option<i32>, Option<i32>, i32) = match &s {
            Store::Sqlite(db) => {
                sqlx::query_as::<_, (i32, Option<i32>, Option<i32>, Option<i32>, i32)>(
                    "SELECT position_ms, duration_ms, loop_a_ms, loop_b_ms, completed \
                     FROM playback_state WHERE object_id = 'obj_play_def'",
                )
                .fetch_one(db)
                .await
                .expect("row must be readable")
            }
            Store::Postgres(db) => {
                sqlx::query_as::<_, (i32, Option<i32>, Option<i32>, Option<i32>, i32)>(
                    "SELECT position_ms, duration_ms, loop_a_ms, loop_b_ms, completed \
                     FROM playback_state WHERE object_id = 'obj_play_def'",
                )
                .fetch_one(db)
                .await
                .expect("row must be readable")
            }
        };
        assert_eq!(pos, 0, "position_ms must default to 0");
        assert_eq!(dur, None, "duration_ms must default to NULL (no probe yet)");
        assert_eq!(a, None, "loop_a_ms must default to NULL");
        assert_eq!(b, None, "loop_b_ms must default to NULL");
        assert_eq!(done, 0, "completed must default to 0");
    });
}

#[tokio::test]
async fn updated_at_is_text_in_both_engines_so_the_two_can_be_compared() {
    // Plan §0.4: timestamps are ISO-8601 UTC TEXT, so a sort needs no timezone
    // function. A millisecond integer in one engine and TEXT in the other would
    // make every "what changed recently" query engine-specific, and the first
    // draft of the spec specified exactly that.
    both_engines!(|s| {
        raw_insert(&s, &state("obj_play_ts", 0, None, None))
            .await
            .expect("insert");
        let v: String = match &s {
            Store::Sqlite(db) => {
                sqlx::query("SELECT updated_at FROM playback_state WHERE object_id='obj_play_ts'")
                    .fetch_one(db)
                    .await
                    .expect("readable")
                    .get(0)
            }
            Store::Postgres(db) => {
                sqlx::query("SELECT updated_at FROM playback_state WHERE object_id='obj_play_ts'")
                    .fetch_one(db)
                    .await
                    .expect("readable")
                    .get(0)
            }
        };
        assert!(
            v.contains('T') && v.ends_with('Z'),
            "updated_at must be ISO-8601 UTC TEXT ending in Z, got {v:?}"
        );
    });
}

#[tokio::test]
async fn the_loop_points_round_trip_through_the_row() {
    // The persistence half of the API the player actually uses: what
    // `PlaybackState` reads back must be what `LoopPoints` means, including the
    // 0-is-unset sentinel surviving the round trip.
    both_engines!(|s| {
        raw_insert(&s, &state("obj_play_rt", 0, Some(1_500), Some(2_500)))
            .await
            .expect("insert");
        let (a, b): (Option<i32>, Option<i32>) = match &s {
            Store::Sqlite(db) => sqlx::query_as(
                "SELECT loop_a_ms, loop_b_ms FROM playback_state WHERE object_id='obj_play_rt'",
            )
            .fetch_one(db)
            .await
            .expect("readable"),
            Store::Postgres(db) => sqlx::query_as(
                "SELECT loop_a_ms, loop_b_ms FROM playback_state WHERE object_id='obj_play_rt'",
            )
            .fetch_one(db)
            .await
            .expect("readable"),
        };
        let l = LoopPoints {
            a_ms: a.unwrap_or(0) as u64,
            b_ms: b.unwrap_or(0) as u64,
        };
        assert_eq!(
            l,
            LoopPoints {
                a_ms: 1_500,
                b_ms: 2_500
            }
        );
        assert!(
            l.is_armed(),
            "a round-tripped loop must still read as armed"
        );
    });
}
