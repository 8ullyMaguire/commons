//! Subtitle streams out of a probe.
//!
//! T-P6-002 step 4. Spec §1 measured what ffprobe actually emits for a muxed
//! subtitle track, and this file pins that measurement to a test so it cannot
//! drift silently.
//!
//! The split from `subtitles_parse.rs` is deliberate and is the reason this
//! ticket has a back end at all: **the probe can enumerate a subtitle track but
//! not read its text.** §1 measured `extradata_size: 176` for an ASS stream —
//! the style/script header — and one packet of `size: 11` with no payload. So
//! timings are discoverable and cues are not, and these two files test the two
//! halves of that split separately. A test that round-tripped a real file
//! through ffmpeg and the parser together could not tell a probe bug from a
//! parser bug.
//!
//! Every fixture stream carries `codec_type`, which is the field the parse
//! actually matches on — `codec_name` is *not* consulted for the stream type.
//! A fixture that omits it parses to zero streams, and every assertion in the
//! test below it fails with "index out of bounds", which reads like a probe bug
//! rather than a fixture one. The first draft of this file omitted it in five
//! places.
//!
//! Fixtures are ffprobe's real JSON shape, copied from an actual run. The
//! `disposition` object is spelled out in full rather than trimmed to the three
//! fields we read, because ffprobe emits all nine and a fixture that omits them
//! does not prove the parser survives them.

use commons_media::probe::{parse_json, MediaInfo};
use serde_json::json;

/// What ffprobe emits for an H.264/AAC MKV with one `ass` stream muxed in at
/// index 2, `language=eng`, `disposition.default=1`. Verbatim from a real run.
fn with_ass_stream() -> MediaInfo {
    parse_json(
        &json!({
            "streams": [
                {
                    "index": 0,
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 1920,
                    "height": 1080,
                    "duration": "600.000000",
                    "start_time": "0.000000"
                },
                {
                    "index": 1,
                    "codec_type": "audio",
                    "codec_name": "aac",
                    "channels": 2,
                    "duration": "600.000000",
                    "start_time": "0.000000",
                    "disposition": { "default": 1 }
                },
                {
                    "index": 2,
                    "codec_type": "subtitle",
                    "codec_name": "ass",
                    "extradata_size": 176,
                    "duration": "600.000000",
                    "start_time": "0.000000",
                    "tags": {
                        "language": "eng",
                        "ENCODER": "Lavc60.3.100 libass",
                        "DURATION": "10:00:00.000"
                    },
                    "disposition": {
                        "default": 1,
                        "dub": 0,
                        "original": 0,
                        "comment": 0,
                        "lyrics": 0,
                        "karaoke": 0,
                        "forced": 0,
                        "hearing_impaired": 0,
                        "visual_impaired": 0,
                        "clean_effects": 0,
                        "attached_pic": 0,
                        "captions": 0,
                        "descriptions": 0,
                        "dependent": 0,
                        "metadata": 0
                    }
                }
            ],
            "format": {
                "filename": "movie.mkv",
                "format_name": "matroska,webm",
                "duration": "600.000000",
                "start_time": "0.000000"
            }
        })
        .to_string(),
    )
    .expect("the fixture is valid ffprobe JSON")
}

fn info() -> MediaInfo {
    with_ass_stream()
}

#[test]
fn a_subtitle_stream_is_enumerated_with_its_index_intact() {
    let m = info();
    assert_eq!(
        1,
        m.subtitle_streams.len(),
        "the ASS track at index 2 must be enumerated, and only it"
    );
    let s = &m.subtitle_streams[0];
    // The index, not the ordinal. The document's `stream_index` is this number,
    // and every `-map` refers to it, so "the second subtitle stream" and "index
    // 2" are the same only by coincidence.
    assert_eq!(2, s.index);
    assert_eq!("ass", s.codec_name);
}

