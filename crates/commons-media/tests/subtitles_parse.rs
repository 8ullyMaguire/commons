//! The pure parsers, tested without a file on disk and without ffmpeg.
//!
//! T-P6-002. The spec (§4) requires this separation for a reason that is a
//! testing reason, not a tidiness one: a parser exercised only by round-tripping
//! a file through ffmpeg cannot tell a parser bug from an extractor bug. If the
//! cue text comes back wrong there is no way to know which half to look at, and
//! you go and read ffmpeg's arguments instead.
//!
//! So these tests hand the parsers strings, and every awkward case the spec names
//! (§8.2) is a case that appears here with a reason it is here.

use commons_media::subtitles::{
    cue_at, escape_vtt_text, format_vtt_timestamp, parse, parse_ass, parse_ass_timestamp,
    parse_srt, parse_timestamp, parse_vtt, strip_ass_markup, strip_vtt_markup, to_webvtt, Cue,
    Document, Format, ParseError,
};

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

#[test]
fn a_timestamp_is_milliseconds() {
    assert_eq!(parse_timestamp("00:00:00,000"), Some(0));
    assert_eq!(parse_timestamp("00:00:01,500"), Some(1500));
    assert_eq!(parse_timestamp("00:01:00,000"), Some(60_000));
    assert_eq!(parse_timestamp("01:00:00,000"), Some(3_600_000));
    // Three hours in, which is the value that overflowed an i32 in an earlier
    // design and the reason positions are u64 at the API boundary.
    assert_eq!(parse_timestamp("03:00:00,000"), Some(10_800_000));
}

#[test]
fn both_fraction_separators_are_accepted_in_both_formats() {
    // WebVTT's dot, SRT's comma, in either parser. Enough tools get this
    // "wrong" that accepting the other one is the difference between parsing a
    // file and refusing a valid-looking one.
    assert_eq!(parse_timestamp("00:00:01.500"), Some(1500));
    assert_eq!(parse_timestamp("00:00:01,500"), Some(1500));
    // The last separator before the fraction is the one that counts, so a file
    // carrying both does not silently lose the milliseconds.
    assert_eq!(parse_timestamp("00:00:01.000,500"), Some(1500));
}

#[test]
fn webvtt_cue_settings_after_the_timestamp_are_ignored() {
    // "00:00:01.000 align:start line:90%" is a timestamp plus settings. A parser
    // that feeds the whole string to the timestamp reader gets None.
    assert_eq!(
        parse_timestamp("00:00:01.000 align:start line:90%"),
        Some(1000)
    );
    assert_eq!(
        parse_timestamp("00:00:01.000 position:50%,line-left"),
        Some(1000)
    );
}

#[test]
fn a_single_digit_hour_field_is_accepted() {
    // WebVTT allows "0:00:01.000". Refusing it loses captions from a file that
    // is otherwise valid.
    assert_eq!(parse_timestamp("0:00:01.000"), Some(1000));
}

#[test]
fn an_impossible_timestamp_is_none_rather_than_a_clamped_one() {
    // 60 seconds is not a second. Clamping would put a caption at 00:01:00
    // that the file said was at 00:00:60, which is a lie the viewer cannot see.
    assert_eq!(parse_timestamp("00:00:60,000"), None);
    assert_eq!(parse_timestamp("00:60:00,000"), None);
    // Two fraction digits is a different format, not a coarser one.
    assert_eq!(parse_timestamp("00:00:01,50"), None);
    // Four field components is not a timestamp.
    assert_eq!(parse_timestamp("00:00:01,000:000"), None);
    assert_eq!(parse_timestamp("not a time"), None);
    assert_eq!(parse_timestamp(""), None);
}

#[test]
fn an_ass_timestamp_is_centiseconds() {
    assert_eq!(parse_ass_timestamp("0:00:00.00"), Some(0));
    assert_eq!(parse_ass_timestamp("0:00:01.50"), Some(1500));
    assert_eq!(parse_ass_timestamp("0:00:01.5"), Some(1500));
    assert_eq!(parse_ass_timestamp("1:00:00.00"), Some(3_600_000));
    assert_eq!(parse_ass_timestamp("0:00:60.00"), None);
    assert_eq!(parse_ass_timestamp("0:00:01"), Some(1000));
}

