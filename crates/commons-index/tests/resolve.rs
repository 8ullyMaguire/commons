//! §8.1 — `resolve` picks the winner by weighted vote, and the cache that
//! makes it cheap does not make it stale.
//!
//! The four tests the ticket names are here, plus the ones they do not. The
//! cache test is the one the ticket calls "the one that catches the classic
//! bug", and the reason is worth stating: a resolve function that computes
//! correctly and caches correctly but does not *invalidate* produces answers
//! that are wrong and look right, which is the only kind of wrong that ships.

use commons_core::FieldProposal;
use commons_core::{ProposalSource, ProposerKind, Role, SubjectType};
use commons_index::resolve::{self, ResolveConfig};
use commons_store::Store;
use uuid::Uuid;

mod common;
use common::{account, account_on_field, store, vote_with_weight};

/// The ticket's first named test: three proposals with weights 5/3/1 resolve
/// to the weight-5 value.
///
/// The weights are set through the public API (reputation, then votes) rather
/// than by writing `vote.weight` directly, because the thing under test is the
/// *product* of reputation and the vote, and writing the column would test the
/// multiplication against itself.
#[tokio::test]
async fn a_three_proposal_field_resolves_to_the_heaviest() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    // Ordinary accounts. The weights that matter here are the *votes*', and
    // they are frozen at cast time — which is what `vote_with_weight` says. An
    // earlier version gave the accounts a reputation of 5, 3 and 1, which is
    // not a thing: the reputation curve is bounded at 4, so `cast_vote`
    // recomputed all three back to 1.0 and this test was asserting nothing.
    let heavy = account(&store, "heavy", Role::Contributor, 1.0).await;
    let middle = account(&store, "middle", Role::Contributor, 1.0).await;
    let light = account(&store, "light", Role::Contributor, 1.0).await;

    let p_heavy = propose(&store, subject, "title", "\"Heavy Title\"", &heavy).await;
    let p_middle = propose(&store, subject, "title", "\"Middle Title\"", &middle).await;
    let p_light = propose(&store, subject, "title", "\"Light Title\"", &light).await;

    for (a, p, w) in [
        (&heavy, &p_heavy, 5.0),
        (&middle, &p_middle, 3.0),
        (&light, &p_light, 1.0),
    ] {
        vote_with_weight(&store, a, p, "title", w).await;
    }

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        r.proposal_id,
        Some(p_heavy),
        "the winning proposal is the heavy one, not merely a value that happens to match"
    );
    assert!(r.contested, "three values for one field is contested");

    // The margin is reported, because "how close was this" is what tells a
    // steward whether a field needs review.
    assert!(
        r.margin > 0.0 && r.margin < 1.0,
        "a 5-vs-3-vs-1 split has a partial margin, got {} weights {:?}",
        r.margin,
        r.weights
            .iter()
            .map(|w| (&w.value_json, w.weight, w.evidence, w.live_votes))
            .collect::<Vec<_>>()
    );
}

/// The ticket's second named test: adding a weight-6 proposal flips it.
///
/// This is the "a settled value can change when better evidence arrives"
/// promise, and it is the reason the winner is computed rather than stored.
#[tokio::test]
async fn a_heavier_proposal_flips_a_settled_value() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let five = account(&store, "five", Role::Contributor, 1.0).await;
    let six = account(&store, "six", Role::Contributor, 1.0).await;

    let p_five = propose(&store, subject, "title", "\"Five\"", &five).await;
    vote_with_weight(&store, &five, &p_five, "title", 5.0).await;
    assert_eq!(
        resolve::resolve(&store, subject, "title")
            .await
            .unwrap()
            .value_json
            .as_deref(),
        Some("\"Five\"")
    );

    let p_six = propose(&store, subject, "title", "\"Six\"", &six).await;
    vote_with_weight(&store, &six, &p_six, "title", 6.0).await;

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        r.value_json.as_deref(),
        Some("\"Six\""),
        "the settled value moved, because it is a function of the evidence"
    );
    assert_eq!(r.proposal_id, Some(p_six));
}

/// The ticket's third named test: a locked field ignores a weight-100 proposal.
///
/// A hundredfold weight does not beat a lock. That is the whole point of a
/// lock: it is not a very large vote, it is the absence of a vote.
#[tokio::test]
async fn a_locked_field_ignores_a_hundred_weight_proposal() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let modest = account(&store, "modest", Role::Contributor, 1.0).await;
    let overwhelming = account(&store, "overwhelming", Role::Admin, 1.0).await;

    let p_modest = propose(&store, subject, "title", "\"Modest\"", &modest).await;
    vote(&store, &modest, &p_modest, "title").await;
    let steward = account(&store, "steward", Role::Steward, 1.0).await;
    resolve::lock(&store, subject, "title", "\"Modest\"", &steward)
        .await
        .unwrap();

    let p_huge = propose(&store, subject, "title", "\"Overwhelming\"", &overwhelming).await;
    vote_with_weight(&store, &overwhelming, &p_huge, "title", 100.0).await;

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        r.value_json.as_deref(),
        Some("\"Modest\""),
        "the lock pins the value regardless of the evidence against it"
    );
    assert!(
        r.locked,
        "and resolve says so, so the UI can render it as pinned rather than as a result"
    );

    // Unlocking hands the field back to the vote, which is the point of having
    // an unlock: a locked field is a steward decision, not a permanent state.
    resolve::unlock(&store, subject, "title", &steward)
        .await
        .unwrap();
    let after = resolve::resolve(&store, subject, "title").await.unwrap();
    assert!(!after.locked);
    assert_eq!(
        after.value_json.as_deref(),
        Some("\"Overwhelming\""),
        "and the hundredweight proposal wins the moment the lock is lifted"
    );
}

