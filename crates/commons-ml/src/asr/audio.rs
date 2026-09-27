//! 16 kHz mono PCM, and the ffmpeg invocation that produces it.
//!
//! T-P6-004, spec §3.3 in `docs/spec/t-p6-004-interviews.md`.
//!
//! # Why the audio is never written to a file
//!
//! A two-hour interview is 230 MB at 16 kHz mono s16le. Writing that to a
//! temporary file means a disk-full crash on a machine that is trying to keep a
//! library on a small SSD, and it leaves a plaintext copy of someone's voice in
//! `/tmp` after the process exits — on a shared machine, under a name that
//! suggests it is safe to delete. So the child process is piped and read in
//! chunks, and at no point does the audio exist anywhere but in memory and in
//! the pipe.
//!
//! # Why the format is not negotiable
//!
//! `-ac 1 -ar 16000` is what every ASR model in this space expects. Resampling
//! inside the model wrapper instead would mean a second resampler, with its own
//! quality bugs, disagreeing with ffmpeg's — and the disagreement would show up
//! as timing drift, which is the one thing §5.1 of the spec says must not happen.
//!
//! # Why `-map 0:a:0` and not `-map 0:a`
//!
//! An interview with three audio tracks would otherwise hand the ASR three
//! files' worth of audio and produce a transcript of whichever stream ffmpeg
//! picked. `:0` is the first audio stream, which is the convention a container
//! author means.

use std::path::Path;
use std::process::{Command, Stdio};

/// Sample rate every ASR engine here expects, in Hz.
pub const SAMPLE_RATE: u32 = 16_000;

/// One second of 16 kHz mono s16le, in bytes. 16000 samples × 2 bytes.
pub const BYTES_PER_SECOND: usize = (SAMPLE_RATE as usize) * 2;

/// The largest single read from the pipe.
///
/// Not a tuning knob: it is the buffer size, and 32 KiB is 1 second of audio, so
/// the reader can hand whole seconds to the chunker without re-buffering and a
/// 2-hour interview never holds more than a second of audio in the read
/// buffer.
pub const READ_CHUNK: usize = 32 * 1024;

/// 16 kHz mono signed 16-bit little-endian PCM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pcm16kMono {
    /// The samples, little-endian as they came off the pipe.
    bytes: Vec<u8>,
}

impl Pcm16kMono {
    /// Wrap raw little-endian s16le bytes.
    ///
    /// Takes the bytes rather than a `Vec<i16>` because the pipe delivers
    /// bytes and converting would mean a copy of 230 MB per interview for no
    /// gain — the only reader is the chunker, which wants bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// The raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// How many bytes of audio this is.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether there is no audio at all.
    ///
    /// Distinguishes itself from "audio exists" because a media file with a
    /// silent track and a media file with no audio track take different paths:
    /// the first transcribes to nothing (a legitimate, if boring, result) and
    /// the second is a probe error.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Duration in milliseconds.
    ///
    /// Computed from the byte count rather than tracked, so it cannot drift
    /// out of step with the samples the way a separately-maintained counter
    /// would. Truncating division is the right rounding here: a duration
    /// rounded *up* would place the last chunk past the end of the media, and
    /// an ASR engine handed a window past the end returns a hallucination.
    pub fn duration_ms(&self) -> u64 {
        ((self.bytes.len() / 2) as u64 * 1000) / (SAMPLE_RATE as u64)
    }

    /// The half-open byte range covering `[start_ms, end_ms)`.
    ///
    /// Clamped to the audio rather than allowed to index past it, because the
    /// chunker's last chunk is *expected* to run off the end and a panic there
    /// would be a crash on the most common input in the library.
    pub fn slice_ms(&self, start_ms: u64, end_ms: u64) -> &[u8] {
        let start = self.ms_to_byte(start_ms);
        let end = self.ms_to_byte(end_ms).min(self.bytes.len());
        if start >= end {
            return &[];
        }
        &self.bytes[start..end]
    }

    /// The byte offset of `ms`, clamped to the end.
    fn ms_to_byte(&self, ms: u64) -> usize {
        let sample = (ms * (SAMPLE_RATE as u64)) / 1000;
        (sample as usize).saturating_mul(2).min(self.bytes.len())
    }