#[test]
fn enumerating_subtitles_does_not_disturb_the_other_streams() {
    // The old code skipped every non-video/audio stream with a `_ => {}` and
    // said so: "ignoring them here is deliberate so they do not inflate the
    // stream count a caller sees today." That is still true for `data` and
    // `attachment`, and it is now false for `subtitle` — so the property to pin
    // is that the video and audio lists are unchanged, not that the total is.
    let m = info();
    assert_eq!(1, m.video_streams.len());
    assert_eq!(1, m.audio_streams.len());
    assert_eq!(0, m.video_streams[0].index);
    assert_eq!(1, m.audio_streams[0].index);
}

#[test]
fn a_subtitle_stream_carries_its_dispositions_not_a_guess_from_its_name() {
    // #4586's "language rulesets" go wrong by inferring a language from a
    // filename. The container is authoritative: it is what the muxer wrote, and
    // it is the only thing that knows a track is forced or is captions.
    let s = &info().subtitle_streams[0];
    assert!(s.is_default, "the fixture's disposition.default is 1");
    assert!(!s.is_forced);
    assert!(!s.is_hearing_impaired);
}

#[test]
fn a_forced_signs_track_is_marked_forced_not_guessed() {
    // The same document with `forced: 1` on the subtitle stream. A title of
    // "English SDH" would say the same thing, and a title of "Signs" would be
    // ambiguous with a description track; the disposition is neither.
    let raw = json!({
        "streams": [{
            "index": 3,
            "codec_type": "subtitle",
            "codec_name": "subrip",
            "duration": "600.000000",
            "start_time": "0.000000",
            "tags": { "language": "eng", "title": "English SDH" },
            "disposition": { "forced": 1, "hearing_impaired": 1 }
        }],
        "format": { "filename": "m.mkv", "duration": "600.000000" }
    });
    let m = parse_json(&raw.to_string()).expect("valid JSON");
    let s = &m.subtitle_streams[0];
    assert!(s.is_forced);
    assert!(s.is_hearing_impaired);
    // The title is carried, not interpreted. Deciding whether "SDH" means
    // hearing-impaired is a policy question, and the disposition already
    // answered it; carrying the title alongside is what lets a future policy
    // change without a re-extract.
    assert_eq!(Some("English SDH"), s.title.as_deref());
}

#[test]
fn the_language_is_carried_raw_and_not_normalised_here() {
    // ffprobe reports `eng`, a three-letter ISO 639-2 code. §1 is explicit that
    // normalisation "has to happen in exactly one place or English, eng and en
    // become three languages", and the probe is not that place: a value read
    // here and read again by the extractor would have to agree about it, and
    // this is the module whose only job is reading ffprobe's JSON.
    let s = &info().subtitle_streams[0];
    assert_eq!(
        Some("eng"),
        s.language.as_deref(),
        "the raw tag, un-normalised"
    );
}

#[test]
fn a_stream_that_starts_late_keeps_its_start_time() {
    // stash#7229, and the same reasoning the video and audio streams already
    // use. A cue extracted from a file that begins at 5s is offset from the
    // video by exactly 5s, and if the probe dropped the number nothing
    // downstream would be able to correct for it.
    let raw = json!({
        "streams": [{
            "index": 2,
            "codec_type": "subtitle",
            "codec_name": "webvtt",
            "start_time": "5.000000",
            "tags": {}
        }],
        "format": { "filename": "m.mkv", "duration": "600.000000" }
    });
    let m = parse_json(&raw.to_string()).expect("valid JSON");
    assert_eq!(5000, m.subtitle_streams[0].start_time_ms);
}

#[test]
fn a_stream_with_no_duration_reports_none_rather_than_zero() {
    // A `0` here is a claim that the track is zero-length, and a caller
    // filtering on `duration_ms > 0` would then hide a track that is present.
    // The video and audio branches already use this `Some(..).filter(..)`
    // shape; the subtitle branch has to match, or the same value means
    // different things in two structs side by side.
    let raw = json!({
        "streams": [{ "index": 2, "codec_type": "subtitle", "codec_name": "ass", "start_time": "0.0" }],
        "format": { "filename": "m.mkv", "duration": "600.000000" }
    });
    let m = parse_json(&raw.to_string()).expect("valid JSON");
    assert_eq!(None, m.subtitle_streams[0].duration_ms);
}

