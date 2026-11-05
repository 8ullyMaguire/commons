//! The bounty rules, tested without a clock of their own and without a database.

use commons_bounty::{
    may_accept, may_propose, settle_the_winner, validate_new, BountyState, MAX_BUDGET,
    MAX_DAYS_OPEN, MIN_DESCRIPTION,
};

const NOW: &str = "2026-11-05T12:00:00Z";

fn long_enough() -> String {
    "Naruto fic read around 2019: Sakura learns a seal, there is a broken tanto in chapter 3, the ending is on \
     a rooftop in the rain."
        .to_owned()
}

#[test]
fn a_bounty_has_to_be_answerable_and_bounded() {
    assert!(validate_new(&long_enough(), 5, NOW, "2026-11-12T12:00:00Z").is_ok());

    // Short descriptions are refused with the concrete floor, not a generic message.
    let thin = validate_new("a fic pls", 5, NOW, "2026-11-12T12:00:00Z").expect_err("thin");
    assert!(thin.contains(&MIN_DESCRIPTION.to_string()), "{thin}");

    // Length and budget bounds are exclusive of nothing: 2000 chars and 500 credits are allowed.
    let two_thousand = "x".repeat(MIN_DESCRIPTION.max(1)).to_owned();
    assert!(validate_new(&two_thousand, MAX_BUDGET, NOW, "2026-11-12T12:00:00Z").is_ok());
    assert!(validate_new(&long_enough(), 0, NOW, "2026-11-12T12:00:00Z").is_err());
    assert!(validate_new(&long_enough(), MAX_BUDGET + 1, NOW, "2026-11-12T12:00:00Z").is_err());
    assert!(validate_new(&"x".repeat(2001), 5, NOW, "2026-11-12T12:00:00Z").is_err());
}

#[test]
fn a_close_date_must_be_in_the_future_and_within_the_limit() {
    // In the past.
    assert!(validate_new(&long_enough(), 5, NOW, "2026-11-05T11:59:59Z")
        .expect_err("past")
        .contains("already in the past"));
    // Exactly now is not open for any time at all.
    assert!(validate_new(&long_enough(), 5, NOW, NOW).is_err());
    // Past the ninety-day limit.
    let beyond = validate_new(&long_enough(), 5, NOW, "2027-03-01T12:00:00Z").expect_err("beyond");
    assert!(beyond.contains(&MAX_DAYS_OPEN.to_string()), "{beyond}");
    // At the limit is fine: the bound is inclusive on the day.
    assert!(validate_new(&long_enough(), 5, NOW, "2027-02-03T12:00:00Z").is_ok());
}

#[test]
fn nobody_accepts_their_own_answer_or_pays_a_closed_bounty() {
    // The self-pay refusal says what to do instead.
    let own = may_accept(
        BountyState::Open,
        "2026-11-12T12:00:00Z",
        "acct-1",
        "acct-1",
        NOW,
    )
    .expect_err("self");
    assert!(own.contains("your own") && own.contains("cancel"), "{own}");

    // Each closed state refuses, naming itself.
    for (state, word) in [
        (BountyState::Accepted, "accepted"),
        (BountyState::Cancelled, "cancelled"),
        (BountyState::Expired, "expired"),
    ] {
        let refused = may_accept(state, "2026-11-12T12:00:00Z", "a", "b", NOW).expect_err("closed");
        assert!(refused.contains(word), "{refused}");
    }

    // An open bounty past its close date is refused, and told to cancel so the credits come back.
    let late =
        may_accept(BountyState::Open, "2026-11-05T11:00:00Z", "a", "b", NOW).expect_err("late");
    assert!(
        late.contains("close date") && late.contains("cancel"),
        "{late}"
    );

    assert!(may_accept(BountyState::Open, "2026-11-12T12:00:00Z", "a", "b", NOW).is_ok());
}

#[test]
fn only_other_people_answer_an_open_bounty() {
    assert!(may_propose(BountyState::Open, "asker", "finder").is_ok());
    assert!(may_propose(BountyState::Open, "asker", "asker")
        .expect_err("own")
        .contains("your own"));
    assert!(may_propose(BountyState::Accepted, "asker", "finder")
        .expect_err("accepted")
        .contains("accepted"));
    // Credits are only committed while open.
    assert!(BountyState::Open.holds_credits());
    assert!(!BountyState::Cancelled.holds_credits());
    assert!(!BountyState::Expired.holds_credits());
    assert!(!BountyState::Accepted.holds_credits());
    // Unknown states do not silently become open.
    assert!(BountyState::parse("reopened").is_none());
}

#[test]
fn the_settlement_order_is_stated_and_can_be_cited() {
    // The rule the host has to follow: pay only after the compare-and-set made this bounty the winner, and only
    // once. This is the sentence code review points at when the order is questioned.
    assert!(settle_the_winner(BountyState::Accepted, true).is_ok());
    assert!(settle_the_winner(BountyState::Accepted, false).is_err());
    for state in [
        BountyState::Open,
        BountyState::Cancelled,
        BountyState::Expired,
    ] {
        assert!(
            settle_the_winner(state, true).is_err(),
            "{state:?} must not pay"
        );
    }
}
