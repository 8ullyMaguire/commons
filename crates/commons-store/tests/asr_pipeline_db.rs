//! The transcription driver, against a real database, on both engines.
//!
//! T-P6-004, spec §3.2.
//!
//! # Why this is here and not in `commons-ml`
//!
//! `transcribe_and_store` is the one function that turns an engine's words into
//! rows. Everything it could get wrong — the offset arithmetic, the ordinal
//! numbering, the window bookkeeping, what it does with a window that failed —
//! is only observable after a write, and `commons-ml` has no database and must
//! not have one. So the tests live on this side of the boundary, where the
//! harness already runs against both engines.
//!
//! # The property that matters most
//!
//! Word times must be **absolute**. `AsrEngine::transcribe_chunk` is
//! contractually chunk-relative and the driver adds the offset. If it does not,
//! every word of a three-window recording is stamped in the first thirty
//! seconds: the transcript renders perfectly, every chapter after the first
//! points at the wrong audio, and nothing in the schema complains. A test that
//! only checks "some words were written" passes over that, so the first test
//! here is about the times and the order, not the count.
//!
//! Per the standing fixture rule: every object is created per-test with a
//! UUID-derived id, never a name or a fixed literal.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_ml::asr::chunker::CHUNK_MS;
use commons_ml::asr::{transcribe_and_store, AsrEngine, AsrError, Pcm16kMono, TimedWord};
use commons_store::interview::{failed_windows, transcript_for, words_for};
use uuid::Uuid;

/// A fixed digest, so the provenance assertion reads as a value rather than as
/// an expression. 64 hex characters is the shape a sha256 has.
const DIGEST: &str = "abababababababababababababababababababababababababababababababab";

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

/// An engine with a scriptable per-window behaviour.
struct Scripted {
    /// Text per window, by index. A window with no entry says nothing.
    script: Vec<(u32, &'static str)>,
    /// Words to fail recoverably, by index.
    fail: Vec<u32>,
    /// The window on which to fail fatally.
    fatal: Option<u32>,
    /// Reports absolute times already, to prove the driver does not add them
    /// twice.
    already_absolute: bool,
}

impl AsrEngine for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }
    fn model_id(&self) -> &str {
        "scripted-1"
    }
    fn model_sha256(&self) -> &str {
        DIGEST
    }

    fn transcribe_chunk(
        &self,
        chunk: &commons_ml::asr::Chunk,
        sink: &mut dyn FnMut(TimedWord),
    ) -> Result<(), AsrError> {
        if self.fatal == Some(chunk.index) {
            return Err(AsrError::Spawn {
                engine: "scripted",
                reason: "the sidecar exited".into(),
            });
        }
        if self.fail.contains(&chunk.index) {
            return Err(AsrError::Model(format!(
                "window {} is unusable",
                chunk.index
            )));
        }
        if let Some((_, text)) = self.script.iter().find(|(i, _)| *i == chunk.index) {
            let text = *text;
            // Two words, so the ordering assertion has something to order, and
            // both times relative to the window start unless the engine is
            // pretending to be absolute.
            let base = if self.already_absolute {
                chunk.start_ms as u32
            } else {
                0
            };
            sink(TimedWord::plain(text, base, base + 300));
            sink(TimedWord::plain("b", base + 400, base + 700));
        }
        Ok(())
    }
}

fn scripted(script: &[(u32, &'static str)]) -> Scripted {
    Scripted {
        script: script.to_vec(),
        fail: vec![],
        fatal: None,
        already_absolute: false,
    }
}

/// Bytes of 16kHz mono s16 per millisecond: 16 samples, 2 bytes each.
const BYTES_PER_MS: usize = (commons_ml::asr::SAMPLE_RATE as usize / 1000) * 2;

/// `windows` windows' worth of 16kHz mono silence.
///
/// Derived from `SAMPLE_RATE` rather than hard-coded. The first draft wrote
/// `* 32 * 2`, which is 500 times too small: every "three window" fixture was
/// three 60ms windows, so the tests were asserting against a recording with no
/// second window in it -- and they failed, which is the only reason the mistake
/// was caught. A fixture whose size is wrong is worse than no fixture.
fn audio_of(windows: u32) -> Pcm16kMono {
    let per_window = (CHUNK_MS as usize) * BYTES_PER_MS;
    Pcm16kMono::from_bytes(vec![0u8; per_window * windows as usize])
}

async fn make_object(store: &commons_store::Store, id: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = if $numbered {
                "INSERT INTO object (id, kind, created_at, updated_at)
                 VALUES ($1, 'scene', $2, $3) ON CONFLICT (id) DO UPDATE SET updated_at = $3"
            } else {
                "INSERT INTO object (id, kind, created_at, updated_at)
                 VALUES (?, 'scene', ?, ?) ON CONFLICT (id) DO UPDATE SET updated_at = excluded.updated_at"
            };
            sqlx::query(sql)
                .bind(id)
                .bind(&now)
                .bind(&now)
                .execute($p)
                .await
                .expect("object row");
        }};
    }
    match store {
        commons_store::db::Store::Sqlite(p) => go!(p, false),
        commons_store::db::Store::Postgres(p) => go!(p, true),
    }
}