#[test]
fn an_ass_rendering_is_rendered_at_the_precision_the_format_has() {
    // Not truncated, not rounded up. A centisecond either rounds or it does not;
    // 1.51cs is 15ms and rounding to 20ms moves a caption a fifth of a frame.
    assert_eq!(format_vtt_timestamp(15), "00:00:00.015");
    assert_eq!(format_vtt_timestamp(0), "00:00:00.000");
    assert_eq!(format_vtt_timestamp(3_600_000), "01:00:00.000");
    assert_eq!(format_vtt_timestamp(90_061_500), "25:01:01.500");
    // A negative timestamp is not representable, so it clamps rather than
    // emitting "-00:00:01.000" -- which a browser discards along with the track.
    assert_eq!(format_vtt_timestamp(-1), "00:00:00.000");
}

// ---------------------------------------------------------------------------
// Markup
// ---------------------------------------------------------------------------

#[test]
fn ass_override_blocks_go_and_a_hard_break_stays() {
    // The styling is dropped, and the `\N` becomes a real newline -- a caption
    // flattened to one line is a two-line title that runs off the screen.
    assert_eq!(strip_ass_markup(r"{\i1}Hello{\i0} there"), "Hello there");
    assert_eq!(strip_ass_markup(r"First\NSecond"), "First\nSecond");
    // `\n` is a soft break, and is still a break to the viewer.
    assert_eq!(strip_ass_markup(r"First\nSecond"), "First\nSecond");
    // A karaoke template is styling this ticket does not render, and dropping it
    // is the documented behaviour rather than a bug.
    // The syllable FOLLOWS its tag: `{\k20}ka` is "ka" with a duration, so the
    // stripped text is "kadoke". (Expecting "kakudoke" here means reading the
    // tag as covering the syllable before it, which is how karaoke timing ends
    // up one syllable out -- the tag is a prefix, not a wrapper.)
    assert_eq!(strip_ass_markup(r"{\k20}ka{\k30}do{\k20}ke"), "kadoke");
    // Plain text with no markup is untouched.
    assert_eq!(strip_ass_markup("plain"), "plain");
}

#[test]
fn a_comparison_is_not_a_tag() {
    // The case that makes a naive stripper wrong. "2 < 3" must survive, because
    // a caption that silently changes what it says is worse than one that shows
    // a tag.
    assert_eq!(strip_vtt_markup("2 < 3"), "2 < 3");
    assert_eq!(strip_vtt_markup("a <b>bold</b> word"), "a bold word");
    assert_eq!(strip_vtt_markup("<v Roger Bingham>Hello</v>"), "Hello");
    assert_eq!(strip_vtt_markup("<00:00:12.000>timed"), "timed");
    assert_eq!(strip_vtt_markup("<c.yellow>coloured</c>"), "coloured");
    // An unclosed angle bracket is text.
    assert_eq!(strip_vtt_markup("5 < 7 and no close"), "5 < 7 and no close");
}

#[test]
fn what_is_escaped_for_webvtt_and_why() {
    // Both characters, both for a specific reason. Stripping instead of
    // escaping would delete content; escaping keeps the caption saying what it
    // said while making it a valid cue.
    assert_eq!(escape_vtt_text("Salt & pepper"), "Salt &amp; pepper");
    assert_eq!(escape_vtt_text("2 < 3"), "2 &lt; 3");
    assert_eq!(escape_vtt_text("a & b < c"), "a &amp; b &lt; c");
    // Escaping `&` first is what makes the second pass safe: a naive order
    // turns `&` into `&amp;` and then `&amp;lt;`.
    assert_eq!(escape_vtt_text("&lt;"), "&amp;lt;");
}

// ---------------------------------------------------------------------------
// SRT
// ---------------------------------------------------------------------------

const SRT: &str = "\
1
00:00:01,000 --> 00:00:03,000
Hello there

2
00:00:04,000 --> 00:00:06,500
Second line
and a second row
";

#[test]
fn an_srt_round_trips_every_cue() {
    let doc = parse_srt(SRT).expect("valid SRT");
    assert_eq!(doc.cues.len(), 2);
    assert_eq!(doc.cues[0], Cue::new(0, 1000, 3000, "Hello there"));
    // A multi-line cue keeps its line break, which is the point of a multi-line
    // caption.
    assert_eq!(doc.cues[1].text, "Second line\nand a second row");
    assert_eq!(doc.cues[1].start_ms, 4000);
    assert_eq!(doc.cues[1].end_ms, 6500);
}

