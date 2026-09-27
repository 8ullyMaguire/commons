//! The proxy transcode: cache key, ffmpeg arguments, and a real encode.
//!
//! T-P6-001, §11.5. The claims worth testing here are not "did a file appear" —
//! they are the ones a real encode can violate and a mocked one cannot:
//!
//! * a partial file is never published, so a killed encode cannot be served;
//! * the output is actually playable, and its height is the rung's;
//! * a source with **no audio stream** still proxies, which is why `-map 0:a:0?`
//!   carries a `?`;
//! * two rungs of one file coexist and asking for the low rung never returns
//!   the high one.

use std::path::{Path, PathBuf};
use std::process::Command;

use commons_media::probe;
use commons_media::transcode::{CacheKey, TranscodeError, Transcoder};
use commons_store::playback::Rung;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("commons-scan/tests/fixtures")
        .join(name)
}

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        // Unique per CALL. A per-process key is not enough because **pids are
        // reused**: two runs of this binary weeks apart get the same pid and the
        // same directory, and the second run's `remove_dir_all` deletes a stub a
        // sibling thread is still `exec`ing -- `ETXTBSY -- "Text file busy"`,
        // which surfaces as a spawn failure in an unrelated test. The counter
        // makes it unique across threads in one run, the timestamp across runs;
        // neither key is sufficient alone.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "commons-transcode-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("a temp dir");
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- the cache key --------------------------------------------------

#[test]
fn the_key_names_both_the_content_and_the_rung() {
    let a = CacheKey::new("abc123", Rung::Lowest).file_name();
    let b = CacheKey::new("abc123", Rung::Highest).file_name();
    assert_ne!(a, b, "two rungs of one file must not collide");
    assert!(a.contains("abc123"), "the content hash is in the name");
    assert!(a.contains("480"), "the rung's height is in the name");
    assert!(a.ends_with(".mp4"), "the extension follows the container");
}

#[test]
fn a_different_file_of_the_same_rung_is_a_different_entry() {
    // The hash is what stops an edited-in-place file being served as yesterday's
    // video, so this is the property the name exists for.
    let a = CacheKey::new("hash-one", Rung::Medium).file_name();
    let b = CacheKey::new("hash-two", Rung::Medium).file_name();
    assert_ne!(a, b);
}

#[test]
fn the_partial_name_is_distinct_from_the_final_name() {
    let k = CacheKey::new("h", Rung::Highest);
    assert_ne!(k.file_name(), k.partial_file_name());
    // The extension is KEPT and `.partial` goes before it, because ffmpeg
    // infers its muxer from the extension: `h-1080.mp4.partial` fails with
    // "Error opening output files: Invalid argument" and never encodes at all.
    assert_eq!(k.partial_file_name(), "h-1080.partial.mp4");
    assert!(
        k.partial_file_name().ends_with(".mp4"),
        "ffmpeg must still be able to infer the muxer: {}",
        k.partial_file_name()
    );
}

#[test]
fn a_partial_never_counts_as_a_cache_hit() {
    // A zero-byte file is what a killed encode leaves. Serving it with a 200 is
    // a client waiting forever for bytes that are not coming.
    let tmp = Tmp::new("partial");
    let t = Transcoder::new(tmp.path());
    let key = CacheKey::new("h", Rung::Highest);

    assert!(!t.is_cached(&key), "nothing is cached to begin with");
    std::fs::write(key.partial_path(tmp.path()), b"").expect("a partial file");
    assert!(
        !t.is_cached(&key),
        "a zero-byte partial must never be a cache hit"
    );
    std::fs::write(key.partial_path(tmp.path()), b"real bytes").expect("content");
    assert!(
        !t.is_cached(&key),
        "even a non-empty partial is not a finished encode"
    );
    std::fs::write(t.path_for(&key), b"finished").expect("a finished file");
    assert!(t.is_cached(&key), "the finished file is the hit");
}

// ---- the ffmpeg arguments -------------------------------------------