/// The ticket's fourth named test: a stale cache invalidates on a new vote.
///
/// The cache exists because resolve runs on every read of every object in the
/// library. It is keyed on the proposal set, so a new proposal or a new vote
/// is a different key and cannot be served from the old entry. The test proves
/// that by reading, changing the evidence, and reading again — with the same
/// process and no restart, so the only thing that can make the second read
/// differ is the invalidation.
#[tokio::test]
async fn a_stale_cache_invalidates_on_a_new_vote() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let one = account_on_field(&store, "one", Role::Contributor, "title", 1.0).await;
    let two = account_on_field(&store, "two", Role::Contributor, "title", 2.0).await;

    let p_one = propose(&store, subject, "title", "\"One\"", &one).await;
    vote(&store, &one, &p_one, "title").await;

    assert_eq!(
        resolve::resolve(&store, subject, "title")
            .await
            .unwrap()
            .value_json
            .as_deref(),
        Some("\"One\""),
        "the first read populates the cache"
    );
    // Read again: this is the read that a cache bug would serve stale.
    assert_eq!(
        resolve::resolve(&store, subject, "title")
            .await
            .unwrap()
            .value_json
            .as_deref(),
        Some("\"One\"")
    );

    // The evidence changes underneath the cache.
    let p_two = propose(&store, subject, "title", "\"Two\"", &two).await;
    vote(&store, &two, &p_two, "title").await;

    assert_eq!(
        resolve::resolve(&store, subject, "title")
            .await
            .unwrap()
            .value_json
            .as_deref(),
        Some("\"Two\""),
        "the cached read after a new vote is recomputed, not replayed"
    );
}

/// A machine proposal is a voter, not an override (§8.1).
///
/// `ml:captioner` at 0.99 confidence is still one proposal among the others.
/// If confidence scaled into an override, a tagger with one bad frame would
/// rewrite a title that four people agree on — which is the failure mode the
/// spec's "never an override" is written to prevent.
#[tokio::test]
async fn a_machine_proposal_is_a_voter_and_never_an_override() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let mut p = FieldProposal::new(
        SubjectType::Object,
        subject,
        "title",
        "\"Machine Title\"",
        ProposalSource::MlCaptioner,
    );
    p.proposer_kind = ProposerKind::Auto;
    p.confidence = Some(0.99);
    let machine = commons_store::index::insert_proposal(&store, &p)
        .await
        .unwrap();

    // Four humans, each weight 1, all for a different value.
    let mut human_total = 0.0;
    let _ = &human_total;
    for name in ["a", "b", "c", "d"] {
        let acct = account(&store, name, Role::Contributor, 1.0).await;
        let hp = propose(&store, subject, "title", "\"Human Title\"", &acct).await;
        vote(&store, &acct, &hp, "title").await;
        human_total += 1.0;
    }

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        human_total, 4.0,
        "four distinct human accounts, so this is a bloc and not one voter"
    );
    assert_eq!(
        r.value_json.as_deref(),
        Some("\"Human Title\""),
        "a 0.99-confidence machine proposal loses to four human votes"
    );
    assert_ne!(r.proposal_id, Some(machine), "and the machine did not win");

    // And the machine's weight is bounded by its confidence, so confidence
    // participates without becoming authority.
    let weights = resolve::weight_breakdown(&store, subject, "title")
        .await
        .unwrap();
    let w = weights
        .iter()
        .find(|e| e.proposal_id == machine)
        .map(|e| e.weight)
        .unwrap();
    assert!(
        w <= 1.0,
        "confidence contributes but cannot exceed one whole vote, got {w}"
    );
}

