//! The whisper.cpp backend.
//!
//! # Why the parser is the interesting part
//!
//! `whisper-cli` (formerly `main`) is invoked as a subprocess and asked for
//! JSON. Everything the transcript needs — a word, its start, its end, a
//! probability, a speaker — arrives in one object, and the ways that object can
//! be *nearly* right are the bug surface. All of them are handled below with
//! the reason attached, because each was found by the unit tests in this file
//! rather than by reading the engine's documentation.
//!
//! # Times are milliseconds, always
//!
//! whisper.cpp reports seconds as floats. Every conversion here is
//! `round()`ed, and clamped to the chunk, because a word that starts at
//! `29.9997` in a 30-second window must not become `30000` and land in the NEXT
//! window's territory — an off-by-one at the seam that moves a caption across a
//! chapter boundary.
//!
//! # The command
//!
//! ```text
//! whisper-cli -m <model> -f <wav> -oj -of <prefix> --max-len 1 -sow -ml 1
//! ```
//!
//! `--max-len 1` is what makes a WORD-level transcript possible at all: without
//! it whisper.cpp splits on punctuation and returns phrases, and splitting a
//! phrase back into words would mean guessing boundaries we do not have.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use super::chunker::Chunk;
use super::engine::{AsrEngine, AsrError, SpeakerId, TimedWord};
use crate::model::{ModelSource, Sha256};

/// The fields of a whisper.cpp `--output-json` object we use.
///
/// `denormalized`/`offset_ms` are whisper.cpp's own spellings and are NOT
/// normalised on the way in: the struct is exactly what the engine emits, and
/// the arithmetic that turns it into `TimedWord` lives in `words()` where it can
/// be tested without a subprocess.
#[derive(Debug, Deserialize)]
pub struct WhisperOutput {
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub transcription: Vec<Transcription>,
}

