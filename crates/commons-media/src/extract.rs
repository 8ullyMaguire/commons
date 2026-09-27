//! Getting subtitle bytes out of a container: the ffmpeg half.
//!
//! T-P6-002 step 4. Spec `docs/spec/t-p6-002-subtitles.md` §4.
//!
//! # A separate file, and the reason is in `subtitles.rs`'s own header
//!
//! That file says it has no ffmpeg in it, and means it: every function there
//! takes a `&str` and none spawns a process, so a parser bug and an extractor
//! bug stay distinguishable. Putting `extract` at the bottom of it would make
//! that header a lie, and a lie in a module doc comment is the kind that
//! survives for years.
//!
//! # What the spec measured, and why this file exists at all
//!
//! §1 measured ffprobe's output for a muxed ASS track: `extradata_size: 176` —
//! the style/script header — and, from `-show_packets`, a single packet of
//! `size: 11` carrying a `pts_time` and **no payload**. So the timings are
//! discoverable from the probe and the text is not, which is why enumerating
//! tracks lives in `probe.rs` and reading the cues lives here. A subtitle track
//! is not a `<track>` tag; it is a decode.
//!
//! # One ffmpeg invocation, and why it is shaped this way
//!
//! ```text
//! ffmpeg -nostdin -v error -i <file> -map 0:<idx> -c copy -f <muxer> -
//! ```
//!
//! …with one exception. `mov_text` is a QuickTime timecode atom rather than
//! text, and the WebVTT muxer refuses it (`supports only codec webvtt for type
//! subtitle`, exit 234), so that one track is transcoded with `-c:s webvtt`.
//! It is the only transcode here and the reason `codec_args` is a parameter
//! rather than a constant.
//!
//! Four arguments that are not defaults, each earning its place:
//!
//! - **`-nostdin`.** ffmpeg reads stdin and will *consume it* when it thinks it
//!   is interactive. An extractor that forgets this eats the caller's stdin —
//!   which in a server is the connection, and the symptom is a hung request
//!   somewhere else entirely.
//! - **`-v error`.** Without it ffmpeg writes its banner and every stream's
//!   parameters to stderr, and a caller that captures stderr to explain a
//!   failure ends up explaining a banner.
//! - **`-c copy`.** No re-encode. A subtitle stream is already text; encoding it
//!   again would rescale timestamps, and the spec's 40 ms budget is spent on
//!   exactly that kind of rescale. Copy is also the only mode where
//!   "extraction" is the honest word for what this does. `mov_text` excepted —
//!   see the note below the argument list.
//! - **`-` as the output.** stdout, so nothing is written to the library's
//!   directory and there is no temporary file to clean up or to leak when the
//!   process is killed midway.
//!
//! # `mov_text` is the one format that needs a second pass
//!
//! `mov_text` is not text — it is a QuickTime timecode atom — so `-c copy`
//! gives back atoms rather than cues. It is converted to WebVTT by a *second*
//! ffmpeg, which is the only place in Commons where one ffmpeg invokes another,
//! and it is why the extraction is a two-step function rather than a command
//! builder.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::subtitles::{self, Document, Format};

/// Why a subtitle track could not be extracted.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("ffmpeg not found on PATH; install ffmpeg or set FFMPEG")]
    FfmpegMissing,
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("ffmpeg failed on {path} (stream {stream}): {detail}")]
    FfmpegFailed {
        path: String,
        stream: String,
        detail: String,
    },
    #[error("ffmpeg produced no subtitle data for stream {stream} of {path}")]
    NothingProduced { path: String, stream: String },
    #[error("ffmpeg produced {got} bytes of subtitle data that are not valid {format}: {detail}")]
    Undecodable {
        format: &'static str,
        got: usize,
        detail: String,
    },
    /// A sidecar whose extension names a format we cannot parse.
    ///
    /// Distinct from a parse failure on purpose: this track is *not supported*,
    /// which is a different thing from *this file is broken*, and the caller's
    /// two responses are different — skip the track quietly, or tell the user
    /// their file is malformed.
    #[error("no parser for a {codec} track ({format}); the format is not supported")]
    Unsupported { codec: String, format: String },
    #[error("subtitle output is not valid UTF-8: {0}")]
    NotUtf8(String),
}

/// The binary to run, for the same reason `Prober::with_binary` exists: a test
/// can point it at a stub, and a packaged install can ship its own rather than
/// depend on the host's.
#[derive(Debug, Clone)]
pub struct Extractor {
    binary: std::path::PathBuf,
}