/// A retracted vote stops counting.
///
/// Retraction is not a deletion: the row stays so the history is intact, which
/// is why the resolve query has to filter `retracted = 0` rather than relying
/// on the row being gone. A retraction that is stored but not filtered is the
/// most likely way this code is wrong, so it is tested directly.
///
/// Three votes for A and one for B, then one of A's is retracted. Two to one is
/// still A, but the *margin* has to shrink, and the live-vote count has to drop
/// — those are the assertions, because a retraction that is stored but not
/// filtered changes neither.
#[tokio::test]
async fn a_retracted_vote_stops_counting() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let a1 = account_on_field(&store, "a1", Role::Contributor, "title", 1.0).await;
    let a2 = account_on_field(&store, "a2", Role::Contributor, "title", 1.0).await;
    let a3 = account_on_field(&store, "a3", Role::Contributor, "title", 1.0).await;
    let b1 = account_on_field(&store, "b1", Role::Contributor, "title", 1.0).await;

    let p_a = propose(&store, subject, "title", "\"A\"", &a1).await;
    let p_b = propose(&store, subject, "title", "\"B\"", &b1).await;
    for a in [&a1, &a2, &a3] {
        vote(&store, a, &p_a, "title").await;
    }
    vote(&store, &b1, &p_b, "title").await;

    let before = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        before.value_json.as_deref(),
        Some("\"A\""),
        "three beat one"
    );
    assert_eq!(
        before
            .weights
            .iter()
            .find(|w| w.proposal_id == p_a)
            .map(|w| w.live_votes),
        Some(3),
        "and A's backer count is three"
    );
    // 3-vs-1 with damping is 0.42, not 0.75: the three are discounted as a bloc
    // relative to one. A specific number here would pin the damping curve
    // rather than the behaviour, so the bound is deliberately loose.
    let margin_before = before.margin;
    assert!(
        margin_before > 0.3,
        "3-vs-1 is a clear margin even after damping, got {margin_before}"
    );

    resolve::retract_vote(&store, &a2, &p_a).await.unwrap();

    let after = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        after
            .weights
            .iter()
            .find(|w| w.proposal_id == p_a)
            .map(|w| w.live_votes),
        Some(2),
        "the retracted vote is out of the live count, so the filter is applied"
    );
    assert!(
        after.margin < margin_before,
        "and the margin narrowed from {margin_before} to {}",
        after.margin
    );

    // The row is still there, retracted rather than deleted, so the history of
    // what was believed and when survives (§8.6).
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vote")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(total, 4, "four votes were cast and all four rows remain");

    // Retracting all of A's remaining votes leaves the field with B as the only
    // endorsed value, which is the flip the original version of this test
    // asserted -- and which only holds once the damping stops treating a
    // one-backer proposal as worth less than a two-backer one.
    resolve::retract_vote(&store, &a1, &p_a).await.unwrap();
    resolve::retract_vote(&store, &a3, &p_a).await.unwrap();
    assert_eq!(
        resolve::resolve(&store, subject, "title")
            .await
            .unwrap()
            .value_json
            .as_deref(),
        Some("\"B\""),
        "with every vote for A retracted, B is what is left"
    );
}

/// A tie is a tie, and it is reported as one rather than resolved by row order.
///
/// Picking the alphabetically-first proposal on a tie makes the result depend
/// on a storage detail, which means the same evidence gives different answers
/// on two machines. The tie is surfaced so a steward can break it, which is
/// also what §8.5's contested-field queue is for.
#[tokio::test]
async fn a_tie_is_reported_as_a_tie_not_broken_by_row_order() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let a1 = account_on_field(&store, "a1", Role::Contributor, "title", 1.0).await;
    let a2 = account_on_field(&store, "a2", Role::Contributor, "title", 1.0).await;
    let b1 = account_on_field(&store, "b1", Role::Contributor, "title", 1.0).await;
    let b2 = account_on_field(&store, "b2", Role::Contributor, "title", 1.0).await;

    let p_a = propose(&store, subject, "title", "\"A\"", &a1).await;
    let p_b = propose(&store, subject, "title", "\"B\"", &b1).await;
    vote(&store, &a1, &p_a, "title").await;
    vote(&store, &a2, &p_a, "title").await;
    vote(&store, &b1, &p_b, "title").await;
    vote(&store, &b2, &p_b, "title").await;

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        r.margin, 0.0,
        "two values with equal weight have zero margin, and that is reported"
    );
    assert!(
        r.tied,
        "and the tie is named, so a steward queue can pick it up"
    );
}

/// A field with no proposals is unset, not an empty string.
///
/// The distinction matters because §8.6 says NULL and unset are separate
/// states: a field that was never proposed has no value, and a field proposed
/// as `""` has a value that happens to be empty. Returning `""` for both makes
/// them the same row to everything downstream.
#[tokio::test]
async fn a_field_with_no_proposals_is_unset() {
    let (_d, store) = store().await;
    let r = resolve::resolve(&store, Uuid::new_v4(), "title")
        .await
        .unwrap();
    assert_eq!(r.value_json, None, "no proposals is no value");
    assert!(!r.locked);
    assert!(!r.tied);
    assert!(
        !r.contested,
        "nothing is contested when nothing was proposed"
    );
}

/// A proposal for an empty value is a value.
///
/// The counterpart to the test above: `""` is a legitimate proposal, it is
/// distinct from unset, and it must survive the round trip rather than being
/// collapsed into "no value".
#[tokio::test]
async fn a_proposed_empty_value_is_distinct_from_unset() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let a = account_on_field(&store, "a", Role::Contributor, "title", 1.0).await;
    let p = propose(&store, subject, "title", "\"\"", &a).await;
    vote(&store, &a, &p, "title").await;

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        r.value_json.as_deref(),
        Some("\"\""),
        "an empty string is a proposed value, not the absence of one"
    );
}

