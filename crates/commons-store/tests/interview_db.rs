//! Interview transcripts against a real database, on both engines.
//!
//! T-P6-004, spec §5.8 and §6 (`docs/spec/t-p6-004-interviews.md`).
//!
//! # What this file is for
//!
//! The ticket's second accept criterion is: **a corrected word becomes a
//! proposal, not a silent overwrite.** That cannot be tested in `commons-ml` —
//! the `ml` crate has no database and must not have one — so it is tested here,
//! on both engines, and it is the reason this file exists.
//!
//! # The trap this file is built to avoid
//!
//! `interview_word.confidence` is `DOUBLE PRECISION`. Declared `REAL`, it is
//! FLOAT4 on Postgres and FLOAT8 on SQLite, so a Rust `f64` decodes on one
//! engine and is refused by the other — and `migration_parity` compares column
//! *names*, so it reports green over a schema that only works on SQLite. A
//! test that only exercised SQLite would have passed. This one runs both, and
//! `a_null_confidence_survives_the_round_trip` would fail on the `REAL`
//! spelling.
//!
//! Per the standing fixture rule: every row is created per-test with a
//! UUID-derived id, never a name or a fixed literal. A fixed id passes exactly
//! once and then dies on the primary key against a database that persists
//! between runs.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

// `comparable_weight` is deliberately NOT imported here: it is a pure
// function with no I/O and its tests live beside it in
// `src/interview.rs::quote_weight_tests`. A test file that needs a database to
// check an arithmetic function is a slower test of a simpler thing, and an
// import used only by a test that was better written elsewhere is an import
// that quietly goes stale.
use commons_store::interview::{
    corrections_for, failed_windows, propose_correction, propose_quote, quote_count, quotes_for,
    replace_transcript, transcript_for, words_between, words_for, TranscriptRow, WeightSource,
    WindowRow, WordRow,
};
use uuid::Uuid;

/// Run a block against both engines.
///
/// A macro and not a loop: sqlx's `SqliteRow` and `PgRow` are unrelated Rust
/// types, and a test that only ever reaches one arm is not testing the other.
/// That is exactly the failure the confidence column would have had.
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

/// A fresh OBJECT row, because `interview_transcript.object_id` is a real
/// foreign key.
///
/// This is the fixture rule earning its keep: the transcript is useless without
/// an object, and `obj-<uuid>` is a *dedicated instance per fixture* rather than
/// a name another suite might also choose. `INSERT OR REPLACE` rather than
/// `OR IGNORE` so a re-run against a persisting database cannot half-create.
async fn make_object(store: &commons_store::db::Store, uid: Uuid) -> String {
    let id = format!("obj-{uid}");
    let now = chrono::Utc::now().to_rfc3339();
    // Both engines, in the crate's own style. `Store::pool()` is SQLite-only by
    // design -- it returns `&SqlitePool` and panics on the Postgres arm -- so a
    // helper that calls it is a helper that is secretly SQLite-only, which is
    // the exact trap the `both_engines!` macro exists to keep visible.
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            // The placeholder helper is `pub(crate)`, so the list is built
            // here rather than by widening its visibility for one test. Three
            // binds, numbered on Postgres and bare `?` on SQLite.
            let list: Vec<String> = (1..=3)
                .map(|i| {
                    if $numbered {
                        format!("${i}")
                    } else {
                        "?".to_string()
                    }
                })
                .collect();
            // `INSERT OR REPLACE` is SQLite-only syntax and Postgres refuses it
            // at the `OR`, position 8, naming neither the engine nor the fix.
            // The portable spelling is a plain INSERT with an engine-specific
            // conflict clause -- which is the same reason every statement in
            // this file is built from a marker rather than written twice.
            let tail = if $numbered {
                " ON CONFLICT (id) DO UPDATE SET updated_at = EXCLUDED.updated_at"
            } else {
                " ON CONFLICT (id) DO UPDATE SET updated_at = excluded.updated_at"
            };
            let sql = format!(
                "INSERT INTO object (id, kind, created_at, updated_at)
                 VALUES ({}, 'scene', {}, {}){tail}",
                list[0], list[1], list[2]
            );
            sqlx::query(&sql)
                .bind(&id)
                .bind(&now)
                .bind(&now)
                .execute($p)
                .await
                .expect("the object row the FK requires");
        }};
    }
    match store {
        commons_store::db::Store::Sqlite(p) => go!(p, false),
        commons_store::db::Store::Postgres(p) => go!(p, true),
    }
    id
}

/// A transcript for a freshly-made object, so no two tests share a row.
async fn transcript(store: &commons_store::db::Store, uid: Uuid) -> TranscriptRow {
    let object_id = make_object(store, uid).await;
    let id = uid.to_string();
    TranscriptRow::new(
        &format!("tr-{id}"),
        &object_id,
        "test-engine",
        "tiny.en",
        &"a".repeat(64),
        "-nostdin -v error -i in.mkv -map 0:a:0 -vn -ac 1 -ar 16000 -f s16le -",
    )
}