    /// The number of `READ_CHUNK`-sized reads this will take.
    ///
    /// Exposed so a caller can size progress reporting without guessing, and so
    /// a test can assert the read loop terminates rather than spinning.
    pub fn read_count(&self) -> usize {
        self.bytes.len().div_ceil(READ_CHUNK)
    }
}

/// Why an audio extraction failed.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// ffmpeg is not where we were told it is.
    #[error("ffmpeg not available at {path}: {reason}")]
    NoFfmpeg { path: String, reason: String },

    /// The source has no audio to extract.
    ///
    /// A separate variant from a generic failure because the caller's response
    /// is different: this is a video, not a bug, and a scan should record "no
    /// audio" and move on rather than failing the object.
    #[error("{path} has no audio stream")]
    NoAudio { path: String },

    /// ffmpeg ran and failed.
    #[error("ffmpeg failed for {path} (exit {code}): {stderr}")]
    FfmpegFailed {
        path: String,
        code: i32,
        stderr: String,
    },

    /// The extracted bytes are not a whole number of samples.
    ///
    /// Odd-length s16le means the pipe was cut mid-sample, which is a real
    /// failure mode of a killed ffmpeg and not a theoretical one. Detected here
    /// because a silently dropped half-sample is a 1-sample shift in every
    /// timestamp after it.
    #[error("extracted {len} bytes, which is not a whole number of s16le samples")]
    RaggedSamples { len: usize },
}

/// Where ffmpeg is.
///
/// Reads `COMMONS_FFMPEG` for the same reason the media tests do: a test must
/// not depend on what is on `PATH` when the developer's `PATH` and CI's differ.
pub fn ffmpeg_path() -> String {
    std::env::var("COMMONS_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string())
}

/// The exact argument vector, exposed so it can be stored and reproduced.
///
/// Stored verbatim in `interview_transcripts.audio_command` (spec §1: a
/// transcript is only as good as the ability to reproduce it), so the function
/// takes no ambient state — a transcript recorded against one ffmpeg build is
/// not reproducible if the command is assembled somewhere else.
pub fn extract_args(input: &Path) -> Vec<String> {
    vec![
        "-nostdin".into(),
        "-v".into(),
        "error".into(),
        "-i".into(),
        input.display().to_string(),
        // The first audio stream. A file with three of them should produce one
        // interview, not a transcript of whichever stream ffmpeg chose.
        "-map".into(),
        "0:a:0".into(),
        // Video is not speech, and decoding it is most of the cost.
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        SAMPLE_RATE.to_string(),
        "-f".into(),
        "s16le".into(),
        "-".into(),
    ]
}

/// Run ffmpeg and stream the audio back.
///
/// `on_chunk` is called with each read as it arrives, so a caller that only
/// wants to transcribe can work chunk by chunk instead of accumulating 230 MB.
/// The reads are also returned, because the tests want the bytes and a caller
/// wants the callback and making one of them a wrapper around the other is a
/// parameter nobody sets honestly.
pub fn extract_streaming<F>(input: &Path, mut on_chunk: F) -> Result<Vec<u8>, AudioError>
where
    F: FnMut(&[u8]),
{
    use std::io::Read;

    let bin = ffmpeg_path();
    let mut child = Command::new(&bin)
        .args(extract_args(input))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AudioError::NoFfmpeg {
            path: bin.clone(),
            reason: e.to_string(),
        })?;

    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut out = Vec::new();
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                // Read the status anyway: ffmpeg has almost always exited by
                // now, and its stderr is the only thing that says why.
                let _ = child.wait();
                return Err(AudioError::NoFfmpeg {
                    path: bin,
                    reason: format!("reading audio: {e}"),
                });
            }
        };
        on_chunk(&buf[..n]);
        out.extend_from_slice(&buf[..n]);
    }

    let status = child.wait().map_err(|e| AudioError::NoFfmpeg {
        path: bin.clone(),
        reason: e.to_string(),
    })?;
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        return Err(AudioError::FfmpegFailed {
            path: input.display().to_string(),
            code: status.code().unwrap_or(-1),
            // `-v error` keeps this short, but not bounded. Truncated so a
            // pathological file cannot put a megabyte of ffmpeg output into an
            // error message that ends up in a log line.
            stderr: stderr.chars().take(500).collect(),
        });
    }

    if out.is_empty() {
        return Err(AudioError::NoAudio {
            path: input.display().to_string(),
        });
    }
    if !out.len().is_multiple_of(2) {
        return Err(AudioError::RaggedSamples { len: out.len() });
    }
    Ok(out)
}