#[test]
fn a_file_with_no_subtitles_reports_an_empty_list_not_an_error() {
    // The overwhelming majority of a library's files have no subtitle track, so
    // this is the common path and it has to be the quiet one.
    let raw = json!({
        "streams": [{ "index": 0, "codec_type": "video", "codec_name": "h264", "start_time": "0.0" }],
        "format": { "filename": "m.mp4", "duration": "10.000000" }
    });
    let m = parse_json(&raw.to_string()).expect("valid JSON");
    assert!(m.subtitle_streams.is_empty());
}

#[test]
fn a_data_stream_is_still_not_a_subtitle_stream() {
    // The `_ => {}` arm survived the change and this is what it is for. A
    // `data` stream in an MKV is a font or a chapter track; enumerating it as a
    // subtitle would put an entry in the player's track list that, when
    // selected, decodes to nothing.
    let raw = json!({
        "streams": [
            { "index": 0, "codec_type": "video", "codec_name": "h264", "start_time": "0.0" },
            { "index": 1, "codec_type": "data", "codec_name": "bin_data", "start_time": "0.0",
              "tags": { "language": "eng" } },
            { "index": 2, "codec_type": "attachment", "codec_name": "ttf", "start_time": "0.0" }
        ],
        "format": { "filename": "m.mkv", "duration": "10.000000" }
    });
    let m = parse_json(&raw.to_string()).expect("valid JSON");
    assert!(
        m.subtitle_streams.is_empty(),
        "got {:?}",
        m.subtitle_streams
    );
}

#[test]
fn a_missing_disposition_object_is_not_a_panic() {
    // The fixture in `a_stream_that_starts_late_keeps_its_start_time` already
    // omits `disposition` and passes, which is the property. Asserted here
    // explicitly because a `.unwrap()` added to the disposition lookup would
    // turn "a hand-made sidecar JSON" into a crash rather than an empty
    // disposition, and hand-made JSON is exactly what a test writes.
    let raw = json!({
        "streams": [{ "index": 2, "codec_type": "subtitle", "codec_name": "ass" }],
        "format": { "filename": "m.mkv", "duration": "10.000000" }
    });
    let m = parse_json(&raw.to_string()).expect("valid JSON");
    let s = &m.subtitle_streams[0];
    assert!(!s.is_default && !s.is_forced && !s.is_hearing_impaired);
    assert_eq!(None, s.language);
    assert_eq!(None, s.title);
}

#[test]
fn every_known_text_subtitle_codec_survives_the_round_trip() {
    // The four container formats Commons can decode. `codec_name` is kept as
    // ffprobe's spelling precisely so this list is a fact about ffprobe rather
    // than a translation table the probe owns; `Format::from_codec` is where the
    // mapping lives and where a new codec has to be added deliberately.
    for (codec, title) in [
        ("ass", "Signs & Songs"),
        ("ssa", "Legacy SubStation Alpha"),
        ("subrip", "English"),
        ("webvtt", "English VTT"),
        ("mov_text", "English (tx3g)"),
    ] {
        let raw = json!({
            "streams": [{
                "index": 2,
                "codec_type": "subtitle",
                "codec_name": codec,
                "start_time": "0.0",
                "tags": { "title": title }
            }],
            "format": { "filename": "m.mkv", "duration": "10.000000" }
        });
        let m = parse_json(&raw.to_string()).expect("valid JSON");
        assert_eq!(1, m.subtitle_streams.len(), "{codec}");
        assert_eq!(codec, m.subtitle_streams[0].codec_name, "{codec}");
        assert_eq!(
            Some(title),
            m.subtitle_streams[0].title.as_deref(),
            "{codec}"
        );
    }
}