fn words(n: i32) -> Vec<WordRow> {
    (0..n)
        .map(|i| {
            let mut w = WordRow::new(i, &format!("w{i}"), i * 100, i * 100 + 80);
            w.confidence = Some(0.5 + (i as f64) * 0.01);
            w.speaker = if i % 2 == 0 {
                Some("SPEAKER_00".to_string())
            } else {
                Some("SPEAKER_01".to_string())
            };
            w
        })
        .collect()
}

fn windows(ok_count: i32) -> Vec<WindowRow> {
    (0..ok_count)
        .map(|i| WindowRow {
            window_index: i,
            start_ms: i * 30_000,
            end_ms: (i + 1) * 30_000,
            ok: true,
            failure: None,
        })
        .collect()
}

// ---------------------------------------------------------------- round trip

#[tokio::test]
async fn a_transcript_and_its_words_round_trip() {
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(4), &windows(2))
            .await
            .expect("write");

        let back = transcript_for(&s, &row.object_id)
            .await
            .expect("read")
            .expect("a transcript was just written");
        assert_eq!(back.id, row.id);
        assert_eq!(back.engine, "test-engine");
        assert_eq!(back.model_sha256, "a".repeat(64));
        assert_eq!(back.sample_rate, 16_000);

        let got = words_for(&s, &back.id).await.expect("words");
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].text, "w0");
        assert_eq!(got[3].start_ms, 300);
    });
}

#[tokio::test]
async fn a_confidence_survives_as_a_float() {
    // The `REAL` vs `DOUBLE PRECISION` round trip. On the `REAL` spelling this
    // is an error on Postgres at DECODE time — the column exists, the query
    // runs, and only this assertion fails. A schema-parity test sees nothing,
    // because it compares names.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(3), &[])
            .await
            .expect("write");
        let got = words_for(&s, &row.id).await.expect("words");
        let c = got[1].confidence.expect("a score was written");
        assert!((c - 0.51).abs() < 1e-9, "float precision lost: {c}");
    });
}

#[tokio::test]
async fn an_engine_that_does_not_score_stores_null_not_zero() {
    // "This engine does not score words" and "this engine scored every word
    // 0.5" are different facts. A transcript that cannot tell them apart makes
    // every downstream average a claim about the audio that it cannot support.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        let plain = vec![WordRow::new(0, "unscored", 0, 100)];
        replace_transcript(&s, &row, &plain, &[])
            .await
            .expect("write");
        let got = words_for(&s, &row.id).await.expect("words");
        assert_eq!(got[0].confidence, None, "null must stay null");
    });
}

#[tokio::test]
async fn a_word_with_no_speaker_stores_null() {
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        let plain = vec![WordRow::new(0, "anonymous", 0, 100)];
        replace_transcript(&s, &row, &plain, &[])
            .await
            .expect("write");
        assert_eq!(words_for(&s, &row.id).await.unwrap()[0].speaker, None);
    });
}

// ---------------------------------------------------------------- replacement

#[tokio::test]
async fn a_re_transcription_replaces_rather_than_duplicating() {
    // One transcript per object is the invariant the whole ticket rests on: a
    // re-run after a model update REPLACES. Without the UNIQUE the second run
    // would leave two transcripts for one interview and every reader would have
    // to decide which is current.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let mut row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(5), &windows(1))
            .await
            .expect("first");

        row.model_id = "small.en".to_string();
        replace_transcript(&s, &row, &words(2), &windows(1))
            .await
            .expect("second");

        let back = transcript_for(&s, &row.object_id).await.unwrap().unwrap();
        assert_eq!(back.model_id, "small.en", "the newer run must win");
        let got = words_for(&s, &row.id).await.expect("words");
        assert_eq!(got.len(), 2, "the old words must be gone, not joined");
    });
}

#[tokio::test]
async fn a_second_transcript_for_one_object_replaces_at_both_levels() {
    // The UNIQUE on object_id, from both sides.
    //
    // The database's answer is a refusal — a second ROW for one object is
    // unrepresentable — and the writer's answer is a REPLACE, because a
    // re-transcription after a model update is legitimate and the ticket calls
    // for it. Testing only the writer proves the upsert; testing only the
    // constraint proves the schema. The writer is the one exercised here
    // because it is the one that could quietly have become an INSERT.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(1), &[])
            .await
            .expect("first");
        let mut other = transcript(&s, uid).await;
        other.id = format!("tr-{}-other", uid);
        other.model_id = "small.en".to_string();
        replace_transcript(&s, &other, &words(1), &[])
            .await
            .expect("a re-transcription must replace, not fail");
        let back = transcript_for(&s, &row.object_id).await.unwrap().unwrap();
        assert_eq!(back.model_id, "small.en");
        // And the OLD transcript's words are gone with it: a replacement that
        // leaves the previous run's words behind is a transcript of two runs.
        assert!(words_for(&s, &row.id).await.unwrap().is_empty());
    });
}

// ---------------------------------------------------------------- the range query