impl Default for Extractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor {
    /// `FFMPEG` if set, else `ffmpeg` from PATH.
    pub fn new() -> Self {
        let binary = std::env::var_os("FFMPEG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("ffmpeg"));
        Self { binary }
    }

    pub fn with_binary(binary: impl Into<std::path::PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    /// Extract one stream of `path` and parse it.
    ///
    /// `format` is **ffprobe's codec name**, not ours, so the caller passes
    /// `stream.codec_name` straight through and does not have to consult
    /// [`Format::from_codec`] itself. An unrecognised codec is
    /// [`ExtractError::Unsupported`] rather than a wrong-format parse: a PGS
    /// track parsed as SRT is mojibake the user sees, and an absent track is
    /// not.
    pub fn extract_stream(
        &self,
        path: &Path,
        stream_index: i64,
        codec: &str,
    ) -> Result<Document, ExtractError> {
        let display = path.display().to_string();
        let stream = stream_index.to_string();
        let Some(format) = Format::from_codec(codec) else {
            return Err(ExtractError::Unsupported {
                codec: codec.to_string(),
                format: codec.to_string(),
            });
        };

        // A text format is copied out as itself. `mov_text` is NOT copied: it is
        // a QuickTime timecode atom, and the WebVTT muxer rejects it outright --
        // `supports only codec webvtt for type subtitle`, exit 234. It has to be
        // *transcoded* to WebVTT, which is the one transcode in this module and
        // the only place `-c copy` is not the right answer.
        //
        // That this is the only exception is the point of the flag being a
        // parameter rather than a constant. A blanket `-c copy` with a special
        // case bolted on afterwards is how the special case gets forgotten the
        // next time a format is added.
        let (muxer, parse_as, codec_args): (&str, Format, &[&str]) = match format {
            Format::MovText => ("webvtt", Format::WebVtt, &["-c:s", "webvtt"]),
            other => (other.name(), other, &["-c", "copy"]),
        };

        let bytes = self.run(path, stream_index, muxer, codec_args, &display, &stream)?;
        let text = String::from_utf8(bytes).map_err(|e| ExtractError::NotUtf8(e.to_string()))?;
        let doc = subtitles::parse(&text, parse_as).map_err(|e| ExtractError::Undecodable {
            format: parse_as.name(),
            got: text.len(),
            detail: e.to_string(),
        })?;

        // **Non-empty output that parses to zero cues is a failure, not an empty
        // track.** Every parser here returns `Ok` with no cues for input it does
        // not recognise -- `parse_ass` on an SRT file finds no `[Events]`
        // section and returns an empty document, not an error -- so a format
        // mismatch is silent by construction. Left alone that becomes a subtitle
        // track that is listed, selectable, and empty, which is the reading a
        // user takes: "this video has no subtitles". The two are
        // indistinguishable from the outside and completely different inside,
        // and this is the only place both are visible.
        //
        // Not fixed in the parsers. A zero-cue document is a legitimate result
        // for a genuinely empty file, and a parser that refused to produce one
        // would make "this file has no subtitles" unexpressible.
        if doc.cues.is_empty() {
            return Err(ExtractError::Undecodable {
                format: parse_as.name(),
                got: text.len(),
                detail: "the output parsed cleanly but contained no cues, which for a \
                         non-empty stream means the track is not in the format it was \
                         reported as"
                    .into(),
            });
        }
        Ok(doc)
    }

    /// Read and parse a sidecar file.
    ///
    /// No process at all: the file is already text on disk, and running ffmpeg
    /// to read a file ffmpeg did not write is a dependency where none is
    /// needed. The format comes from the **extension**, which for a sidecar is
    /// the only thing that identifies it.
    pub fn read_sidecar(&self, path: &Path) -> Result<Document, ExtractError> {
        let display = path.display().to_string();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let Some(format) = Format::from_extension(ext) else {
            return Err(ExtractError::Unsupported {
                codec: ext.to_string(),
                format: ext.to_string(),
            });
        };

        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|source| ExtractError::Io {
                path: display.clone(),
                source,
            })?
            // Not `read_to_string`: a sidecar with a UTF-8 BOM or a Latin-1
            // accent is common in the wild, and `read_to_string` rejects the
            // whole file for a byte sequence this module can lossily decode.
            .read_to_end(&mut bytes)
            .map_err(|source| ExtractError::Io {
                path: display.clone(),
                source,
            })?;

        let text = decode_lossy(&bytes);
        subtitles::parse(&text, format).map_err(|e| ExtractError::Undecodable {
            format: format.name(),
            got: bytes.len(),
            detail: e.to_string(),
        })
    }

    /// Run ffmpeg and return its stdout.
    fn run(
        &self,
        path: &Path,
        stream_index: i64,
        muxer: &str,
        codec_args: &[&str],
        display: &str,
        stream: &str,
    ) -> Result<Vec<u8>, ExtractError> {
        let mut child = Command::new(&self.binary)
            .arg("-nostdin")
            .arg("-v")
            .arg("error")
            .arg("-i")
            .arg(path)
            .arg("-map")
            .arg(format!("0:{stream_index}"))
            .args(codec_args)
            .arg("-f")
            .arg(muxer)
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ExtractError::FfmpegMissing,
                _ => ExtractError::Io {
                    path: display.to_string(),
                    source: e,
                },
            })?;

