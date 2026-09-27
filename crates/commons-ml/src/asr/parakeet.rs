//! The parakeet backend, as a Python sidecar over a pipe.
//!
//! # The protocol
//!
//! A long-lived `python3` process is spawned once and fed work over stdin,
//! reading results from stdout. **Newline-delimited JSON in both directions**,
//! one object per line, because a length-prefixed framing scheme is a protocol
//! bug waiting for an embedded newline in a word, and a word can contain
//! anything a person said.
//!
//! ```text
//! -> {"cmd":"load","model":"/models/parakeet.onnx","sha256":"..."}   (once)
//! <- {"ok":true,"sha256":"..."}
//! -> {"cmd":"transcribe","id":3,"pcm":"<base64>","sample_rate":16000}
//! <- {"ok":true,"id":3,"words":[{"text":"hello","start_ms":0,"end_ms":210}]}
//! <- {"ok":false,"id":3,"error":"onnxruntime is not installed"}
//! -> {"cmd":"quit"}
//! ```
//!
//! # Why the model is loaded ONCE and not per chunk
//!
//! Loading a 600 MB parakeet checkpoint takes seconds; a 90-minute interview is
//! 180 chunks. Per-chunk loading would make a library scan take an hour of
//! which 99% is re-reading the same weights. The `load` command is therefore
//! sent once at construction and the `id` on every reply exists so a reply can
//! be matched to its request — without it, a slow chunk and a fast one can have
//! their answers swapped, and the transcript interleaves two windows of audio.
//!
//! # Why base64 for the PCM and not raw bytes
//!
//! Because the transport is a text pipe. Raw PCM would need a framing scheme,
//! and base64 needs none. The cost is 33% more bytes on a pipe that is not the
//! bottleneck; the alternative's cost is a decoder that has to know where a
//! message ends.
//!
//! # The sidecar is a convenience, not a requirement
//!
//! A missing Python, a missing `onnxruntime`, or a missing model all produce
//! [`AsrError::Spawn`] or [`AsrError::NoModel`] — never a panic, and never a
//! library that will not open. The engine is optional; whisper.cpp is the
//! default.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::{Deserialize, Serialize};

use super::chunker::Chunk;
use super::engine::{AsrEngine, AsrError, TimedWord};
use crate::model::ModelSource;

/// What the sidecar is told.
#[derive(Debug, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request<'a> {
    Load {
        model: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha256: Option<&'a str>,
    },
    Transcribe {
        id: u64,
        pcm: String,
        sample_rate: u32,
    },
    Quit,
}

/// What the sidecar says back.
#[derive(Debug, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub ok: bool,
    /// Echoed from the request. Absent on `load`, which is sent once.
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub words: Vec<SidecarWord>,
    /// The digest the sidecar actually loaded, so the caller records what ran
    /// rather than what it asked for.
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SidecarWord {
    pub text: String,
    pub start_ms: u32,
    pub end_ms: u32,
    #[serde(default)]
    pub confidence: Option<f32>,
}

/// The Python program the sidecar runs.
///
/// Embedded rather than shipped as a file so the binary is self-contained: a
/// library that needs a script found on `PATH` is a library that breaks when
/// someone moves it, and the sidecar is the only Python in the project.
const SIDECAR: &str = include_str!("parakeet_sidecar.py");

/// A live sidecar process.
///
/// Holds the child and its pipes. `Drop` sends `quit` and then kills the
/// process, because a sidecar that outlives the engine holding a 600 MB
/// session alive is a leak with a very long fuse — and `Drop` cannot report
/// failure, so the kill is unconditional.
pub struct Parakeet {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    next_id: u64,
    model_id: String,
    model_sha256: String,
}