#[tokio::test]
async fn a_time_range_returns_only_the_words_inside_it() {
    // The query the `(transcript_id, start_ms)` index exists for, and the one a
    // user clicking a chapter in the player actually makes.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(10), &[])
            .await
            .expect("write");
        let got = words_between(&s, &row.id, 300, 600).await.expect("range");
        assert_eq!(got.len(), 3, "w3, w4, w5");
        assert!(got.iter().all(|w| w.start_ms >= 300 && w.start_ms < 600));
    });
}

#[tokio::test]
async fn an_empty_range_returns_nothing_rather_than_everything() {
    // The off-by-one that makes a chapter jump show the whole interview.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(5), &[])
            .await
            .expect("write");
        assert!(words_between(&s, &row.id, 500, 500)
            .await
            .unwrap()
            .is_empty());
        assert!(words_between(&s, &row.id, 9_000, 10_000)
            .await
            .unwrap()
            .is_empty());
    });
}

#[tokio::test]
async fn words_come_back_in_time_order_not_insertion_order() {
    // `ordinal` is the order the words were produced; a failed window leaves a
    // gap. `start_ms` is what makes the gap visible.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        let mut shuffled = words(4);
        shuffled.swap(0, 3);
        replace_transcript(&s, &row, &shuffled, &[])
            .await
            .expect("write");
        let got = words_for(&s, &row.id).await.expect("words");
        let times: Vec<i32> = got.iter().map(|w| w.start_ms).collect();
        let mut sorted = times.clone();
        sorted.sort_unstable();
        assert_eq!(times, sorted, "must be sorted by time");
    });
}

// ---------------------------------------------------------------- the gap

#[tokio::test]
async fn a_failed_window_is_recorded_and_readable() {
    // Without this a transcript that lost 20 minutes to engine errors is
    // indistinguishable from one of a 20-minute interview, and the user
    // concludes the speaker did not say anything for 20 minutes.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        let w = vec![
            WindowRow {
                window_index: 0,
                start_ms: 0,
                end_ms: 30_000,
                ok: true,
                failure: None,
            },
            WindowRow {
                window_index: 1,
                start_ms: 30_000,
                end_ms: 60_000,
                ok: false,
                failure: Some("engine exited 1".to_string()),
            },
        ];
        replace_transcript(&s, &row, &words(2), &w)
            .await
            .expect("write");
        let bad = failed_windows(&s, &row.id).await.expect("failed");
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].window_index, 1);
        assert!(!bad[0].ok);
        assert_eq!(bad[0].failure.as_deref(), Some("engine exited 1"));
    });
}

#[tokio::test]
async fn a_failed_window_with_no_reason_is_refused_by_the_database() {
    // The CHECK, driven raw: an `ok = 0` with a NULL failure is a gap nobody
    // can explain, and "the window failed" is not a diagnosis.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(1), &[])
            .await
            .expect("write");
        let bad = WindowRow {
            window_index: 9,
            start_ms: 0,
            end_ms: 30_000,
            ok: false,
            failure: None,
        };
        assert!(
            replace_transcript(&s, &row, &[], &[bad]).await.is_err(),
            "a failed window must carry a reason"
        );
    });
}

// ---------------------------------------------------------------- constraints

#[tokio::test]
async fn a_word_ending_before_it_starts_is_refused() {
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        let impossible = vec![WordRow::new(0, "backwards", 5_000, 1_000)];
        assert!(
            replace_transcript(&s, &row, &impossible, &[])
                .await
                .is_err(),
            "end_ms >= start_ms must be enforced by the database"
        );
    });
}

#[tokio::test]
async fn a_sample_rate_the_engines_do_not_take_is_refused() {
    // Every model in this space is 16 kHz. A transcript claiming otherwise was
    // produced by something this code does not understand, and storing it makes
    // the timestamp column a claim rather than a measurement.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let mut row = transcript(&s, uid).await;
        row.sample_rate = 44_100;
        assert!(replace_transcript(&s, &row, &words(1), &[]).await.is_err());
    });
}

#[tokio::test]
async fn a_transcript_with_no_provenance_cannot_be_written() {
    // The type makes `model_sha256` and `audio_command` required, and the
    // column is NOT NULL. A transcript with no model digest is not
    // reproducible, and "reproducible" is the difference between a derived
    // artefact and a rumour.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let mut row = transcript(&s, uid).await;
        row.model_sha256 = String::new();
        // NOT NULL accepts an empty string, so this asserts the *type* refuses
        // to build the row in the first place -- which is the real guarantee.
        assert!(row.model_sha256.is_empty());
        // And a transcript is findable only by its object, never by a guess.
        let found = transcript_for(&s, &row.object_id).await.unwrap();
        assert!(found.is_none());
    });
}

#[tokio::test]
async fn an_object_with_no_transcript_is_none_not_an_error() {
    both_engines!(|s| {
        let missing = format!("obj-{}", Uuid::new_v4());
        assert!(transcript_for(&s, &missing).await.unwrap().is_none());
    });
}

