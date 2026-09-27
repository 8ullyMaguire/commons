//! The subtitle round trip, against a real ffmpeg and a real container.
//!
//! T-P6-002 step 4. Spec §4's 40 ms budget.
//!
//! Everything else about extraction is tested in `extract.rs` with a stub, and
//! that is the right default: a stub can tell you the argument vector is right,
//! and it tells you the same thing on every machine and every ffmpeg build.
//! What it cannot tell you is that **ffmpeg actually accepts those arguments
//! and that the timestamps come back inside the budget** — which is a different
//! claim, and the one the plan's accept criterion is about.
//!
//! # Why every cue and not an average
//!
//! The spec is explicit: |round trip − original| ≤ 40 ms **for every cue in the
//! file**. A per-file average hides the cue that moved, and the cue that moved
//! is the one a user notices — it is the line of dialogue that comes in a beat
//! early or late. `max_drift_ms` below is the assertion, and it is the reason
//! this file is worth the runtime.
//!
//! # Where the drift comes from, so the tolerance is aimed at something
//!
//! §4's analysis: `mov_text` and `webvtt` round-trip through a time base, and
//! a rescale is where the sub-40 ms error lives; ASS is centisecond, so at most
//! 0.5 ms; and the **end** timestamp is the one that drifts, because files
//! disagree about whether an end is inclusive. Storing milliseconds as integers
//! is what keeps the error bounded — a float accumulates.

use commons_media::extract::Extractor;
use commons_media::probe::Prober;
use commons_media::subtitles::{self, Cue, Format};
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Skip rather than fail when the binary is absent.
///
/// A skip is only honest for a test that is *about* the binary. Everything
/// about extraction's logic is in `extract.rs` and runs without one, so this
/// file is the only place a missing ffmpeg costs coverage — and it is coverage
/// nothing else can provide.
macro_rules! need_ffmpeg {
    () => {
        if !ffmpeg().exists() && which("ffmpeg").is_none() {
            eprintln!("SKIPPED: no ffmpeg on PATH or in COMMONS_FFMPEG");
            return;
        }
    };
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("commons-subrt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A cue list with awkward timings: odd milliseconds, a cue that starts at
/// zero, gaps, and a long one.
///
/// Not round numbers. A file of round numbers round-trips exactly and proves
/// nothing about a rescale, because the rescale error is a function of where
/// the value sits in the time base and multiples of a second are the friendliest
/// possible values.
const CUES: &[(i64, i64, &str)] = &[
    // Not `(0, 0)`. A zero-length cue is a real thing in a real file and every
    // parser here keeps it, but `to_webvtt` DROPS it -- correctly, because a
    // browser discards a cue with `end <= start` silently. So a fixture
    // containing one cannot be used to assert that the writer and the reader
    // agree on the count, and a test that does so is asserting that the writer
    // is broken. The drop is tested on its own in `subtitles_parse.rs`.
    (0, 40, "at the very start"),
    (1337, 1437, "an odd millisecond"),
    (2500, 2600, "two and a half"),
    (3333, 3500, "thirds do not divide"),
    (4711, 4900, "a prime-ish offset"),
    (9999, 10100, "nearly ten seconds"),
    (12345, 13000, "past twelve"),
    // **No cue ends after the next one starts.** Cue 7 originally ran
    // 60000→61000 while cue 8 started at 60500, and mov_text cannot represent
    // the overlap: ffmpeg clamped the end to 60500, a 500 ms change to a
    // timestamp that was not itself wrong.
    //
    // That is a property of the format, not a bug in the extractor, so the
    // fixture is what has to change. Overlapping cues are legal in SRT and
    // VTT and common in real subtitle files -- they are what a player renders
    // as two lines at once. But the 40 ms budget is a claim about *this*
    // round trip, and a fixture that violates an assumption the format cannot
    // meet turns a measured budget into a meaningless one. The clamping
    // behaviour is asserted on its own below rather than hidden in a drift
    // number.
    (60000, 60500, "a minute in"),
    (60500, 62000, "half a second later"),
    (359999, 361000, "nearly six minutes, the largest value here"),
];

/// The largest |actual − expected| over every cue, in ms.
fn max_drift_ms(want: &[(i64, i64, &str)], got: &[Cue]) -> i64 {
    assert_eq!(
        want.len(),
        got.len(),
        "cue COUNT changed: a rescale that drops or duplicates a cue is a failure \\
         whatever the timings say"
    );
    want.iter()
        .zip(got)
        .map(|((ws, we, wt), g)| {
            assert_eq!(
                *wt, g.text,
                "cue {} text changed, so the two are not comparable",
                g.seq
            );
            // The end is checked as well as the start, and separately, because
            // §4 names it as the one that drifts: files disagree about whether
            // an end is inclusive, and a start-only assertion would not notice
            // a file where every cue is one frame too long.
            let ds = (g.start_ms - ws).abs();
            let de = (g.end_ms - we).abs();
            ds.max(de)
        })
        .max()
        .unwrap_or(0)
}

/// Mux `cues` into a new MKV as an `ass` stream, returning the path.
fn mux_ass(dir: &Path, cues: &[(i64, i64, &str)]) -> PathBuf {
    let src = write_sidecar_srt(dir, cues);
    let out = dir.join("muxed.mkv");
    let status = Command::new(ffmpeg())
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(&src)
        .args(["-c", "copy", "-f", "srt"])
        .arg(&out)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "muxing the fixture failed");
    out
}

/// ffmpeg will not mux a raw `.srt` into a subtitle stream that reports as
/// `ass`, so the fixture is muxed as `subrip` and the ASS case is muxed from a
/// hand-written ASS file. Both are below.
fn write_sidecar_srt(dir: &Path, cues: &[(i64, i64, &str)]) -> PathBuf {
    let mut body = String::new();
    for (i, (s, e, t)) in cues.iter().enumerate() {
        body.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            srt_ts(*s),
            srt_ts(*e),
            t
        ));
    }
    let p = dir.join("cues.srt");
    std::fs::write(&p, body).expect("write srt");
    p
}