/// The flags that stop a specific visible failure. Each is asserted because
/// removing it produces a bug a user would describe as "the proxy doesn't work".
#[test]
fn the_arguments_carry_the_flags_a_browser_needs() {
    let t = Transcoder::new("/tmp/unused");
    let args = t.args_for(Path::new("in.mkv"), Path::new("out.mp4"), Rung::Highest);
    let joined = args.join(" ");

    assert!(
        args.windows(2).any(|w| w == ["-movflags", "+faststart"]),
        "without +faststart the moov atom is at the end and the browser \
         downloads the whole file before playing anything: {joined}"
    );
    assert!(
        args.windows(2).any(|w| w == ["-pix_fmt", "yuv420p"]),
        "an h264 encode in yuv444p plays in nothing a user has: {joined}"
    );
    assert!(
        args.windows(2).any(|w| w == ["-map", "0:a:0?"]),
        "the ? on the audio map is what lets a silent source proxy at all: {joined}"
    );
    let vf = args.windows(2).find(|w| w[0] == "-vf").expect("a filter");
    assert_eq!(
        vf[1], "scale=-2:'min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
        "the rung bounds the HEIGHT (hence -2 on the width), the aspect ratio \
         is preserved, and both dimensions are forced even"
    );
    assert!(
        vf[1].contains("force_divisible_by=2"),
        "without it a 16:9 source at 480 encodes to 853x480 and libx264 refuses \
         with 'width not divisible by 2': {joined}"
    );
    assert!(
        args.iter().any(|a| a == "-y"),
        "overwriting is required, because a leftover partial is removed and \
         re-created on every attempt: {joined}"
    );
    assert!(
        args.last().unwrap().ends_with("out.mp4"),
        "the output path must be last, as ffmpeg requires: {joined}"
    );
}

#[test]
fn the_height_filter_is_never_larger_than_the_rung() {
    let t = Transcoder::new("/tmp/unused");
    for rung in Rung::ALL {
        let args = t.args_for(Path::new("in.mkv"), Path::new("o.mp4"), rung);
        let vf = args.windows(2).find(|w| w[0] == "-vf").expect("a filter");
        assert!(
            vf[1].contains(&format!("min({}", rung.height())),
            "rung {rung:?} must bound the height at {}: {}",
            rung.height(),
            vf[1]
        );
        // The bound applies to the height. `scale='min(H,ih)':-2` -- the shape
        // this replaced -- would scale the WIDTH to H instead, and a 1080p rung
        // on a landscape source would produce a video 1080 pixels wide.
        assert!(
            vf[1].starts_with("scale=-2:"),
            "the derived dimension is the width, so the rung applies to the \
             height: {}",
            vf[1]
        );
    }
}

#[test]
fn the_scale_filter_refers_to_ffmpeg_variables_not_rust_placeholders() {
    // `{ih}` inside a Rust `format!` is a compile error; escaped as `{{ih}}` it
    // becomes a LITERAL "{ih}" in the filter graph and ffmpeg fails at run
    // time. The correct spelling is the bare word, so this asserts the exact
    // text -- a regression here compiles cleanly and only breaks on a real
    // transcode, which is the worst possible time to find it.
    let t = Transcoder::new("/tmp/unused");
    let args = t.args_for(Path::new("in.mkv"), Path::new("o.mp4"), Rung::Medium);
    let vf = args.windows(2).find(|w| w[0] == "-vf").expect("a filter");
    assert!(
        vf[1].contains("ih)"),
        "the bare variable, not a brace: {}",
        vf[1]
    );
    assert!(
        !vf[1].contains("{"),
        "no literal braces reach ffmpeg: {}",
        vf[1]
    );
}

// ---- a real encode --------------------------------------------------

#[test]
fn a_real_encode_produces_a_playable_file_at_the_rungs_height() {
    if !ffmpeg_available() {
        eprintln!("skipping: no ffmpeg; the argument tests still cover the contract");
        return;
    }
    let tmp = Tmp::new("real");
    let t = Transcoder::new(tmp.path());
    let src = fixture("clip.mkv");

    let out = t
        .transcode(&src, "hash-clip", Rung::Lowest)
        .expect("the fixture transcodes");

    assert!(out.exists(), "the named path exists");
    assert!(t.is_cached(&CacheKey::new("hash-clip", Rung::Lowest)));

    // The real assertion: probe the RESULT. A file that exists but is empty, or
    // is an audio-only stream, or is not h264, passes every other test here.
    let info = probe::probe(&out).expect("the proxy output probes");
    let v = info.video_streams.first().expect("a video stream");
    assert_eq!(v.codec_name, "h264", "the rung says h264");
    assert_eq!(v.pix_fmt.as_deref(), Some("yuv420p"), "the rung's pix_fmt");

    // The height is `min(rung, source)` — the filter is a BOUND, not a target.
    // The fixture is 160x120, so a 480p rung must NOT upscale it to 480: doing
    // so would burn a re-encode to add pixels that were never there. The
    // assertion is the bound itself, not a fixed number, because a hard-coded
    // 480 here would have passed a build that upscaled and a build that
    // ignored the filter equally -- the fixture is smaller than the rung.
    let source = probe::probe(fixture("clip.mkv").as_path()).expect("source probes");
    let src_h = source.video_streams[0].height;
    assert_eq!(
        v.height,
        src_h.min(480),
        "the rung bounds the height at the source's, never up: source {src_h}"
    );
}