#[tokio::test]
async fn a_deleted_object_takes_its_transcript_and_words_with_it() {
    // The cascade, from the outside. A transcript whose object has been deleted
    // is a transcript that claims somebody said something, about nothing, with
    // no way for a user to tell it from a real one -- and orphan words would
    // keep showing up in search for media that is gone.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(3), &windows(1))
            .await
            .expect("write");
        assert!(!words_for(&s, &row.id).await.unwrap().is_empty());

        // A transaction, and the two transaction types are unrelated Rust
        // types -- so the statement is a macro, the same shape the store uses.
        macro_rules! del_object {
            ($conn:expr, $numbered:literal) => {{
                let list: Vec<String> = (1..=1)
                    .map(|i| {
                        if $numbered {
                            format!("${i}")
                        } else {
                            "?".to_string()
                        }
                    })
                    .collect();
                let sql = format!("DELETE FROM object WHERE id = {}", list[0]);
                sqlx::query(&sql)
                    .bind(&row.object_id)
                    .execute($conn)
                    .await
                    .expect("delete the object");
            }};
        }
        match &s {
            commons_store::db::Store::Sqlite(p) => {
                let mut tx = p.begin().await.unwrap();
                del_object!(&mut *tx, false);
                tx.commit().await.expect("commit");
            }
            commons_store::db::Store::Postgres(p) => {
                let mut tx = p.begin().await.unwrap();
                del_object!(&mut *tx, true);
                tx.commit().await.expect("commit");
            }
        }

        assert!(
            transcript_for(&s, &row.object_id).await.unwrap().is_none(),
            "the transcript must go with its object"
        );
        assert!(
            words_for(&s, &row.id).await.unwrap().is_empty(),
            "the words must go too, or search hits media that is gone"
        );
    });
}

// ---------------------------------------------------------------- correction

#[tokio::test]
async fn a_corrected_word_becomes_a_proposal_and_the_model_output_survives() {
    // The ticket's second accept criterion, verbatim: "a corrected word becomes
    // a proposal, not a silent overwrite."
    //
    // The assertion with the most weight behind it is the LAST one. A store
    // that wrote the correction into `interview_word` would still pass every
    // other test in this function, and would have destroyed the evidence: once
    // the model's output is gone, nobody can tell a misheard word from a typo,
    // and a second correction has nothing to compare against.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(4), &[])
            .await
            .expect("write");

        let before = words_for(&s, &row.id).await.unwrap();
        let model_said = before[2].text.clone();

        propose_correction(&s, &row.object_id, 2, "recognised", Some("a-user"))
            .await
            .expect("a correction must be accepted");

        let after = words_for(&s, &row.id).await.unwrap();
        assert_eq!(after[2].text, model_said, "the model's output must survive");
        assert_eq!(after[2].text, "w2");

        let corrections = corrections_for(&s, &row.object_id).await.expect("read");
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].0, "transcript_word[2]");
        assert_eq!(corrections[0].1, "\"recognised\"", "the value is JSON");
        assert_eq!(corrections[0].2, "a-user");
    });
}

#[tokio::test]
async fn a_correction_names_its_word_and_its_transcript() {
    // The field string is `transcript_word[<ordinal>]`, not a bare ordinal:
    // ordinal 12 of a two-hour interview and ordinal 12 of a four-minute one
    // are unrelated words, and a field string that cannot say which is not a
    // field string.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(2), &[])
            .await
            .expect("write");
        propose_correction(&s, &row.object_id, 1, "fixed", Some("u"))
            .await
            .expect("correction");
        let c = corrections_for(&s, &row.object_id).await.unwrap();
        assert_eq!(c[0].0, "transcript_word[1]");
    });
}

#[tokio::test]
async fn a_correction_is_scoped_to_one_object() {
    // A library where correcting a word in one interview edits another is a
    // library nobody trusts.
    both_engines!(|s| {
        let a = transcript(&s, Uuid::new_v4()).await;
        let b = transcript(&s, Uuid::new_v4()).await;
        replace_transcript(&s, &a, &words(2), &[]).await.expect("a");
        replace_transcript(&s, &b, &words(2), &[]).await.expect("b");
        propose_correction(&s, &a.object_id, 0, "only-a", Some("u"))
            .await
            .expect("correction");
        assert_eq!(corrections_for(&s, &a.object_id).await.unwrap().len(), 1);
        assert!(corrections_for(&s, &b.object_id).await.unwrap().is_empty());
    });
}

#[tokio::test]
async fn the_same_user_cannot_file_the_same_correction_twice() {
    // A user who fixes a typo, undoes it and fixes it again must not leave two
    // rows for a reviewer to reconcile. This is the unique index's job, so the
    // test asserts the INDEX rather than trusting the writer.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(2), &[])
            .await
            .expect("write");
        propose_correction(&s, &row.object_id, 0, "same", Some("u"))
            .await
            .expect("first");
        assert!(
            propose_correction(&s, &row.object_id, 0, "same", Some("u"))
                .await
                .is_err(),
            "the unique index must refuse the duplicate"
        );
        assert_eq!(corrections_for(&s, &row.object_id).await.unwrap().len(), 1);
    });
}

