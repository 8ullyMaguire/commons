//! Audio in, a stored transcript out.
//!
//! T-P6-004, spec §3.2. The module that makes an `AsrEngine` usable: it owns
//! the loop that no single adapter should.
//!
//! # Why this is not in the adapters
//!
//! Each engine knows one thing — how to turn 30 seconds of audio into words —
//! and none of them knows how to run sixty of those in order, add each window's
//! offset, renumber the result, or record that window eleven failed. That loop
//! lives here, and each adapter stays the one thing it was written to be.
//!
//! # It reuses the chunker rather than reimplementing it
//!
//! [`Chunker::absolutise`] already owns the offset arithmetic, and it is
//! deliberately fallible: a word a backend reports *before* its window cannot be
//! made absolute without lying. A second implementation here would be a second
//! set of rules about when to drop a word, and the two would drift.
//!
//! What this adds over [`crate::asr::chunker::run_chunks`] is the part that
//! function cannot do: it *reports* what happened. `run_chunks` drops a failed
//! window and moves on, which is right for producing words and wrong for
//! storing a transcript — a gap with no record is a gap nobody can find.
//!
//! # A failed window is recorded, not fatal
//!
//! A two-hour interview is sixty windows and on a laptop one of them will fail:
//! the model runs out of memory, or a window is silence the engine rejects. An
//! abort throws away fifty-nine windows and the user cannot tell which tenth of
//! the interview is missing. So each window is recorded in `interview_windows`
//! with `ok = false` and the reason, and the transcript carries the windows that
//! did work. The UI reads `failed_windows` and can say "11:20 to 12:05 was not
//! transcribed" — true and actionable, where "transcription failed" is not.
//!
//! An engine that has *died* is a different case and is fatal: a sidecar that
//! exited answers no later window, and the run would write an identical failure
//! row for every one of them.

use std::sync::Arc;

use commons_store::interview::{replace_transcript, TranscriptRow, WindowRow, WordRow};
use commons_store::Store;

use crate::asr::audio::{self, Pcm16kMono};
use crate::asr::chunker::Chunker;
use crate::asr::{AsrEngine, AsrError, TimedWord};

/// How a run went.
#[derive(Debug, Default, PartialEq)]
pub struct RunReport {
    /// Words written, renumbered from zero across all windows.
    pub words: usize,
    /// Windows that produced words.
    pub windows_ok: usize,
    /// Windows skipped as too short to be worth an engine call.
    ///
    /// Counted rather than passed over: a run whose windows are all "too
    /// short" produced no transcript, and the reason is not "the model failed".
    pub windows_skipped: usize,
    /// Words dropped because their time could not be made absolute.
    pub dropped: usize,
    /// Windows that failed, and why.
    pub failed: Vec<(u32, String)>,
    /// The audio duration, in milliseconds.
    pub duration_ms: i64,
}

impl RunReport {
    /// Whether every window that was worth transcribing worked.
    pub fn complete(&self) -> bool {
        self.failed.is_empty()
    }

    /// A sentence about what happened, for a log line or an error.
    pub fn summary(&self) -> String {
        if self.complete() {
            let mut s = format!(
                "{} words in {} windows, {}ms of audio",
                self.words, self.windows_ok, self.duration_ms
            );
            if self.windows_skipped > 0 {
                s.push_str(&format!(" ({} window(s) too short)", self.windows_skipped));
            }
            if self.dropped > 0 {
                s.push_str(&format!(" ({} word(s) dropped)", self.dropped));
            }
            return s;
        }
        // Names the first failure, because "something failed" is what a user
        // cannot act on and "window 11: out of memory" is what they can.
        let first = &self.failed[0];
        format!(
            "{} words in {} windows; {} window(s) failed, first: window {}: {}",
            self.words,
            self.windows_ok,
            self.failed.len(),
            first.0,
            first.1
        )
    }
}

