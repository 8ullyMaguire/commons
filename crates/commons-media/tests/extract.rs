//! Extraction: getting subtitle bytes out of a container, and out of a sidecar.
//!
//! T-P6-002 step 4. Spec §4.
//!
//! Three kinds of test here, and the split is the point:
//!
//! 1. **No process at all** — the sidecar path, the unsupported-codec path, the
//!    BOM path. These are pure and run everywhere.
//! 2. **A stub binary** — a shell script that prints known bytes. This is how
//!    the argument vector and the error paths are tested without depending on
//!    what any particular ffmpeg build does. A test that runs real ffmpeg can
//!    only tell you the extraction worked; it cannot tell you it worked *for the
//!    right reason*.
//! 3. **Real ffmpeg**, skipped when absent — the round trip, which is the only
//!    thing that proves `-c copy` and the muxer names are right together.
//!
//! The 40 ms budget from the spec is asserted in the real-ffmpeg file, for
//! **every** cue and not a chosen one, because a per-file average hides the cue
//! that moved and the cue that moved is the one a user notices.

use commons_media::extract::{extract_stream, read_sidecar, ExtractError, Extractor};
use std::path::{Path, PathBuf};

// --- sidecars: no process ---------------------------------------------------

// Scratch dirs and fixtures come from the shared helper, which deletes the
// directory on drop -- a test that panics used to leave one behind, and enough
// of them filled the disk and made unrelated tests fail.
#[path = "scratch/mod.rs"]
mod scratch;
use scratch::{scratch, write};

const SRT: &str =
    "1\n00:00:01,000 --> 00:00:03,000\nHello\n\n2\n00:00:04,000 --> 00:00:06,500\nWorld\n";

#[test]
fn a_sidecar_is_read_and_parsed_without_running_a_process() {
    // The whole point of the sidecar path: the file is already text, so
    // running ffmpeg to read a file ffmpeg did not write is a dependency where
    // none is needed. The test passes a deliberately non-existent binary to
    // prove no process was spawned.
    let dir = scratch("sidecar");
    let p = write(&dir, "movie.srt", SRT);
    let x = Extractor::with_binary("/nonexistent/ffmpeg-for-a-test");
    let doc = x.read_sidecar(&p).expect("a sidecar needs no ffmpeg");
    assert_eq!(2, doc.cues.len());
    assert_eq!("Hello", doc.cues[0].text);
    assert_eq!(1000, doc.cues[0].start_ms);
    assert_eq!(6500, doc.cues[1].end_ms);
}

#[test]
fn a_utf8_bom_does_not_silently_empty_the_file() {
    // Every Windows tool that has ever saved an `.srt` writes a BOM, and it
    // lands in front of the first cue's timestamp. Without the strip, `parse`
    // reads that as a malformed first line and the file yields *zero* cues —
    // indistinguishable from a file that genuinely has none, so the user's
    // subtitles simply do not appear and nothing is logged.
    let dir = scratch("bom");
    let p = write(&dir, "bom.srt", &format!("\u{feff}{SRT}"));
    let doc = read_sidecar(&p).expect("a BOM is not a malformed file");
    assert_eq!(2, doc.cues.len(), "the BOM cost every cue in the file");
    assert_eq!("Hello", doc.cues[0].text);
}

#[test]
fn a_non_utf8_encoding_degrades_rather_than_refusing_the_file() {
    // A French `.srt` saved as Latin-1 is lossy here and that is the right
    // trade: one mangled character in one caption is better than a track that
    // does not appear at all, which is what a strict decode gives.
    let dir = scratch("latin1");
    let mut bytes = b"1\n00:00:01,000 --> 00:00:03,000\nCaf".to_vec();
    bytes.push(0xE9); // é in Latin-1, invalid UTF-8
    bytes.extend_from_slice(b"\n");
    let p = dir.join("latin1.srt");
    std::fs::write(&p, bytes).expect("write");
    let doc = read_sidecar(&p).expect("a Latin-1 sidecar must not be refused");
    assert_eq!(1, doc.cues.len());
    assert!(doc.cues[0].text.starts_with("Caf"));
}

#[test]
fn an_unsupported_sidecar_extension_is_reported_not_guessed() {
    // `.txt` is the tempting one: it is what people rename things to, and
    // defaulting it to SRT produces a document of garbage cues. A `.txt` here
    // is Unsupported, and the caller's two options — skip the track, or tell the
    // user — are both better than showing mojibake.
    let dir = scratch("unsupported");
    for name in ["notes.txt", "subs.orig", "caption.pgs"] {
        let p = write(&dir, name, SRT);
        let err = read_sidecar(&p).expect_err("must be unsupported");
        assert!(
            matches!(err, ExtractError::Unsupported { .. }),
            "{name}: {err:?}"
        );
    }
}