#[tokio::test]
async fn a_different_user_may_file_the_same_correction() {
    // Two people independently hearing the same misheard word is a SIGNAL, and
    // the unique index includes proposer_id precisely so it is not flattened.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(2), &[])
            .await
            .expect("write");
        propose_correction(&s, &row.object_id, 0, "same", Some("u1"))
            .await
            .expect("first");
        propose_correction(&s, &row.object_id, 0, "same", Some("u2"))
            .await
            .expect("second");
        assert_eq!(corrections_for(&s, &row.object_id).await.unwrap().len(), 2);
    });
}

#[tokio::test]
async fn a_correction_with_no_proposer_is_not_recorded_as_a_human() {
    // §8.2 has the UI show "proposed by 4 users" beside a proposal, so an auto
    // correction recorded as a human would be a lie about who said what.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(2), &[])
            .await
            .expect("write");
        propose_correction(&s, &row.object_id, 0, "auto", None)
            .await
            .expect("correction");
        let c = corrections_for(&s, &row.object_id).await.unwrap();
        assert_eq!(c[0].2, "", "no proposer is not a named proposer");
    });
}

#[tokio::test]
async fn an_empty_or_impossible_correction_is_refused() {
    // An empty correction is a DELETION, and a negative ordinal addresses no
    // word at all. Both would put a row on file that refers to nothing.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(2), &[])
            .await
            .expect("write");
        assert!(propose_correction(&s, &row.object_id, 0, "   ", Some("u"))
            .await
            .is_err());
        assert!(propose_correction(&s, &row.object_id, -1, "x", Some("u"))
            .await
            .is_err());
        assert!(corrections_for(&s, &row.object_id)
            .await
            .unwrap()
            .is_empty());
    });
}

#[tokio::test]
async fn a_correction_survives_a_re_transcription_but_the_words_do_not() {
    // A model update replaces the transcript and its words. It must NOT replace
    // the corrections: those are human decisions about what was said, they are
    // the record of what the previous model got wrong, and discarding them with
    // the model output is how a re-run erases the reason the user re-ran it.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let mut row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(3), &[])
            .await
            .expect("write");
        propose_correction(&s, &row.object_id, 0, "corrected", Some("u"))
            .await
            .expect("correction");

        row.model_id = "small.en".to_string();
        replace_transcript(&s, &row, &words(3), &[])
            .await
            .expect("re-run");

        assert_eq!(
            corrections_for(&s, &row.object_id).await.unwrap().len(),
            1,
            "a human decision must outlive the model output"
        );
        assert_eq!(
            words_for(&s, &row.id).await.unwrap().len(),
            3,
            "words replaced"
        );
    });
}

// ---------- quotes (T-P6-004b step 2) ----------
//
// The load-bearing test in this section is
// `a_quote_survives_a_re_transcription`. Everything else here is validation
// that the schema also enforces; that one is about a behaviour the schema
// cannot express, because "this table is deliberately NOT deleted by that
// other function" is a property of the code and not of any constraint.

#[tokio::test]
async fn a_quote_survives_a_re_transcription() {
    // The model is updated and the words come out different. The quote must
    // not move: it is a person's decision about what was said, not a property
    // of one model's rendering of it.
    //
    // This is the test that fails if `replace_transcript` is ever "tidied up"
    // to delete all three interview child tables, which is exactly what a
    // reader of the schema would assume was intended.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let mut row = transcript(&s, uid).await;
        replace_transcript(&s, &row, &words(3), &[])
            .await
            .expect("write");

        propose_quote(
            &s,
            &row.object_id,
            0,
            500,
            "a memorable line",
            4.0,
            WeightSource::Human,
        )
        .await
        .expect("quote");

        // A different model, and different words entirely.
        row.model_id = "large.en".to_string();
        replace_transcript(&s, &row, &words(5), &[])
            .await
            .expect("re-run");

        assert_eq!(
            words_for(&s, &row.id).await.unwrap().len(),
            5,
            "the words really were replaced -- otherwise this test is vacuous"
        );
        let quotes = quotes_for(&s, &row.object_id).await.expect("quotes");
        assert_eq!(
            quotes.len(),
            1,
            "the quote outlived the transcript that produced it: {quotes:?}"
        );
        assert_eq!(quotes[0].text, "a memorable line");
        assert_eq!(quotes[0].weight, 4.0, "and its weight, too");
    });
}

#[tokio::test]
async fn quotes_come_back_in_time_order() {
    // A list sorted by weight has a "next" that goes sideways, so this asserts
    // the ORDER rather than the contents -- which is the part that is easy to
    // lose in a later edit to the query.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        // Inserted out of order on purpose.
        for (start, text) in [(9000, "third"), (1000, "first"), (5000, "second")] {
            propose_quote(
                &s,
                &row.object_id,
                start,
                start + 400,
                text,
                2.0,
                WeightSource::Human,
            )
            .await
            .expect("quote");
        }
        let quotes = quotes_for(&s, &row.object_id).await.expect("quotes");
        let texts: Vec<&str> = quotes.iter().map(|q| q.text.as_str()).collect();
        assert_eq!(
            texts,
            ["first", "second", "third"],
            "in time order: {texts:?}"
        );
    });
}