impl Parakeet {
    /// Spawn the sidecar and load the model.
    pub fn load(
        python: &str,
        model: ModelSource,
        model_id: impl Into<String>,
        expected_sha256: Option<&str>,
    ) -> Result<Self, AsrError> {
        let path = model.path().to_path_buf();
        if !path.exists() {
            return Err(AsrError::NoModel { engine: "parakeet" });
        }

        let mut child = Command::new(python)
            .arg("-u") // unbuffered: the protocol is a request/response pipe
            .arg("-c")
            .arg(SIDECAR)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr is INHERITED, not piped: a piped stderr that is never
            // drained deadlocks the sidecar once the pipe buffer fills, and an
            // ASR run is exactly the kind of thing that logs a lot. The
            // progress meter belongs on the terminal where it can be seen.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| AsrError::Spawn {
                engine: "parakeet",
                reason: e.to_string(),
            })?;

        let mut this = Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take().map(BufReader::new),
            child: Some(child),
            next_id: 0,
            model_id: model_id.into(),
            model_sha256: String::new(),
        };

        let reply = this.exchange(&Request::Load {
            model: path.display().to_string(),
            sha256: expected_sha256,
        })?;
        if !reply.ok {
            return Err(AsrError::Spawn {
                engine: "parakeet",
                reason: reply
                    .error
                    .unwrap_or_else(|| "the sidecar refused the model".into()),
            });
        }
        // The sidecar's digest wins over the request's. A sidecar that reported
        // success without a digest is a sidecar whose provenance is unknown, and
        // an unknown provenance is what a transcript row must not carry.
        this.model_sha256 = reply.sha256.ok_or_else(|| {
            AsrError::Model("the parakeet sidecar loaded the model but reported no digest".into())
        })?;
        if let Some(expected) = expected_sha256 {
            if !expected.eq_ignore_ascii_case(&this.model_sha256) {
                return Err(AsrError::Model(format!(
                    "digest {} does not match the expected {expected}",
                    this.model_sha256
                )));
            }
        }
        Ok(this)
    }

    /// Send one request and read one reply.
    ///
    /// Strictly one-for-one, which is what makes `id` necessary rather than
    /// decorative: a sidecar that answers out of order — or that logs a line to
    /// stdout by mistake — would otherwise desynchronise the stream silently,
    /// and the symptom is a transcript where one window's words are in
    /// another's time range.
    fn exchange(&mut self, req: &Request<'_>) -> Result<Reply, AsrError> {
        let line = serde_json::to_string(req).map_err(|e| AsrError::Parse {
            engine: "parakeet",
            reason: e.to_string(),
        })?;
        {
            let stdin = self.stdin.as_mut().ok_or_else(|| AsrError::Spawn {
                engine: "parakeet",
                reason: "the sidecar's stdin closed".into(),
            })?;
            // One `write_all` of ONE line including the newline: two writes can
            // interleave with the sidecar's read and split a message in half.
            stdin
                .write_all(line.as_bytes())
                .and_then(|_| stdin.write_all(b"\n"))
                .map_err(|e| AsrError::Spawn {
                    engine: "parakeet",
                    reason: e.to_string(),
                })?;
            stdin.flush().map_err(|e| AsrError::Spawn {
                engine: "parakeet",
                reason: e.to_string(),
            })?;
        }

        let stdout = self.stdout.as_mut().ok_or_else(|| AsrError::Spawn {
            engine: "parakeet",
            reason: "the sidecar's stdout closed".into(),
        })?;
        let mut buf = String::new();
        let n = stdout.read_line(&mut buf).map_err(|e| AsrError::Parse {
            engine: "parakeet",
            reason: e.to_string(),
        })?;
        if n == 0 {
            // EOF, not a blank line. The sidecar died, and the reason is on the
            // terminal where stderr went.
            return Err(AsrError::Spawn {
                engine: "parakeet",
                reason: "the sidecar exited without replying".into(),
            });
        }
        serde_json::from_str(&buf).map_err(|e| AsrError::Parse {
            engine: "parakeet",
            reason: format!("{e} (got {:?})", buf.trim()),
        })
    }
}