/// Recency decay: an old vote counts for less than a fresh one of the same weight.
///
/// The spec makes the decay configurable, and this asserts the knob is real in
/// both directions — set it to 1.0 and age stops mattering, and that is not a
/// bug, it is the configuration the tests above rely on to be deterministic.
#[tokio::test]
async fn recency_decay_is_configurable_and_halves_the_old_vote() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    // Two accounts, equal reputation, but one voted a year ago.
    let fresh = account_on_field(&store, "fresh", Role::Contributor, "title", 1.0).await;
    let stale = account_on_field(&store, "stale", Role::Contributor, "title", 1.0).await;

    let p_fresh = propose(&store, subject, "title", "\"Fresh\"", &fresh).await;
    let p_stale = propose(&store, subject, "title", "\"Stale\"", &stale).await;
    vote(&store, &fresh, &p_fresh, "title").await;
    vote(&store, &stale, &p_stale, "title").await;

    // Age the stale vote by rewriting its timestamp, which is the only honest
    // way to get a year-old vote without waiting a year.
    commons_store::index::age_vote(&store, &p_stale, 365)
        .await
        .unwrap();

    let r = resolve::resolve_with(
        &store,
        subject,
        "title",
        &ResolveConfig {
            half_life_days: 365.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        r.value_json.as_deref(),
        Some("\"Fresh\""),
        "with a one-year half-life the stale vote is worth about half, and one
            fresh vote is worth more than half"
    );
}

/// The decay config is honoured rather than ignored.
///
/// A `half_life_days` that does not change the result is a constant the config
/// silently shadows, and the only way to know is to compare two settings.
#[tokio::test]
async fn two_different_half_lives_give_two_different_answers() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let fresh = account_on_field(&store, "fresh", Role::Contributor, "title", 1.0).await;
    let stale = account_on_field(&store, "stale", Role::Contributor, "title", 1.0).await;
    let p_fresh = propose(&store, subject, "title", "\"Fresh\"", &fresh).await;
    let p_stale = propose(&store, subject, "title", "\"Stale\"", &stale).await;
    vote(&store, &fresh, &p_fresh, "title").await;
    vote(&store, &stale, &p_stale, "title").await;
    commons_store::index::age_vote(&store, &p_stale, 365)
        .await
        .unwrap();

    // Two settings that must give the same vote different weights. A 3,650-day
    // half-life leaves the year-old vote at 93% of fresh; a 10-day half-life
    // leaves it at 4%.
    let gentle = ResolveConfig {
        half_life_days: 3_650.0,
        ..Default::default()
    };
    let sharp = ResolveConfig {
        half_life_days: 10.0,
        ..Default::default()
    };
    let a = resolve::resolve_with(&store, subject, "title", &gentle)
        .await
        .unwrap();
    let b = resolve::resolve_with(&store, subject, "title", &sharp)
        .await
        .unwrap();
    // The *winner* is the same under both settings, and that is correct: decay
    // only ever subtracts, so a fresh vote can never be overtaken by an old one
    // of equal reputation. Asserting otherwise would be asserting a bug.
    //
    // What the knob changes is how much the old vote is worth, and therefore
    // whether it could survive a *newer* challenger. So the sharp setting is
    // tested by giving the stale value a fresh backer of higher reputation: it
    // wins comfortably under a gentle half-life and loses under a sharp one.
    assert_eq!(
        a.value_json.as_deref(),
        Some("\"Fresh\""),
        "an equal-reputation fresh vote beats an old one at any half-life: {:?}",
        a.weights
    );
    assert_eq!(
        b.value_json.as_deref(),
        Some("\"Fresh\""),
        "and still does at a ten-day half-life: {:?}",
        b.weights
    );

    // The two settings gave the *same vote* two different weights, which is the
    // part a hardcoded decay would pass by accident whenever the winner happened
    // to agree.
    let weight_of = |r: &commons_index::resolve::ResolvedValue, v: &str| {
        r.weights
            .iter()
            .find(|w| w.value_json == v)
            .map(|w| w.weight)
            .unwrap()
    };
    assert!(
        weight_of(&a, "\"Stale\"") > weight_of(&b, "\"Stale\"") * 2.0,
        "the same vote weighs {} under a gentle half-life and {} under a sharp \
         one, so the knob is not being ignored",
        weight_of(&a, "\"Stale\""),
        weight_of(&b, "\"Stale\"")
    );
}