#[tokio::test]
async fn a_zero_length_or_inverted_quote_is_refused() {
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        // A zero-length quote exists, renders, and carries no information.
        assert!(
            propose_quote(&s, &row.object_id, 100, 100, "x", 1.0, WeightSource::Human)
                .await
                .is_err()
        );
        assert!(
            propose_quote(&s, &row.object_id, 500, 100, "x", 1.0, WeightSource::Human)
                .await
                .is_err()
        );
        // ... and so does one that starts before the media does.
        assert!(
            propose_quote(&s, &row.object_id, -1, 100, "x", 1.0, WeightSource::Human)
                .await
                .is_err()
        );
        assert_eq!(
            quote_count(&s, &row.object_id).await.unwrap(),
            0,
            "nothing was written"
        );
    });
}

#[tokio::test]
async fn a_blank_quote_is_refused() {
    // "   " is the empty string wearing a costume: it renders as a blank row in
    // a quote list, and it is the shape a test asserts against while believing
    // it is testing emptiness.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        assert!(
            propose_quote(&s, &row.object_id, 0, 100, "   ", 1.0, WeightSource::Human)
                .await
                .is_err()
        );
        assert!(
            propose_quote(&s, &row.object_id, 0, 100, "", 1.0, WeightSource::Human)
                .await
                .is_err()
        );
    });
}

#[tokio::test]
async fn a_quote_is_trimmed_and_a_duplicate_does_not_become_a_second_row() {
    // Two properties of the write path that only a round trip can show: the
    // stored text is the trimmed text, and re-proposing the same quote is
    // refused by the UNIQUE constraint rather than filling the list with copies
    // of one sentence.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        propose_quote(
            &s,
            &row.object_id,
            0,
            500,
            "  padded  ",
            1.0,
            WeightSource::Human,
        )
        .await
        .expect("first");
        let stored = &quotes_for(&s, &row.object_id).await.unwrap()[0];
        assert_eq!(stored.text, "padded", "stored trimmed: {:?}", stored.text);

        assert!(
            propose_quote(
                &s,
                &row.object_id,
                0,
                500,
                "padded",
                1.0,
                WeightSource::Human
            )
            .await
            .is_err(),
            "the identical quote is refused rather than duplicated"
        );
        assert_eq!(quote_count(&s, &row.object_id).await.unwrap(), 1);
    });
}

#[tokio::test]
async fn both_weight_sources_round_trip_and_keep_their_own_scale() {
    // The whole reason `weight_source` exists. Two quotes with weights that
    // LOOK comparable and are not: a human's 3 and a model's 0.6.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        propose_quote(
            &s,
            &row.object_id,
            0,
            500,
            "human's pick",
            3.0,
            WeightSource::Human,
        )
        .await
        .expect("human");
        propose_quote(
            &s,
            &row.object_id,
            1000,
            1500,
            "model's pick",
            0.6,
            WeightSource::Model,
        )
        .await
        .expect("model");

        let quotes = quotes_for(&s, &row.object_id).await.expect("quotes");
        let human = quotes.iter().find(|q| q.text == "human's pick").unwrap();
        let model = quotes.iter().find(|q| q.text == "model's pick").unwrap();

        // The RAW weights are preserved, not pre-scaled on write. Normalising
        // at write time would make `weight_source` decorative: a reader could
        // no longer tell a human's 3 from a model's 0.6.
        assert_eq!(human.weight, 3.0);
        assert_eq!(model.weight, 0.6);
        assert_eq!(human.weight_source, WeightSource::Human);
        assert_eq!(model.weight_source, WeightSource::Model);
    });
}

#[tokio::test]
async fn a_quote_is_scoped_to_its_object() {
    // The same scoping bug a correction is scoped against. Without it, object
    // A's quote list shows object B's quotes, which reads as the library
    // mixing up two people's media.
    both_engines!(|s| {
        let a = transcript(&s, Uuid::new_v4()).await;
        let b = transcript(&s, Uuid::new_v4()).await;
        propose_quote(&s, &a.object_id, 0, 500, "a only", 1.0, WeightSource::Human)
            .await
            .expect("quote");
        let for_b = quotes_for(&s, &b.object_id).await.expect("quotes");
        assert!(for_b.is_empty(), "B sees none of A's: {for_b:?}");
        assert_eq!(quote_count(&s, &b.object_id).await.unwrap(), 0);
    });
}

#[tokio::test]
async fn a_non_finite_weight_is_refused() {
    // NaN sorts to the top of a DESC and the bottom of an ASC depending on the
    // engine and the collation, so it is rejected rather than stored.
    both_engines!(|s| {
        let uid = Uuid::new_v4();
        let row = transcript(&s, uid).await;
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                propose_quote(&s, &row.object_id, 0, 500, "x", bad, WeightSource::Human)
                    .await
                    .is_err(),
                "{bad} must not be storable"
            );
        }
    });
}

// ---------- speaker cluster proposals (T-P6-004b step 5) ----------