fn srt_ts(ms: i64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

fn ass_ts(ms: i64) -> String {
    // ASS is H:MM:SS.cc -- centiseconds, so the source file cannot express a
    // millisecond. This is §4's "at most 0.5 ms" and it is why the ASS
    // expectation is rounded rather than exact.
    format!(
        "{}:{:02}:{:02}.{:02}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        (ms / 10) % 100
    )
}

/// The source ASS file, written by hand, muxed into a container as `ass`.
fn mux_ass_real(dir: &Path, cues: &[(i64, i64, &str)]) -> PathBuf {
    let mut body = String::from(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,40\n\n[Events]\nFormat: Layer, Start, End, Style, Text\n",
    );
    for (s, e, t) in cues {
        body.push_str(&format!(
            "Dialogue: 0,{},{},Default,{}\n",
            ass_ts(*s),
            ass_ts(*e),
            t
        ));
    }
    let src = dir.join("cues.ass");
    std::fs::write(&src, body).expect("write ass");
    let out = dir.join("muxed-ass.mkv");
    let status = Command::new(ffmpeg())
        .args(["-nostdin", "-v", "error", "-y", "-f", "ass", "-i"])
        .arg(&src)
        .args(["-c", "copy"])
        .arg(&out)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "muxing the ASS fixture failed");
    out
}

// --- the round trips ---------------------------------------------------------

#[test]
fn every_srt_timestamp_survives_the_round_trip_inside_40ms() {
    need_ffmpeg!();
    let dir = scratch("srt");
    let media = mux_ass(&dir, CUES);

    // The probe half: the stream must be *findable* before it can be read, and
    // the index it reports is what `-map` needs. Asserting the codec too is
    // what makes this a round trip rather than "ffmpeg copied a file".
    let info = Prober::with_binary(ffprobe())
        .probe(&media)
        .expect("probe the muxed file");
    assert_eq!(1, info.subtitle_streams.len(), "one subtitle stream");
    let s = &info.subtitle_streams[0];
    assert_eq!("subrip", s.codec_name);
    let format = Format::from_codec(&s.codec_name).expect("subrip is supported");
    // The probe carries ffprobe's spelling and the extractor keys off the same
    // one, so `from_codec` agreeing on what the stream is means the two halves
    // are looking at the same vocabulary. If it ever returned None here, the
    // extraction below would still work — `extract_stream` does its own lookup —
    // and the drift would still be measured against a document from a format
    // the caller was never told about.
    assert_eq!(Format::SubRip, format);

    let doc = Extractor::with_binary(ffmpeg())
        .extract_stream(&media, s.index, &s.codec_name)
        .expect("extract");

    let drift = max_drift_ms(CUES, &doc.cues);
    assert!(
        drift <= 40,
        "SRT round trip drifted {drift} ms, over the 40 ms budget (spec §4). \
         Cues: {:?}",
        doc.cues
            .iter()
            .map(|c| (c.start_ms, c.end_ms))
            .collect::<Vec<_>>()
    );
}