/// Transcribe `audio` with `engine` and store the result for `object_id`.
///
/// The transcript row records what RAN — the engine's own name, model id and
/// digest — so a transcript can be audited months later against the model that
/// produced it rather than against the config that was current at the time.
///
/// A partial run is stored as a partial run. The window rows are what make that
/// honest, and they go in the same transaction as the words, so a transcript can
/// never claim a window succeeded that has no row.
pub async fn transcribe_and_store(
    store: &Store,
    object_id: &str,
    audio: &Pcm16kMono,
    engine: &dyn AsrEngine,
    language: Option<&str>,
) -> Result<RunReport, AsrError> {
    let chunker = Chunker::default();
    let duration_ms = audio.duration_ms() as i64;

    let mut words: Vec<WordRow> = Vec::new();
    let mut windows: Vec<WindowRow> = Vec::new();
    let mut report = RunReport {
        duration_ms,
        ..Default::default()
    };

    for chunk in chunker.windows(audio) {
        if !chunk.is_worth_transcribing() {
            report.windows_skipped += 1;
            continue;
        }

        let mut relative: Vec<TimedWord> = Vec::new();
        let outcome = {
            let mut collect = |w: TimedWord| relative.push(w);
            engine.transcribe_chunk(&chunk, &mut collect)
        };

        match outcome {
            Ok(()) => {
                for w in relative {
                    // The offset, applied by the one function that owns it.
                    let Some((start, end)) = Chunker::absolutise(&chunk, w.start_ms, w.end_ms)
                    else {
                        report.dropped += 1;
                        continue;
                    };
                    words.push(WordRow {
                        // Ordinals are renumbered at the end, so the number is a
                        // property of the stored transcript rather than of the
                        // order the windows happened to finish in.
                        ordinal: 0,
                        text: w.text,
                        start_ms: start as i32,
                        end_ms: end as i32,
                        confidence: w.confidence.map(|c| c as f64),
                        speaker: w.speaker.map(|s| s.label()),
                    });
                }
                report.windows_ok += 1;
                windows.push(window_row(&chunk, true, None));
            }
            Err(e) => {
                // An engine that is GONE cannot be asked again. Distinguishing
                // the two is what stops a dead sidecar producing one identical
                // failure row per remaining window.
                if is_fatal(&e) {
                    return Err(e);
                }
                let reason = e.to_string();
                report.failed.push((chunk.index, reason.clone()));
                windows.push(window_row(&chunk, false, Some(reason)));
            }
        }
    }

    for (i, w) in words.iter_mut().enumerate() {
        w.ordinal = i32::try_from(i).unwrap_or(i32::MAX);
    }

    let mut row = TranscriptRow::new(
        &transcript_id(object_id, engine),
        object_id,
        engine.name(),
        engine.model_id(),
        engine.model_sha256(),
        &audio_command(),
    );
    row.language = language.map(str::to_string);
    row.word_count = i64::try_from(words.len()).unwrap_or(i64::MAX);
    row.duration_ms = duration_ms;

    replace_transcript(store, &row, &words, &windows)
        .await
        .map_err(|e| AsrError::Store {
            reason: e.to_string(),
        })?;

    report.words = words.len();
    Ok(report)
}

/// A stable id for a transcript, so re-transcribing the same object with the
/// same engine replaces the row instead of accumulating one per attempt.
///
/// The engine name is in the id, so switching from whisper.cpp to parakeet
/// produces a SECOND row rather than overwriting the first: a user's choice of
/// engine is a fact worth keeping, and the transcript table's UNIQUE constraint
/// is on `object_id`, which is why the row's own `object_id` column still points
/// at the object.
fn transcript_id(object_id: &str, engine: &dyn AsrEngine) -> String {
    format!("{object_id}#{}", engine.name())
}

fn window_row(chunk: &crate::asr::Chunk, ok: bool, failure: Option<String>) -> WindowRow {
    WindowRow {
        window_index: i32::try_from(chunk.index).unwrap_or(i32::MAX),
        start_ms: i32::try_from(chunk.start_ms).unwrap_or(i32::MAX),
        end_ms: i32::try_from(chunk.end_ms).unwrap_or(i32::MAX),
        ok,
        failure,
    }
}

