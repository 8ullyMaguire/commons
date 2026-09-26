//! T-P2-006 acceptance, part two: the encoder and thread settings reach real
//! ffmpeg and change real output (#894, #819).
//!
//! The unit tests in `encode.rs` prove the argument *lists* are right. This
//! file proves they arrive: that a configured format is the format of the
//! bytes ffmpeg wrote, that a lower quality really is a smaller file, and that
//! a thread count really appears on the command line *in the position ffmpeg
//! applies it*.
//!
//! That last one is the subtle half. `-threads` after the input is accepted
//! silently and applied to the output encoder instead of the run — no warning,
//! no error, and a setting that appears to do nothing. A test that only
//! compares argument lists passes with the flag in the wrong place forever, so
//! this intercepts the command with a script that records its own argv.

use commons_media::encode::{EncodeSettings, OutputFormat, ThreadCount};
use commons_media::probe::MediaInfo;
use commons_media::thumbs::{Generator, Kind};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The ffmpeg under test.
///
/// Falls back to `PATH` rather than panicking, matching the convention in
/// `hwaccel_acceptance.rs`. A missing environment variable is a missing
/// prerequisite, and a prerequisite should be reported by the test that needs
/// it — not by `expect` in a helper, which turns "this host has no ffmpeg"
/// into nine unrelated panics.
fn ffmpeg() -> String {
    std::env::var("COMMONS_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string())
}

fn info(duration_ms: u64) -> MediaInfo {
    MediaInfo {
        duration_ms,
        ..Default::default()
    }
}

/// A real H.264 video, generated rather than committed, so the decode path is
/// genuinely exercised.
fn make_video(dir: &Path, seconds: u32) -> PathBuf {
    if !ffmpeg_present() {
        panic!("no ffmpeg available; set COMMONS_FFMPEG");
    }
    let out = dir.join("source.mp4");
    let status = Command::new(ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=320x240:rate=10:duration={seconds}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&out)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "could not build a test video");
    out
}