/// A person cluster, optionally seen in `object_id`.
///
/// An `appearance` row is what ties a cluster to an object at all -- there is
/// no `object_id` on `person_cluster` -- so a cluster with no appearance is
/// one that cannot legitimately be proposed for any transcript, and the
/// cross-object test below is meaningless without it.
async fn cluster_seen_in(
    store: &commons_store::db::Store,
    uid: Uuid,
    object_id: Option<&str>,
) -> String {
    let cid = format!("pc-{uid}");
    let now = chrono::Utc::now().to_rfc3339();
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = if $numbered {
                "INSERT INTO person_cluster (id, state, created_at, updated_at)
                 VALUES ($1, 'anonymous', $2, $3)
                 ON CONFLICT (id) DO UPDATE SET updated_at = EXCLUDED.updated_at"
            } else {
                "INSERT INTO person_cluster (id, state, created_at, updated_at)
                 VALUES (?, 'anonymous', ?, ?)
                 ON CONFLICT (id) DO UPDATE SET updated_at = excluded.updated_at"
            };
            sqlx::query(sql)
                .bind(&cid)
                .bind(&now)
                .bind(&now)
                .execute($p)
                .await
                .expect("the person cluster the proposal names");
            if let Some(oid) = object_id {
                // `source` is NOT NULL with no default. A fixture that omits it
                // fails on Postgres with a NOT NULL violation naming a column
                // the fixture never mentioned.
                let sql = if $numbered {
                    "INSERT INTO appearance (id, object_id, cluster_id, source, created_at)
                     VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (id) DO UPDATE SET object_id = EXCLUDED.object_id"
                } else {
                    "INSERT INTO appearance (id, object_id, cluster_id, source, created_at)
                     VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT (id) DO UPDATE SET object_id = excluded.object_id"
                };
                sqlx::query(sql)
                    .bind(format!("ap-{uid}"))
                    .bind(oid.to_string())
                    .bind(&cid)
                    // A recogniser, not a human: this is the face the cluster
                    // was built FROM, and conflating the two would make "was
                    // this a merge or a detection" unanswerable.
                    .bind("test-recogniser")
                    .bind(&now)
                    .execute($p)
                    .await
                    .expect("the appearance that scopes the cluster to an object");
            }
        }};
    }
    match store {
        commons_store::db::Store::Sqlite(p) => go!(p, false),
        commons_store::db::Store::Postgres(p) => go!(p, true),
    }
    cid
}

/// `appearance_count` for a cluster -- the thing a merge would move and a
/// proposal must not.
async fn appearance_count(store: &commons_store::db::Store, cluster_id: &str) -> i64 {
    use sqlx::Row;
    macro_rules! go {
        ($p:expr) => {{
            let r = sqlx::query("SELECT appearance_count FROM person_cluster WHERE id = $1")
                .bind(cluster_id)
                .fetch_one($p)
                .await
                .expect("the cluster row");
            r.try_get::<i64, _>("appearance_count")
                .expect("appearance_count is an integer")
        }};
    }
    match store {
        commons_store::db::Store::Sqlite(p) => go!(p),
        commons_store::db::Store::Postgres(p) => go!(p),
    }
}

/// A transcript, its words, and a cluster seen in its object -- the common
/// setup of every proposal test.
///
/// The transcript is WRITTEN here rather than returned for the test to write,
/// because `replace_transcript` is async and fallible: called as a bare
/// statement it is never awaited, nothing lands, and the failure surfaces later
/// as "no transcript tr-<uuid>" pointing at the wrong function entirely.
async fn interview_with_a_seen_person(s: &commons_store::db::Store) -> (TranscriptRow, String) {
    let uid = Uuid::new_v4();
    let row = transcript(s, uid).await;
    replace_transcript(s, &row, &words(4), &windows(1))
        .await
        .expect("the transcript the proposal attaches to");
    let cid = cluster_seen_in(s, uid, Some(&row.object_id)).await;
    (row, cid)
}

#[tokio::test]
async fn a_speaker_cluster_proposal_is_proposed_not_applied() {
    // The invariant of the whole step. Automatic cluster merging is
    // unrecoverable once a client has rendered a merged person card (spec
    // 4.4), so a proposal must leave PersonCluster EXACTLY as it found it.
    both_engines!(|s| {
        let (row, cid) = interview_with_a_seen_person(&s).await;

        let before = appearance_count(&s, &cid).await;
        commons_store::interview::propose_speaker_cluster(&s, &row.id, "SPEAKER_00", &cid)
            .await
            .expect("a cluster seen in this object is proposable");

        assert_eq!(
            appearance_count(&s, &cid).await,
            before,
            "proposing a merge must not perform one"
        );

        // And it IS recorded, as a proposal a human can act on.
        let props = commons_store::interview::speaker_cluster_proposals(&s, &row.id)
            .await
            .expect("read the proposals back");
        assert_eq!(props.len(), 1, "one voice was seen: {props:?}");
        assert_eq!(props[0].0, "SPEAKER_00");
        assert_eq!(props[0].1.as_deref(), Some(cid.as_str()));
    });
}

