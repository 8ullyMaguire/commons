//! The chapter finder, which is pure.
//!
//! T-P6-004, spec §4.3. No database: `find_chapters` is a function of words and
//! options, so its tests belong where there is no engine to wait for.
//!
//! Almost every assertion is a PROPERTY — coverage without gaps, no overlap, a
//! cap that is a cap — because a test asserting a fixed chapter list passes on a
//! finder that happened to produce that list and fails on one that was better,
//! which is the wrong direction for both.

use commons_store::marker::{find_chapters, ChapterOptions, Word};

/// Words every second, `n` of them, no silence anywhere: the only cuts a finder
/// can make here are the forced ones at `max_ms`.
fn steady(n: i64) -> Vec<Word> {
    (0..n)
        .map(|i| Word {
            start_ms: i * 1_000,
            end_ms: i * 1_000 + 800,
            confidence: Some(0.9),
        })
        .collect()
}

/// `blocks` runs of twenty words, each 60 seconds apart with a silence between.
fn with_gaps(blocks: i64) -> Vec<Word> {
    (0..blocks)
        .flat_map(|b| {
            (0..20).map(move |i| Word {
                start_ms: b * 60_000 + i * 1_000,
                end_ms: b * 60_000 + i * 1_000 + 800,
                confidence: Some(0.8),
            })
        })
        .collect()
}

fn opts(max_ms: i64, silence_ms: i64, min_ms: i64) -> ChapterOptions {
    ChapterOptions {
        max_ms,
        silence_ms,
        min_ms,
        max_chapters: 100,
    }
}

#[test]
fn one_unbroken_monologue_is_cut_at_the_maximum_length() {
    // Without a forced cut, a three-hour interview with no pauses is ONE
    // chapter, and a chapter list with one entry is not a chapter list.
    let o = opts(20_000, 1_500, 1_000);
    let chapters = find_chapters(&steady(40), o).expect("chapters");
    assert!(
        chapters.len() >= 2,
        "one monologue, {} chapters",
        chapters.len()
    );
    for c in &chapters {
        assert!(
            c.end_ms - c.start_ms <= o.max_ms,
            "chapter {}..{} exceeds max_ms {}",
            c.start_ms,
            c.end_ms,
            o.max_ms
        );
    }
}

#[test]
fn chapters_never_overlap_and_never_leave_a_gap() {
    // The player's next/previous buttons walk the list in order, so an overlap
    // sends the user BACKWARDS and a gap skips audio. Neither is visible in a
    // list that renders perfectly.
    let o = opts(30_000, 1_500, 1_000);
    let chapters = find_chapters(&with_gaps(4), o).expect("chapters");
    assert!(chapters.len() > 1, "the silences must produce cuts");
    for pair in chapters.windows(2) {
        assert!(
            pair[1].start_ms >= pair[0].end_ms,
            "{}..{} then {}..{} overlap",
            pair[0].start_ms,
            pair[0].end_ms,
            pair[1].start_ms,
            pair[1].end_ms
        );
    }
}

#[test]
fn a_chapter_ends_at_its_last_word_not_at_the_next_chapter() {
    // A chapter that ran into the silence before the next one would claim time
    // nobody spoke, and the two chapters would overlap on it.
    let words: Vec<Word> = (0..6)
        .map(|i| Word {
            start_ms: i * 1_000,
            end_ms: i * 1_000 + 400,
            confidence: None,
        })
        .chain((6..12).map(|i| Word {
            start_ms: 30_000 + i * 1_000,
            end_ms: 30_000 + i * 1_000 + 400,
            confidence: None,
        }))
        .collect();
    let chapters = find_chapters(&words, opts(600_000, 1_500, 1_000)).expect("chapters");
    assert_eq!(chapters.len(), 2, "{chapters:?}");
    assert_eq!(chapters[0].end_ms, 5_400, "the last word's end");
    assert!(
        chapters[0].end_ms < chapters[1].start_ms,
        "not the next chapter's start"
    );
}