/// Is there an ffmpeg to test against at all?
fn ffmpeg_present() -> bool {
    Command::new(ffmpeg())
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The encoder ffmpeg reports for a file, e.g. `webp` or `mjpeg`.
fn probe_encoder(path: &Path) -> String {
    let out = Command::new(ffmpeg())
        .args(["-hide_banner", "-i"])
        .arg(path)
        .output()
        .expect("ffmpeg runs");
    let text = String::from_utf8_lossy(&out.stderr);
    let line = text
        .lines()
        .find(|l| l.contains("Stream #") && l.contains("Video:"))
        .unwrap_or_else(|| panic!("no video stream in {}:\n{text}", path.display()));
    line.split("Video: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        // ffmpeg writes "png, 320x240 ..." -- the comma is a separator, not
        // part of the name, and leaving it in makes every exact comparison
        // fail for a reason that is not about the encoder.
        .trim_end_matches(',')
        .to_string()
}

fn generator() -> Generator {
    Generator::with_binaries(ffmpeg(), "ffprobe")
}

/// Render one thumbnail at `width` with `settings`; returns the byte count.
fn render(dir: &Path, name: &str, settings: EncodeSettings, width: u32) -> u64 {
    let src = make_video(dir, 2);
    let out = dir.join(name);
    let mut g = generator();
    g.set_encode(settings).expect("settings are valid");
    let result = g.generate(&src, &out, Kind::Thumbnail { width }, &info(2000));
    assert!(result.is_wrote(), "generation failed for {name}");
    std::fs::metadata(&out).unwrap().len()
}

#[test]
fn a_configured_format_is_the_format_of_the_bytes_written() {
    // The mislabelled-file bug, end to end. Ask for JPEG, get JPEG — in a
    // .jpg file whose contents ffmpeg agrees are mjpeg, not a WebP stream
    // behind a JPEG extension.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("frame.jpg");
    let src = make_video(dir.path(), 2);
    let mut g = generator();
    g.set_encode(EncodeSettings {
        format: OutputFormat::Jpeg,
        quality: 90,
        ..Default::default()
    })
    .unwrap();
    let result = g.generate(&src, &out, Kind::Thumbnail { width: 320 }, &info(2000));
    assert!(result.is_wrote(), "jpeg generation failed");

    let enc = probe_encoder(&out);
    assert!(
        enc.starts_with("mjpeg"),
        "asked for jpeg, ffmpeg wrote {enc} — the extension would lie"
    );
}

#[test]
fn the_default_is_still_webp() {
    // Whatever happens to the settings, the pipeline's default output is
    // unchanged, because every existing artifact in the wild depends on it.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("frame.webp");
    let src = make_video(dir.path(), 2);
    let result = generator().generate(&src, &out, Kind::Thumbnail { width: 320 }, &info(2000));
    assert!(result.is_wrote());
    let enc = probe_encoder(&out);
    assert!(enc.contains("webp"), "default wrote {enc}, expected webp");
}

#[test]
fn quality_actually_moves_the_file_size() {
    // A setting that reaches the argument list but not the encoder would pass
    // every argument-shape test in encode.rs. This one compares bytes.
    let dir = tempfile::tempdir().unwrap();
    let low = render(
        dir.path(),
        "low.webp",
        EncodeSettings {
            quality: 5,
            ..Default::default()
        },
        640,
    );
    let high = render(
        dir.path(),
        "high.webp",
        EncodeSettings {
            quality: 100,
            ..Default::default()
        },
        640,
    );
    assert!(
        low < high,
        "quality 5 wrote {low} bytes and quality 100 wrote {high}; \
         the knob is not reaching the encoder"
    );
}

#[test]
fn lossless_produces_a_larger_file() {
    let dir = tempfile::tempdir().unwrap();
    let lossy = render(
        dir.path(),
        "lossy.webp",
        EncodeSettings {
            quality: 100,
            ..Default::default()
        },
        640,
    );
    let lossless = render(
        dir.path(),
        "lossless.webp",
        EncodeSettings {
            quality: 100,
            lossless: true,
            ..Default::default()
        },
        640,
    );
    assert!(
        lossless > lossy,
        "lossless webp ({lossless}) is not larger than quality-100 lossy ({lossy})"
    );
}

#[test]
fn png_output_really_is_png() {
    // `png` is in the config because a user will want lossless output, and it
    // is the one format whose quality axis does not exist. Both facts are only
    // checkable against real ffmpeg.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("frame.png");
    let src = make_video(dir.path(), 2);
    let mut g = generator();
    g.set_encode(EncodeSettings {
        format: OutputFormat::Png,
        ..Default::default()
    })
    .unwrap();
    let result = g.generate(&src, &out, Kind::Thumbnail { width: 320 }, &info(2000));
    assert!(result.is_wrote(), "png generation failed");
    assert_eq!(probe_encoder(&out), "png");
}

/// A stand-in for ffmpeg that records its argv, so argument *position* can be
/// asserted. Returns the script path and the recording path.
fn spy(dir: &Path) -> (PathBuf, PathBuf) {
    let script = dir.join("spy-ffmpeg.sh");
    let record = dir.join("argv.txt");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\n\
             for a in \"$@\"; do last=\"$a\"; done\n: > \"$last\"\n",
            record.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut p = std::fs::metadata(&script).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&script, p).unwrap();
    (script, record)
}

fn recorded_args(record: &Path) -> Vec<String> {
    std::fs::read_to_string(record)
        .expect("the spy recorded its arguments")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_fixed_thread_count_reaches_the_command_line_before_the_input() {
    let dir = tempfile::tempdir().unwrap();
    let (script, record) = spy(dir.path());
    let out = dir.path().join("ignored.webp");

    let mut g = Generator::with_binaries(script, "ffprobe");
    g.set_encode(EncodeSettings {
        threads: ThreadCount::Fixed(5),
        ..Default::default()
    })
    .unwrap();
    let result = g.generate(
        &make_video(dir.path(), 1),
        &out,
        Kind::Thumbnail { width: 120 },
        &info(1000),
    );
    assert!(
        result.is_wrote(),
        "the spy should exit 0 and produce the file"
    );

    let args = recorded_args(&record);
    let threads_at = args.iter().position(|a| a == "-threads");
    assert_eq!(
        threads_at,
        Some(0),
        "-threads must be the first argument on the command line: {args:?}"
    );
    assert_eq!(
        args.get(1).map(String::as_str),
        Some("5"),
        "wrong count: {args:?}"
    );

    let input_at = args
        .iter()
        .position(|a| a == "-i")
        .unwrap_or_else(|| panic!("no -i in {args:?}"));
    let threads_at = threads_at.expect("-threads present, asserted above");
    assert!(
        threads_at < input_at,
        "-threads at {threads_at:?} is after -i at {input_at}: ffmpeg would apply it \
         to the output encoder only, silently"
    );
}

#[test]
fn the_thread_count_appears_exactly_once() {
    // A duplicated -threads is the shape a "prepend in two places" refactor
    // takes, and ffmpeg's last-one-wins means it half-works: the value is
    // right and the setting is applied to the wrong scope. Counting the
    // occurrences is the only way to see it.
    let dir = tempfile::tempdir().unwrap();
    let (script, record) = spy(dir.path());
    let out = dir.path().join("ignored.webp");

    let mut g = Generator::with_binaries(script, "ffprobe");
    g.set_encode(EncodeSettings {
        threads: ThreadCount::Fixed(3),
        ..Default::default()
    })
    .unwrap();
    let result = g.generate(
        &make_video(dir.path(), 1),
        &out,
        Kind::Thumbnail { width: 120 },
        &info(1000),
    );
    assert!(result.is_wrote());

    let args = recorded_args(&record);
    let count = args.iter().filter(|a| a.as_str() == "-threads").count();
    assert_eq!(count, 1, "-threads appears {count} times: {args:?}");
    // And the value follows the flag, immediately, so "3" cannot be read as
    // something else by a later reader of the command line.
    let at = args.iter().position(|a| a == "-threads").unwrap();
    assert_eq!(args.get(at + 1).map(String::as_str), Some("3"), "{args:?}");
}

#[test]
fn a_single_thread_request_emits_no_thread_flag() {
    // Emitting `-threads 1` is not the same as omitting it: it also suppresses
    // slice threading that ffmpeg would otherwise choose for itself.
    let dir = tempfile::tempdir().unwrap();
    let (script, record) = spy(dir.path());
    let out = dir.path().join("ignored.webp");

    let mut g = Generator::with_binaries(script, "ffprobe");
    g.set_encode(EncodeSettings {
        threads: ThreadCount::Fixed(1),
        ..Default::default()
    })
    .unwrap();
    let result = g.generate(
        &make_video(dir.path(), 1),
        &out,
        Kind::Thumbnail { width: 120 },
        &info(1000),
    );
    assert!(result.is_wrote());

    let args = recorded_args(&record);
    assert!(
        !args.iter().any(|a| a == "-threads"),
        "a single-threaded request emitted -threads: {args:?}"
    );
}

#[test]
fn auto_threads_resolve_from_the_machine_not_a_literal() {
    // The old code hardcoded 4. This asserts the value is a function of the
    // machine, so a 2-core NAS no longer runs 4 ffmpeg threads.
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let resolved = ThreadCount::Auto.resolve(cores);
    assert!((1..=8).contains(&resolved), "auto resolved to {resolved}");
    if cores > 1 && cores != 5 {
        assert_ne!(
            resolved, 4,
            "auto resolved to the old hardcoded 4 on a {cores}-core machine"
        );
    }
}

#[test]
fn an_invalid_setting_is_refused_and_the_old_one_kept() {
    let mut g = generator();
    let err = g
        .set_encode(EncodeSettings {
            format: OutputFormat::Jpeg,
            lossless: true,
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.contains("lossless"), "{err}");
    // The generator still holds valid settings, so a rejected change cannot
    // leave it in a state where the next generation is broken.
    assert_eq!(g.encode_settings().format, OutputFormat::Webp);
    assert!(g.encode_settings().validate().is_ok());
}