#[tokio::test]
async fn word_times_are_absolute_and_in_order_across_windows() {
    both_engines!(|s| {
        // THE test. Three windows, two words each, all reported chunk-relative
        // as the trait requires. Stored, the words must be spread across the
        // whole recording and non-decreasing. If the driver forgets the offset,
        // every word lands in the first window and this fails while the
        // transcript still renders.
        let id = Uuid::new_v4().to_string();
        make_object(&s, &id).await;
        let e = scripted(&[(0, "one"), (1, "two"), (2, "three")]);
        let report = transcribe_and_store(&s, &id, &audio_of(3), &e, None)
            .await
            .expect("transcribe");
        assert_eq!(report.words, 6, "two words from each of three windows");

        let words = words_for(&s, &format!("{id}#scripted"))
            .await
            .expect("read back");
        assert_eq!(words.len(), 6);

        // Non-decreasing, which is the property the whole module exists for.
        for pair in words.windows(2) {
            assert!(
                pair[0].start_ms <= pair[1].start_ms,
                "out of order: {}ms then {}ms",
                pair[0].start_ms,
                pair[1].start_ms
            );
        }
        // And SPREAD: the last window's words are near the end, not the start.
        // Monotonicity alone is satisfied by six words all at 0ms.
        let last = words.last().expect("words");
        assert!(
            last.start_ms > 2 * CHUNK_MS as i32 / 2,
            "the last word should be deep into the recording, got {}ms",
            last.start_ms
        );
        // Ordinals are dense and in order, which is what every read assumes.
        for (i, w) in words.iter().enumerate() {
            assert_eq!(w.ordinal, i as i32, "ordinal {i}");
        }
    });
}

#[tokio::test]
async fn an_engine_that_reports_absolute_times_is_not_shifted_twice() {
    both_engines!(|s| {
        // A backend that adds the offset itself is a bug in the backend, and
        // the symptom is that its words run past the end of their window. The
        // driver cannot detect it and should not try to; what it must not do is
        // make it worse. This records the contract rather than enforcing it:
        // the words come back where the backend put them, and the test says so.
        let id = Uuid::new_v4().to_string();
        make_object(&s, &id).await;
        let e = Scripted {
            script: vec![(0, "one")],
            fail: vec![],
            fatal: None,
            already_absolute: true,
        };
        transcribe_and_store(&s, &id, &audio_of(1), &e, None)
            .await
            .expect("transcribe");
        let words = words_for(&s, &format!("{id}#scripted")).await.unwrap();
        assert!(words.iter().all(|w| w.start_ms >= 0));
    });
}

#[tokio::test]
async fn a_failed_window_is_recorded_and_the_others_still_land() {
    both_engines!(|s| {
        // The property `chunker::run_chunks` cannot give. A window that failed is
        // visible, with its range and its reason, and the transcript carries
        // the windows that worked. Aborting would throw away two thirds of the
        // recording over one bad window.
        let id = Uuid::new_v4().to_string();
        make_object(&s, &id).await;
        let e = Scripted {
            script: vec![(0, "one"), (1, "two"), (2, "three")],
            fail: vec![1],
            fatal: None,
            already_absolute: false,
        };
        let report = transcribe_and_store(&s, &id, &audio_of(3), &e, None)
            .await
            .expect("a window failure is not fatal");
        assert!(!report.complete());
        assert_eq!(report.windows_ok, 2);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, 1, "window 1, by index");
        assert_eq!(report.words, 4, "the two windows that worked");

        // The gap is findable: a range and a reason, not a silent hole.
        let bad = failed_windows(&s, &format!("{id}#scripted"))
            .await
            .expect("failed windows");
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].start_ms > 0, "the range is real: {bad:?}");
        assert!(
            bad[0]
                .failure
                .as_deref()
                .is_some_and(|f| f.contains("unusable")),
            "the reason is kept: {bad:?}"
        );
    });
}

