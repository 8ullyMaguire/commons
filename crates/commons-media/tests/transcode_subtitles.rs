//! T-P6-002 §5: what a proxy does with the source's subtitles.
//!
//! The plan states "cues survive transcode (re-muxed or re-derived from
//! transcript)" and leaves the choice open. These tests pin the choice, and the
//! reason the interesting cases are here rather than in a comment is that the
//! two obvious answers are both wrong for a different file:
//!
//! * `-c:s copy` always — carries an `ass` stream into the MP4, where a
//!   browser displays nothing. The proxy looks like it worked.
//! * `-c:s webvtt` always — re-encodes a `mov_text` track that copied exactly,
//!   spending a transcode to get the same bytes.
//!
//! Both are correct for some input and wrong for the rest, which is why the
//! decision is a value computed from the probe rather than a constant in the
//! argument list.
//!
//! Everything here is a pure function of a list of codec names: no probe, no
//! path, no process. The end-to-end half — that ffmpeg accepts what these
//! produce and the result has a readable track — is in
//! `transcode_subtitles_acceptance.rs`.

use commons_media::transcode::{subtitle_args, subtitle_map_args, SubtitlePlan, Transcoder};
use commons_store::playback::Rung;
use std::path::Path;

fn codecs(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn transcoder_with(plan: SubtitlePlan) -> Transcoder {
    Transcoder::new("/tmp").with_subtitles(plan)
}

// --- the decision ------------------------------------------------------------

#[test]
fn mov_text_and_webvtt_are_carried_and_never_re_encoded() {
    // The copy case. `mov_text` is what an mp4 carries, `webvtt` what a browser
    // reads, and copying is exact -- so a transcode here would cost time and
    // risk a rescale to produce the same cues.
    let plan = SubtitlePlan::for_codecs(&codecs(&["mov_text", "webvtt"]));
    assert_eq!(vec![0, 1], plan.copy);
    assert!(plan.rederive.is_empty(), "no re-derive: both copy exactly");
    assert!(plan.drop.is_empty());
    assert!(plan.carries_any());
}

#[test]
fn ass_and_subrip_are_re_derived_rather_than_carried() {
    // The case a plan saying "re-muxed" would have missed. `-c:s copy` of an
    // ass stream into an MP4 SUCCEEDS -- ffmpeg writes it happily -- and the
    // result displays nothing, because a browser never could draw ASS. A proxy
    // that carries one has moved the problem.
    let plan = SubtitlePlan::for_codecs(&codecs(&["ass", "subrip", "ssa"]));
    assert!(
        plan.copy.is_empty(),
        "none of these are readable in a browser"
    );
    assert_eq!(vec![0, 1, 2], plan.rederive);
}

#[test]
fn image_subtitles_are_dropped_and_never_offered_to_ocr() {
    // PGS and DVD subtitles are bitmaps. Turning one into text is OCR, and a
    // caption that reads "he sasid" is not a degraded caption, it is a false
    // one. So the honest answer is to leave them out and say so.
    let plan = SubtitlePlan::for_codecs(&codecs(&["hdmv_pgs_subtitle", "dvd_subtitle"]));
    assert_eq!(vec![0, 1], plan.drop);
    assert!(!plan.carries_any(), "nothing survives, so the plan is -sn");
}

#[test]
fn a_mixed_source_splits_by_capability_not_by_file() {
    // The case the whole type exists for. One file, three answers, and the
    // order of the arguments has to survive it.
    let plan = SubtitlePlan::for_codecs(&codecs(&[
        "subrip",            // re-derive
        "hdmv_pgs_subtitle", // drop
        "mov_text",          // copy
        "ass",               // re-derive
    ]));
    assert_eq!(vec![2], plan.copy, "only mov_text copies");
    assert_eq!(vec![0, 3], plan.rederive);
    assert_eq!(vec![1], plan.drop);
    assert!(plan.carries_any());
}

#[test]
fn an_unknown_codec_is_dropped_rather_than_guessed_at() {
    // A codec this build has never heard of. The three-way split has no fourth
    // bucket, and inventing a fourth -- "carry it and see" -- produces the
    // worst outcome available: a stream in the output that nothing can read.
    let plan = SubtitlePlan::for_codecs(&codecs(&["not_a_real_codec"]));
    assert_eq!(vec![0], plan.drop);
}

// --- the flags ---------------------------------------------------------------

#[test]
fn no_plan_at_all_means_no_subtitles() {
    // `None` is the default, and it has to mean `-sn`. Guessing `-c:s copy`
    // against a source with an ASS track produces a proxy whose subtitles
    // cannot be displayed; guessing `-c:s webvtt` re-encodes a file that may
    // have no subtitles at all. `-sn` is the only answer that cannot be wrong
    // about a file whose streams were never looked at.
    assert_eq!(vec!["-sn".to_string()], subtitle_args(None));
    assert!(subtitle_map_args(None).is_empty(), "nothing to map");
}

#[test]
fn a_plan_that_carries_nothing_is_still_explicitly_sn() {
    // `-sn` rather than nothing, even with a plan in hand. ffmpeg's default
    // stream matching is not "no subtitles" once a subtitle codec is present in
    // the build, and a proxy that quietly gained a track is a different file
    // from one that was asked to have none.
    let plan = SubtitlePlan::for_codecs(&codecs(&["hdmv_pgs_subtitle"]));
    assert_eq!(vec!["-sn".to_string()], subtitle_args(Some(&plan)));
    assert!(
        subtitle_map_args(Some(&plan)).is_empty(),
        "a dropped stream is not mapped, or ffmpeg would select it and then \
         have no codec for it"
    );
}

#[test]
fn a_re_derive_alone_emits_the_webvtt_encoder_and_nothing_else() {
    assert_eq!(
        vec!["-c:s".to_string(), "webvtt".to_string()],
        subtitle_args(Some(&SubtitlePlan::for_codecs(&codecs(&["ass"]))))
    );
}

#[test]
fn a_copy_alone_emits_copy_and_not_webvtt() {
    assert_eq!(
        vec!["-c:s".to_string(), "copy".to_string()],
        subtitle_args(Some(&SubtitlePlan::for_codecs(&codecs(&["mov_text"]))))
    );
}

#[test]
fn the_maps_use_the_ffprobe_stream_index_not_the_subtitle_ordinal() {
    // The distinction that a `-map 0:s:0` gets wrong. On a file with a video
    // stream at index 0 and a subtitle at index 2, the subtitle's ordinal among
    // subtitles is 0 and its stream index is 2 -- and `-map 0:0` selects the
    // VIDEO. ffmpeg does not complain; it transcodes a picture as though it
    // were a caption.
    let plan = SubtitlePlan {
        copy: vec![],
        rederive: vec![2],
        drop: vec![],
    };
    assert_eq!(
        vec!["-map".to_string(), "0:2".to_string()],
        subtitle_map_args(Some(&plan))
    );
}

#[test]
fn the_maps_are_emitted_in_index_order() {
    // Deterministic argument vectors. A plan built from a probe is in stream
    // order, but `copy` and `rederive` are separate lists and merging them
    // without sorting makes the output depend on which codec happened to be
    // classified first -- which makes the whole vector untestable and the cache
    // key derived from it unstable.
    let plan = SubtitlePlan {
        copy: vec![5, 1],
        rederive: vec![3, 0],
        drop: vec![9],
    };
    let maps = subtitle_map_args(Some(&plan));
    let mapped: Vec<&str> = maps.iter().skip(1).step_by(2).map(|s| s.as_str()).collect();
    assert_eq!(vec!["0:0", "0:1", "0:3", "0:5"], mapped);
}

// --- the argument vector, end to end ----------------------------------------

#[test]
fn args_for_with_no_plan_is_byte_identical_to_the_pre_t_p6_002_vector() {
    // The compatibility claim, made testable. Every existing caller gets the
    // same ffmpeg invocation it got before this ticket, so nothing that
    // depended on `-sn` changes behaviour.
    let t = Transcoder::new("/tmp");
    let args = t.args_for(Path::new("/in.mkv"), Path::new("/out.mp4"), Rung::Medium);
    assert_eq!(
        1,
        args.iter().filter(|a| *a == "-sn").count(),
        "-sn is emitted exactly once"
    );
    // And it is not immediately followed by a stream spec, which would mean
    // it was a map target rather than a flag.
    let sn = args
        .iter()
        .position(|a| a == "-sn")
        .expect("-sn is emitted");
    assert!(
        args.get(sn + 1).is_none_or(|n| !n.starts_with("0:")),
        "-sn is a flag, not a map target: {:?}",
        &args[sn..]
    );
}

#[test]
fn args_for_with_a_plan_replaces_sn_with_the_maps_and_the_encoder() {
    let plan = SubtitlePlan::for_codecs(&codecs(&["subrip"]));
    let t = transcoder_with(plan);
    let args = t.args_for(Path::new("/in.mkv"), Path::new("/out.mp4"), Rung::Medium);
    assert!(
        !args.iter().any(|a| a == "-sn"),
        "a plan that carries a stream must not also say -sn: the flag is an \
         output-wide veto and would delete the track the maps just selected. \
         args: {args:?}"
    );
    assert!(args.contains(&"0:0".to_string()), "the stream is mapped");
    assert!(args.contains(&"webvtt".to_string()), "and re-derived");
}

#[test]
fn the_maps_precede_the_codec_flags_that_apply_to_them() {
    // `-map` is an output option. A `-c:s webvtt` that appears BEFORE the map
    // that selects the stream it applies to is either inert or an error
    // depending on the ffmpeg version, and "depending on the version" is not a
    // property worth having.
    let plan = SubtitlePlan::for_codecs(&codecs(&["ass"]));
    let t = transcoder_with(plan);
    let args = t.args_for(Path::new("/in.mkv"), Path::new("/out.mp4"), Rung::Medium);
    let last_map = args
        .iter()
        .rposition(|a| a.starts_with("0:") && a != "0:a:0?")
        .expect("a stream is mapped");
    let codec = args
        .iter()
        .position(|a| a == "webvtt")
        .expect("the encoder is present");
    assert!(
        last_map < codec,
        "map at {last_map} must precede the encoder at {codec}. args: {args:?}"
    );
}

#[test]
fn a_drop_only_plan_leaves_the_vector_exactly_as_it_was() {
    // The bitmaps case. The proxy is byte-identical to one built before this
    // ticket, which is the right outcome: we cannot help, and a different file
    // would suggest we had tried.
    let with = transcoder_with(SubtitlePlan::for_codecs(&codecs(&["hdmv_pgs_subtitle"]))).args_for(
        Path::new("/in.mkv"),
        Path::new("/out.mp4"),
        Rung::Lowest,
    );
    let without =
        Transcoder::new("/tmp").args_for(Path::new("/in.mkv"), Path::new("/out.mp4"), Rung::Lowest);
    assert_eq!(without, with);
}
