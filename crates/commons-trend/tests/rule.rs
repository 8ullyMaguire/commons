//! The rule, before any data: both halves of "emerging", and the cases where the right answer is no verdict
//! at all.

use commons_trend::{assess, Counts, Verdict, MIN_RECENT};

fn counts(recent: i64, baseline: i64, baseline_weeks: f64) -> Counts {
    Counts {
        recent,
        baseline,
        baseline_weeks,
        // The default recent window is two weeks, and the tests are written in those terms.
        recent_weeks: 2.0,
    }
}

#[test]
fn a_fandom_that_doubled_its_rate_is_emerging_and_one_that_did_not_is_not() {
    // 8 baseline weeks, 2 works a week; 14 recent days with 8 works is 4 a week: 2x exactly.
    let at_threshold = assess(&counts(8, 16, 8.0));
    assert_eq!(at_threshold.verdict, Verdict::Emerging);
    assert!((at_threshold.ratio.expect("ratio") - 2.0).abs() < 1e-9);

    let just_under = assess(&counts(7, 16, 8.0));
    assert_eq!(just_under.verdict, Verdict::Steady, "{:?}", just_under);
}

#[test]
fn three_works_from_zero_baseline_is_new_not_infinitely_emerging() {
    let verdict = assess(&counts(3, 0, 8.0));
    assert_eq!(verdict.verdict, Verdict::New);
    assert_eq!(
        verdict.ratio, None,
        "no baseline means no ratio, not a division by zero"
    );
    assert!(
        verdict.reason.contains("none in the baseline"),
        "{}",
        verdict.reason
    );
}

#[test]
fn too_few_works_is_no_verdict_and_says_how_many_are_needed() {
    let quiet = assess(&counts(MIN_RECENT - 1, 200, 8.0));
    assert_eq!(quiet.verdict, Verdict::NoVerdict);
    assert!(
        quiet.reason.contains(&MIN_RECENT.to_string()),
        "the reason names the threshold: {}",
        quiet.reason
    );
    // A fandom with no history and one work is the case this guard exists for: the ratio would be
    // infinite and the honest answer is that we have seen one work.
    assert_eq!(assess(&counts(1, 0, 8.0)).verdict, Verdict::NoVerdict);
}

#[test]
fn a_collapsing_fandom_is_fading_and_the_middle_is_steady() {
    assert_eq!(
        assess(&counts(3, 64, 8.0)).verdict,
        Verdict::Fading,
        "0.375x baseline"
    );
    assert_eq!(
        assess(&counts(6, 32, 8.0)).verdict,
        Verdict::Steady,
        "1.5x baseline"
    );
}

#[test]
fn a_window_without_a_baseline_is_a_refusal_not_a_verdict() {
    let none = assess(&counts(10, 0, 0.0));
    assert_eq!(none.verdict, Verdict::NoVerdict);
    assert!(none.reason.contains("baseline"), "{}", none.reason);
}