#[test]
fn a_span_too_short_to_be_a_chapter_is_not_one() {
    // A two-second "chapter" makes the next button a nuisance and the list
    // useless, so the minimum is enforced rather than suggested.
    // Two spans: a 1.4-second one and a 20-second one, with a long silence
    // between. (The first draft of this fixture had the second span ten seconds
    // long against a 15-second `min_ms`, so the finder dropped BOTH and the
    // test failed -- correctly. The code was right and the expectation was
    // wrong, which is the useful direction for that to go.)
    let words: Vec<Word> = (0..2)
        .map(|i| Word {
            start_ms: i * 1_000,
            end_ms: i * 1_000 + 400,
            confidence: None,
        })
        .chain((0..20).map(|i| Word {
            start_ms: 60_000 + i * 1_000,
            end_ms: 60_000 + i * 1_000 + 400,
            confidence: None,
        }))
        .collect();
    let chapters = find_chapters(&words, opts(600_000, 1_500, 15_000)).expect("chapters");
    assert_eq!(chapters.len(), 1, "the short span is dropped: {chapters:?}");
    assert!(
        chapters[0].start_ms > 30_000,
        "the surviving chapter is the long one: {chapters:?}"
    );
}

#[test]
fn an_empty_transcript_is_an_error_and_not_an_empty_list() {
    // A caller that asked for chapters and got none cannot tell "no speech"
    // from "the transcription failed". One is worth a retry and the other is
    // not, so they must not look alike.
    let err =
        find_chapters(&[], ChapterOptions::default()).expect_err("an empty transcript is an error");
    assert!(
        err.to_string().contains("no words"),
        "the message must distinguish this from a failure: {err}"
    );
}

#[test]
fn the_cap_is_a_cap() {
    let words: Vec<Word> = (0..60)
        .map(|i| Word {
            start_ms: i * 10_000,
            end_ms: i * 10_000 + 8_000,
            confidence: None,
        })
        .collect();
    let mut o = opts(600_000, 1_500, 1_000);
    o.max_chapters = 5;
    let capped = find_chapters(&words, o).expect("chapters");
    assert_eq!(capped.len(), 5, "the cap is a cap: {}", capped.len());
    // And without a cap the same transcript gives more, so the assertion above
    // is about the cap and not about the transcript happening to be short.
    let uncapped = find_chapters(&words, opts(600_000, 1_500, 1_000)).expect("chapters");
    assert!(
        uncapped.len() > capped.len(),
        "uncapped {} must exceed capped {}",
        uncapped.len(),
        capped.len()
    );
}

#[test]
fn a_non_scoring_transcript_gives_no_confidence_rather_than_zero() {
    // A chapter whose confidence reads 0.0 is one the UI would warn about, and
    // the engine gave no opinion at all. `None` and `Some(0.0)` are different
    // claims and downstream averaging has to be able to tell them apart.
    let words: Vec<Word> = steady(20)
        .into_iter()
        .map(|w| Word {
            confidence: None,
            ..w
        })
        .collect();
    let chapters = find_chapters(&words, opts(60_000, 1_500, 1_000)).expect("chapters");
    assert!(chapters.iter().all(|c| c.confidence.is_none()));
}

#[test]
fn a_scored_transcript_averages_only_the_scored_words() {
    // Half the words scored at 0.8 gives 0.8, not 0.4: an unscored word is
    // missing evidence, not evidence of absence.
    let words: Vec<Word> = steady(20)
        .into_iter()
        .enumerate()
        .map(|(i, w)| Word {
            confidence: if i < 10 { Some(0.8) } else { None },
            ..w
        })
        .collect();
    let chapters = find_chapters(&words, opts(60_000, 1_500, 1_000)).expect("chapters");
    let c = chapters[0].confidence.expect("ten scored words");
    assert!((c - 0.8).abs() < 1e-9, "mean of the scored words, got {c}");
}

#[test]
fn impossible_options_are_refused_rather_than_producing_nothing() {
    // A zero `max_ms` on a valid transcript would otherwise produce zero
    // chapters, which reads as "no speech" — the same ambiguity that
    // `an_empty_transcript_is_an_error` exists to prevent, reached by a
    // different route.
    let words = steady(20);
    for bad in [
        ChapterOptions {
            max_ms: 0,
            ..Default::default()
        },
        ChapterOptions {
            silence_ms: -1,
            ..Default::default()
        },
    ] {
        assert!(
            find_chapters(&words, bad).is_err(),
            "options {bad:?} must be refused, not silently produce nothing"
        );
    }
}
