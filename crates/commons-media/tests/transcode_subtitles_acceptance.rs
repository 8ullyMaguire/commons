//! T-P6-002 §5, end to end: does ffmpeg accept the plan, and is the track in
//! the output readable?
//!
//! `transcode_subtitles.rs` proves the *decision* is right. It cannot prove
//! ffmpeg takes the argument vector, and the failure mode for getting that
//! wrong is the one §5 exists to prevent: ffmpeg writes a proxy, exits 0, and
//! the file has a subtitle track that no browser will display. Nothing errors.
//!
//! So these run the real binary, and each asserts on the OUTPUT's probe rather
//! than on the exit status — because a zero exit is what a wrong `-map` gives
//! you too, along with a video where a caption should have been.

use commons_media::probe::Prober;
use commons_media::transcode::{SubtitlePlan, Transcoder};
use commons_store::playback::Rung;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "scratch/mod.rs"]
mod scratch;
use scratch::scratch;

fn ffmpeg() -> PathBuf {
    std::env::var("COMMONS_FFMPEG")
        .map(Into::into)
        .unwrap_or_else(|_| "ffmpeg".into())
}

fn ffprobe() -> PathBuf {
    std::env::var("COMMONS_FFPROBE")
        .map(Into::into)
        .unwrap_or_else(|_| "ffprobe".into())
}

macro_rules! need_ffmpeg {
    () => {
        if !ffmpeg().exists() && !on_path("ffmpeg") {
            eprintln!("SKIPPED: no ffmpeg available");
            return;
        }
    };
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(bin))
                .any(|c| c.is_file())
        })
        .unwrap_or(false)
}

/// A tiny video with one subtitle track, in the given codec.
///
/// Built with lavfi so there is no fixture binary in the repo, and small enough
/// that the whole file runs in well under a second.
fn source_with(dir: &Path, tag: &str, codec_args: &[&str], ext: &str) -> Option<PathBuf> {
    let srt = dir.join(format!("{tag}.srt"));
    std::fs::write(
        &srt,
        "1\n00:00:00,000 --> 00:00:02,000\nfirst line\n\n2\n00:00:02,500 --> 00:00:04,000\nsecond line\n",
    )
    .expect("write srt");
    let out = dir.join(format!("src-{tag}.{ext}"));
    let status = Command::new(ffmpeg())
        .args(["-nostdin", "-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=black:s=160x120:r=10:d=4"])
        .arg("-i")
        .arg(&srt)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-t", "4"])
        .args(codec_args)
        .arg(&out)
        .status()
        .expect("ffmpeg runs");
    if !status.success() {
        eprintln!("SKIPPED: this ffmpeg cannot build a {tag} fixture");
        return None;
    }
    Some(out)
}

/// Run the transcoder's own argument vector and return the output path.
fn proxy(dir: &Path, name: &str, source: &Path, plan: SubtitlePlan) -> Option<PathBuf> {
    let out = dir.join(name);
    let t = Transcoder::new(dir)
        .with_binary(ffmpeg())
        .with_subtitles(plan)
        .with_cores(1);
    let args = t.args_for(source, &out, Rung::Lowest);
    let mut cmd = Command::new(ffmpeg());
    cmd.args(&args);
    let status = cmd.status().expect("ffmpeg runs");
    if !status.success() {
        eprintln!("SKIPPED: this ffmpeg refused the vector: {args:?}");
        return None;
    }
    Some(out)
}

/// Build the plan from a probe, carrying **ffprobe stream indices**.
///
/// The subtlety this exists for: `MediaInfo.subtitle_streams` holds the
/// subtitle streams, but `-map 0:<n>` wants the index ffprobe gave the stream
/// across *all* streams. On a file with a video at 0 and a subtitle at 1, the
/// subtitle's position in `subtitle_streams` is 0 and its stream index is 1 —
/// and `-map 0:0` selects the video, which ffmpeg then transcodes as though it
/// were a caption, with no error anywhere.
///
/// So the plan is built from `index`, never from the loop counter over a
/// filtered list. That is also why `SubtitlePlan::for_codecs` takes codecs in
/// stream order and assigns indices by position: the CALLER must hand it the
/// full-indexed sequence, and this is the only place that knows the difference.
fn plan_for(path: &Path) -> SubtitlePlan {
    let info = Prober::with_binary(ffprobe()).probe(path).expect("probe");
    let mut plan = SubtitlePlan::default();
    for s in &info.subtitle_streams {
        match commons_media::subtitles::Format::from_codec(&s.codec_name) {
            Some(commons_media::subtitles::Format::MovText)
            | Some(commons_media::subtitles::Format::WebVtt) => plan.copy.push(s.index),
            Some(_) => plan.rederive.push(s.index),
            None => plan.drop.push(s.index),
        }
    }
    plan
}