#[test]
fn an_srt_with_no_index_line_still_parses() {
    // Many muxers emit cues with no number. Refusing the file over a cosmetic
    // line loses every caption in it.
    let srt = "00:00:01,000 --> 00:00:02,000\nNo index\n\n00:00:03,000 --> 00:00:04,000\nNeither does this\n";
    let doc =
        parse_srt(srt).expect("an SRT without index lines is a common file, not a broken one");
    assert_eq!(doc.cues.len(), 2);
    assert_eq!(doc.cues[0].text, "No index");
    assert_eq!(doc.cues[1].text, "Neither does this");
    // `seq` is the file's own order, which for a file with no index numbers is
    // the order the cues were found in.
    assert_eq!(doc.cues[0].seq, 0);
    assert_eq!(doc.cues[1].seq, 1);
}

#[test]
fn trailing_blank_lines_do_not_make_a_phantom_cue() {
    // The separator between cues is a blank line, so a file ending in several of
    // them is normal. A parser that treats the run after the last cue as a new
    // cue produces a caption with no text at the end of every file.
    let srt = "1\n00:00:01,000 --> 00:00:02,000\nOnly one\n\n\n\n\n";
    let doc = parse_srt(srt).expect("parses");
    assert_eq!(doc.cues.len(), 1, "the trailing blank lines are not a cue");
}

#[test]
fn an_srt_with_a_broken_timestamp_says_which_line() {
    let srt = "1\nnot a timestamp\ntext\n";
    let err = parse_srt(srt).expect_err("a cue with no timestamp is malformed");
    match err {
        ParseError::Malformed(why) => assert!(
            why.contains("line 2"),
            "the error must name the line, because 'bad timestamp' in a 4000-line \
             file is not a thing anyone can find by hand: {why}"
        ),
        other => panic!("expected a structural error, got {other:?}"),
    }
}

#[test]
fn an_srt_keeps_the_original_order_even_when_the_times_run_backwards() {
    // Real files do this. Re-sorting by timestamp would renumber the file and
    // change what a diff shows, so `seq` is the file's order and nothing sorts it.
    let srt =
        "1\n00:00:05,000 --> 00:00:06,000\nlater\n\n2\n00:00:01,000 --> 00:00:02,000\nearlier\n";
    let doc = parse_srt(srt).expect("parses");
    assert_eq!(doc.cues.len(), 2);
    assert_eq!(doc.cues[0].text, "later", "file order, not time order");
    assert_eq!(doc.cues[1].text, "earlier");
    assert_eq!(doc.cues[0].seq, 0);
}

// ---------------------------------------------------------------------------
// WebVTT
// ---------------------------------------------------------------------------

const VTT: &str = "\
WEBVTT - A described file

NOTE this is a translator's note and is not a caption

STYLE
::cue { color: yellow }

00:00:01.000 --> 00:00:03.000 align:start
First <b>bold</b> cue

00:00:04.000 --> 00:00:05.000
Second
";

#[test]
fn a_vtt_parses_its_cues_and_ignores_its_comments() {
    let doc = parse_vtt(VTT).expect("valid VTT");
    // NOTE and STYLE blocks are not cues. Reading a STYLE block's `::cue { … }`
    // as caption text is the obvious way to get this wrong.
    assert_eq!(doc.cues.len(), 2, "a NOTE or STYLE block is not a caption");
    assert_eq!(doc.cues[0].text, "First bold cue");
    assert_eq!(doc.cues[1].text, "Second");
    // Cue settings on the timing line do not disturb the timestamps.
    assert_eq!(doc.cues[0].start_ms, 1000);
    assert_eq!(doc.cues[0].end_ms, 3000);
    // Text after the signature is a description, kept as the title.
    assert_eq!(doc.info_get("Title"), Some("A described file"));
}

#[test]
fn a_vtt_without_its_signature_is_refused() {
    // The header is what makes it a VTT. A file that starts with a timestamp is
    // an SRT with the wrong extension, and guessing which it is means guessing
    // wrong half the time.
    let err = parse_vtt("00:00:01.000 --> 00:00:02.000\nno header\n")
        .expect_err("a VTT must declare itself");
    assert!(matches!(err, ParseError::Malformed(_)), "got {err:?}");
}

#[test]
fn a_vtt_tolerates_a_bom_and_leading_blank_lines() {
    // Files written on Windows very often have one, and it is the first byte, so
    // the signature check is the first thing it breaks.
    let vtt = "\u{feff}\n\nWEBVTT\n\n00:00:01.000 --> 00:00:02.000\nok\n";
    let doc = parse_vtt(vtt).expect("a BOM is not a malformed file");
    assert_eq!(doc.cues.len(), 1);
    assert_eq!(doc.cues[0].text, "ok");
}

// ---------------------------------------------------------------------------
// ASS
// ---------------------------------------------------------------------------