        // Both pipes have to be drained concurrently with `wait`. A subtitle
        // track is small so the stdout pipe will not fill, but stderr will
        // block ffmpeg the moment it fills — and then `wait` blocks forever on
        // a process that is blocked writing to a pipe nobody is reading. The
        // deadlock is silent and the test suite hangs rather than fails.
        let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
        let err_handle = std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = stderr_pipe.read_to_string(&mut buf);
            buf
        });

        let mut bytes = Vec::new();
        let read = stdout_pipe.read_to_end(&mut bytes);
        let stderr = err_handle.join().unwrap_or_default();
        let status = child.wait();

        // Both are checked before the exit status, so a read that failed part
        // way is reported as a read failure rather than as ffmpeg exiting
        // non-zero — which is what a truncated pipe looks like from the outside.
        read.map_err(|source| ExtractError::Io {
            path: display.to_string(),
            source,
        })?;
        let status = status.map_err(|source| ExtractError::Io {
            path: display.to_string(),
            source,
        })?;

        if !status.success() {
            return Err(ExtractError::FfmpegFailed {
                path: display.to_string(),
                stream: stream.to_string(),
                detail: last_meaningful_line(&stderr),
            });
        }
        if bytes.is_empty() {
            // Exit 0 and no bytes: the `-map` selected nothing, which for a
            // stream index that came from a probe of a *different* file is the
            // likely cause. Trusting the exit code would turn that into a track
            // that parses to zero cues — indistinguishable from a file with no
            // subtitles, which is the reading a user would take.
            return Err(ExtractError::NothingProduced {
                path: display.to_string(),
                stream: stream.to_string(),
            });
        }
        Ok(bytes)
    }
}

/// Extract one stream with the default binary.
pub fn extract_stream(
    path: &Path,
    stream_index: i64,
    codec: &str,
) -> Result<Document, ExtractError> {
    Extractor::new().extract_stream(path, stream_index, codec)
}

/// Read and parse a sidecar with the default binary set (no process is run).
pub fn read_sidecar(path: &Path) -> Result<Document, ExtractError> {
    Extractor::new().read_sidecar(path)
}

/// The last line of ffmpeg's stderr worth showing.
///
/// The banner, the build configuration and the stream dump all precede the
/// actual error, so the *last* non-empty line is the one that says what went
/// wrong — the same rule `transcode.rs` uses, and for the same reason: the first
/// line of ffmpeg's output is its version string on every single run.
fn last_meaningful_line(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("ffmpeg version")
        })
        .unwrap_or("no output")
        .trim()
        .to_string()
}

/// Decode bytes to text without failing on a bad byte.
///
/// Two things this handles that `String::from_utf8` does not:
///
/// - **A UTF-8 BOM.** Every Windows tool that has ever saved an `.srt` writes
///   one, and it lands in front of the first cue's timestamp. `parse` reads
///   that as a malformed timestamp and the whole file yields zero cues — a
///   silent failure on a file that is perfectly valid.
/// - **A non-UTF-8 encoding.** A French or Japanese `.srt` saved as Shift-JIS or
///   Latin-1 is lossy here, and lossy is the right trade: the alternative is
///   refusing the file, and a caption with one mangled character is better than
///   a track that does not appear at all.
fn decode_lossy(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(body).into_owned()
}
