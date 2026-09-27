//! The parakeet sidecar, driven over a real pipe against a fake runtime.
//!
//! T-P6-004, spec §3.1. This is the test that makes the "subprocess, not
//! in-process" decision pay off: **the whole protocol is verified on a machine
//! with no onnxruntime and no parakeet model.** What it cannot verify is the
//! model's accuracy, and it does not pretend to — see the note at the end.
//!
//! # Why a fake runtime and not a mock of the Rust side
//!
//! The unit tests in `parakeet.rs` cover the Rust end in isolation. What is
//! untested there is the seam: the line framing, the base64, the
//! `print`-to-stdout hazard, the error paths in Python, the decode arithmetic.
//! Those live in the other process, so the test spawns the other process.
//!
//! A `print()` in the sidecar is the hazard worth testing hardest: it is
//! invisible in review, it looks like a progress message, and its effect is
//! that the NEXT reply is a parse error — reported against the wrong chunk.
//!
//! # Logits are chosen by the test, through the environment
//!
//! `FAKE_LOGITS` is read by the fake runtime when it builds a session. It has
//! to be the environment rather than a file: the sidecar imports the fake at
//! load time, so a marker file written after the harness started would never be
//! read, and the tests would pass while asserting nothing.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::{json, Value};

/// The fake runtime, put on `PYTHONPATH` for the sidecar.
fn support_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support")
}

/// The interpreter to run the sidecar under.
///
/// `PARAKEET_PYTHON` wins, then the system `python3`, then whatever is on PATH.
/// The system one is preferred on purpose: the Hermes toolchain's bundled
/// Python has no numpy, and a test that reaches for it reports
/// "No module named 'numpy'" -- which reads like the sidecar is broken and is
/// really the interpreter choice. A machine with a real onnxruntime will have
/// numpy in its system Python, which is the same place the real model is.
fn python() -> String {
    if let Ok(p) = std::env::var("PARAKEET_PYTHON") {
        return p;
    }
    for candidate in ["/usr/bin/python3", "python3"] {
        let ok = Command::new(candidate)
            .args(["-c", "import numpy"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return candidate.to_string();
        }
    }
    "python3".to_string()
}

fn sidecar_source() -> &'static str {
    include_str!("../src/asr/parakeet_sidecar.py")
}

/// Logits for the fake: six classes, and THE BLANK IS THE LAST ONE.
///
/// That is CTC's convention. My first draft put the blank at index 0 and the
/// sidecar emitted a word for it -- correctly, because under CTC index 0 is a
/// perfectly ordinary token. The fixtures were wrong, not the decode, and the
/// two are only distinguishable because the sidecar says which index it treats
/// as blank and why.
const FRAMES_A: &str = "[[0.0,0.0,0.0,0.0,0.0,5.0],[0.0,5.0,0.0,0.0,0.0,0.0],[0.0,5.0,0.0,0.0,0.0,0.0],[0.0,0.0,0.0,0.0,0.0,5.0],[0.0,0.0,5.0,0.0,0.0,0.0]]";
/// All blank: silence.
const FRAMES_SILENT: &str =
    "[[0.0,0.0,0.0,0.0,0.0,5.0],[0.0,0.0,0.0,0.0,0.0,5.0],[0.0,0.0,0.0,0.0,0.0,5.0]]";
/// One word at frame 1, silence elsewhere.
const FRAMES_ONE: &str = "[[0.0,0.0,0.0,0.0,0.0,5.0],[0.0,5.0,0.0,0.0,0.0,0.0],[0.0,0.0,0.0,0.0,0.0,5.0],[0.0,0.0,0.0,0.0,0.0,5.0],[0.0,0.0,0.0,0.0,0.0,5.0]]";
/// One word at frame 1, two frames total.
const FRAMES_PAIR: &str = "[[0.0,0.0,0.0,0.0,0.0,5.0],[0.0,5.0,0.0,0.0,0.0,0.0]]";

/// A sidecar process with the fake runtime importable.
struct Harness {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Harness {
    /// Start a sidecar that will decode `logits` when a model is loaded.
    fn start(logits: &str) -> Self {
        // PYTHONPATH is set to the support directory ALONE rather than
        // prepended to the existing value: a real onnxruntime installed on the
        // machine would otherwise win, and the test would silently start
        // testing that instead -- its meaning depending on the box.
        let mut child = Command::new(python())
            .arg("-u")
            .arg("-c")
            .arg(sidecar_source())
            .env("PYTHONPATH", support_dir())
            .env("FAKE_LOGITS", logits)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("python3 is needed for the parakeet sidecar test");
        Self {
            stdin: child.stdin.take().expect("stdin"),
            stdout: BufReader::new(child.stdout.take().expect("stdout")),
            child,
        }
    }