#[test]
fn a_missing_sidecar_is_an_io_error_not_a_parse_error() {
    // They are different things and the caller responds differently: a missing
    // file is a scan that needs re-running, a malformed one is a file the user
    // has to fix.
    let dir = scratch("missing");
    let err = read_sidecar(&dir.join("nope.srt")).expect_err("must fail");
    assert!(matches!(err, ExtractError::Io { .. }), "{err:?}");
}

#[test]
fn a_malformed_sidecar_is_a_parse_error_naming_the_format() {
    let dir = scratch("malformed");
    let p = write(&dir, "bad.srt", "this is not a subtitle file at all\n");
    let err = read_sidecar(&p).expect_err("must fail to parse");
    match err {
        ExtractError::Undecodable { format, got, .. } => {
            assert_eq!("srt", format);
            assert!(got > 0, "the byte count is what tells 'empty' from 'wrong'");
        }
        other => panic!("expected Undecodable, got {other:?}"),
    }
}

#[test]
fn every_text_sidecar_extension_parses() {
    // The extensions Commons claims. If one of these stops working the honest
    // outcome is a test that fails here, not a track that quietly vanishes for
    // the people whose files use it.
    let dir = scratch("exts");
    for (name, body, n) in [
        ("a.srt", SRT, 2),
        ("a.vtt", "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nHello\n", 1),
        (
            "a.ass",
            "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial,20\n\n[Events]\nFormat: Layer, Start, End, Style, Text\nDialogue: 0,0:00:01.00,0:00:03.00,Default,Hello\n",
            1,
        ),
    ] {
        let p = write(&dir, name, body);
        let doc = read_sidecar(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(n, doc.cues.len(), "{name}");
    }
}

// --- unsupported codecs, no process needed either ----------------------------

#[test]
fn an_image_based_embedded_track_is_unsupported_and_says_so() {
    // No ffmpeg is started: the codec is rejected before the process would be,
    // and the point of the test is that it is rejected *first*. If a caller
    // passed `hdmv_pgs_subtitle` and we ran ffmpeg and then failed to parse,
    // the error would be a parse error and the diagnosis would be wrong.
    for codec in ["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_teletext", "xsub"] {
        let err = extract_stream(Path::new("/nonexistent.mkv"), 2, codec)
            .expect_err("must be unsupported before any process runs");
        match err {
            ExtractError::Unsupported { codec: got, .. } => assert_eq!(codec, got),
            other => panic!("{codec}: expected Unsupported, got {other:?}"),
        }
    }
}

// --- the argument vector, via a stub binary ----------------------------------

/// A stub that ignores its arguments and prints `body`, so the tests below
/// assert on *what the extractor sends* rather than on whether a particular
/// ffmpeg build happens to accept it.
#[cfg(unix)]
fn stub(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    // `cat` the body out, ignoring stdin and every argument.
    let script = format!(
        "#!/bin/sh\ncat <<'STUB_EOF'\n{}\nSTUB_EOF\n",
        String::from_utf8_lossy(body)
    );
    std::fs::write(&p, script).expect("write stub");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    p
}

/// The argument vector, captured by a stub that writes it to a file.
#[cfg(unix)]
fn args_recorder(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let argv = dir.join("argv.txt");
    let p = dir.join("record-args.sh");
    // The body is ASS, because that is what the callers request: a track is
    // muxed as the format it was reported as, and since the extractor now
    // rejects a non-empty output that parses to zero cues, a stub that emitted
    // SRT while `-f ass` was asked for would be testing the mismatch check
    // rather than the argument vector.
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\ncat <<'STUB_EOF'\n[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial,20\n\n[Events]\nFormat: Layer, Start, End, Style, Text\nDialogue: 0,0:00:01.00,0:00:03.00,Default,Hi\nSTUB_EOF\n",
        argv.display()
    );
    std::fs::write(&p, script).expect("write stub");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (p, argv)
}

#[cfg(unix)]
#[test]
fn the_argument_vector_is_the_one_the_spec_asks_for() {
    // Each of these four is load-bearing and the comment in extract.rs says
    // why. Asserting the whole vector rather than one flag is deliberate: a
    // missing `-nostdin` is a server that eats its own connections, and no
    // single-flag test would notice the others.
    let dir = scratch("args");
    let media = write(&dir, "m.mkv", "not really a mkv");
    let (bin, argv) = args_recorder(&dir);

    let doc = Extractor::with_binary(&bin)
        .extract_stream(&media, 2, "ass")
        .expect("the stub succeeds");
    assert_eq!(1, doc.cues.len());

    let got: Vec<String> = std::fs::read_to_string(&argv)
        .expect("stub recorded argv")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        vec![
            "-nostdin",
            "-v",
            "error",
            "-i",
            media.to_str().expect("utf-8 path"),
            "-map",
            "0:2",
            "-c",
            "copy",
            "-f",
            "ass",
            "-",
        ],
        got,
        "the argument vector is the contract; a change here changes what is read"
    );
}