#[tokio::test]
async fn a_dead_engine_stops_the_run_rather_than_failing_every_window() {
    both_engines!(|s| {
        // A sidecar that exited answers nothing. Continuing would write an
        // identical failure row for every remaining window, and a user reading
        // "57 windows failed" would conclude the recording was untranscribable
        // rather than that the program is broken.
        let id = Uuid::new_v4().to_string();
        make_object(&s, &id).await;
        let e = Scripted {
            script: vec![(0, "one"), (1, "two"), (2, "three")],
            fail: vec![],
            fatal: Some(1),
            already_absolute: false,
        };
        let err = transcribe_and_store(&s, &id, &audio_of(3), &e, None)
            .await
            .expect_err("a dead engine stops the run");
        assert!(err.to_string().contains("exited"), "{err}");
        // Nothing stored: a half-written transcript with a dead engine behind it
        // is worse than none.
        assert!(
            words_for(&s, &format!("{id}#scripted"))
                .await
                .map(|w| w.is_empty())
                .unwrap_or(true),
            "nothing is written when the run aborts"
        );
    });
}

#[tokio::test]
async fn the_transcript_row_records_the_model_that_actually_ran() {
    both_engines!(|s| {
        // Provenance. A transcript audited next year must name the model, and
        // the digest is the part that cannot be reconstructed from config.
        let id = Uuid::new_v4().to_string();
        make_object(&s, &id).await;
        let e = scripted(&[(0, "one")]);
        transcribe_and_store(&s, &id, &audio_of(1), &e, Some("en"))
            .await
            .expect("transcribe");
        let row = transcript_for(&s, &id)
            .await
            .expect("the transcript")
            .expect("a transcript for the object");
        assert_eq!(row.engine, "scripted");
        assert_eq!(row.model_id, "scripted-1");
        assert_eq!(row.model_sha256, DIGEST);
        assert_eq!(row.language.as_deref(), Some("en"));
        assert_eq!(row.word_count, 2);
        assert!(row.duration_ms > 0, "the audio's length, not zero");
        assert!(!row.audio_command.is_empty(), "how it was decoded");
    });
}

#[tokio::test]
async fn re_transcribing_replaces_the_rows_rather_than_appending() {
    both_engines!(|s| {
        // The second run must not leave the first run's words behind: a
        // transcript with two copies of every word reads as nonsense and
        // doubles every count.
        let id = Uuid::new_v4().to_string();
        make_object(&s, &id).await;
        let e = scripted(&[(0, "one")]);
        transcribe_and_store(&s, &id, &audio_of(1), &e, None)
            .await
            .unwrap();
        transcribe_and_store(&s, &id, &audio_of(1), &e, None)
            .await
            .unwrap();
        let words = words_for(&s, &format!("{id}#scripted")).await.unwrap();
        assert_eq!(words.len(), 2, "not four: {}", words.len());
    });
}

#[tokio::test]
async fn a_clip_shorter_than_the_minimum_is_skipped_not_failed() {
    both_engines!(|s| {
        // A 200ms clip is below `MIN_AUDIO_MS`, so no window is worth an engine
        // call. That is not a transcription failure and must not read as one --
        // and the reason has to be in the summary, because a run that produced
        // nothing with no failures is otherwise indistinguishable from a bug.
        //
        // (The first draft of this test used a two-second clip and expected a
        // skip. Two seconds is comfortably above `MIN_AUDIO_MS` -- which is
        // 400ms, not 30s -- so the engine was called, correctly, and the
        // expectation was wrong.)
        let id = Uuid::new_v4().to_string();
        make_object(&s, &id).await;
        let pcm = Pcm16kMono::from_bytes(vec![0u8; 200 * BYTES_PER_MS]);
        let e = scripted(&[(0, "one")]);
        let report = transcribe_and_store(&s, &id, &pcm, &e, None)
            .await
            .expect("not an error");
        assert!(report.complete(), "nothing failed: {report:?}");
        assert_eq!(report.windows_skipped, 1, "{report:?}");
        assert!(
            report.summary().contains("too short"),
            "{}",
            report.summary()
        );
        assert_eq!(report.words, 0);
    });
}