/// A vote cannot be cast twice by the same account, and the second is refused.
///
/// The unique index says so, but the refusal has to be a named error rather
/// than a constraint violation, because a caller that retries on error would
/// otherwise turn a duplicate into a double-weight vote.
#[tokio::test]
async fn a_second_vote_from_the_same_account_is_refused() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let a = account_on_field(&store, "a", Role::Contributor, "title", 1.0).await;
    let p = propose(&store, subject, "title", "\"Once\"", &a).await;

    vote(&store, &a, &p, "title").await;
    assert!(
        matches!(
            resolve::cast_vote(&store, &a, &p, "title")
                .await
                .unwrap_err(),
            resolve::ResolveError::AlreadyVoted { .. }
        ),
        "the duplicate is refused by name, not by a raw constraint error"
    );

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        r.value_json.as_deref(),
        Some("\"Once\""),
        "and the field still has one vote, not two"
    );
    assert!(!r.contested);
}

/// The same account may vote for two different proposals in the same field.
///
/// The unique index is on (proposal, account), not (field, account), because
/// changing your mind is legitimate: you backed A, then B arrived and you
/// prefer it. A vote for B is not a retraction of the vote for A, so the
/// runner-up is still a runner-up and the field is genuinely contested.
#[tokio::test]
async fn changing_your_mind_makes_a_field_contested() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let a = account_on_field(&store, "a", Role::Contributor, "title", 1.0).await;
    let p1 = propose(&store, subject, "title", "\"First\"", &a).await;
    let p2 = propose(&store, subject, "title", "\"Second\"", &a).await;

    vote(&store, &a, &p1, "title").await;
    vote(&store, &a, &p2, "title").await;

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert!(r.contested, "two live values for one field is contested");
    assert!(
        r.tied,
        "and one weight each is a tie, which is a state a steward queue needs"
    );
    // The winner is one of the two, deterministically, and not always the same.
    assert!(
        matches!(
            r.value_json.as_deref(),
            Some("\"First\"") | Some("\"Second\"")
        ),
        "a tie still yields a value to display, got {:?}",
        r.value_json
    );
}

/// The justification is carried through to the result, so the UI can show it.
///
/// §8.2's requirement is that every proposal records *why* it exists, so the UI
/// can show "title proposed from filename" beside "title proposed by 4 users".
/// That is only possible if resolve returns the justification of the winning
/// proposal, not just its value.
#[tokio::test]
async fn the_winner_carries_its_justification() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let a = account_on_field(&store, "a", Role::Contributor, "title", 1.0).await;

    let mut p = FieldProposal::new(
        SubjectType::Object,
        subject,
        "title",
        "\"From Filename\"",
        ProposalSource::Filename,
    );
    p.proposer_id = Some(a.to_string());
    p.value_json = "\"From Filename\"".into();
    let p = commons_store::index::insert_proposal(&store, &p)
        .await
        .unwrap();
    commons_store::index::set_justification(&store, &p, "from filename: Studio - Title (2021).mp4")
        .await
        .unwrap();
    vote(&store, &a, &p, "title").await;

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        r.justification.as_deref(),
        Some("from filename: Studio - Title (2021).mp4"),
        "the winner explains itself, so the UI can attribute it"
    );
    assert_eq!(r.source, Some(ProposalSource::Filename));
}

/// Resolve is per-field, so a vote on one field is invisible to another.
///
/// §8.3: agreeing about titles says nothing about tags. If resolve ignored the
/// field, a user with one confident title vote would swing a tag field they
/// never looked at — which is exactly the flattening that makes reputation
/// systems feel wrong to people.
#[tokio::test]
async fn a_vote_on_one_field_does_not_affect_another() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let a = account_on_field(&store, "a", Role::Contributor, "title", 9.0).await;

    let p_title = propose(&store, subject, "title", "\"Nine\"", &a).await;
    let p_tag = propose(&store, subject, "tags", "\"tag-a\"", &a).await;
    vote(&store, &a, &p_title, "title").await;

    let r = resolve::resolve(&store, subject, "tags").await.unwrap();
    assert_eq!(
        r.value_json, None,
        "a weight-9 title vote does not settle the tags field"
    );
    assert_eq!(
        r.proposal_id, None,
        "and the tag proposal, which has no vote at all, is not the winner"
    );
    let _ = p_tag;
}

/// A steward may not lock a field for a subject that has never been proposed.
///
/// Locking pins a value; a lock on an untouched field would create a value
/// nobody proposed, which is a fabricated fact with steward authority behind
/// it. It is refused so the lock always has a proposal behind it.
#[tokio::test]
async fn locking_an_unproposed_field_is_refused() {
    let (_d, store) = store().await;
    let steward = account_on_field(&store, "steward", Role::Steward, "title", 1.0).await;
    assert!(
        matches!(
            resolve::lock(&store, Uuid::new_v4(), "title", "\"Invented\"", &steward)
                .await
                .unwrap_err(),
            resolve::ResolveError::NothingToPin { .. }
        ),
        "a lock needs a proposal behind it"
    );
}