/// Extract the whole clip. Convenient, and the right thing for the 30-second
/// fixtures the tests use.
pub fn extract(input: &Path) -> Result<Pcm16kMono, AudioError> {
    extract_streaming(input, |_| {}).map(Pcm16kMono::from_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(seconds: u64) -> Pcm16kMono {
        Pcm16kMono::from_bytes(vec![0u8; (seconds as usize) * BYTES_PER_SECOND])
    }

    #[test]
    fn duration_is_derived_from_the_bytes() {
        // 30 s of audio is 960_000 bytes, and must not come back as 29999 or
        // 30001: the timestamp maths downstream multiplies this by 1000.
        assert_eq!(pcm(30).duration_ms(), 30_000);
        assert_eq!(pcm(1).duration_ms(), 1_000);
        assert_eq!(Pcm16kMono::default().duration_ms(), 0);
    }

    #[test]
    fn a_half_sample_does_not_round_the_duration_up() {
        // Rounding up would place the final chunk past the end of the media, and
        // an engine handed a window past the end returns a hallucination.
        let odd = Pcm16kMono::from_bytes(vec![0u8; BYTES_PER_SECOND - 1]);
        assert_eq!(odd.duration_ms(), 999);
    }

    #[test]
    fn a_slice_lands_where_the_arithmetic_says() {
        let a = pcm(10);
        // 16 kHz is 16000 samples/s, so 1 s is byte 32_000. Getting this wrong
        // by a factor of two would halve every timestamp in the library.
        assert_eq!(a.slice_ms(0, 1_000).len(), 32_000);
        assert_eq!(a.slice_ms(5_000, 6_000).len(), 32_000);
        assert_eq!(a.slice_ms(9_000, 10_000).len(), 32_000);
    }

    #[test]
    fn a_slice_past_the_end_is_clamped_not_panicked() {
        // The chunker's last chunk is *expected* to run off the end. A panic
        // here would be a crash on the most common input in the library.
        let a = pcm(10);
        // 9s..end is 1s of audio = 32_000 bytes, and the request for 11s is
        // clamped to 10s rather than indexing past the end.
        assert_eq!(a.slice_ms(9_000, 11_000).len(), 32_000);
        // A window entirely past the end is empty, not a panic. 11s..12s is
        // past the end of a 10s clip, and 50ms..60ms is not -- it is a 10ms
        // window near the start, which is 320 bytes and correct.
        assert!(a.slice_ms(11_000, 12_000).is_empty());
        assert_eq!(a.slice_ms(50, 60).len(), 320);
    }

    #[test]
    fn an_inverted_range_is_empty_rather_than_panicking() {
        let a = pcm(10);
        assert!(a.slice_ms(5_000, 4_000).is_empty());
        assert!(a.slice_ms(5_000, 5_000).is_empty());
    }

    #[test]
    fn the_command_is_the_one_the_spec_pins() {
        // This vector is stored in the transcript row, so a change here is a
        // change to what every past transcript can be reproduced with.
        let args = extract_args(Path::new("/tmp/in.mkv"));
        assert_eq!(args[0], "-nostdin");
        assert!(args.windows(2).any(|w| w == ["-map", "0:a:0"]));
        assert!(args.windows(2).any(|w| w == ["-ac", "1"]));
        assert!(args.windows(2).any(|w| w == ["-ar", "16000"]));
        assert!(args.windows(2).any(|w| w == ["-f", "s16le"]));
        assert_eq!(args.last().unwrap(), "-", "must write to the pipe");
        assert!(args.contains(&"-vn".to_string()), "-vn must be set");
    }

    #[test]
    fn read_count_never_reports_zero_for_real_audio() {
        // A caller sizes a progress bar from this. Reporting 0 reads for
        // 100 bytes of audio would spin a bar backwards.
        assert_eq!(pcm(1).read_count(), 1);
        assert_eq!(Pcm16kMono::default().read_count(), 0);
        let two_chunks = Pcm16kMono::from_bytes(vec![0u8; READ_CHUNK + 1]);
        assert_eq!(two_chunks.read_count(), 2);
    }
}