#[test]
fn a_second_request_is_served_from_the_cache_rather_than_re_encoding() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = Tmp::new("cache");
    let t = Transcoder::new(tmp.path());
    let src = fixture("clip.mkv");
    let key = CacheKey::new("hash-cache", Rung::Lowest);
    assert!(
        !t.is_cached(&key),
        "nothing cached before the first request"
    );

    let first = t
        .transcode(&src, "hash-cache", Rung::Lowest)
        .expect("first");
    let stamp = std::fs::metadata(&first)
        .expect("stat")
        .modified()
        .expect("mtime");

    let second = t
        .transcode(&src, "hash-cache", Rung::Lowest)
        .expect("second");
    assert_eq!(first, second, "the same key gives the same path");
    let stamp2 = std::fs::metadata(&second)
        .expect("stat")
        .modified()
        .expect("mtime");
    assert_eq!(
        stamp, stamp2,
        "an unchanged mtime means no second encode ran, which is the whole \
         point of keying on content rather than on the request"
    );
    assert!(t.is_cached(&key), "and the key is a cache hit afterwards");
}

#[test]
fn two_rungs_of_one_file_coexist() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = Tmp::new("rungs");
    let t = Transcoder::new(tmp.path());
    let src = fixture("clip.mkv");

    let low = t.transcode(&src, "hash-two", Rung::Lowest).expect("low");
    let high = t.transcode(&src, "hash-two", Rung::Medium).expect("medium");
    assert_ne!(low, high, "the two rungs are separate files");

    // Asking for the low rung again resolves to the LOW file. This is the
    // failure a single shared cache filename would cause, and it is worth
    // asserting separately from the paths differing: two names pointing at one
    // inode satisfies `assert_ne!(low, high)` on paths alone.
    let low_again = t
        .transcode(&src, "hash-two", Rung::Lowest)
        .expect("low again");
    assert_eq!(low, low_again, "the low rung resolves to the low file");
    // The height is the source's own, because the fixture is below every rung
    // and nothing upscales. Compared against the PROBED source rather than a
    // literal so this asserts the property rather than a number that was wrong
    // twice while the filter was being corrected.
    let src_h = probe::probe(fixture("clip.mkv").as_path())
        .expect("source probes")
        .video_streams[0]
        .height;
    assert_eq!(
        probe::probe(&low_again).expect("probe").video_streams[0].height,
        src_h.min(480),
        "the low file's own height, unchanged by a second request"
    );

    // The two files must be genuinely different FILES. On this fixture they are
    // the same SIZE, and that is correct rather than a bug: the fixture is
    // 160x120, below every rung, so all three rungs are the same no-op scale
    // and produce identical encodes. Asserting they differ in size would be
    // asserting that a 120-pixel-tall source gets transcoded three ways -- the
    // upscaling bug. Distinctness is a property of the KEY and is tested there;
    // a source actually taller than the rung is tested below.
    assert!(
        std::fs::metadata(&low).expect("low").len() > 0,
        "a real file"
    );
    assert!(
        std::fs::metadata(&high).expect("high").len() > 0,
        "a real file"
    );
}