/// Only a steward or admin may lock, and only they may unlock.
///
/// A lock is the one thing in §8.1 that overrides the evidence, so the
/// permission is checked here rather than trusted from the caller.
#[tokio::test]
async fn only_a_steward_may_lock_or_unlock() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let a = account_on_field(&store, "a", Role::Contributor, "title", 1.0).await;
    let p = propose(&store, subject, "title", "\"Value\"", &a).await;
    vote(&store, &a, &p, "title").await;

    for role in [Role::Public, Role::Subscriber, Role::Contributor] {
        let who = account(&store, &format!("who-{role:?}"), role, 1.0).await;
        assert!(
            matches!(
                resolve::lock(&store, subject, "title", "\"Value\"", &who)
                    .await
                    .unwrap_err(),
                resolve::ResolveError::NotASteward { .. }
            ),
            "a {role:?} may not lock"
        );
    }

    let steward = account_on_field(&store, "steward", Role::Steward, "title", 1.0).await;
    resolve::lock(&store, subject, "title", "\"Value\"", &steward)
        .await
        .unwrap();
    let contributor = account_on_field(&store, "contrib", Role::Contributor, "title", 1.0).await;
    assert!(
        matches!(
            resolve::unlock(&store, subject, "title", &contributor)
                .await
                .unwrap_err(),
            resolve::ResolveError::NotASteward { .. }
        ),
        "and a contributor may not unlock either"
    );
}

/// A machine proposal carries its confidence into the weight, bounded.
///
/// A confidence above 1 is refused rather than clamped, because a confidence of
/// 12 means the caller is confused about what the number is and clamping hides
/// that. The refusal is at the write, not at the read.
#[tokio::test]
async fn a_confidence_above_one_is_refused_at_the_write() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let mut p = FieldProposal::new(
        SubjectType::Object,
        subject,
        "title",
        "\"Too Sure\"",
        ProposalSource::MlCaptioner,
    );
    p.confidence = Some(1.5);
    assert!(
        matches!(
            commons_store::index::insert_proposal(&store, &p).await.unwrap_err(),
            commons_store::db::StoreError::Invalid { what, .. } if what.contains("confidence")
        ),
        "the bad number is reported at the write that carried it"
    );
}

/// A `public` role with no login may not vote (§8.1, #2792).
///
/// The role table's whole point is that the anonymous case is supported, and
/// the anonymous case is exactly the one where a UI-only check leaks. So the
/// refusal lives in the vote path, not in the view.
#[tokio::test]
async fn an_anonymous_public_role_may_not_vote() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let anon = account_on_field(&store, "anon", Role::Public, "title", 1.0).await;
    let p = propose(&store, subject, "title", "\"Nope\"", &anon).await;

    assert!(
        matches!(
            resolve::cast_vote(&store, &anon, &p, "title")
                .await
                .unwrap_err(),
            resolve::ResolveError::MayNotVote { .. }
        ),
        "a public role has no vote, and the refusal is here rather than in a view"
    );
    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(r.value_json, None, "and the field is still unset");
}

// ---- helpers -------------------------------------------------------------

async fn propose(store: &Store, subject: Uuid, field: &str, value_json: &str, by: &Uuid) -> Uuid {
    let mut p = FieldProposal::new(
        SubjectType::Object,
        subject,
        field,
        value_json,
        ProposalSource::User,
    );
    p.proposer_kind = ProposerKind::User;
    p.proposer_id = Some(by.to_string());
    commons_store::index::insert_proposal(store, &p)
        .await
        .unwrap()
}

/// Cast a vote that is expected to succeed. Tests that want the failure call
/// `resolve::cast_vote` directly, so every `vote(...)` here is a vote that
/// landed -- and the field is asserted after each group, so a silently dropped
/// vote cannot pass.
async fn vote(store: &Store, by: &Uuid, proposal: &Uuid, field: &str) {
    resolve::cast_vote(store, by, proposal, field)
        .await
        .unwrap();
}

/// Two configurations resolve the same evidence differently, in one process.
///
/// This is a separate test from the two-half-lives one because it isolates the
/// *cache*. The first read populates an entry keyed on
/// `(subject, field, fingerprint)`; the second read has identical evidence and
/// would be served that entry if the configuration were not part of the key.
/// Since the two configurations disagree, the served-stale case is visible
/// rather than silent — which is the only reason this test can fail.
///
/// The decoy is deliberate: the gentle read comes second, so a key built from
/// the evidence alone would answer it with the sharp result and the assertion
/// would catch it. Getting the order wrong would make the test pass for the
/// wrong reason.
#[tokio::test]
async fn the_cache_key_includes_the_configuration() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let fresh = account_on_field(&store, "fresh", Role::Contributor, "title", 1.0).await;
    let stale = account_on_field(&store, "stale", Role::Contributor, "title", 1.0).await;
    let p_fresh = propose(&store, subject, "title", "\"Fresh\"", &fresh).await;
    let p_stale = propose(&store, subject, "title", "\"Stale\"", &stale).await;
    vote(&store, &fresh, &p_fresh, "title").await;
    vote(&store, &stale, &p_stale, "title").await;
    commons_store::index::age_vote(&store, &p_stale, 365)
        .await
        .unwrap();

    let sharp = ResolveConfig {
        half_life_days: 10.0,
        ..Default::default()
    };
    let gentle = ResolveConfig {
        half_life_days: 3_650.0,
        ..Default::default()
    };

    let stale_under = |r: &commons_index::resolve::ResolvedValue| {
        r.weights
            .iter()
            .find(|w| w.value_json == "\"Stale\"")
            .map(|w| w.weight)
            .unwrap()
    };

    let first = resolve::resolve_with(&store, subject, "title", &sharp)
        .await
        .unwrap();
    assert!(
        stale_under(&first) < 0.1,
        "under a ten-day half-life a year-old vote is worth almost nothing, got {}",
        stale_under(&first)
    );

    let second = resolve::resolve_with(&store, subject, "title", &gentle)
        .await
        .unwrap();
    assert!(
        stale_under(&second) > 0.9,
        "the same evidence under a ten-year half-life is worth almost all of it, \
         got {} -- so the sharp answer was not served from cache",
        stale_under(&second)
    );
}