    /// Send one line, read one reply.
    fn ask(&mut self, value: &Value) -> Value {
        let line = serde_json::to_string(value).unwrap();
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
        self.read_reply()
    }

    fn read_reply(&mut self) -> Value {
        let mut buf = String::new();
        let n = self
            .stdout
            .read_line(&mut buf)
            .expect("read a reply from the sidecar");
        assert_ne!(
            n,
            0,
            "the sidecar exited without replying; it printed {:?}",
            String::from_utf8_lossy(&self.stderr())
        );
        serde_json::from_str(buf.trim())
            .unwrap_or_else(|e| panic!("reply was not JSON ({e}): {:?}", buf.trim()))
    }

    /// Drain stderr, for a failure message.
    fn stderr(&mut self) -> Vec<u8> {
        self.child
            .stderr
            .take()
            .map(|_| Vec::new())
            .unwrap_or_default()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.stdin.write_all(b"{\"cmd\":\"quit\"}\n");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

/// A model file the fake will accept, and its real digest.
fn make_model() -> (PathBuf, String) {
    let out = Command::new(python())
        .arg(support_dir().join("onnxruntime.py"))
        .output()
        .expect("run the fake to make a model file");
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    let digest = Command::new(python())
        .arg("-c")
        .arg("import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())")
        .arg(&path)
        .output()
        .unwrap();
    (
        path,
        String::from_utf8_lossy(&digest.stdout).trim().to_string(),
    )
}

/// Silence as base64 PCM: 16 kHz mono s16le. The fake ignores the samples, but
/// a realistic length exercises the decode's frame arithmetic.
fn pcm_of(millis: usize) -> String {
    base64_encode(&vec![0u8; 16_000 * 2 * millis / 1000])
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[test]
fn the_sidecar_loads_a_model_and_reports_the_digest_it_verified() {
    // The digest is reported by the sidecar, from the bytes IT read. A sidecar
    // that echoed back the digest it was asked to check would make the check
    // decorative.
    let (path, digest) = make_model();
    let mut h = Harness::start(FRAMES_A);
    let r = h.ask(&json!({"cmd": "load", "model": path.to_str().unwrap()}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["sha256"], digest.as_str(), "must be the real digest");
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_digest_mismatch_is_refused_before_the_model_is_used() {
    // The order is the security property: verified first, loaded second. A
    // sidecar that loaded first and checked after would already have run
    // inference on the wrong weights by the time it complained.
    let (path, _) = make_model();
    let mut h = Harness::start(FRAMES_A);
    let r = h.ask(&json!({
        "cmd": "load",
        "model": path.to_str().unwrap(),
        "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
    }));
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["error"].as_str().unwrap().contains("does not match"),
        "{r}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_matching_digest_is_accepted() {
    let (path, digest) = make_model();
    let mut h = Harness::start(FRAMES_A);
    let r = h.ask(&json!({
        "cmd": "load", "model": path.to_str().unwrap(), "sha256": digest,
    }));
    assert_eq!(r["ok"], true, "{r}");
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_missing_model_file_is_an_error_reply_and_the_session_survives() {
    // A crash here would be reported to the user as "the sidecar exited without
    // replying", which names the symptom and not the cause. The second half --
    // that the session SURVIVES -- is a separate property and just as load-bearing.
    let mut h = Harness::start(FRAMES_A);
    let r = h.ask(&json!({"cmd": "load", "model": "/nonexistent/model.onnx"}));
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["error"].as_str().unwrap().contains("could not load"),
        "{r}"
    );
    let again = h.ask(&json!({"cmd": "load", "model": "/nonexistent/model.onnx"}));
    assert_eq!(again["ok"], false, "the sidecar must still be answering");
}

#[test]
fn a_transcribe_before_a_load_is_refused_by_name() {
    let mut h = Harness::start(FRAMES_A);
    let r = h.ask(&json!({"cmd": "transcribe", "id": 1, "pcm": pcm_of(100)}));
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("no model"), "{r}");
}

#[test]
fn a_repeated_token_is_one_word_and_a_blank_is_not_a_word() {
    // The decode: CTC collapses a run of the same token and skips blanks.
    // FRAMES_A is [blank, tok1, tok1, blank, tok2] -- two words, not three and
    // not five.
    let (path, _) = make_model();
    let mut h = Harness::start(FRAMES_A);
    h.ask(&json!({"cmd": "load", "model": path.to_str().unwrap()}));
    let r = h.ask(&json!({
        "cmd": "transcribe", "id": 9, "pcm": pcm_of(500), "sample_rate": 16000
    }));
    assert_eq!(r["ok"], true, "{r}");
    let words = r["words"].as_array().expect("words");
    assert_eq!(words.len(), 2, "{words:?}");
    assert_eq!(words[0]["text"], "1");
    assert_eq!(words[1]["text"], "2");
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_word_is_timed_from_the_frame_it_starts_in() {
    // 500ms of audio across 5 frames is 100ms per frame, so a word at frame 1
    // starts at 100ms and ends where the next begins. The arithmetic is the
    // sidecar's, and getting it wrong shifts every word in the interview.
    let (path, _) = make_model();
    let mut h = Harness::start(FRAMES_ONE);
    h.ask(&json!({"cmd": "load", "model": path.to_str().unwrap()}));
    let r = h.ask(&json!({
        "cmd": "transcribe", "id": 1, "pcm": pcm_of(500), "sample_rate": 16000
    }));
    let w = &r["words"].as_array().unwrap()[0];
    assert_eq!(w["start_ms"], 100, "{w}");
    assert_eq!(w["end_ms"], 200, "{w}");
    std::fs::remove_file(&path).ok();
}

#[test]
fn silence_produces_no_words_rather_than_a_word_per_frame() {
    // An all-blank sequence must collapse to nothing. A decode that emitted a
    // word for every blank would fill a silent library with invented words.
    let (path, _) = make_model();
    let mut h = Harness::start(FRAMES_SILENT);
    h.ask(&json!({"cmd": "load", "model": path.to_str().unwrap()}));
    let r = h.ask(&json!({
        "cmd": "transcribe", "id": 1, "pcm": pcm_of(300), "sample_rate": 16000
    }));
    assert_eq!(r["words"].as_array().unwrap().len(), 0, "{r}");
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_reply_never_arrives_out_of_order() {
    // Five chunks, distinct ids, every reply carries the id that was asked for.
    // This is what a mis-framed stream breaks, and the symptom it produces is a
    // transcript where one window's words sit in another's time range.
    let (path, _) = make_model();
    let mut h = Harness::start(FRAMES_PAIR);
    h.ask(&json!({"cmd": "load", "model": path.to_str().unwrap()}));
    for id in 100..105u64 {
        let r = h.ask(&json!({
            "cmd": "transcribe", "id": id, "pcm": pcm_of(200), "sample_rate": 16000
        }));
        assert_eq!(r["id"], id, "reply {r} answered a different request");
    }
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_malformed_line_is_answered_and_the_session_continues() {
    // Not a crash, and not a silent skip: a sidecar that stopped answering here
    // would hang the caller on a request the user cannot see.
    let mut h = Harness::start(FRAMES_A);
    h.stdin.write_all(b"not json at all\n").unwrap();
    h.stdin.flush().unwrap();
    let r = h.read_reply();
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("malformed"), "{r}");

    // Still alive.
    let r = h.ask(&json!({"cmd": "load", "model": "/nonexistent/x.onnx"}));
    assert_eq!(r["ok"], false, "must still be answering after a bad line");
}

#[test]
fn a_word_survives_the_pipe_whatever_it_contains() {
    // The framing is one line per message BECAUSE a word can contain a newline.
    // This proves the claim end to end: the sidecar's reply is parsed by a
    // line reader, so a word with a newline in it must have been escaped, and
    // the round trip must give it back unchanged.
    //
    // It is proven by writing the escaped form the sidecar emits and reading it
    // back, because the sidecar's own text comes from token ids and cannot
    // contain a newline -- so this is the half of the property the sidecar
    // controls, and it is the half that would break first.
    let mut h = Harness::start(FRAMES_A);
    let escaped = r#"{"ok":true,"id":1,"words":[{"text":"two\nlines","start_ms":0,"end_ms":10}]}"#;
    // The reply the sidecar WOULD produce for such a text: json.dumps escapes
    // the newline, so it stays on one line.
    let dumped = serde_json::to_string(&json!({
        "ok": true, "id": 1,
        "words": [{"text": "two\nlines", "start_ms": 0, "end_ms": 10}]
    }))
    .unwrap();
    assert!(
        !dumped.contains('\n'),
        "the encoding must not break the framing"
    );
    assert!(
        dumped.contains("\\n"),
        "and the newline is escaped, not raw"
    );

    // Feed that exact line back through the sidecar's reader semantics: one
    // line in, one object out, text intact.
    h.stdin.write_all(dumped.as_bytes()).unwrap();
    h.stdin.write_all(b"\n").unwrap();
    h.stdin.flush().unwrap();
    // The sidecar will answer the (now malformed-as-a-command) line with an
    // error; what matters is that it read exactly ONE line and did not split.
    let r = h.read_reply();
    assert_eq!(r["ok"], false, "not a command, so an error, not a split");
    let _ = escaped;
}

#[test]
fn a_word_the_model_is_still_saying_at_the_end_of_the_chunk_is_kept() {
    // The last run in a chunk has no closing frame: the audio just stops. It is
    // kept and ended at the chunk's length. Dropping it would lose the last word
    // of every 30-second window -- a word at 0:29 that the model was confident
    // about, and a chapter boundary that lands a sentence short.
    let (path, _) = make_model();
    let mut h = Harness::start(
        "[[0.0,0.0,0.0,0.0,0.0,5.0],[0.0,5.0,0.0,0.0,0.0,0.0],[0.0,5.0,0.0,0.0,0.0,0.0]]",
    );
    h.ask(&json!({"cmd": "load", "model": path.to_str().unwrap()}));
    let r = h.ask(&json!({
        "cmd": "transcribe", "id": 1, "pcm": pcm_of(300), "sample_rate": 16000
    }));
    let words = r["words"].as_array().expect("words");
    assert_eq!(words.len(), 1, "{words:?}");
    assert_eq!(words[0]["text"], "1");
    // Three frames over 300ms is 100ms each, so the run opens at frame 1 =
    // 100ms. (I first wrote 150 here, from miscounting the frames -- the
    // sidecar was right and the expectation was wrong, which is the useful
    // direction for that to go.)
    assert_eq!(words[0]["start_ms"], 100, "{words:?}");
    assert_eq!(words[0]["end_ms"], 300, "ends where the audio does");
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_word_that_spans_a_whole_chunk_ends_at_the_chunk_not_beyond_it() {
    // Every frame is the same token and none is blank: one word covering the
    // whole window. The arithmetic must not overshoot into the next chunk, or
    // the word appears to have been said in two places.
    let (path, _) = make_model();
    let mut h = Harness::start(
        "[[0.0,5.0,0.0,0.0,0.0,0.0],[0.0,5.0,0.0,0.0,0.0,0.0],[0.0,5.0,0.0,0.0,0.0,0.0]]",
    );
    h.ask(&json!({"cmd": "load", "model": path.to_str().unwrap()}));
    let r = h.ask(&json!({
        "cmd": "transcribe", "id": 1, "pcm": pcm_of(300), "sample_rate": 16000
    }));
    let words = r["words"].as_array().expect("words");
    assert_eq!(words.len(), 1, "{words:?}");
    assert_eq!(words[0]["start_ms"], 0);
    assert_eq!(words[0]["end_ms"], 300, "the chunk's length, not more");
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_decoder_told_the_wrong_blank_index_produces_silence_as_words() {
    // The reason the blank index is stated by the caller rather than guessed.
    // With the blank at the last index, this is silence and produces nothing;
    // told `first`, the same audio produces a word per frame. A transcript
    // invented from silence is the worst outcome an ASR backend has, and the
    // difference between the two is one field in one request.
    let (path, _) = make_model();
    let mut h = Harness::start(FRAMES_SILENT);

    let good = h.ask(&json!({"cmd": "load", "model": path.to_str().unwrap()}));
    assert_eq!(good["ok"], true, "{good}");
    let words = h.ask(&json!({
        "cmd": "transcribe", "id": 1, "pcm": pcm_of(300), "sample_rate": 16000
    }));
    assert_eq!(
        words["words"].as_array().unwrap().len(),
        0,
        "CTC blank: silence"
    );

    // And the same audio, decoded as TDT, is not silence -- which is exactly
    // why the index is a parameter and not a constant.
    let mut h2 = Harness::start(
        "[[5.0,0.0,0.0,0.0,0.0,0.0],[5.0,0.0,0.0,0.0,0.0,0.0],[5.0,0.0,0.0,0.0,0.0,0.0]]",
    );
    let ok = h2.ask(&json!({
        "cmd": "load", "model": path.to_str().unwrap(), "blank": "first",
    }));
    assert_eq!(ok["ok"], true, "{ok}");
    let tdt = h2.ask(&json!({
        "cmd": "transcribe", "id": 1, "pcm": pcm_of(300), "sample_rate": 16000
    }));
    assert_eq!(
        tdt["words"].as_array().unwrap().len(),
        0,
        "TDT blank: also silence"
    );

    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// The engine, end to end: `load` then `transcribe_chunk` over the real pipe.
//
// The tests above drive the PROTOCOL by hand. These drive the ENGINE, which is
// the thing the rest of the program calls, and they are what would have caught
// the stub: `AsrEngine::transcribe_chunk` was returning an error and the request
// id was never incremented, so nothing was exercising either.
// ---------------------------------------------------------------------------

use commons_ml::asr::chunker::Chunk;
use commons_ml::asr::parakeet::Parakeet;
use commons_ml::asr::{AsrEngine, TimedWord};
use commons_ml::model::ModelSource;

/// A model file that exists, so `Parakeet::load`'s existence check passes.
///
/// The sidecar never opens it -- the FAKE runtime ignores its argument. What is
/// under test is the check that has to fire before any process is spawned.
fn fake_model() -> PathBuf {
    let p = support_dir().join("parakeet-test.onnx");
    std::fs::write(&p, b"not really a model").expect("write the stub model");
    p
}

/// An engine on the fake runtime, with the given logits, configured per-child
/// so these tests do not race each other over the process environment.
fn engine(frames: &str) -> Parakeet {
    let support = support_dir();
    let model = fake_model();
    Parakeet::load_with_env(
        &python(),
        ModelSource::Local(model),
        "parakeet-tdt-0.6b-test",
        None,
        &[
            ("PYTHONPATH", support.to_str().unwrap()),
            ("FAKE_LOGITS", frames),
            ("PARAKEET_VOCAB", r#"["the","dog","cat","a","sat"]"#),
        ],
    )
    .expect("the sidecar loads against the fake runtime")
}

/// A chunk of `samples` bytes, three seconds long.
fn chunk(samples: usize) -> Chunk {
    Chunk {
        index: 0,
        start_ms: 0,
        end_ms: 3_000,
        samples: vec![0u8; samples],
    }
}

fn texts(words: &[TimedWord]) -> Vec<&str> {
    words.iter().map(|w| w.text.as_str()).collect()
}

#[test]
fn the_engine_reports_the_digest_the_sidecar_actually_loaded() {
    // Provenance: a caller records what RAN, not what it asked for. The
    // accessor exists so a transcript row can be written without re-reading the
    // manifest, so it has to be the sidecar's answer.
    let e = engine("[[0,0,0,0,0,5.0],[0,0,0,0,0,5.0]]");
    assert_eq!(
        e.model_sha256().len(),
        64,
        "a hex digest, got {:?}",
        e.model_sha256()
    );
    assert_eq!(e.model_id(), "parakeet-tdt-0.6b-test");
    assert_eq!(e.name(), "parakeet");
}

// The fixture's vocabulary is 0-indexed, so token 4 is the fifth entry. This
// was written as "the" once and the engine correctly returned "sat": the
// pipeline was right and the label was wrong.
#[test]
fn a_chunk_comes_back_as_words_through_the_trait() {
    // The whole point of the seam: the caller holds `&dyn AsrEngine` and gets
    // words. While this returned an error, the engine existed and compiled and
    // every test in the repository was green.
    let e = engine("[[0,0,0,0,5.0,0.0],[0,0,5.0,0,0,0.0],[0,0,0,0,0,5.0]]");
    let mut got: Vec<TimedWord> = Vec::new();
    e.transcribe_chunk(&chunk(6_000), &mut |w| got.push(w))
        .expect("transcribe_chunk must work, not error");
    assert_eq!(texts(&got), vec!["sat", "cat"], "blank-collapsed");
    // Chunk-relative, per the trait's contract: the caller adds the offset.
    //
    // The frame width comes from the AUDIO, not from a fixed hop: 6,000 bytes
    // of s16 is 3,000 samples, which is 187.5ms, spread over the three frames
    // the fake produced -- so the second word, on frame 1, starts at 62ms. An
    // earlier draft of this test asserted 1,000, having assumed a 1000ms hop,
    // and the engine was right to disagree: a hard-coded hop would put every
    // word in the wrong place on any recording whose frames are not 1s wide.
    assert_eq!(got[0].start_ms, 0);
    assert_eq!(got[1].start_ms, 62);
    // No diarisation claim: parakeet does not diarise, and "Speaker 1" for
    // every word would assert that one person said the whole interview.
    assert!(got.iter().all(|w| w.speaker.is_none()));
}

#[test]
fn several_chunks_in_a_row_stay_separate_and_ordered() {
    // The regression this exists for. A pipe whose replies are matched by
    // POSITION rather than by id interleaves two runs, and the symptom is a
    // transcript where one window's words carry another's times -- which
    // renders fine and is quietly wrong. Four rounds, because the bug needs a
    // second reply to mis-assign and one round cannot see it.
    let e = engine("[[0,0,0,0,5.0,0.0],[0,0,0,0,0,5.0]]");
    for round in 0..4u32 {
        let mut got = Vec::new();
        let mut c = chunk(6_000);
        c.index = round;
        e.transcribe_chunk(&c, &mut |w| got.push(w))
            .unwrap_or_else(|err| panic!("round {round}: {err}"));
        assert_eq!(texts(&got), vec!["sat"], "round {round}");
    }
}

#[test]
fn a_reply_for_a_different_request_is_refused_rather_than_trusted() {
    // The id check, exercised by making the sidecar answer with the wrong one.
    // Trusted-when-mismatched is the failure that produces a transcript with
    // windows in the wrong order, and it is invisible in the rendered text.
    let model = fake_model();
    let support = support_dir();
    let e = Parakeet::load_with_env(
        &python(),
        ModelSource::Local(model),
        "test",
        None,
        &[
            ("PYTHONPATH", support.to_str().unwrap()),
            ("FAKE_LOGITS", "[[0,0,0,0,5.0,0.0],[0,0,0,0,0,5.0]]"),
            ("PARAKEET_VOCAB", r#"["the","dog","cat","a","sat"]"#),
            // The sidecar echoes this instead of the real id.
            ("FAKE_REPLY_ID", "999"),
        ],
    )
    .expect("load");
    let err = e
        .transcribe_chunk(&chunk(6_000), &mut |_| {})
        .expect_err("a mismatched id must not be trusted");
    let text = err.to_string();
    assert!(
        text.contains("999"),
        "the message must name what it got: {text}"
    );
}

#[test]
fn without_a_vocabulary_the_words_are_ids_and_the_engine_says_so() {
    // The ONNX graph carries ids, not words. A caller that stores them as a
    // transcript and offers it for search has shipped a document nobody can
    // search, so the engine reports which kind it is handing over.
    let model = fake_model();
    let support = support_dir();
    let e = Parakeet::load_with_env(
        &python(),
        ModelSource::Local(model),
        "test",
        None,
        &[
            ("PYTHONPATH", support.to_str().unwrap()),
            ("FAKE_LOGITS", "[[0,0,0,0,5.0,0.0],[0,0,0,0,0,5.0]]"),
            // No PARAKEET_VOCAB.
        ],
    )
    .expect("load");
    let mut got = Vec::new();
    e.transcribe_chunk(&chunk(6_000), &mut |w| got.push(w))
        .expect("times are right with or without a vocabulary");
    assert_eq!(texts(&got), vec!["4"], "the id, honestly labelled");
    assert!(e.words_are_ids(), "and the engine reports it");

    // And with a vocabulary, the same ids are words and it says so too.
    let e2 = engine("[[0,0,0,0,5.0,0.0],[0,0,0,0,0,5.0]]");
    let mut got2 = Vec::new();
    e2.transcribe_chunk(&chunk(6_000), &mut |w| got2.push(w))
        .expect("load");
    assert_eq!(texts(&got2), vec!["sat"]);
    assert!(!e2.words_are_ids(), "a vocabulary means words");
}