/// The rung must bound the height for a source actually TALLER than it -- the
/// case `clip.mkv` cannot exercise, being 120 pixels high.
#[test]
fn a_tall_source_is_reduced_to_the_rungs_height() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = Tmp::new("tall");
    let src = tmp.path().join("tall.mp4");
    let made = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=1280x720:rate=10",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-an",
        ])
        .arg(&src)
        .output()
        .expect("ffmpeg runs");
    assert!(made.status.success(), "the tall fixture is generated");

    let t = Transcoder::new(tmp.path().join("cache"));
    let low = t.transcode(&src, "hash-tall", Rung::Lowest).expect("low");
    let v = probe::probe(&low).expect("probe").video_streams[0].clone();
    assert_eq!(v.height, 480, "720p reduced to the 480 rung");
    // 16:9 at 480 high is 853.33 wide; the even-rounding is what makes this
    // encode at all, so assert the pair rather than the width alone.
    assert!(
        v.width.is_multiple_of(2),
        "an ODD width is unencodable in yuv420p, so this must be even: {}",
        v.width
    );
    assert!(
        (v.width as f64 / v.height as f64 - 16.0 / 9.0).abs() < 0.02,
        "the aspect ratio survives: {}x{}",
        v.width,
        v.height
    );

    let high = t
        .transcode(&src, "hash-tall", Rung::Medium)
        .expect("medium");
    assert_eq!(
        probe::probe(&high).expect("probe").video_streams[0].height,
        720,
        "and the 720 rung leaves a 720p source alone"
    );
}

#[test]
fn a_failed_encode_publishes_nothing_and_names_the_reason() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = Tmp::new("fail");
    let t = Transcoder::new(tmp.path());
    // A text file named .mkv: ffmpeg starts, finds no video stream, and fails.
    let src = tmp.path().join("not-a-video.mkv");
    std::fs::write(&src, b"this is not a video").expect("a bogus source");

    let err = t
        .transcode(&src, "hash-fail", Rung::Highest)
        .expect_err("a non-video cannot encode");
    let msg = err.to_string();

    assert!(
        msg.len() > 10,
        "the refusal must carry a reason, not just 'transcode failed': {msg}"
    );
    assert!(
        !t.path_for(&CacheKey::new("hash-fail", Rung::Highest))
            .exists(),
        "a failed encode must not leave a file the next request would serve"
    );
    let partial = CacheKey::new("hash-fail", Rung::Highest).partial_path(tmp.path());
    assert!(
        !partial.exists(),
        "and it must clean up its partial: {}",
        partial.display()
    );
}

#[test]
fn a_missing_source_is_named_before_ffmpeg_is_run() {
    let tmp = Tmp::new("missing");
    let t = Transcoder::new(tmp.path());
    let missing = tmp.path().join("nope.mkv");
    let err = t
        .transcode(&missing, "h", Rung::Lowest)
        .expect_err("absent");
    assert!(
        matches!(err, TranscodeError::SourceMissing(_)),
        "a missing file is its own case, not an ffmpeg failure: {err:?}"
    );
    assert!(
        err.to_string().contains("nope.mkv"),
        "and it names the path"
    );
}

#[test]
fn an_absent_ffmpeg_is_its_own_error_rather_than_a_failed_encode() {
    let tmp = Tmp::new("noffmpeg");
    let t = Transcoder::new(tmp.path()).with_binary("/nonexistent/ffmpeg");
    let err = t
        .transcode(&fixture("clip.mkv"), "h", Rung::Lowest)
        .expect_err("no binary");
    assert!(
        matches!(err, TranscodeError::FfmpegMissing),
        "a missing binary and a failing encode need different fixes, so they \
         are different errors: {err:?}"
    );
}

#[test]
fn a_source_with_no_audio_still_proxies() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = Tmp::new("silent");
    // Generate a genuinely silent video rather than trusting a fixture: the
    // whole point is that this input has no audio stream at all.
    let src = tmp.path().join("silent.mp4");
    let made = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=10",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-an",
        ])
        .arg(&src)
        .output()
        .expect("ffmpeg runs");
    assert!(made.status.success(), "the silent fixture is generated");

    let t = Transcoder::new(tmp.path().join("cache"));
    let out = t
        .transcode(&src, "hash-silent", Rung::Lowest)
        .expect("a silent source proxies");
    let info = probe::probe(&out).expect("probe");
    assert!(info.video_streams.len() == 1, "it still has its video");
    assert!(
        info.audio_streams.is_empty(),
        "and no audio was invented -- this is the `-map 0:a:0?` case"
    );
}