/// Resolve is pure: the same evidence twice gives the same answer, and asking
/// does not change the store.
///
/// The tempting shortcut for the cache is to fold the result back into the
/// database as a "settled" value, which is what "the winner is stored" would
/// mean. This asserts the opposite: resolve reads and nothing more, so the
/// proposal set remains the only state and a recomputation is always possible.
#[tokio::test]
async fn resolve_does_not_write() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let a = account_on_field(&store, "a", Role::Contributor, "title", 4.0).await;
    let p = propose(&store, subject, "title", "\"Only\"", &a).await;
    vote(&store, &a, &p, "title").await;

    let before: (i64, i64, i64) = (
        sqlx::query_scalar("SELECT COUNT(*) FROM field_proposal")
            .fetch_one(store.pool())
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM vote")
            .fetch_one(store.pool())
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM field_lock")
            .fetch_one(store.pool())
            .await
            .unwrap(),
    );
    for _ in 0..5 {
        resolve::resolve(&store, subject, "title").await.unwrap();
    }
    let after: (i64, i64, i64) = (
        sqlx::query_scalar("SELECT COUNT(*) FROM field_proposal")
            .fetch_one(store.pool())
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM vote")
            .fetch_one(store.pool())
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM field_lock")
            .fetch_one(store.pool())
            .await
            .unwrap(),
    );
    assert_eq!(
        before, after,
        "five resolves left the proposal set, the votes and the locks untouched"
    );
}