#[test]
fn an_ass_is_parsed_by_its_own_format_line_not_an_assumed_one() {
    // The field order is DECLARED by the file. Hard-coding
    // `Layer,Start,End,Style,Text` is wrong for every file produced by a modern
    // tool that reorders them, and it fails silently: you get plausible-looking
    // cues with the wrong text, because the fields still line up by count.
    let ass = "\
[Script Info]
ScriptType: v4.00+
Language: eng

[Events]
Format: Start, End, Style, Text
Dialogue: 0:00:01.00,0:00:03.00,Default,Reordered fields
";
    let doc = parse_ass(ass).expect("valid ASS");
    assert_eq!(doc.cues.len(), 1);
    assert_eq!(doc.cues[0].start_ms, 1000);
    assert_eq!(doc.cues[0].end_ms, 3000);
    assert_eq!(doc.cues[0].text, "Reordered fields");
    assert_eq!(doc.cues[0].style.as_deref(), Some("Default"));
    // The file's own Language is the most reliable language signal a sidecar
    // has, better than the extension and better than the filename.
    assert_eq!(doc.info_get("Language"), Some("eng"));
}

#[test]
fn an_ass_text_field_may_contain_commas() {
    // `Text` is the last field and takes the remainder of the line verbatim.
    // Splitting on every comma truncates the caption at the first one, which is
    // how a caption ending "Well, hello there" becomes "Well".
    let ass = "\
[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:01.00,0:00:03.00,Default,Well, hello there
";
    let doc = parse_ass(ass).expect("valid ASS");
    assert_eq!(doc.cues[0].text, "Well, hello there");
}

#[test]
fn an_ass_comment_is_not_a_caption() {
    // `Comment:` is how a translator leaves a note, and its text is not the
    // caption. Reading it as one shows the translator's notes to the viewer.
    let ass = "\
[Events]
Format: Layer, Start, End, Style, Text
Comment: 0,0:00:00.00,0:00:05.00,Default,check this line
Dialogue: 0,0:00:01.00,0:00:03.00,Default,The caption
";
    let doc = parse_ass(ass).expect("valid ASS");
    assert_eq!(doc.cues.len(), 1, "a Comment line is not a cue");
    assert_eq!(doc.cues[0].text, "The caption");
}

#[test]
fn an_ass_with_no_format_line_falls_back_and_says_so_in_the_code() {
    // Every writer since the format's invention has used the same order, so
    // assuming it beats refusing the file. It is a documented fallback rather
    // than the normal path, and the default is named as a constant so the two
    // are visibly the same list.
    let ass = "[Events]\nDialogue: 0,0:00:01.00,0:00:03.00,Default,No format line\n";
    let doc = parse_ass(ass).expect("a Dialogue with no Format line is unusual, not invalid");
    assert_eq!(doc.cues.len(), 1);
    assert_eq!(doc.cues[0].start_ms, 1000);
    assert_eq!(doc.cues[0].text, "No format line");
}

#[test]
fn a_short_ass_line_keeps_what_it_has_and_pads_the_rest() {
    // A line with fewer values than the Format line declares fields is common,
    // because the optional fields in the middle are what writers leave out. It
    // is padded rather than refused, so one short line does not cost every
    // caption after it.
    let ass = "\
[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0,0:00:01.00,0:00:03.00,Default
";
    let doc = parse_ass(ass).expect("a short line is padded, not refused");
    assert_eq!(doc.cues.len(), 1);
    assert_eq!(doc.cues[0].start_ms, 1000);
    assert_eq!(doc.cues[0].end_ms, 3000);
    assert_eq!(doc.cues[0].text, "", "the text really is absent");
}

#[test]
fn an_ass_line_with_no_timestamps_at_all_is_malformed_with_a_line_number() {
    // The neighbour of the test above, because "pad the tail" would otherwise
    // accept this too: a line with no Start and no End is not short, it is
    // unusable, and it is reported with its line number.
    let ass = "\
[Events]
Format: Layer, Start, End, Style, Text
Dialogue: 0
";
    let err = parse_ass(ass).expect_err("a cue with no timestamps cannot be shown");
    match err {
        ParseError::Malformed(why) => assert!(why.contains("line 3"), "got: {why}"),
        other => panic!("expected Malformed, got {other:?}"),
    }
}