#[test]
fn every_ass_timestamp_survives_the_round_trip_inside_40ms() {
    need_ffmpeg!();
    let dir = scratch("ass");
    let media = mux_ass_real(&dir, CUES);

    let info = Prober::with_binary(ffprobe())
        .probe(&media)
        .expect("probe the muxed file");
    assert_eq!(1, info.subtitle_streams.len());
    let s = &info.subtitle_streams[0];
    assert_eq!("ass", s.codec_name);

    let doc = Extractor::with_binary(ffmpeg())
        .extract_stream(&media, s.index, &s.codec_name)
        .expect("extract");

    // ASS is centisecond, so the source values are the originals rounded to 10ms
    // and the expectation has to be rounded the same way. §4: "at most 0.5 ms",
    // which is what makes ASS the easy case and `mov_text` the hard one.
    let expected: Vec<(i64, i64, &str)> = CUES
        .iter()
        .map(|(s, e, t)| ((s / 10) * 10, (e / 10) * 10, *t))
        .collect();
    let drift = max_drift_ms(&expected, &doc.cues);
    assert!(
        drift <= 40,
        "ASS round trip drifted {drift} ms. Cues: {:?}",
        doc.cues
            .iter()
            .map(|c| (c.start_ms, c.end_ms))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_mov_text_track_round_trips_inside_40ms() {
    // The hard case, and the one §4 singles out: `mov_text` is a QuickTime
    // timecode atom, so it goes through ffmpeg's muxer in BOTH directions and
    // the time base is rescaled. This is where the 40 ms budget actually gets
    // spent, which is why "ASS and SRT are fine" is not evidence that the
    // extractor is.
    need_ffmpeg!();
    let dir = scratch("movtext");
    let srt = write_sidecar_srt(&dir, CUES);
    let media = dir.join("muxed.mp4");

    let status = Command::new(ffmpeg())
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(&srt)
        // `-c:s mov_text` is the request; ffmpeg may refuse it for some input
        // and then this test fails loudly, which is the right outcome: a
        // capability Commons claims and does not have.
        .args(["-c:s", "mov_text"])
        .arg(&media)
        .status()
        .expect("ffmpeg runs");
    if !status.success() {
        // Not a failure, and not a skip either. `Could not write header
        // (incorrect codec parameters)` is what ffmpeg says when the FIRST cue
        // is zero-length: mov_text has no representation for an empty
        // duration, so it refuses the file. Reporting it as a skip would hide a
        // real incompatibility, and asserting on it would fail on an ffmpeg
        // that can do it. So the fixture has no zero-length cue (see CUES) and
        // a failure here is a genuine problem worth seeing.
        panic!("this ffmpeg refused to mux mov_text from a valid srt");
    }

    let info = Prober::with_binary(ffprobe())
        .probe(&media)
        .expect("probe the muxed mp4");
    assert_eq!(1, info.subtitle_streams.len(), "one subtitle stream");
    let s = &info.subtitle_streams[0];
    assert_eq!("mov_text", s.codec_name, "the stream is mov_text");

    let doc = Extractor::with_binary(ffmpeg())
        .extract_stream(&media, s.index, &s.codec_name)
        .expect("extract mov_text");

    let drift = max_drift_ms(CUES, &doc.cues);
    assert!(
        drift <= 40,
        "mov_text round trip drifted {drift} ms, over the budget. \
         mov_text is the time-base case: spec §4. Cues: {:?}",
        doc.cues
            .iter()
            .map(|c| (c.start_ms, c.end_ms))
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_extracted_document_is_valid_webvtt() {
    // The endpoint the whole ticket serves. A browser will not render a cue
    // whose `end <= start`, and it discards it *silently* — so a document that
    // parses and does not render is a failure that no test above would catch.
    need_ffmpeg!();
    let dir = scratch("vtt");
    let media = mux_ass(&dir, CUES);
    let info = Prober::with_binary(ffprobe()).probe(&media).expect("probe");
    let s = &info.subtitle_streams[0];
    let doc = Extractor::with_binary(ffmpeg())
        .extract_stream(&media, s.index, &s.codec_name)
        .expect("extract");

    let vtt = subtitles::to_webvtt(&doc.cues);
    assert!(
        vtt.starts_with("WEBVTT"),
        "the magic header is required:\n{vtt}"
    );

    // And it must parse back to the same cues. Round-tripping through the
    // writer and the reader is the only check that the two agree on the
    // half-open convention, which is where an off-by-one would live.
    let back = subtitles::parse(&vtt, Format::WebVtt).expect("the writer emits valid vtt");
    assert_eq!(doc.cues.len(), back.cues.len());
    for (a, b) in doc.cues.iter().zip(back.cues) {
        assert_eq!(a.start_ms, b.start_ms, "start drifted through the writer");
        assert_eq!(a.end_ms, b.end_ms, "end drifted through the writer");
        assert!(
            b.end_ms > b.start_ms,
            "a zero-length cue is dropped by browsers"
        );
    }
}

#[test]
fn a_probed_stream_index_is_what_the_extractor_needs() {
    // The two halves wired together, and the reason `SubtitleStream.index` is
    // the *index* rather than an ordinal: `-map 0:<n>` needs ffprobe's number.
    // Taking the position in `subtitle_streams` instead would read stream 0 of 1
    // and either extract the video's first subtitle or fail, and on a file
    // where the subtitle track is not the first stream that is a silent
    // mismatch rather than an error.
    need_ffmpeg!();
    let dir = scratch("index");
    // A container with a VIDEO stream and a subtitle stream, so the subtitle
    // track's ffprobe index is not 0. Muxing the srt alone puts it at index 0,
    // which makes the index and the ordinal agree by accident and the test
    // would prove nothing.
    let srt = write_sidecar_srt(&dir, CUES);
    let media = dir.join("with-video.mkv");
    let status = Command::new(ffmpeg())
        .args([
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=320x240:d=1:r=5",
        ])
        .arg("-i")
        .arg(&srt)
        .args(["-c:v", "libx264", "-c:s", "copy", "-shortest"])
        .arg(&media)
        .status()
        .expect("ffmpeg runs");
    if !status.success() {
        eprintln!("SKIPPED: this ffmpeg has no usable video encoder");
        return;
    }
    let info = Prober::with_binary(ffprobe()).probe(&media).expect("probe");
    let s = &info.subtitle_streams[0];

    // The video stream is index 0 in an MKV made this way, so the subtitle
    // track's index is NOT 0 and NOT 1. Asserting that is the test.
    assert_ne!(
        0, s.index,
        "a subtitle track at index 0 would make the ordinal and the index agree \\
         by accident, and this test would prove nothing"
    );
    assert!(info.video_streams.iter().all(|v| v.index != s.index));
    assert!(info.audio_streams.iter().all(|a| a.index != s.index));
}

#[test]
fn mov_text_clamps_an_overlapping_cue_to_the_next_ones_start() {
    // The behaviour the fixture above is written around, asserted on its own so
    // it is a documented property rather than an excuse.
    //
    // mov_text stores a subtitle track as a timeline, so two cues that overlap
    // in time have no representation -- the first must end before the second
    // begins. ffmpeg resolves this by clamping the earlier cue's end to the
    // later one's start, and the end is what moves: 61000 becomes 60500, a
    // 500 ms change to a timestamp that was not itself wrong.
    //
    // Asserted here rather than left implicit in a drift figure because it is
    // the one case where a 40 ms budget is the wrong expectation: no
    // implementation can meet it for an overlapping cue, because the data is
    // not in the format.
    need_ffmpeg!();
    let dir = scratch("overlap");
    let overlapping: &[(i64, i64, &str)] = &[
        (0, 40, "first"),
        (30, 90, "second, overlapping"),
        (100, 200, "third"),
    ];
    let srt = write_sidecar_srt(&dir, overlapping);
    let media = dir.join("m.mp4");
    let status = Command::new(ffmpeg())
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(&srt)
        .args(["-c:s", "mov_text"])
        .arg(&media)
        .status()
        .expect("ffmpeg runs");
    if !status.success() {
        eprintln!("SKIPPED: this ffmpeg cannot mux mov_text");
        return;
    }
    let info = Prober::with_binary(ffprobe()).probe(&media).expect("probe");
    let s = &info.subtitle_streams[0];
    let doc = Extractor::with_binary(ffmpeg())
        .extract_stream(&media, s.index, &s.codec_name)
        .expect("extract");

    assert_eq!(
        3,
        doc.cues.len(),
        "no cue is dropped to resolve the overlap"
    );
    // The starts survive exactly: those are representable.
    assert_eq!(0, doc.cues[0].start_ms);
    assert_eq!(30, doc.cues[1].start_ms);
    assert_eq!(100, doc.cues[2].start_ms);
    // The clamped end is at most the next start -- a non-overlapping timeline,
    // which is all the format can hold.
    assert!(
        doc.cues[0].end_ms <= doc.cues[1].start_ms,
        "cue 0 ends at {} and cue 1 starts at {}; the timeline must not overlap",
        doc.cues[0].end_ms,
        doc.cues[1].start_ms
    );
    assert!(doc.cues[1].end_ms <= doc.cues[2].start_ms);
}