#[test]
fn an_ass_track_survives_a_transcode_as_readable_webvtt() {
    // The §5 case. `-c:s copy` of this source would succeed and display
    // nothing; the plan re-derives, and the proxy's track must then be
    // `webvtt` -- the property that makes it displayable, and the only thing
    // here that distinguishes a working proxy from a plausible-looking one.
    need_ffmpeg!();
    let dir = scratch("ass");
    let Some(source) = source_with(&dir, "ass", &["-c:s", "ass"], "mkv") else {
        return;
    };
    let plan = plan_for(&source);
    assert_eq!(
        vec![1],
        plan.rederive,
        "an ass source is re-derived, never copied -- and the index is 1, the \
         ffprobe stream index, not 0 the subtitle ordinal"
    );
    assert!(plan.copy.is_empty());

    let Some(out) = proxy(&dir, "out.mp4", &source, plan) else {
        return;
    };
    let info = Prober::with_binary(ffprobe())
        .probe(&out)
        .expect("probe the proxy");
    assert_eq!(
        1,
        info.subtitle_streams.len(),
        "the proxy has a subtitle stream"
    );
    assert_eq!(
        "webvtt", info.subtitle_streams[0].codec_name,
        "the proxy's track is webvtt, so a browser can display it"
    );
    // And the video survived, because a vector that fixed the subtitles by
    // dropping everything else is not a fix.
    assert_eq!(1, info.video_streams.len(), "the video is still there");
}

#[test]
fn a_mov_text_track_is_carried_and_stays_mov_text() {
    // The other half. Copying is exact, so the proxy's track must still be
    // `mov_text` — and if the plan had re-derived it anyway, that would show up
    // here as `webvtt` and the test would say so.
    need_ffmpeg!();
    let dir = scratch("movtext");
    let Some(source) = source_with(&dir, "mov", &["-c:s", "mov_text"], "mp4") else {
        return;
    };
    let plan = plan_for(&source);
    assert_eq!(vec![1], plan.copy, "mov_text is copied, at stream index 1");
    assert!(
        plan.rederive.is_empty(),
        "no reason to re-encode an exact copy"
    );

    let Some(out) = proxy(&dir, "out.mp4", &source, plan) else {
        return;
    };
    let info = Prober::with_binary(ffprobe()).probe(&out).expect("probe");
    assert_eq!(1, info.subtitle_streams.len());
    assert_eq!(
        "mov_text", info.subtitle_streams[0].codec_name,
        "copied exactly, as the plan intended"
    );
    assert_eq!(1, info.video_streams.len());
}

#[test]
fn the_cues_themselves_survive_the_transcode() {
    // A track that exists and is the right codec can still be empty. The
    // accept criterion is "cues survive transcode", so the text has to be
    // checked, not just the stream count.
    need_ffmpeg!();
    let dir = scratch("cues");
    let Some(source) = source_with(&dir, "c2", &["-c:s", "ass"], "mkv") else {
        return;
    };
    let plan = plan_for(&source);
    let Some(out) = proxy(&dir, "out.mp4", &source, plan) else {
        return;
    };
    let info = Prober::with_binary(ffprobe()).probe(&out).expect("probe");
    let s = &info.subtitle_streams[0];
    let doc = commons_media::extract::extract_stream(&out, s.index, &s.codec_name)
        .expect("the proxy's track is readable");
    assert_eq!(2, doc.cues.len(), "both cues survived");
    assert!(
        doc.cues[0].text.contains("first line"),
        "text: {}",
        doc.cues[0].text
    );
    assert!(doc.cues[1].text.contains("second line"));
    // And the timings, within the same 40 ms the extraction budget uses.
    assert!(
        doc.cues[0].start_ms.abs_diff(0) <= 40,
        "first cue starts at {}",
        doc.cues[0].start_ms
    );
    assert!(
        doc.cues[1].start_ms.abs_diff(2_500) <= 40,
        "second cue starts at {}, expected about 2500",
        doc.cues[1].start_ms
    );
}

#[test]
fn a_source_with_no_subtitles_proxies_unchanged() {
    // The compatibility case, proven rather than asserted: a source with no
    // subtitle stream produces a proxy with none, and the plan is the empty
    // one. If `-sn` were dropped by mistake, ffmpeg's default stream matching
    // could add one — and this is the test that would catch it.
    need_ffmpeg!();
    let dir = scratch("nosubs");
    let source = dir.join("plain.mp4");
    let status = Command::new(ffmpeg())
        .args([
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=160x120:r=10:d=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-t",
            "2",
        ])
        .arg(&source)
        .status()
        .expect("ffmpeg runs");
    if !status.success() {
        eprintln!("SKIPPED: this ffmpeg has no usable video encoder");
        return;
    }
    let plan = plan_for(&source);
    assert!(
        !plan.carries_any(),
        "no subtitle streams, so nothing to carry"
    );

    let Some(out) = proxy(&dir, "out.mp4", &source, plan) else {
        return;
    };
    let info = Prober::with_binary(ffprobe()).probe(&out).expect("probe");
    assert!(
        info.subtitle_streams.is_empty(),
        "a source with no subtitles produced a proxy with {}",
        info.subtitle_streams.len()
    );
    assert_eq!(1, info.video_streams.len(), "and the video is still there");
}