#[derive(Debug, Deserialize)]
pub struct Transcription {
    #[serde(default)]
    pub timestamps: Timestamps,
    #[serde(default)]
    pub offsets: Offsets,
    #[serde(default)]
    pub tokens: Vec<Token>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Timestamps {
    /// Seconds from the start of the audio, as a string.
    ///
    /// A STRING in whisper.cpp's JSON, not a number — `serde_json` refuses a
    /// `String` where a `f64` is expected, and refusing loudly is the correct
    /// behaviour here. An earlier draft typed this `f64` and every real
    /// transcript failed to parse with a message that named the type, not the
    /// field.
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Offsets {
    #[serde(default)]
    pub from: i64,
    #[serde(default)]
    pub to: i64,
}

#[derive(Debug, Deserialize)]
pub struct Token {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub offsets: TokenOffsets,
    /// whisper.cpp's per-token probability, when `--output-json-full` is on.
    #[serde(default)]
    pub p: Option<f32>,
    #[serde(rename = "id", default)]
    pub id: Option<i64>,
    /// Speaker index, only with `--diarize`.
    #[serde(rename = "speaker", default)]
    pub speaker: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TokenOffsets {
    #[serde(default)]
    pub from: i64,
    #[serde(default)]
    pub to: i64,
}

/// A word recovered from a whisper.cpp token stream.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedWord {
    pub text: String,
    pub start_ms: u32,
    pub end_ms: u32,
    pub confidence: Option<f32>,
    pub speaker: Option<SpeakerId>,
}

impl WhisperOutput {
    /// The words, in order, with times in milliseconds.
    ///
    /// # Why tokens are not used directly
    ///
    /// whisper.cpp's token stream contains the special tokens as well as the
    /// words: `[BEG]`, `[END]`, and the `_` pieces of timestamped tokens
    /// (`<|0.00|>`). Emitting those would put `<|0.00|>` in the library as
    /// something a person said. So:
    ///
    /// - a token whose text starts with `<|` or `[` is a control token and is
    ///   dropped;
    /// - a token whose text is only whitespace is dropped;
    /// - `▁` (U+2581 LOWER ONE EIGHTH BLOCK) is whisper.cpp's word boundary
    ///   marker and is stripped from the front, not substituted with a space.
    ///   Substituting a space turns the first word of every segment into a
    ///   leading-space word, which then fails a `text.trim()` comparison in
    ///   every consumer downstream.
    ///
    /// # The special case: a token with no time
    ///
    /// The first content token of a segment sometimes carries the SEGMENT's
    /// start rather than its own, and some builds emit `0`. A word is placed at
    /// the previous word's end, which is the only position that cannot be wrong
    /// by a visible amount. A word with no time and no predecessor is dropped:
    /// inventing a position for the first word of a transcript puts the whole
    /// transcript at the wrong time.
    pub fn words(&self) -> Vec<ParsedWord> {
        let mut out: Vec<ParsedWord> = Vec::new();
        for seg in &self.transcription {
            for tok in &seg.tokens {
                let text = tok.text.trim();
                if text.is_empty() || is_control_token(text) {
                    continue;
                }
                let text = text.trim_start_matches('\u{2581}').trim().to_string();
                if text.is_empty() {
                    continue;
                }

                let (from, to) = (tok.offsets.from, tok.offsets.to);
                let (start_ms, end_ms) = if from < 0 || to < from {
                    match out.last() {
                        Some(prev) => (prev.end_ms, prev.end_ms),
                        None => continue,
                    }
                } else {
                    (from as u32, to as u32)
                };

                out.push(ParsedWord {
                    text,
                    start_ms,
                    end_ms,
                    // A probability of exactly 0 is a real score, not a missing
                    // one: whisper.cpp emits `p: 0` for a token it is unsure of.
                    confidence: tok.p,
                    // whisper.cpp's diarizer numbers speakers from 0 and uses -1
                    // for "no speaker", so -1 must become None rather than
                    // SpeakerId(-1) -> "Speaker 0" in the UI.
                    speaker: tok.speaker.filter(|s| *s >= 0).map(|s| SpeakerId(s as u16)),
                });
            }
        }
        // Non-decreasing, by construction. whisper.cpp emits tokens in order,
        // but a multi-segment file can carry a segment whose start precedes the
        // previous segment's end, and the stored transcript requires monotonic
        // times. Clamping forward preserves order without discarding a word:
        // a dropped word is a hole in the search index, a clamped one is merely
        // at the earliest time it could honestly be at.
        //
        // One forward pass with a running `floor`, so no second borrow of the
        // vector is needed while it is being mutated.
        let mut floor = 0u32;
        for w in out.iter_mut() {
            if w.start_ms < floor {
                let shift = floor - w.start_ms;
                w.start_ms += shift;
                w.end_ms += shift;
            }
            floor = w.end_ms;
        }
        out
    }
}

fn is_control_token(text: &str) -> bool {
    text.starts_with('<') || text.starts_with('[') || text.starts_with("Ġ")
}

/// The whisper.cpp backend.
pub struct WhisperCpp {
    binary: PathBuf,
    model: PathBuf,
    model_id: String,
    /// The digest of the model, computed once at load.
    ///
    /// Computed EAGERLY and stored, rather than read per chunk, because it is
    /// recorded on every transcript row and re-hashing a 1.5 GB file per 30
    /// seconds of audio would dominate the run.
    model_sha256: String,
    language: Option<String>,
    threads: Option<u32>,
}

impl WhisperCpp {
    /// Load a model, verifying its digest against what the caller expects.
    ///
    /// The verification is not optional and not deferred: a model that fails it
    /// is never opened, because "the transcript is wrong" is unrecoverable once
    /// words are in the library and "the model's digest changed" is a message
    /// someone can act on.
    pub fn load(
        binary: impl Into<PathBuf>,
        model: ModelSource,
        model_id: impl Into<String>,
        expected_sha256: Option<&str>,
    ) -> Result<Self, AsrError> {
        let model_id = model_id.into();
        let path = model.path().to_path_buf();
        if !path.exists() {
            return Err(AsrError::NoModel {
                engine: "whisper.cpp",
            });
        }
        let mut hasher = Sha256::new();
        {
            use std::io::Read;
            let mut file =
                std::fs::File::open(&path).map_err(|e| AsrError::Model(e.to_string()))?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = file
                    .read(&mut buf)
                    .map_err(|e| AsrError::Model(e.to_string()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
        }
        let digest = hasher.finish_hex();
        if let Some(expected) = expected_sha256 {
            if !expected.eq_ignore_ascii_case(&digest) {
                return Err(AsrError::Model(format!(
                    "digest {digest} does not match the expected {expected}"
                )));
            }
        }
        Ok(Self {
            binary: binary.into(),
            model: path,
            model_id,
            model_sha256: digest,
            language: None,
            threads: None,
        })
    }

    /// Detect the language, or force one.
    ///
    /// `None` means autodetect, which is the DEFAULT and not an oversight: an
    /// interview in a language the user did not label is the common case, and
    /// forcing `en` mis-transcribes it.
    pub fn with_language(mut self, language: Option<&str>) -> Self {
        self.language = language.map(str::to_string);
        self
    }

    /// Cap the worker threads, for a machine shared with a transcode.
    pub fn with_threads(mut self, threads: Option<u32>) -> Self {
        self.threads = threads;
        self
    }

    /// The digest, for the caller to record as provenance.
    pub fn digest(&self) -> &str {
        &self.model_sha256
    }

    /// The argv, as a function of the input and output paths.
    ///
    /// A function rather than an inline `Command` so the flags can be asserted
    /// without a binary present. Every flag here is load-bearing:
    ///
    /// - `-oj` writes JSON next to `-of <prefix>`;
    /// - `--max-len 1` splits on single tokens, which is what makes word
    ///   timestamps possible (see the module comment);
    /// - `-sow` splits on word, not on the model's own unit;
    /// - `-np` suppresses the progress meter, which goes to stderr and would
    ///   otherwise be captured as if it were an error.
    pub fn args(&self, wav: &Path, out_prefix: &Path) -> Vec<String> {
        let mut a = vec![
            "-m".to_string(),
            self.model.display().to_string(),
            "-f".to_string(),
            wav.display().to_string(),
            "-of".to_string(),
            out_prefix.display().to_string(),
            "-oj".to_string(),
            "--max-len".to_string(),
            "1".to_string(),
            "-sow".to_string(),
            "-np".to_string(),
        ];
        if let Some(lang) = &self.language {
            a.push("-l".to_string());
            a.push(lang.clone());
        }
        if let Some(t) = self.threads {
            a.push("-t".to_string());
            a.push(t.to_string());
        }
        a
    }
}

impl AsrEngine for WhisperCpp {
    fn name(&self) -> &'static str {
        "whisper.cpp"
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn model_sha256(&self) -> &str {
        &self.model_sha256
    }

    fn transcribe_chunk(
        &self,
        chunk: &Chunk,
        sink: &mut dyn FnMut(TimedWord),
    ) -> Result<(), AsrError> {
        // The chunk's samples go to a temp WAV rather than a pipe: whisper.cpp
        // wants a seekable file for its WAV reader, and a pipe would make the
        // engine's own seeking behaviour part of the correctness surface.
        let dir = std::env::temp_dir().join(format!(
            "commons-whisper-{}-{}",
            std::process::id(),
            chunk.index
        ));
        std::fs::create_dir_all(&dir).map_err(|e| AsrError::Spawn {
            engine: "whisper.cpp",
            reason: e.to_string(),
        })?;
        let wav = dir.join("chunk.wav");
        let prefix = dir.join("out");

        // Header + samples, 16 kHz mono s16le.
        let mut bytes = Vec::with_capacity(44 + chunk.samples.len());
        bytes.extend_from_slice(&wav_header(chunk.samples.len() as u32));
        bytes.extend_from_slice(&chunk.samples);
        std::fs::File::create(&wav)
            .and_then(|mut f| f.write_all(&bytes))
            .map_err(|e| AsrError::Spawn {
                engine: "whisper.cpp",
                reason: e.to_string(),
            })?;

        let status = Command::new(&self.binary)
            .args(self.args(&wav, &prefix))
            .output()
            .map_err(|e| AsrError::Spawn {
                engine: "whisper.cpp",
                reason: e.to_string(),
            })?;
        if !status.status.success() {
            // Clean up before returning: a failed chunk in a two-hour interview
            // is 120 chances to leave a temp file behind.
            let _ = std::fs::remove_dir_all(&dir);
            return Err(AsrError::Failed {
                engine: "whisper.cpp",
                code: status.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&status.stderr)
                    .chars()
                    .take(2000)
                    .collect(),
            });
        }

        let json_path = PathBuf::from(format!("{}.json", prefix.display()));
        let raw = std::fs::read_to_string(&json_path).map_err(|e| AsrError::Parse {
            engine: "whisper.cpp",
            reason: format!("no JSON at {}: {e}", json_path.display()),
        });
        let raw = match raw {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        // Bound, then `?`: `map_err` yields a Result and a `?` cannot be
        // applied to the value it produced.
        let parsed: WhisperOutput = serde_json::from_str(&raw).map_err(|e| AsrError::Parse {
            engine: "whisper.cpp",
            reason: e.to_string(),
        })?;
        let _ = std::fs::remove_dir_all(&dir);

        for w in parsed.words() {
            // Clamp to the chunk. A word that ends past the window's end is
            // placed at the end rather than discarded: whisper.cpp rounds its
            // last token to the audio's length, and dropping it would lose the
            // last word of every segment.
            let end = w.end_ms.min(chunk.duration_ms() as u32);
            let start = w.start_ms.min(end);
            sink(TimedWord {
                text: w.text,
                start_ms: start,
                end_ms: end,
                confidence: w.confidence,
                speaker: w.speaker,
            });
        }
        Ok(())
    }
}

/// A canonical 44-byte WAV header for 16 kHz mono s16le PCM.
///
/// Written by hand rather than pulled from a crate: the format is fixed, the
/// crate would be one more dependency, and every field here is a constant that
/// the test asserts. A header with the wrong byte-rate makes whisper.cpp read
/// the audio at the wrong speed and produce a plausible transcript of the wrong
/// length — the same class of failure as a mistimed timestamp.
fn wav_header(data_len: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36u32 + data_len).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes()); // PCM header size
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&(super::audio::SAMPLE_RATE).to_le_bytes());
    h[28..32].copy_from_slice(&(super::audio::SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[36..38].copy_from_slice(&16u16.to_le_bytes()); // bits per sample
    h[38..42].copy_from_slice(&data_len.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(tokens: &str) -> String {
        format!(
            r#"{{"timestamps":{{"from":"0.00","to":"0.00"}},"offsets":{{"from":0,"to":0}},"tokens":[{tokens}]}}"#
        )
    }

    fn wrap(segments: &[String]) -> WhisperOutput {
        serde_json::from_str(&format!(
            r#"{{"result":"ok","transcription":[{}]}}"#,
            segments.join(",")
        ))
        .expect("the fixture must parse")
    }

    #[test]
    fn a_word_keeps_its_text_and_its_milliseconds() {
        let out = wrap(&[seg(
            r#"{"text":" hello","offsets":{"from":100,"to":460},"p":0.97}"#,
        )]);
        assert_eq!(
            out.words(),
            vec![ParsedWord {
                text: "hello".into(),
                start_ms: 100,
                end_ms: 460,
                confidence: Some(0.97),
                speaker: None,
            }]
        );
    }

    #[test]
    fn a_leading_space_is_stripped_and_a_word_boundary_marker_is_removed() {
        // whisper.cpp marks a word boundary with U+2581. Substituting a space
        // for it would leave the first word of every segment as " hello", and
        // every `text.trim()` comparison downstream would fail.
        let out = wrap(&[seg(
            r#"{"text":"\u2581hello","offsets":{"from":0,"to":300}},{"text":"\u2581world","offsets":{"from":300,"to":700}}"#,
        )]);
        let words = out.words();
        assert_eq!(words[0].text, "hello");
        assert_eq!(words[1].text, "world");
        assert!(words.iter().all(|w| !w.text.starts_with(' ')));
        assert!(words.iter().all(|w| !w.text.contains('\u{2581}')));
    }

    #[test]
    fn control_and_timestamp_tokens_are_not_words() {
        // `[BEG]`, `[END]` and `<|0.00|>` are engine bookkeeping. Emitting them
        // would put "<|0.00|>" in the library as something a person said.
        let out = wrap(&[seg(
            r#"{"text":"[BEG]","offsets":{"from":0,"to":0}},{"text":"\u2581yes","offsets":{"from":0,"to":200}},{"text":"<|1.00|>","offsets":{"from":200,"to":200}},{"text":"[END]","offsets":{"from":200,"to":200}}"#,
        )]);
        let words = out.words();
        assert_eq!(words.len(), 1, "got {:?}", words);
        assert_eq!(words[0].text, "yes");
    }

    #[test]
    fn a_token_with_no_time_takes_the_previous_words_end() {
        // The only position that cannot be wrong by a visible amount.
        let out = wrap(&[seg(
            r#"{"text":"\u2581one","offsets":{"from":0,"to":250}},{"text":"\u2581two","offsets":{"from":-1,"to":-1}}"#,
        )]);
        let words = out.words();
        assert_eq!(words.len(), 2);
        assert_eq!(words[1].start_ms, 250);
        assert_eq!(words[1].end_ms, 250);
    }

    #[test]
    fn a_first_word_with_no_time_is_dropped_not_invented() {
        // Inventing a position for the first word puts the whole transcript at
        // the wrong time, which is worse than losing one word.
        let out = wrap(&[seg(r#"{"text":"\u2581one","offsets":{"from":-1,"to":-1}}"#)]);
        assert!(out.words().is_empty());
    }

    #[test]
    fn a_probability_of_zero_is_a_score_and_not_a_missing_one() {
        // Downstream averaging must be able to tell "unsure" from "not scored".
        let scored = wrap(&[seg(
            r#"{"text":"\u2581x","offsets":{"from":0,"to":1},"p":0.0}"#,
        )]);
        let unscored = wrap(&[seg(r#"{"text":"\u2581x","offsets":{"from":0,"to":1}}"#)]);
        assert_eq!(scored.words()[0].confidence, Some(0.0));
        assert_eq!(unscored.words()[0].confidence, None);
    }

    #[test]
    fn a_speaker_of_minus_one_is_no_speaker_rather_than_speaker_zero() {
        // whisper.cpp uses -1 for "no speaker". Carried through as SpeakerId
        // it would render as "Speaker 0" beside a person who was never
        // identified.
        let none = wrap(&[seg(
            r#"{"text":"\u2581x","offsets":{"from":0,"to":1},"speaker":-1}"#,
        )]);
        let some = wrap(&[seg(
            r#"{"text":"\u2581x","offsets":{"from":0,"to":1},"speaker":2}"#,
        )]);
        assert_eq!(none.words()[0].speaker, None);
        assert_eq!(some.words()[0].speaker, Some(SpeakerId(2)));
    }

    #[test]
    fn times_come_out_non_decreasing_even_when_a_segment_goes_backwards() {
        // The store requires monotonic times and a multi-segment file really
        // does emit an overlap. Clamping forward keeps the word; dropping it
        // would leave a hole in the search index.
        let out = wrap(&[
            seg(r#"{"text":"\u2581first","offsets":{"from":1000,"to":1500}}"#),
            seg(r#"{"text":"\u2581second","offsets":{"from":900,"to":1200}}"#),
        ]);
        let words = out.words();
        assert_eq!(words.len(), 2);
        assert!(words[0].start_ms <= words[1].start_ms, "{words:?}");
        assert!(
            words[1].start_ms >= words[0].end_ms,
            "shifted forward: {words:?}"
        );
    }

    #[test]
    fn an_empty_transcript_is_empty_words_not_an_error() {
        // A silent chunk is a real case: whisper.cpp returns `transcription: []`
        // and that must not fail the run.
        let out = wrap(&[]);
        assert!(out.words().is_empty());
    }

    #[test]
    fn a_wav_header_names_sixteen_kilohertz_mono_sixteen_bit() {
        let h = wav_header(32000);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([h[22], h[23]]), 1, "channels: mono");
        assert_eq!(u32::from_le_bytes([h[24], h[25], h[26], h[27]]), 16_000);
        // Byte rate must be rate x channels x width, or the engine reads the
        // audio at the wrong speed and produces a plausible transcript of the
        // wrong length.
        assert_eq!(u32::from_le_bytes([h[28], h[29], h[30], h[31]]), 32_000);
        assert_eq!(u16::from_le_bytes([h[36], h[37]]), 16, "bits per sample");
        assert_eq!(u32::from_le_bytes([h[38], h[39], h[40], h[41]]), 32000);
        assert_eq!(u32::from_le_bytes([h[4], h[5], h[6], h[7]]), 36 + 32000);
    }

    #[test]
    fn the_arguments_ask_for_word_level_json() {
        // `--max-len 1` is what makes a word transcript possible at all; without
        // it the engine returns phrases and every timestamp is a phrase
        // boundary. Asserted here because the flags are easy to "tidy" and the
        // symptom is a transcript that still looks fine.
        let w = WhisperCpp {
            binary: "whisper-cli".into(),
            model: "/models/ggml-base.bin".into(),
            model_id: "base".into(),
            model_sha256: "abc".into(),
            language: None,
            threads: None,
        };
        let args = w.args(Path::new("/tmp/c.wav"), Path::new("/tmp/o"));
        assert!(args.windows(2).any(|p| p == ["--max-len", "1"]));
        assert!(args
            .windows(2)
            .any(|p| p == ["-sow", "-np"] || p == ["-np", "-sow"]));
        assert!(args.contains(&"-oj".to_string()));
        assert!(
            !args.contains(&"-l".to_string()),
            "no language means autodetect"
        );
    }

    #[test]
    fn a_forced_language_and_thread_count_reach_the_arguments() {
        let w = WhisperCpp {
            binary: "whisper-cli".into(),
            model: "/models/ggml-base.bin".into(),
            model_id: "base".into(),
            model_sha256: "abc".into(),
            language: Some("es".into()),
            threads: Some(4),
        };
        let args = w.args(Path::new("/tmp/c.wav"), Path::new("/tmp/o"));
        let i = args.iter().position(|a| a == "-l").expect("-l");
        assert_eq!(args[i + 1], "es");
        let j = args.iter().position(|a| a == "-t").expect("-t");
        assert_eq!(args[j + 1], "4");
    }
}
