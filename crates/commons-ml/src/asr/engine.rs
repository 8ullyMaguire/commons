//! The engine seam: what every ASR backend must provide.
//!
//! T-P6-004, spec §3.1.
//!
//! # The units are fixed here, once
//!
//! `TimedWord` is milliseconds, `u32`, absolute from the start of the *media*.
//! whisper.cpp reports milliseconds relative to the chunk it was given; an ONNX
//! model typically reports frames and needs the sample rate to convert. Both
//! conversions happen in the adapter, and neither the storage layer nor the UI
//! ever sees a frame index or a chunk-relative time.

/// Who said a word, as the engine numbered them.
///
/// A small integer scoped to one transcript, and deliberately not a
/// `PersonId`: a `SPEAKER_00` in one interview is not known to be the same
/// person as a `SPEAKER_00` in another, and the join to a `PersonCluster` is a
/// proposal (§8.1) rather than a fact. Making this a `PersonId` would bake that
/// guess into the type and there would be no way to express the uncertainty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpeakerId(pub u16);

impl SpeakerId {
    /// The conventional label, for display. `Speaker 1` is one-based because it
    /// is a person-facing label and people count from one.
    pub fn label(&self) -> String {
        format!("Speaker {}", self.0 as usize + 1)
    }
}

impl std::fmt::Display for SpeakerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}
/// One recognised word, timed absolutely.
///
/// `text` is the surface form as recognised, with no case folding and no
/// punctuation repair: the store normalises for search in a derived column
/// instead, because a transcript that has been silently corrected is a
/// transcript nobody can audit against the audio.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    /// Milliseconds from the start of the media. Absolute, not chunk-relative.
    pub start_ms: u32,
    /// Milliseconds from the start of the media. Never before `start_ms`.
    pub end_ms: u32,
    /// The engine's own confidence, 0.0..=1.0.
    ///
    /// `None` means *this engine does not score words*, which is a different
    /// fact from "scored every word 0.5". Downstream code that averages
    /// confidence must treat `None` as absent rather than as a zero — a zero
    /// would make every word from a non-scoring engine rank last, which is a
    /// claim about the words rather than about the engine.
    pub confidence: Option<f32>,
    /// The speaker, when this engine diarises.
    pub speaker: Option<SpeakerId>,
}

impl TimedWord {
    /// A word with no score and no speaker, for engines that provide neither.
    pub fn plain(text: impl Into<String>, start_ms: u32, end_ms: u32) -> Self {
        Self {
            text: text.into(),
            start_ms,
            end_ms,
            confidence: None,
            speaker: None,
        }
    }
}

/// A whole transcript: words in order, plus what the engine can say about
/// itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transcript {
    pub words: Vec<TimedWord>,
    /// BCP-47-ish tag when the engine detected one, `None` when it did not.
    pub language: Option<String>,
}

/// Why a transcription failed.
///
/// Split by cause for the same reason `ModelError` is: a missing model is a
/// feature the user has not turned on, a checksum failure is a corrupted
/// download or an attack, and a subprocess that died is a crash to report. One
/// `Err(String)` loses exactly the information that decides what to do.
#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    /// No model, or none configured.
    #[error("no ASR model configured for engine {engine}")]
    NoModel { engine: &'static str },

    /// The model file failed verification. See `commons_ml::model::ModelError`.
    #[error("ASR model failed verification: {0}")]
    Model(String),

    /// The engine process could not be started.
    #[error("could not start {engine}: {reason}")]
    Spawn {
        engine: &'static str,
        reason: String,
    },

    /// The engine process ran and failed.
    #[error("{engine} failed (exit {code}): {stderr}")]
    Failed {
        engine: &'static str,
        code: i32,
        stderr: String,
    },

    /// The engine's output could not be parsed.
    #[error("could not parse {engine} output: {reason}")]
    Parse {
        engine: &'static str,
        reason: String,
    },

    /// No audio to transcribe.
    #[error("no audio to transcribe")]
    NoAudio,
}

/// What every backend implements.
///
/// `transcribe` takes one chunk and pushes words into a sink rather than
/// returning them, because the sink is where the absolute timestamps are
/// applied and validated — a backend cannot get them wrong if it never gets to
/// choose them.
pub trait AsrEngine: Send + Sync {
    /// The engine's name, as recorded in `interview_transcripts.engine`.
    fn name(&self) -> &'static str;

    /// The model identifier, as recorded alongside the transcript.
    fn model_id(&self) -> &str;

    /// The digest of the model that is loaded *right now*, so a caller can
    /// store provenance without re-reading the manifest.
    fn model_sha256(&self) -> &str;

    /// Transcribe one chunk, pushing words with times **relative to the chunk
    /// start**. The caller adds the chunk offset and enforces monotonicity.
    ///
    /// Relative in, absolute out: that is the whole contract, and it is why a
    /// backend cannot be the place a timestamp bug lives.
    fn transcribe_chunk(
        &self,
        chunk: &super::chunker::Chunk,
        sink: &mut dyn FnMut(TimedWord),
    ) -> Result<(), AsrError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_labels_count_from_one() {
        // One-based because it is a person-facing label.
        assert_eq!(SpeakerId(0).label(), "Speaker 1");
        assert_eq!(SpeakerId(3).label(), "Speaker 4");
        assert_eq!(SpeakerId(0).to_string(), "Speaker 1");
    }

    #[test]
    fn a_plain_word_has_no_score_and_no_speaker() {
        // Explicitly None, not Some(0.0): a non-scoring engine is not a
        // confident-zero engine, and downstream averaging must be able to tell.
        let w = TimedWord::plain("hello", 0, 400);
        assert_eq!(w.confidence, None);
        assert_eq!(w.speaker, None);
    }
}