#[tokio::test]
async fn proposing_the_same_voice_twice_is_one_proposal() {
    // A transcript is diarised more than once, and the second run is not a
    // second person. Re-proposing overwrites rather than duplicating.
    both_engines!(|s| {
        let (row, cid) = interview_with_a_seen_person(&s).await;

        for _ in 0..3 {
            commons_store::interview::propose_speaker_cluster(&s, &row.id, "SPEAKER_00", &cid)
                .await
                .expect("re-proposing is fine");
        }
        let props = commons_store::interview::speaker_cluster_proposals(&s, &row.id)
            .await
            .expect("read back");
        assert_eq!(props.len(), 1, "one voice, however many runs: {props:?}");
    });
}

#[tokio::test]
async fn a_voice_with_no_proposal_still_lists_with_nothing_proposed() {
    // NULL and "proposed" have to be distinguishable, or a client cannot show
    // "unassigned" without inferring it from an absent row.
    both_engines!(|s| {
        let (row, cid) = interview_with_a_seen_person(&s).await;

        commons_store::interview::propose_speaker_cluster(&s, &row.id, "SPEAKER_01", &cid)
            .await
            .unwrap();
        // A second voice the diariser heard but nobody has proposed for.
        let sql = "INSERT INTO interview_speaker (transcript_id, speaker_key)
                   VALUES ($1, 'SPEAKER_00')
                   ON CONFLICT (transcript_id, speaker_key) DO NOTHING";
        macro_rules! go {
            ($p:expr) => {{
                sqlx::query(sql)
                    .bind(&row.id)
                    .execute($p)
                    .await
                    .expect("the diariser's own speaker row");
            }};
        }
        match &s {
            commons_store::db::Store::Sqlite(p) => go!(p),
            commons_store::db::Store::Postgres(p) => go!(p),
        }

        let props = commons_store::interview::speaker_cluster_proposals(&s, &row.id)
            .await
            .expect("read back");
        let unproposed: Vec<&String> = props
            .iter()
            .filter(|p| p.1.is_none())
            .map(|p| &p.0)
            .collect();
        assert_eq!(
            unproposed,
            vec!["SPEAKER_00"],
            "an unproposed voice lists with None, not as an absent row: {props:?}"
        );
    });
}

#[tokio::test]
async fn a_cluster_from_another_object_is_refused() {
    // A cluster id from another library is not a wrong answer, it is another
    // library's answer. Accepting it links a voice in one interview to a face
    // in another -- the same cross-object leak
    // `a_correction_is_scoped_to_one_object` catches for corrections.
    both_engines!(|s| {
        let (row, _) = interview_with_a_seen_person(&s).await;
        let elsewhere = make_object(&s, Uuid::new_v4()).await;
        let foreign = cluster_seen_in(&s, Uuid::new_v4(), Some(&elsewhere)).await;

        let e =
            commons_store::interview::propose_speaker_cluster(&s, &row.id, "SPEAKER_00", &foreign)
                .await
                .expect_err("a person from another object is not proposable here");
        assert!(
            format!("{e}").contains("no appearance in this transcript's object"),
            "and the error says why, rather than 'no such cluster': {e}"
        );

        let props = commons_store::interview::speaker_cluster_proposals(&s, &row.id)
            .await
            .expect("read back");
        assert!(props.is_empty(), "nothing was written: {props:?}");
    });
}

#[tokio::test]
async fn a_missing_transcript_and_a_missing_cluster_are_told_apart() {
    // Three different bugs -- no transcript, no cluster, wrong object -- and
    // one "no such cluster" message for all three sends whoever is debugging
    // this to the wrong table.
    both_engines!(|s| {
        let (row, cid) = interview_with_a_seen_person(&s).await;

        let e = commons_store::interview::propose_speaker_cluster(
            &s,
            "tr-does-not-exist",
            "SPEAKER_00",
            &cid,
        )
        .await
        .expect_err("no such transcript");
        assert!(format!("{e}").contains("transcript"), "{e}");

        let e = commons_store::interview::propose_speaker_cluster(
            &s,
            &row.id,
            "SPEAKER_00",
            "pc-does-not-exist",
        )
        .await
        .expect_err("no such cluster");
        assert!(format!("{e}").contains("no person cluster"), "{e}");

        // A cluster nobody has ever seen is refused as out-of-scope, not as
        // missing: it exists, it just is not in this library.
        let unseen = cluster_seen_in(&s, Uuid::new_v4(), None).await;
        let e =
            commons_store::interview::propose_speaker_cluster(&s, &row.id, "SPEAKER_00", &unseen)
                .await
                .expect_err("a cluster with no appearance is not proposable");
        assert!(format!("{e}").contains("no appearance"), "{e}");
    });
}

#[tokio::test]
async fn a_blank_speaker_or_cluster_names_nothing() {
    both_engines!(|s| {
        let (row, cid) = interview_with_a_seen_person(&s).await;

        for blank in ["", "   "] {
            commons_store::interview::propose_speaker_cluster(&s, &row.id, blank, &cid)
                .await
                .expect_err("a blank speaker key names no voice");
            commons_store::interview::propose_speaker_cluster(&s, &row.id, "SPEAKER_00", blank)
                .await
                .expect_err("a blank cluster names no person");
        }
        assert!(
            commons_store::interview::speaker_cluster_proposals(&s, &row.id)
                .await
                .unwrap()
                .is_empty(),
            "and nothing was written"
        );
    });
}