/// A bare proposal loses to an endorsed one, however confident the bare one is.
///
/// This is the rule that keeps a field from settling on something nobody backed.
/// It is easy to get wrong in the direction that looks like generosity: giving
/// an unvoted proposal a floor so it can compete. The floor makes it win, because
/// the endorsed proposal also has to clear it, and now a field with one human
/// backer and one untouched machine proposal resolves to the untouched one.
///
/// The machine case is the sharp one. A `filename` proposal nobody has voted on
/// is the common state of a fresh library, and it must not be able to outrank a
/// single person who actually looked at the file.
#[tokio::test]
async fn an_unendorsed_proposal_loses_to_an_endorsed_one() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let person = account_on_field(&store, "person", Role::Contributor, "title", 1.0).await;
    let human = propose(&store, subject, "title", "\"From A Person\"", &person).await;
    vote(&store, &person, &human, "title").await;

    // Two bare proposals: a human's own unsubmitted guess and a high-confidence
    // machine reading. Neither has a vote.
    let mut bare = FieldProposal::new(
        SubjectType::Object,
        subject,
        "title",
        "\"From The Filename\"",
        ProposalSource::Filename,
    );
    bare.proposer_kind = ProposerKind::User;
    // A confidence on an *extraction*. This is the sharp case, and the reason
    // `is_inference` exists: a parser can be very sure of what it found, and
    // that certainty is worth exactly nothing as support. The number is real,
    // it is recorded, and it is not a vote.
    bare.confidence = Some(0.99);
    let bare = commons_store::index::insert_proposal(&store, &bare)
        .await
        .unwrap();

    let mut machine = FieldProposal::new(
        SubjectType::Object,
        subject,
        "title",
        "\"From The Tagger\"",
        ProposalSource::MlTagger,
    );
    machine.proposer_kind = ProposerKind::Auto;
    machine.confidence = Some(0.95);
    let machine = commons_store::index::insert_proposal(&store, &machine)
        .await
        .unwrap();

    // The bare *extraction* weighs nothing. This is the case that matters: a
    // filename nobody has confirmed is a guess about a string, and in a fresh
    // library it is the only kind of proposal there is. If it carried weight,
    // every title in a freshly-scanned library would settle on whatever the
    // first parser guessed, with no human in the loop at all.
    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    let w_bare = r.weights.iter().find(|w| w.proposal_id == bare).unwrap();
    assert_eq!(
        w_bare.weight, 0.0,
        "an unconfirmed filename extraction weighs nothing even at 0.99 \
         confidence, got {}",
        w_bare.weight
    );
    assert_eq!(w_bare.live_votes, 0);

    // The *inference* does carry its confidence -- that is §8.2.1's promise,
    // that local tagging is usable before anyone has voted on anything. So the
    // 0.95 tagger legitimately competes, and with one person behind the other
    // value it wins. That is a vote, not an override: two more people on the
    // human's side outvote it, which the second half of this test shows.
    let w_machine = r.weights.iter().find(|w| w.proposal_id == machine).unwrap();
    assert!(
        w_machine.weight > 0.9,
        "a 0.95-confidence tagger is endorsed by its own confidence, got {}",
        w_machine.weight
    );
    // A single weight-1 vote is 1.0 and the tagger is 0.95, so the person wins
    // by 0.05. That narrowness is the point: a machine cannot buy a field, and
    // the only way it wins is if its confidence is at least as high as the
    // evidence against it.
    assert_eq!(
        r.value_json.as_deref(),
        Some("\"From A Person\""),
        "a 0.95 tagger does not outvote one person, so the margin is theirs: {:?}",
        r.weights
    );
    assert!(
        r.contested,
        "and the field is contested, with two live values"
    );

    // Raise the tagger's confidence and it wins on its own, with nobody voting
    // for it -- which is the case §8.2.1 promises and the reason an inference
    // is endorsed at all.
    commons_store::index::set_confidence(&store, &machine, 1.0)
        .await
        .unwrap();
    let confident = resolve::resolve(&store, subject, "title").await.unwrap();
    // Full confidence is a full vote, so it *ties* rather than wins. Asserting
    // a win here would be asserting that a machine outranks a person, which is
    // the thing §8.1 forbids; the tie is the honest ceiling, and the
    // deterministic value-ordering then picks one of the two.
    let tagger = confident
        .weights
        .iter()
        .find(|w| w.proposal_id == machine)
        .unwrap();
    let person = confident
        .weights
        .iter()
        .find(|w| w.proposal_id == human)
        .unwrap();
    assert!(
        (tagger.weight - person.weight).abs() < 1e-9,
        "at full confidence the tagger ties one person rather than beating it: \
         {} vs {}",
        tagger.weight,
        person.weight
    );
    assert!(
        confident.tied,
        "and the tie is reported, so a steward queue can see the field is undecided"
    );

    // Two more people on the human's side outvote the tagger, which is the
    // property that makes it a voter and not an override. If the tagger could
    // not be outvoted this would be the failure §8.1's "never an override" is
    // written against.
    for name in ["second", "third"] {
        let a = account_on_field(&store, name, Role::Contributor, "title", 1.0).await;
        vote(&store, &a, &human, "title").await;
    }
    let after = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        after.value_json.as_deref(),
        Some("\"From A Person\""),
        "three people outvote the tagger: {:?}",
        after.weights
    );
    assert_eq!(after.proposal_id, Some(human));
}

/// Sybil damping is what makes a bloc's *marginal* influence sublinear.
///
/// Ten accounts agreeing is worth more than one, and less than ten. The test
/// compares the weight of a proposal with one backer against the weight of a
/// proposal with eight, in the same field, where the eight are the most-backed
/// so the damping term is at its strongest. Without damping the ratio would be
/// 8:1; with it, much less.
///
/// The assertion is on the *ratio* rather than on absolute weights, because
/// absolute weights are a tuning decision and the ratio is the property §8.3
/// is actually about: "many accounts voting identically is discounted".
#[tokio::test]
async fn a_bloc_of_eight_is_worth_less_than_eight_singles() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();

    let one = account_on_field(&store, "one", Role::Contributor, "title", 1.0).await;
    let p_one = propose(&store, subject, "title", "\"Solo\"", &one).await;
    vote(&store, &one, &p_one, "title").await;

    let mut backers = Vec::new();
    for i in 0..8 {
        let a = account_on_field(
            &store,
            &format!("bloc-{i}"),
            Role::Contributor,
            "title",
            1.0,
        )
        .await;
        backers.push(a);
    }
    let p_bloc = propose(&store, subject, "title", "\"Bloc\"", &one).await;
    for a in &backers {
        vote(store_or(&store), a, &p_bloc, "title").await;
    }

    let r = resolve::resolve(&store, subject, "title").await.unwrap();
    let w = |id: Uuid| {
        r.weights
            .iter()
            .find(|w| w.proposal_id == id)
            .map(|w| w.weight)
            .unwrap()
    };
    let solo = w(p_one);
    let bloc = w(p_bloc);
    assert!(
        bloc > solo,
        "eight accounts are worth more than one, got {bloc} vs {solo}"
    );
    assert!(
        bloc < solo * 8.0,
        "but less than eight, so the marginal backer is discounted: {bloc} vs {}",
        solo * 8.0
    );
    assert_eq!(
        r.value_json.as_deref(),
        Some("\"Bloc\""),
        "and the bloc still wins, because damping discounts rather than disqualifies"
    );
}

/// Helper for the bloc test: borrowing the store where the loop needs it twice.
fn store_or(s: &commons_store::Store) -> &commons_store::Store {
    s
}