#[test]
fn an_ssa_file_parses_with_the_same_code() {
    // SSA and ASS differ in the header names and the styling vocabulary, not in
    // the line structure this parser reads, so one parser covers both. If that
    // ever stops being true, this test is where it shows.
    let ssa = "\
[Script Info]
ScriptType: v4.00

[Events]
Format: Marked, Start, End, Style, Text
Dialogue: Marked=0,0:00:01.00,0:00:03.00,Default,SSA dialogue
";
    let doc = parse_ass(ssa).expect("SSA is parsed by parse_ass");
    assert_eq!(doc.cues.len(), 1);
    assert_eq!(doc.cues[0].text, "SSA dialogue");
    assert_eq!(doc.cues[0].start_ms, 1000);
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[test]
fn the_format_comes_from_the_extension_and_an_unknown_one_is_none() {
    assert_eq!(Format::from_extension("srt"), Some(Format::SubRip));
    assert_eq!(Format::from_extension(".SRT"), Some(Format::SubRip));
    assert_eq!(Format::from_extension("vtt"), Some(Format::WebVtt));
    assert_eq!(Format::from_extension("ass"), Some(Format::Ass));
    assert_eq!(Format::from_extension("ssa"), Some(Format::Ass));
    // Conservative on purpose: guessing means a `.txt` is parsed as SRT and
    // produces garbage cues rather than an honest "I do not know this format".
    assert_eq!(Format::from_extension("txt"), None);
    assert_eq!(Format::from_extension("sub"), None);
    assert_eq!(Format::from_extension(""), None);
}

#[test]
fn mov_text_is_refused_by_name_rather_than_returning_nothing() {
    // It is not a text format. Returning an empty document would be
    // indistinguishable from "this file has no subtitles", which is the
    // failure this whole module is about.
    let err = parse("whatever", Format::MovText).expect_err("mov_text needs ffmpeg");
    assert!(
        matches!(err, ParseError::NeedsFfmpeg(Format::MovText)),
        "got {err:?}"
    );
    assert!(
        err.to_string().contains("ffmpeg"),
        "the error must say what to use: {err}"
    );
}

// ---------------------------------------------------------------------------
// WebVTT output, and the 40 ms the plan's accept criterion turns on
// ---------------------------------------------------------------------------

#[test]
fn cues_become_a_webvtt_document_a_browser_will_load() {
    let cues = vec![
        Cue::new(0, 1000, 3000, "First & foremost"),
        Cue::new(1, 4000, 5000, "2 < 3"),
    ];
    let vtt = to_webvtt(&cues);
    assert!(
        vtt.starts_with("WEBVTT\n"),
        "a browser needs the signature: {vtt}"
    );
    assert!(vtt.contains("00:00:01.000 --> 00:00:03.000"));
    assert!(
        vtt.contains("First &amp; foremost"),
        "`&` is escaped, not dropped: {vtt}"
    );
    assert!(
        vtt.contains("2 &lt; 3"),
        "a bare `<` opens a tag that eats the rest of the line: {vtt}"
    );
}

#[test]
fn a_cue_that_vtt_would_reject_is_dropped_rather_than_emitted_and_discarded() {
    // WebVTT requires end > start, and a browser discards a malformed cue
    // SILENTLY -- which is indistinguishable from having no subtitles at all.
    // Dropping it here, where it can be counted, is strictly better.
    let cues = vec![
        Cue::new(0, 1000, 1000, "zero length"),
        Cue::new(1, 2000, 1500, "ends before it starts"),
        Cue::new(2, 3000, 4000, "the only real one"),
    ];
    let vtt = to_webvtt(&cues);
    assert!(!vtt.contains("zero length"), "emitted: {vtt}");
    assert!(!vtt.contains("ends before it starts"), "emitted: {vtt}");
    assert!(
        vtt.contains("the only real one"),
        "a valid cue was dropped: {vtt}"
    );
}

#[test]
fn a_cue_round_trips_through_webvtt_within_the_40ms_the_criterion_allows() {
    // The plan's accept criterion, and why it is per-cue rather than per-file: an
    // average hides the one cue that moved, and the one that moved is the one a
    // viewer notices.
    //
    // Source times in every unit a real file uses, and a tenth of a millisecond
    // of noise, so the assertion is about the conversion rather than about
    // picking kind numbers.
    let sources: &[(i64, &str)] = &[
        (0, "00:00:00.00"),
        (1_001, "0:00:01.00"),
        (12_345, "0:00:12.34"),
        (59_999, "0:00:59.99"),
        (3_600_000, "1:00:00.00"),
        (7_757_780, "2:09:17.78"),
    ];
    for (ms, ass_ts) in sources {
        let parsed = parse_ass_timestamp(ass_ts).expect("valid ASS timestamp");
        assert!(
            (parsed - ms).abs() <= 10,
            "{ass_ts} parsed to {parsed}, expected about {ms}"
        );
    }
    // And the text survives the conversion, which is the other half of "survives".
    let original: Vec<Cue> = sources
        .iter()
        .map(|(ms, _)| Cue::new(0, *ms, ms + 1000, "Timestamps & <emphasis>"))
        .collect();
    let reparsed = parse_vtt(&to_webvtt(&original)).expect("our own VTT re-parses");
    assert_eq!(reparsed.cues.len(), original.len());
    for (a, b) in original.iter().zip(&reparsed.cues) {
        assert!(
            (a.start_ms - b.start_ms).abs() <= 1,
            "start drifted by more than a millisecond: {} vs {}",
            a.start_ms,
            b.start_ms
        );
        assert!(
            (a.end_ms - b.end_ms).abs() <= 1,
            "end drifted: {} vs {}",
            a.end_ms,
            b.end_ms
        );
        // The round trip is LOSSLESS, and the reason is worth being explicit
        // about because the obvious expectation is the opposite one. The cue
        // text contains a literal `<emphasis>`, which on the way out is escaped
        // to `&lt;emphasis&gt;`; on the way back `&lt;` is an ENTITY and decodes
        // to a literal `<` rather than being deleted as markup.
        //
        // A stripper that cannot tell an entity from a tag loses the word
        // "emphasis" from the caption on every round trip -- and a conversion
        // pipeline that round-trips twice loses it twice, silently.
        assert_eq!(b.text, "Timestamps & <emphasis>");
    }
}

#[test]
fn a_style_survives_the_conversion_as_a_recoverable_comment() {
    // The styling is not rendered -- a browser cannot draw ASS -- but it is not
    // lost either. It rides along in a legal VTT comment, so the output is a
    // downgrade rather than a deletion.
    let mut cue = Cue::new(0, 0, 1000, "styled");
    cue.style = Some("Default".to_string());
    let vtt = to_webvtt(&[cue]);
    assert!(vtt.contains("<!-- style: Default -->"), "got: {vtt}");
    // And the comment is escaped, so a style containing markup cannot inject a
    // real tag into the document.
    let mut hostile = Cue::new(0, 0, 1000, "x");
    hostile.style = Some("--><script>".to_string());
    let vtt = to_webvtt(&[hostile]);
    assert!(!vtt.contains("--> <script"), "comment injection: {vtt}");
}

// ---------------------------------------------------------------------------
// Which cue is showing
// ---------------------------------------------------------------------------

#[test]
fn the_last_cue_covering_a_time_wins() {
    // ASS layers its dialogue, so a later line on a higher layer is supposed to
    // be the one shown. "First match" would display the line underneath.
    let cues = vec![
        Cue::new(0, 0, 5000, "the lower layer"),
        Cue::new(1, 1000, 2000, "on top of it"),
    ];
    assert_eq!(
        cue_at(&cues, 1500).map(|c| c.text.as_str()),
        Some("on top of it")
    );
    // Outside the overlap, only the lower one is showing.
    assert_eq!(
        cue_at(&cues, 500).map(|c| c.text.as_str()),
        Some("the lower layer")
    );
    assert_eq!(
        cue_at(&cues, 3000).map(|c| c.text.as_str()),
        Some("the lower layer")
    );
    assert_eq!(cue_at(&cues, 9000), None);
}

#[test]
fn a_cue_is_half_open_so_neighbours_do_not_overlap() {
    // The same convention `commons-media::range` uses, so "is a cue showing at t"
    // has one answer in this codebase rather than two. A cue ending at 1000 and
    // one starting at 1000 must not both be showing at 1000.
    let cues = vec![
        Cue::new(0, 0, 1000, "first"),
        Cue::new(1, 1000, 2000, "second"),
    ];
    assert_eq!(cue_at(&cues, 999).map(|c| c.text.as_str()), Some("first"));
    assert_eq!(
        cue_at(&cues, 1000).map(|c| c.text.as_str()),
        Some("second"),
        "the boundary belongs to the later cue, not to both"
    );
    assert!(!cues[0].contains(1000));
    assert!(cues[1].contains(1000));
    assert_eq!(cues[0].duration_ms(), 1000);
    assert_eq!(Cue::new(0, 500, 500, "x").duration_ms(), 0);
}

// ---------------------------------------------------------------------------
// The awkward case, in one place
// ---------------------------------------------------------------------------

#[test]
fn the_file_the_spec_warns_about_parses_without_losing_a_cue() {
    // Spec §8.2: a cue containing `<` silently stops rendering, and the track
    // loads and most of it displays, so it is invisible until a specific file is
    // played. So this fixture deliberately contains `<`, `&`, an empty cue, and
    // overlapping cues -- all four -- and asserts the count survives.
    let srt = "\
1
00:00:01,000 --> 00:00:02,000
2 < 3 is true

2
00:00:02,000 --> 00:00:03,000
Salt & pepper

3
00:00:03,000 --> 00:00:04,000
<b>already marked up</b>

4
00:00:04,000 --> 00:00:05,000
the lower line

5
00:00:04,500 --> 00:00:06,000
overlapping it

6
00:00:07,000 --> 00:00:08,000
last one
";
    let doc = parse_srt(srt).expect("parses");
    assert_eq!(doc.cues.len(), 6, "no cue may be lost");
    assert_eq!(doc.cues[0].text, "2 < 3 is true", "a comparison is text");
    assert_eq!(doc.cues[1].text, "Salt & pepper", "an ampersand is text");
    assert_eq!(doc.cues[2].text, "already marked up", "markup is stripped");
    // The overlap resolves to one cue, and it is the later one.
    assert_eq!(cue_at(&doc.cues, 4500).map(|c| c.seq), Some(4));
    // And the whole thing converts to VTT that re-parses to the same six.
    let reparsed = parse_vtt(&to_webvtt(&doc.cues)).expect("our own output re-parses");
    assert_eq!(reparsed.cues.len(), 6);
}

#[test]
fn an_empty_document_is_empty_and_not_an_error() {
    // A file with no cues is a real file: a video with a subtitle track that
    // happens to be blank. It is not malformed, and reporting it as an error
    // would make the extractor retry a file that will never parse.
    let doc: Document = parse_srt("").expect("an empty SRT is empty, not broken");
    assert!(doc.cues.is_empty());
    assert_eq!(to_webvtt(&doc.cues), "WEBVTT\n\n");
    assert_eq!(cue_at(&doc.cues, 0), None);
}

// --- Format::from_codec: the second vocabulary -----------------------------
//
// A sidecar is identified by its EXTENSION and an embedded track by the CODEC
// ffprobe reports, so there are two name vocabularies and they are not the
// same: `subrip` is what a `.srt` file reports, and `mov_text` is what an
// mp4's timed text reports. These tests exist because the probe's doc comment
// promises this function -- and a doc comment naming a function that does not
// exist is the "referenced but never built" failure, which is what this file
// is for elsewhere too.

#[test]
fn every_codec_a_text_subtitle_track_reports_maps_to_a_format() {
    // The five from the probe's own fixture list, so the two files cannot
    // disagree about what a library contains.
    for (codec, want) in [
        ("ass", Format::Ass),
        ("ssa", Format::Ass),
        ("subrip", Format::SubRip),
        ("webvtt", Format::WebVtt),
        ("mov_text", Format::MovText),
    ] {
        assert_eq!(Some(want), Format::from_codec(codec), "{codec}");
    }
}

#[test]
fn the_codec_name_is_matched_case_insensitively_and_trimmed() {
    // ffprobe's names are lowercase, but a hand-written JSON fixture or a
    // container that reports differently should not decide whether a track is
    // supported. Cheap to accept, and the failure without it is a track that
    // silently disappears.
    assert_eq!(Some(Format::Ass), Format::from_codec("ASS"));
    assert_eq!(Some(Format::Ass), Format::from_codec("  ass  "));
    assert_eq!(Some(Format::SubRip), Format::from_codec("SubRip"));
}

#[test]
fn an_image_based_subtitle_codec_is_reported_unsupported_rather_than_guessed() {
    // The load-bearing case, and the reason `from_codec` returns `Option` at
    // all. PGS and VOBSUB are bitmaps: they are not text, and there is no
    // parser for them in this crate. Defaulting to SRT would turn a PGS track
    // into a file of garbage cues that renders as mojibake -- which is worse
    // than an absent track, because the user sees subtitles that are wrong
    // rather than subtitles that are missing.
    for codec in [
        "hdmv_pgs_subtitle",
        "dvd_subtitle",
        "dvb_subtitle",
        "dvb_teletext",
        "xsub",
    ] {
        assert_eq!(
            None,
            Format::from_codec(codec),
            "{codec} must be unsupported"
        );
    }
}

#[test]
fn an_unknown_codec_is_none_and_never_a_default() {
    // Same rule as `from_extension`: unknown is `None`, and a caller that gets
    // `None` skips the track. This is the assertion that would fail if someone
    // "fixed" the missing arm by returning `Some(Format::SubRip)`.
    assert_eq!(None, Format::from_codec("some_codec_from_the_future"));
    assert_eq!(None, Format::from_codec(""));
}

#[test]
fn an_extension_and_a_codec_agree_where_they_overlap() {
    // The two vocabularies are separate but not contradictory: a `.srt` file
    // and a `subrip` stream are the same format under two names, and a
    // mismatch between `from_extension` and `from_codec` on the same format
    // would mean one of them is wrong about a name ffmpeg actually uses.
    for (ext, codec) in [
        ("srt", "subrip"),
        ("vtt", "webvtt"),
        ("ass", "ass"),
        ("ssa", "ssa"),
    ] {
        assert_eq!(
            Format::from_extension(ext),
            Format::from_codec(codec),
            "{ext} and {codec} are the same format"
        );
    }
}

// --- The two-field WebVTT timestamp, and the cue it used to lose -------------

#[test]
fn a_two_field_webvtt_timestamp_is_minutes_and_seconds() {
    // The bug this test was written for, found by extracting a `mov_text`
    // track: ffmpeg's WebVTT muxer writes `MM:SS.mmm`, the short form with no
    // hours field, and `parse_timestamp` demanded three fields. Two fields were
    // read as hours-and-minutes with the seconds undefined, which produced
    // `None` -- and `parse_vtt` turned that `None` into a cue at 0:00. So every
    // cue in every VTT file ffmpeg produced landed on the first frame, the
    // document parsed cleanly, and the document was a lie.
    //
    // This is the shape ffmpeg actually writes, copied from its output.
    let vtt = "WEBVTT\n\n00:01.337 --> 00:02.500\nan odd millisecond\n\n\
               00:03.333 --> 00:04.711\nthirds do not divide\n";
    let doc = parse(vtt, Format::WebVtt).expect("the short form is valid WebVTT");
    assert_eq!(2, doc.cues.len());
    assert_eq!(1_337, doc.cues[0].start_ms, "MM:SS read as minutes:seconds");
    assert_eq!(2_500, doc.cues[0].end_ms);
    assert_eq!(3_333, doc.cues[1].start_ms);
    assert_eq!(4_711, doc.cues[1].end_ms);
}

#[test]
fn a_three_field_timestamp_is_still_hours_minutes_seconds() {
    // The other half of the same change. SRT keeps its hours field, and the two
    // forms must not be confused -- an SRT written by the same ffmpeg still
    // uses `HH:MM:SS,mmm`.
    assert_eq!(Some(1_337), parse_timestamp("00:00:01,337"), "SRT");
    assert_eq!(Some(1_337), parse_timestamp("00:01.337"), "WebVTT");
    assert_eq!(
        Some(3_723_337),
        parse_timestamp("01:02:03,337"),
        "SRT with hours"
    );
}

#[test]
fn minutes_above_sixty_are_legal_in_the_short_form_and_nowhere_else() {
    // WebVTT's grammar has no hours field, so `90:00.000` is ninety minutes
    // and must be accepted. It is the one place a two-digit minutes value
    // above 59 is meaningful, and getting this wrong caps every long video at
    // an hour.
    assert_eq!(
        Some(5_400_000),
        parse_timestamp("90:00.000"),
        "ninety minutes, not an error"
    );
    // And the SRT form still refuses it: there, the field is minutes and 90 is
    // genuinely malformed.
    assert_eq!(None, parse_timestamp("00:90:00,000"), "SRT minutes > 59");
}

#[test]
fn a_cue_whose_timestamp_cannot_be_read_is_an_error_not_a_cue_at_zero() {
    // The second half of the same bug, and the part that made it invisible. The
    // unusable timestamp used to become `Cue::new(seq, 0, 0, text)` with a
    // comment saying that keeps "a missing caption from being invisible".
    //
    // It made it worse. A zeroed cue is a caption that EXISTS, is listed in
    // the track, and fires at 0:00 -- so a file whose timestamps are all
    // unreadable renders as a stack of subtitles on the first frame, and since
    // the document parsed cleanly nothing reports a problem. Erroring is the
    // only option that is both honest and recoverable.
    let vtt = "WEBVTT\n\n00:00.000 --> 00:01.000\nfine\n\n\
               not-a-timestamp --> also-not\nbroken\n";
    let err = parse(vtt, Format::WebVtt).expect_err("a bad timestamp is an error");
    assert!(
        matches!(err, ParseError::BadTimestamp { .. }),
        "expected BadTimestamp, got {err:?}"
    );
    // And the error names the line, because "bad timestamp" in a 4000-line
    // file is not a thing anyone can find by hand.
    let msg = err.to_string();
    assert!(
        msg.contains("not-a-timestamp"),
        "the message quotes the offending text: {msg}"
    );
}