/// The command the audio came through, recorded on the transcript.
///
/// So "the words are wrong" can be answered with "the audio was decoded at the
/// wrong rate" rather than by re-running the transcription and hoping.
fn audio_command() -> String {
    format!("ffmpeg ({}) -> s16le 16kHz mono", audio::ffmpeg_path())
}

/// Whether the engine can usefully be asked again.
///
/// An engine that cannot start, that has died, or whose output no longer parses
/// will not do better on the next window: a desynchronised pipe has lost the
/// ability to say which reply belongs to which request, and every later window
/// would be misattributed rather than merely missing.
///
/// A model error and a missing-audio error are the opposite case: a window of
/// near-silence can make a model unhappy, and the next window may be fine.
fn is_fatal(e: &AsrError) -> bool {
    matches!(
        e,
        AsrError::Spawn { .. }
            | AsrError::Parse { .. }
            | AsrError::Failed { .. }
            | AsrError::NoModel { .. }
    )
}

/// Extract from a file, then transcribe and store.
///
/// Both halves in one call because every caller wants both, and a caller that
/// forgets the extract gets a transcript of silence rather than an error.
pub async fn transcribe_file(
    store: &Store,
    object_id: &str,
    input: &std::path::Path,
    engine: &Arc<dyn AsrEngine>,
    language: Option<&str>,
) -> Result<RunReport, AsrError> {
    // `audio::extract` is blocking -- it drives ffmpeg and reads its output --
    // so it runs on the blocking pool. Calling it on an async worker would park
    // a core for the length of a two-hour decode.
    let input = input.to_path_buf();
    let pcm = tokio::task::spawn_blocking(move || audio::extract(&input))
        .await
        .map_err(|e| AsrError::Audio {
            reason: format!("the decode task did not finish: {e}"),
        })?
        .map_err(|e| AsrError::Audio {
            reason: e.to_string(),
        })?;
    transcribe_and_store(store, object_id, &pcm, engine.as_ref(), language).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::Chunk;

    /// A fixed digest, so a provenance assertion reads as a value.
    const DIGEST: &str = "abababababababababababababababababababababababababababababababab";

    /// An engine that says one word per window, at a fixed offset within it.
    struct OneWord {
        /// Offset within the window, so the driver's arithmetic is visible.
        at_ms: u32,
        /// Windows to fail recoverably.
        fail_on: Vec<u32>,
        /// The window on which to fail fatally.
        fatal_on: Option<u32>,
    }

    impl AsrEngine for OneWord {
        fn name(&self) -> &'static str {
            "one-word"
        }
        fn model_id(&self) -> &str {
            "test"
        }
        fn model_sha256(&self) -> &str {
            DIGEST
        }
        fn transcribe_chunk(
            &self,
            chunk: &Chunk,
            sink: &mut dyn FnMut(TimedWord),
        ) -> Result<(), AsrError> {
            if self.fatal_on == Some(chunk.index) {
                return Err(AsrError::Spawn {
                    engine: "one-word",
                    reason: "the sidecar exited".into(),
                });
            }
            if self.fail_on.contains(&chunk.index) {
                return Err(AsrError::Model(format!(
                    "window {} is unusable",
                    chunk.index
                )));
            }
            sink(TimedWord::plain("word", self.at_ms, self.at_ms + 100));
            Ok(())
        }
    }

    /// A chunk as the chunker would make it, for the offset test.
    fn chunk_at(index: u32, start_ms: u64) -> Chunk {
        Chunk {
            index,
            start_ms,
            end_ms: start_ms + 30_000,
            samples: vec![0u8; 100],
        }
    }

    #[test]
    fn a_missing_offset_lands_every_word_in_the_first_window() {
        // The property the whole module exists for. If the offset arithmetic is
        // dropped, every word in a three-window transcript reports the same
        // times: the transcript renders, and every chapter after the first is
        // wrong. Nothing else catches it.
        let e = OneWord {
            at_ms: 1_000,
            fail_on: vec![],
            fatal_on: None,
        };
        let chunks: Vec<Chunk> = (0..3).map(|i| chunk_at(i, i as u64 * 30_000)).collect();
        let mut all = Vec::new();
        for (n, c) in chunks.iter().enumerate() {
            let mut got = Vec::new();
            e.transcribe_chunk(c, &mut |w| got.push(w)).unwrap();
            for w in got {
                let (start, _) = Chunker::absolutise(c, w.start_ms, w.end_ms).unwrap();
                all.push(start);
            }
            assert_eq!(all[n], n as u32 * 30_000 + 1_000, "window {n}");
        }
        assert_eq!(
            all,
            vec![1_000, 31_000, 61_000],
            "one word per window, spread out"
        );
    }

    #[test]
    fn a_report_says_which_window_failed_not_just_that_something_did() {
        // "something failed" is not actionable; naming the window is.
        let r = RunReport {
            words: 40,
            windows_ok: 39,
            failed: vec![(11, "out of memory".into()), (12, "out of memory".into())],
            duration_ms: 1_800_000,
            ..Default::default()
        };
        assert!(!r.complete());
        let s = r.summary();
        assert!(s.contains("window 11"), "{s}");
        assert!(s.contains("out of memory"), "{s}");
    }

    #[test]
    fn a_clean_run_is_complete_and_does_not_say_failed() {
        let r = RunReport {
            words: 3,
            windows_ok: 3,
            duration_ms: 90_000,
            ..Default::default()
        };
        assert!(r.complete());
        assert!(!r.summary().contains("failed"), "{}", r.summary());
    }

    #[test]
    fn a_run_with_skipped_windows_says_so_rather_than_claiming_success() {
        // A run whose every window was "too short" produced no transcript. The
        // summary must not read like a clean one.
        let r = RunReport {
            windows_skipped: 4,
            duration_ms: 40,
            ..Default::default()
        };
        assert!(r.complete(), "nothing failed");
        assert!(r.summary().contains("too short"), "{}", r.summary());
    }

    #[test]
    fn a_model_error_is_recoverable_and_a_dead_engine_is_not() {
        // The distinction that decides whether a run keeps its other windows. A
        // model error on one window of silence is that window's problem; a
        // sidecar that exited is the run's.
        assert!(!is_fatal(&AsrError::Model("bad window".into())));
        assert!(!is_fatal(&AsrError::NoAudio));
        for fatal in [
            AsrError::Spawn {
                engine: "x",
                reason: "exited".into(),
            },
            AsrError::Parse {
                engine: "x",
                reason: "garbage".into(),
            },
            AsrError::Failed {
                engine: "x",
                code: 1,
                stderr: String::new(),
            },
        ] {
            assert!(is_fatal(&fatal), "{fatal} must stop the run");
        }
    }

    #[test]
    fn a_transcript_id_names_the_engine_so_switching_engines_keeps_both() {
        // Overwriting would destroy a user's earlier choice and make the second
        // transcript unreproducible against the first.
        struct Named(&'static str);
        impl AsrEngine for Named {
            fn name(&self) -> &'static str {
                self.0
            }
            fn model_id(&self) -> &str {
                "m"
            }
            fn model_sha256(&self) -> &str {
                DIGEST
            }
            fn transcribe_chunk(
                &self,
                _chunk: &Chunk,
                _sink: &mut dyn FnMut(TimedWord),
            ) -> Result<(), AsrError> {
                Ok(())
            }
        }
        let w = transcript_id("obj-1", &Named("whisper.cpp"));
        let p = transcript_id("obj-1", &Named("parakeet"));
        assert_ne!(w, p, "a different engine is a different transcript");
        assert_eq!(w, transcript_id("obj-1", &Named("whisper.cpp")), "stable");
        assert!(w.starts_with("obj-1"), "traceable to its object: {w}");
    }
}