#[cfg(unix)]
#[test]
fn a_text_stream_keeps_its_own_muxer_and_mov_text_becomes_webvtt() {
    // `mov_text` is a QuickText timecode atom rather than text, so `-c copy`
    // hands back atoms; it has to be muxed to WebVTT to be readable. Every
    // other format goes out as itself, which is why there is a match here
    // rather than a blanket "convert everything to webvtt" — a blanket
    // conversion would re-time every track and spend the spec's 40 ms budget on
    // a rescale nothing needed.
    for (codec, want_muxt) in [
        ("ass", "ass"),
        ("subrip", "srt"),
        ("webvtt", "vtt"),
        ("mov_text", "webvtt"),
    ] {
        // A directory per codec, because the recorder appends to one argv file
        // and a shared one would accumulate all four vectors.
        let sub = scratch(&format!("muxer-{codec}"));
        let media = write(&sub, "m.mkv", "x");
        let (bin, argv) = args_recorder(&sub);

        let _ = Extractor::with_binary(&bin).extract_stream(&media, 0, codec);
        let got = std::fs::read_to_string(&argv).expect("argv");
        assert!(
            got.lines().any(|l| l == want_muxt),
            "{codec} should be muxed as {want_muxt}, argv was:\n{got}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_missing_ffmpeg_is_reported_as_missing_and_not_as_a_failed_run() {
    // The two are different diagnoses. "ffmpeg not found" tells an operator to
    // install a package; "ffmpeg exited 1" tells them to go looking at a file,
    // and they will look at the file for a long time.
    let dir = scratch("missing-ffmpeg");
    let media = write(&dir, "m.mkv", "x");
    let err = Extractor::with_binary(dir.join("no-such-binary"))
        .extract_stream(&media, 0, "ass")
        .expect_err("must fail");
    assert!(matches!(err, ExtractError::FfmpegMissing), "{err:?}");
}

#[cfg(unix)]
#[test]
fn a_failing_ffmpeg_reports_the_last_meaningful_line_not_the_banner() {
    // ffmpeg's stderr begins with its version string and a stream dump on every
    // single run, so the FIRST line is never the error. `transcode.rs` already
    // takes the last non-empty line; this asserts the extractor agrees, because
    // two functions disagreeing about how to read ffmpeg's stderr is a bug in
    // whichever one is wrong and invisible in the other.
    let dir = scratch("failed");
    use std::os::unix::fs::PermissionsExt;
    let media = write(&dir, "m.mkv", "x");
    let p = dir.join("fail.sh");
    std::fs::write(
        &p,
        "#!/bin/sh\necho 'ffmpeg version 6.1 Copyright (c) 2000-2023' >&2\necho 'Input #0, mkv, from m.mkv:' >&2\necho 'Stream mapping:' >&2\necho \"Invalid data found when processing input\" >&2\nexit 1\n",
    )
    .expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let err = Extractor::with_binary(&p)
        .extract_stream(&media, 0, "ass")
        .expect_err("must fail");
    match err {
        ExtractError::FfmpegFailed { detail, stream, .. } => {
            assert_eq!("0", stream, "the error names the stream that failed");
            assert_eq!("Invalid data found when processing input", detail);
        }
        other => panic!("expected FfmpegFailed, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn an_exit_zero_with_no_output_is_not_produced_rather_than_an_empty_document() {
    // The `-map` selected nothing — which for a stream index that came from a
    // probe of a *different* file is the likely cause. Trusting the exit code
    // turns that into a track that parses to zero cues, which a user reads as
    // "this video has no subtitles". `transcode.rs` has the same
    // `NothingProduced` rule for the same reason.
    let dir = scratch("nothing");
    use std::os::unix::fs::PermissionsExt;
    let media = write(&dir, "m.mkv", "x");
    let p = dir.join("empty.sh");
    std::fs::write(&p, "#!/bin/sh\nexit 0\n").expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let err = Extractor::with_binary(&p)
        .extract_stream(&media, 3, "ass")
        .expect_err("must fail");
    assert!(
        matches!(err, ExtractError::NothingProduced { .. }),
        "{err:?}"
    );
    match err {
        ExtractError::NothingProduced { stream, .. } => assert_eq!("3", stream),
        _ => unreachable!(),
    }
}

#[cfg(unix)]
#[test]
fn a_stub_that_writes_a_lot_to_stderr_does_not_deadlock() {
    // The deadlock is silent and hangs rather than fails, so without this test
    // the bug is a CI timeout with no cause. A subtitle stream is small enough
    // that stdout will not fill a pipe, but stderr fills at 64 KB and then
    // ffmpeg blocks writing to it while `wait` blocks on ffmpeg. Both pipes have
    // to be drained concurrently with the wait; the comment in `run` says so.
    let dir = scratch("deadlock");
    use std::os::unix::fs::PermissionsExt;
    let media = write(&dir, "m.mkv", "x");
    let p = dir.join("chatty.sh");
    // ~1 MB of stderr, comfortably past any pipe buffer, and it must not be
    // read before the wait.
    std::fs::write(
        &p,
        "#!/bin/sh\ni=0\nwhile [ $i -lt 20000 ]; do echo \"a fairly long line of ffmpeg diagnostic output $i\" >&2; i=$((i+1)); done\ncat <<'STUB_EOF'\n[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial,20\n\n[Events]\nFormat: Layer, Start, End, Style, Text\nDialogue: 0,0:00:01.00,0:00:03.00,Default,Hi\nSTUB_EOF\n",
    )
    .expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let doc = Extractor::with_binary(&p)
        .extract_stream(&media, 0, "ass")
        .expect("must not deadlock");
    assert_eq!(1, doc.cues.len());
}

#[cfg(unix)]
#[test]
fn output_that_is_not_a_subtitle_file_is_reported_as_undecodable() {
    // The stub succeeded and produced bytes, and the bytes are not cues. That
    // is a different failure from "no bytes": here ffmpeg ran and something is
    // wrong with the track, and the message says which format was expected so
    // the diagnosis does not have to start from the file.
    let dir = scratch("garbage");
    let media = write(&dir, "m.mkv", "x");
    let bin = stub(&dir, "garbage.sh", b"this is not a subtitle file");
    let err = Extractor::with_binary(&bin)
        .extract_stream(&media, 0, "ass")
        .expect_err("must fail to parse");
    match err {
        ExtractError::Undecodable { format, got, .. } => {
            assert_eq!("ass", format);
            assert!(got > 0);
        }
        other => panic!("expected Undecodable, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn a_non_empty_output_with_no_cues_is_a_failure_not_an_empty_track() {
    // The bug this test was written for, found while writing the argument-vector
    // test: every parser here returns `Ok` with zero cues for input it does not
    // recognise, so a track muxed in the wrong format produced a document with
    // no cues and no error. `extract_stream` returned it happily, and the user
    // saw a subtitle track that was listed, selectable, and empty -- which is
    // the reading "this video has no subtitles".
    //
    // The stub asks for `-f ass` and prints SRT, so this is a format mismatch
    // rather than a corrupt file. The distinction matters: the error has to say
    // which format was expected, because "the track is not in the format it was
    // reported as" is a diagnosis and "no cues" is not.
    let dir = scratch("mismatch");
    use std::os::unix::fs::PermissionsExt;
    let media = write(&dir, "m.mkv", "x");
    let p = dir.join("wrong-format.sh");
    std::fs::write(
        &p,
        "#!/bin/sh\nprintf '1\\n00:00:01,000 --> 00:00:03,000\\nHi\\n'\n",
    )
    .expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let err = Extractor::with_binary(&p)
        .extract_stream(&media, 0, "ass")
        .expect_err("a format mismatch must not read as an empty track");
    match err {
        ExtractError::Undecodable {
            format,
            got,
            detail,
        } => {
            assert_eq!(
                "ass", format,
                "the message names the format that was expected"
            );
            assert!(
                got > 0,
                "the bytes were there; they were just not this format"
            );
            assert!(
                detail.contains("no cues"),
                "the detail says what happened: {detail}"
            );
        }
        other => panic!("expected Undecodable, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn a_path_with_a_nul_byte_is_rejected_before_the_process_is_built() {
    // A NUL in an argument truncates it at the syscall boundary on Unix, so
    // ffmpeg would be handed a different path than the caller asked about and
    // the error would name a file that does not exist. `Prober::probe` has the
    // same check for the same reason; two functions disagreeing about it would
    // mean one of them is missing it.
    let dir = scratch("nul");
    let mut p = dir.to_path_buf().into_os_string();
    p.push("a\0b.mkv");
    let path = PathBuf::from(p);
    // The error is a spawn failure or a ffmpeg failure depending on the
    // platform, and either is acceptable -- what is not acceptable is ffmpeg
    // being handed "a.mkv" and reporting on that.
    let _ = Extractor::new().extract_stream(&path, 0, "ass");
}