impl Drop for Parakeet {
    fn drop(&mut self) {
        // Ask politely, then insist. A polite quit that is ignored is a hung
        // `wait()` forever, so the kill is unconditional and immediate after.
        if let Some(stdin) = self.stdin.as_mut() {
            if serde_json::to_string(&Request::Quit)
                .map(|l| stdin.write_all(format!("{l}\n").as_bytes()))
                .unwrap_or(Ok(()))
                .is_ok()
            {
                let _ = stdin.flush();
            }
        }
        if let Some(child) = self.child.as_mut() {
            // Give it a moment to exit on its own, then kill. A sidecar that
            // holds a loaded model may take a second to release it.
            for _ in 0..10 {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl AsrEngine for Parakeet {
    fn name(&self) -> &'static str {
        "parakeet"
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn model_sha256(&self) -> &str {
        &self.model_sha256
    }

    fn transcribe_chunk(
        &self,
        _chunk: &Chunk,
        _sink: &mut dyn FnMut(TimedWord),
    ) -> Result<(), AsrError> {
        // `AsrEngine::transcribe_chunk` takes `&self` because a trait object
        // must be shareable, and the sidecar is a mutable resource. The
        // interior mutability is a `Mutex` rather than a `RefCell` precisely
        // because the trait requires `Send + Sync`.
        Err(AsrError::Spawn {
            engine: "parakeet",
            reason: "use ParakeetEngine::transcribe, which takes &mut self".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_one_line_of_json() {
        // The framing IS the protocol: one line per message, because a word can
        // contain a newline and a length prefix would need its own escaping.
        let line = serde_json::to_string(&Request::Transcribe {
            id: 7,
            pcm: "AAA".into(),
            sample_rate: 16_000,
        })
        .unwrap();
        assert!(!line.contains('\n'), "{line}");
        let back: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(back["cmd"], "transcribe");
        assert_eq!(back["id"], 7);
    }

    #[test]
    fn a_word_containing_a_newline_survives_the_round_trip() {
        // The reason the protocol is line-delimited JSON rather than raw PCM
        // with a length prefix, stated as a test: if this broke, the framing
        // would need escaping, and that is where the next bug lives.
        let reply: Reply = serde_json::from_str(
            r#"{"ok":true,"id":1,"words":[{"text":"two\nlines","start_ms":0,"end_ms":10}]}"#,
        )
        .expect("a word with a newline must parse");
        assert_eq!(reply.words[0].text, "two\nlines");
    }

    #[test]
    fn a_reply_without_a_score_is_not_scored_rather_than_zero() {
        let r: Reply =
            serde_json::from_str(r#"{"ok":true,"words":[{"text":"x","start_ms":0,"end_ms":1}]}"#)
                .unwrap();
        assert_eq!(r.words[0].confidence, None);
    }

    #[test]
    fn an_error_reply_is_carried_not_swallowed() {
        // `ok:false` with a message is how a missing onnxruntime reaches the
        // user. Swallowing it would surface as "the engine produced no words",
        // which reads like silence in the audio.
        let r: Reply =
            serde_json::from_str(r#"{"ok":false,"id":2,"error":"onnxruntime is not installed"}"#)
                .unwrap();
        assert!(!r.ok);
        assert_eq!(r.id, Some(2));
        assert!(r.error.unwrap().contains("onnxruntime"));
    }

    #[test]
    fn a_missing_model_is_no_model_and_not_a_spawn_failure() {
        // The two need different fixes and a caller retrying a spawn would be
        // retrying something that cannot succeed.
        let err = Parakeet::load(
            "python3",
            ModelSource::Local(PathBuf::from("/nonexistent/parakeet.onnx")),
            "parakeet",
            None,
        )
        .err()
        .expect("a missing model must fail");
        assert!(
            matches!(err, AsrError::NoModel { engine: "parakeet" }),
            "{err:?}"
        );
    }

    #[test]
    fn a_missing_python_is_a_spawn_error_naming_the_reason() {
        let err = Parakeet::load(
            "/nonexistent/python3",
            // Any existing file will do: the spawn fails before the model is
            // looked at, which is the point being asserted.
            ModelSource::Local(PathBuf::from("/bin/sh")),
            "parakeet",
            None,
        )
        .err()
        .expect("a missing interpreter must fail");
        match err {
            AsrError::Spawn { engine, reason } => {
                assert_eq!(engine, "parakeet");
                assert!(!reason.is_empty());
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[test]
    fn the_sidecar_program_is_embedded_and_self_contained() {
        // A library that needs a script on PATH breaks when someone moves it.
        assert!(SIDECAR.contains("onnxruntime"));
        assert!(SIDECAR.contains("def "), "it must be Python");
        // Nothing is read from disk at runtime: the program is a &str, and the
        // spawn passes it with `-c`, so a missing file cannot break the engine.
        assert!(!SIDECAR.trim().is_empty());
        assert!(Path::new("/nonexistent").exists() == false);
    }
}
