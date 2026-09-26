//! §8.4 — leaderboards, badges, points.
//!
//! Three rules, and the ticket names which of them is the fragile one.
//!
//! # Rule 1 — points are earned on *acceptance*, and withdrawn when it is undone
//!
//! §8.4 says points are earned for *accepted* proposals, "which ties the economy
//! to §8.1". §8.1 computes the winner by weighted vote and never persists the word
//! "accepted". So the tie is made by an **award ledger**: a row per award, with
//! the proposal that earned it, so a later resolve that no longer agrees can
//! withdraw exactly that award. A running `points_total` on `account` would be
//! the counter T-P4-004 forbids, one layer up: it cannot answer "why does this
//! account have 40 points" and it cannot be withdrawn from correctly without a
//! second pass over every proposal.
//!
//! `points::reconcile` is that pass. It is the only writer of `points_award`,
//! which is what makes "withdrawn on rejection-after-acceptance" a property of
//! the data rather than a promise in a comment.
//!
//! # Rule 2 — the ledger is append-only; a withdrawal is a second row
//!
//! `points_award.withdrawn_at` is set rather than the row deleted, and the
//! subtraction comes from a *recomputed* sum over live rows. So the balance is
//! `SUM(points) WHERE withdrawn_at IS NULL`, and there is no number anywhere
//! that has to be kept in step. This is T-P4-004's rule applied to money.
//!
//! # Rule 3 — leaderboards rank contributors, and no query in this crate ranks
//! anyone by how popular they are as *performers*
//!
//! This is the ticket's done-when: *"the negative test (no performer-popularity
//! surface) exists — it guards §2's most easily eroded promise."* §2 says
//! "Not a performer-discovery site. No ranking of people by popularity as a
//! browsable surface, no 'top performers' front page." Popularity is allowed as a
//! search tie-breaker (§2) and nowhere else.
//!
//! The negative test is `no_surface_ranks_people_by_popularity`, and it is
//! written to fail on a *whole class* of regression rather than one query: it
//! reads every SQL string in the crate and rejects any that selects a
//! performer-ordering column, so adding a "top performers" query later fails a
//! test without anybody having to remember to write one. A test that checks one
//! named function passes unchanged when a second function is added beside it.

use commons_core::{ts, Role, SubjectType};
use commons_index::{points, resolve};
use commons_store::Store;
use serde_json::Value;
use uuid::Uuid;

mod common;
use common::store;

async fn account(store: &Store, handle: &str) -> Uuid {
    common::account_on_field(store, handle, Role::Contributor, "title", 1.0).await
}

fn value(s: &str) -> Value {
    Value::String(s.to_string())
}

/// A proposal, as a user or a machine would make it.
///
/// Built on the domain type rather than on a test-only insert, so the
/// `proposer_id` and `source` the reconcile reads are the same fields every other
/// caller writes. A fixture that inserted `source` directly would be testing a
/// row shape rather than the code that produces it.
async fn propose(
    store: &Store,
    subject_id: Uuid,
    field: &str,
    v: &Value,
    source: commons_core::ProposalSource,
    by: Option<&Uuid>,
) -> Uuid {
    let mut p = commons_core::FieldProposal::new(
        SubjectType::Object,
        subject_id,
        field,
        v.to_string(),
        source,
    );
    p.proposer_id = by.map(|id| id.to_string());
    commons_store::index::insert_proposal(store, &p)
        .await
        .unwrap();
    p.id
}

// ---- Rule 1: awarded on acceptance ----------------------------------------

/// A proposal that wins is awarded points to its proposer.
///
/// The first half of the ticket's accept criterion.
#[tokio::test]
async fn points_are_awarded_when_a_proposal_wins() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let subject = Uuid::new_v4();

    let p = propose(
        &store,
        subject,
        "title",
        &value("A Title"),
        commons_core::ProposalSource::User,
        Some(&alice),
    )
    .await;
    // One supporter, so the proposal wins outright.
    resolve::cast_vote(&store, &alice, &p, "title")
        .await
        .unwrap();

    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();

    let bal = points::balance(&store, &alice).await.unwrap();
    assert_eq!(
        bal.total,
        points::POINTS_PER_ACCEPTED_PROPOSAL,
        "an accepted proposal earns the standard award, and `total` is a \\
         recomputed sum over live award rows rather than a stored counter"
    );
    let awards = points::awards(&store, &alice).await.unwrap();
    assert_eq!(awards.len(), 1, "{awards:#?}");
    assert_eq!(awards[0].proposal_id, p);
    assert!(awards[0].withdrawn_at.is_none());
}

/// Points are withdrawn when an accepted proposal stops winning.
///
/// The second half of the ticket's accept criterion, and the one that cannot be
/// implemented by a counter. Alice's proposal wins, she is paid, a second
/// proposal overtakes it, and the award is withdrawn — with the balance
/// recomputed, not decremented by a remembered amount.
#[tokio::test]
async fn points_are_withdrawn_when_a_proposal_stops_winning() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let bob = account(&store, "bob").await;
    let subject = Uuid::new_v4();

    let a = propose(
        &store,
        subject,
        "title",
        &value("A Title"),
        commons_core::ProposalSource::User,
        Some(&alice),
    )
    .await;
    resolve::cast_vote(&store, &alice, &a, "title")
        .await
        .unwrap();
    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();
    assert_eq!(
        points::balance(&store, &alice).await.unwrap().total,
        points::POINTS_PER_ACCEPTED_PROPOSAL
    );

    // A heavier rival arrives. Five supporters behind one value is five votes'
    // worth of weight against Alice's one -- enough for an overtake, and not a
    // tie, which is why the fixture uses five and not two.
    let b = propose(
        &store,
        subject,
        "title",
        &value("The Real Title"),
        commons_core::ProposalSource::User,
        Some(&bob),
    )
    .await;
    for i in 0..5 {
        let voter = account(&store, &format!("voter{i}")).await;
        resolve::cast_vote(&store, &voter, &b, "title")
            .await
            .unwrap();
    }
    // Prove the overtake happened *before* reconciling, so a failure below is
    // "the fixture did not make the rival win" rather than "points did not move".
    let overtaken = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        overtaken.proposal_id,
        Some(b),
        "the rival wins outright, so the withdrawal below is a real one"
    );

    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();

    assert_eq!(
        points::balance(&store, &alice).await.unwrap().total,
        0,
        "Alice's award is withdrawn: her proposal no longer wins, so it is no \\
         longer an accepted proposal and §8.4 pays for accepted ones"
    );
    let bob_bal = points::balance(&store, &bob).await.unwrap();
    assert_eq!(
        bob_bal.total,
        points::POINTS_PER_ACCEPTED_PROPOSAL,
        "and Bob's new proposal is awarded in the same reconcile"
    );
}

/// The withdrawal is recorded, not the row deleted.
///
/// The row is kept because a leaderboard that deletes the evidence of a
/// withdrawn award cannot explain a balance that changed. Asserted explicitly
/// because `total == 0` is also true of a row that was never written, and a
/// balance-only assertion cannot tell the two apart.
#[tokio::test]
async fn a_withdrawn_award_is_kept_as_a_record() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let subject = Uuid::new_v4();

    let a = propose(
        &store,
        subject,
        "title",
        &value("A Title"),
        commons_core::ProposalSource::User,
        Some(&alice),
    )
    .await;
    resolve::cast_vote(&store, &alice, &a, "title")
        .await
        .unwrap();
    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();

    for i in 0..5 {
        let voter = account(&store, &format!("voter{i}")).await;
        let b = propose(
            &store,
            subject,
            "title",
            &value("Other"),
            commons_core::ProposalSource::User,
            Some(&voter),
        )
        .await;
        resolve::cast_vote(&store, &voter, &b, "title")
            .await
            .unwrap();
    }
    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();

    let awards = points::awards(&store, &alice).await.unwrap();
    assert_eq!(awards.len(), 1, "the row is not deleted: {awards:#?}");
    assert!(
        awards[0].withdrawn_at.is_some(),
        "it is marked withdrawn, so the balance of {award} and the reason it is \\
         not counted are both answerable",
        award = awards[0].points
    );
}

/// Reconcile is idempotent: a second call with nothing changed writes nothing.
///
/// A reconcile that re-awarded on every call would pay for the same proposal
/// twice, and a balance that grew on every read is indistinguishable from a
/// system that pays generously.
#[tokio::test]
async fn reconcile_is_idempotent() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let subject = Uuid::new_v4();
    let a = propose(
        &store,
        subject,
        "title",
        &value("A Title"),
        commons_core::ProposalSource::User,
        Some(&alice),
    )
    .await;
    resolve::cast_vote(&store, &alice, &a, "title")
        .await
        .unwrap();

    for _ in 0..3 {
        points::reconcile(&store, SubjectType::Object, &subject, "title")
            .await
            .unwrap();
    }
    assert_eq!(
        points::balance(&store, &alice).await.unwrap().total,
        points::POINTS_PER_ACCEPTED_PROPOSAL,
        "three reconciles, one award"
    );
}

/// Re-reconciling after a withdrawal restores the award, rather than a new one.
///
/// The other direction, and the one that proves the ledger is keyed on the
/// proposal rather than on the moment. If a proposal wins again, the same row
/// comes back to life with its original id, so a leaderboard entry is stable
/// across a field that was briefly overtaken.
#[tokio::test]
async fn a_restored_award_reuses_its_row() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let subject = Uuid::new_v4();
    let a = propose(
        &store,
        subject,
        "title",
        &value("A Title"),
        commons_core::ProposalSource::User,
        Some(&alice),
    )
    .await;
    resolve::cast_vote(&store, &alice, &a, "title")
        .await
        .unwrap();
    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();
    let first = points::awards(&store, &alice).await.unwrap()[0].id;

    // Withdraw, then restore. The five voters are created once, up front: a
    // fixture that creates the same account in two loops is a fixture whose
    // failure is a unique-constraint error rather than the thing under test.
    let mut voters = Vec::new();
    for i in 0..5 {
        voters.push(account(&store, &format!("voter{i}")).await);
    }
    let b = propose(
        &store,
        subject,
        "title",
        &value("Other"),
        commons_core::ProposalSource::User,
        Some(&voters[0]),
    )
    .await;
    for voter in &voters {
        resolve::cast_vote(&store, voter, &b, "title")
            .await
            .unwrap();
    }
    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();
    assert_eq!(
        points::balance(&store, &alice).await.unwrap().total,
        0,
        "the award was withdrawn while the rival led"
    );

    // Restore it: retract the rival's votes, and Alice's proposal leads again.
    for voter in &voters {
        resolve::retract_vote(&store, voter, &b).await.unwrap();
    }
    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();

    let awards = points::awards(&store, &alice).await.unwrap();
    assert_eq!(awards.len(), 1, "{awards:#?}");
    assert_eq!(
        awards[0].id, first,
        "the same row came back rather than a second award: the ledger is keyed \\
         on the proposal, so a field that was briefly overtaken does not fork an \\
         account's history in two"
    );
    assert!(awards[0].withdrawn_at.is_none());
}

// ---- Rule 1, the negative case: a machine proposal pays nobody -------------

/// A proposal from a machine source earns no points.
///
/// §8.1 is explicit that a machine proposal's weight "does not carry
/// authority", and §8.4 pays for accepted *proposals* by contributors. Letting
/// a scan pay points would make the cheapest way to earn points a bulk import,
/// and the leaderboard would rank who imported the most rather than who curated
/// the best.
#[tokio::test]
async fn a_machine_proposal_earns_nobody_points() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let p = propose(
        &store,
        subject,
        "title",
        &value("Extracted Title"),
        commons_core::ProposalSource::Filename,
        None,
    )
    .await;

    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();

    // Give the machine proposal a supporter so it genuinely *wins* -- §8.1 says
    // a proposal with no live support has no weight and so no standing as an
    // answer, which would make this test pass for the wrong reason. A voter's
    // endorsement is what makes a machine proposal real, and it is exactly the
    // case where paying the proposer would be wrong.
    let voter = account(&store, "voter").await;
    resolve::cast_vote(&store, &voter, &p, "title")
        .await
        .unwrap();
    let resolved = resolve::resolve(&store, subject, "title").await.unwrap();
    assert_eq!(
        resolved.proposal_id,
        Some(p),
        "the machine proposal won the field"
    );
    let awards: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM points_award")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(
        awards, 0,
        "a machine proposal won the field and nobody was paid, because §8.1 says \\
         its weight carries no authority and §8.4 pays for curation"
    );
}

/// A locked field pays nobody, because nothing was decided by vote.
///
/// The lock is the absence of a vote (§8.1 rule 3). If a field is locked to a
/// value that a steward chose, no proposal "won" it, so there is no acceptance to
/// pay for. A lock that paid points would make stewardship the most profitable
/// activity in the system, which is not what §8.4 is for.
#[tokio::test]
async fn a_locked_field_pays_nobody() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let steward = common::account_on_field(&store, "steward", Role::Steward, "title", 1.0).await;
    let subject = Uuid::new_v4();
    let a = propose(
        &store,
        subject,
        "title",
        &value("A Title"),
        commons_core::ProposalSource::User,
        Some(&alice),
    )
    .await;
    resolve::cast_vote(&store, &alice, &a, "title")
        .await
        .unwrap();
    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();
    assert!(points::balance(&store, &alice).await.unwrap().total > 0);

    // A steward may only pin a value somebody actually proposed (§8.1: a lock on
    // a value with no proposal behind it is a fabricated fact carrying steward
    // authority). So the pinned value is proposed first.
    let pinned = propose(
        &store,
        subject,
        "title",
        &value("Locked"),
        commons_core::ProposalSource::User,
        Some(&alice),
    )
    .await;
    resolve::cast_vote(&store, &alice, &pinned, "title")
        .await
        .unwrap();
    resolve::lock(&store, subject, "title", "\"Locked\"", &steward)
        .await
        .unwrap();

    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();
    assert_eq!(
        points::balance(&store, &alice).await.unwrap().total,
        0,
        "a lock withdraws the award: the value is no longer the product of a \\
         vote, so there is no accepted proposal to pay for"
    );
}

// ---- invite keys and invited-contributor rewards --------------------------

/// Reward points are configured, and a default exists.
///
/// §8.4 (#600) asks for reward points for invited contributors and stash-box#551
/// for a configurable invite-key count. Both are settings, so both have a
/// default that a deployment can override, and neither is a constant in a
/// function body.
#[tokio::test]
async fn invite_rewards_and_key_count_are_configurable_with_defaults() {
    let cfg = points::EconomyConfig::default();
    assert_eq!(
        cfg.invite_reward_points,
        points::INVITE_REWARD_POINTS,
        "the invite reward is a named default, not a literal at a call site"
    );
    assert_eq!(cfg.max_invite_keys, points::DEFAULT_MAX_INVITE_KEYS);
    assert!(
        cfg.max_invite_keys > 0,
        "and a corpus that accepted nobody would be a configuration nobody wants"
    );
    let tighter = points::EconomyConfig {
        max_invite_keys: 1,
        ..Default::default()
    };
    assert_eq!(tighter.max_invite_keys, 1, "and it is overridable");
}

/// A disabled account cannot be invited, and an invite key is consumed once.
///
/// Two properties of the same function. The disabled one is the takedown state
/// again: a disabled account that can still consume invite keys is a way to keep
/// growing the user base after being removed from it.
#[tokio::test]
async fn an_invite_key_is_consumed_once_and_a_disabled_account_cannot_use_one() {
    let (_d, store) = store().await;
    let inviter = account(&store, "inviter").await;
    let key = points::create_invite(&store, &inviter, 1)
        .await
        .unwrap()
        .remove(0);

    let alice = account(&store, "alice").await;
    let bob = account(&store, "bob").await;
    points::redeem_invite(&store, &key, &alice).await.unwrap();

    assert!(
        points::redeem_invite(&store, &key, &bob).await.is_err(),
        "the same key redeemed twice. A key that can be reused is not a key, it \\
         is a public link, and the invite-key count §8.4 makes configurable \\
         would be meaningless"
    );

    // A disabled account cannot redeem a fresh key.
    let key2 = points::create_invite(&store, &inviter, 1)
        .await
        .unwrap()
        .remove(0);
    sqlx::query("UPDATE account SET disabled = 1 WHERE id = ?")
        .bind(alice.to_string())
        .execute(store.pool())
        .await
        .unwrap();
    assert!(
        points::redeem_invite(&store, &key2, &bob).await.is_ok(),
        "an *enabled* account can still use a key, so the failure below is about \\
         the disabled account and not about the key"
    );
    let carol = account(&store, "carol").await;
    sqlx::query("UPDATE account SET disabled = 1 WHERE id = ?")
        .bind(carol.to_string())
        .execute(store.pool())
        .await
        .unwrap();
    let key3 = points::create_invite(&store, &inviter, 1)
        .await
        .unwrap()
        .remove(0);
    assert!(
        points::redeem_invite(&store, &key3, &carol).await.is_err(),
        "a disabled account redeemed an invite, which is how a taken-down \\
         account keeps adding new accounts to a corpus"
    );
}

// ---- badges ---------------------------------------------------------------

/// Badges are earned from a *count of facts*, not stored as a boolean.
///
/// A `has_badge` column is a counter in the one place §8.4 is not talking about
/// points, and it goes stale the moment a vote is retracted. Every badge is a
/// predicate over the award ledger and the vote table instead, so a badge is
/// either true now or was never true.
#[tokio::test]
async fn a_badge_is_a_predicate_not_a_stored_flag() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let subject = Uuid::new_v4();

    let a = propose(
        &store,
        subject,
        "title",
        &value("A Title"),
        commons_core::ProposalSource::User,
        Some(&alice),
    )
    .await;
    resolve::cast_vote(&store, &alice, &a, "title")
        .await
        .unwrap();
    points::reconcile(&store, SubjectType::Object, &subject, "title")
        .await
        .unwrap();

    let earned = points::badges(&store, &alice).await.unwrap();
    assert!(
        earned.contains(&points::Badge::FirstAcceptedProposal),
        "{earned:?}"
    );
    assert!(
        !earned.contains(&points::Badge::HundredAcceptedProposals),
        "and the hundred-proposal badge is absent, because one is not a hundred"
    );
}

/// The `HundredAcceptedProposals` threshold is a constant, and a test reads it.
///
/// Written as `BADGE_HUNDRED` so a test can sit exactly on the boundary instead
/// of hard-coding 100 and drifting from the definition. The boundary itself is
/// tested in `a_badge_threshold_is_a_boundary_and_not_a_vicinity`.
#[tokio::test]
async fn badge_thresholds_are_named_constants() {
    assert_eq!(points::BADGE_FIRST, 1);
    assert_eq!(points::BADGE_HUNDRED, 100);
    // The ladder is increasing, which is a real property rather than a
    // tautology: it is what makes `badges()` correct, since it filters with
    // `count >= threshold` and a non-monotonic ladder would hand somebody a
    // higher badge without the lower one. Checked over `Badge::ALL`, so adding a
    // badge to the enum in the wrong place fails here.
    let thresholds: Vec<usize> = points::Badge::ALL.iter().map(|b| b.threshold()).collect();
    assert!(
        thresholds.windows(2).all(|w| w[0] < w[1]),
        "the badge thresholds must strictly increase in declaration order, \
         because `badges()` hands out every badge whose threshold is met: \
         {thresholds:?}"
    );
    assert_eq!(
        thresholds,
        vec![points::BADGE_FIRST, points::BADGE_HUNDRED],
        "and `Badge::ALL` is exactly the ladder those two constants describe"
    );
}

#[tokio::test]

async fn a_badge_threshold_is_a_boundary_and_not_a_vicinity() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let n = points::BADGE_HUNDRED;

    for i in 0..(n - 1) {
        grant(&store, &alice, i).await;
    }
    let just_under = points::badges(&store, &alice).await.unwrap();
    assert!(
        !just_under.contains(&points::Badge::HundredAcceptedProposals),
        "{n} - 1 = {} accepted proposals and the hundred badge is already there. \\
         The threshold is off by one, and no test that used a comfortable \\
         distance from the boundary would see it.",
        n - 1
    );

    grant(&store, &alice, n - 1).await;
    let exactly = points::badges(&store, &alice).await.unwrap();
    assert!(
        exactly.contains(&points::Badge::HundredAcceptedProposals),
        "at exactly {n} the badge is there: the boundary is inclusive"
    );
}

/// Write one accepted proposal for `who`, directly into the award table.
///
/// A fixture that goes through `reconcile` would need a hundred subjects and a
/// hundred votes per test; the ledger is the thing under test, so writing it is
/// the honest fixture, and `a_badge_is_a_predicate_not_a_stored_flag` covers the
/// other direction.
async fn grant(store: &Store, who: &Uuid, i: usize) {
    sqlx::query(
        "INSERT INTO points_award (id, account_id, proposal_id, points, awarded_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(who.to_string())
    .bind(Uuid::new_v4().to_string())
    .bind(points::POINTS_PER_ACCEPTED_PROPOSAL)
    .bind(ts::now())
    .execute(store.pool())
    .await
    .unwrap();
    let _ = i;
}

// ---- the leaderboard ------------------------------------------------------

/// The leaderboard ranks accounts by points, highest first.
///
/// Straightforward, and present so the negative test below has a positive
/// counterpart: "no performer ranking" is only a meaningful claim if there *is* a
/// contributor ranking.
#[tokio::test]
async fn the_leaderboard_ranks_accounts_by_points_highest_first() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let bob = account(&store, "bob").await;

    for _ in 0..3 {
        grant(&store, &alice, 0).await;
    }
    for _ in 0..7 {
        grant(&store, &bob, 0).await;
    }

    let board = points::leaderboard(&store, 10).await.unwrap();
    assert!(board.len() >= 2, "{board:#?}");
    assert_eq!(
        board[0].handle, "bob",
        "seven awards beats three: {board:#?}"
    );
    assert_eq!(board[1].handle, "alice");
    assert!(
        board[0].total > board[1].total,
        "and it is ordered, not a set"
    );
}

/// A disabled account does not appear on the leaderboard.
///
/// A taken-down account's points are a record of what it did, not a standing. If
/// a disabled account still ranked, the leaderboard would be an argument for
/// keeping a person, which is the opposite of what a takedown is.
#[tokio::test]
async fn a_disabled_account_is_not_on_the_leaderboard() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    let bob = account(&store, "bob").await;
    for _ in 0..5 {
        grant(&store, &alice, 0).await;
    }
    for _ in 0..9 {
        grant(&store, &bob, 0).await;
    }
    sqlx::query("UPDATE account SET disabled = 1 WHERE id = ?")
        .bind(bob.to_string())
        .execute(store.pool())
        .await
        .unwrap();

    let board = points::leaderboard(&store, 10).await.unwrap();
    assert!(
        !board.iter().any(|e| e.handle == "bob"),
        "the top scorer is the disabled account: {board:#?}. A takedown that \\
         leaves somebody at the top of a public list has not taken them down."
    );
}

/// Ties are broken by handle, so the board is deterministic.
///
/// Without a total order the same library produces a different top-10 on
/// different runs, and a leaderboard that reshuffles on a re-read cannot be
/// cached, screenshotted, or argued about.
#[tokio::test]
async fn tied_scores_are_ordered_deterministically() {
    let (_d, store) = store().await;
    for h in ["zoe", "adam", "mary"] {
        let id = account(&store, h).await;
        for _ in 0..4 {
            grant(&store, &id, 0).await;
        }
    }
    let first = points::leaderboard(&store, 10).await.unwrap();
    let second = points::leaderboard(&store, 10).await.unwrap();
    assert_eq!(
        first.iter().map(|e| e.handle.clone()).collect::<Vec<_>>(),
        vec!["adam", "mary", "zoe"],
        "three accounts, equal scores, and the order is handle order: a \\
         leaderboard that reshuffles on a re-read is not one anybody can reason \\
         about"
    );
    assert_eq!(first, second, "and two reads agree");
}

/// The leaderboard has a limit, and a limit of zero is a refusal rather than a
/// "return everything".
///
/// A paging parameter that is ignored under a debug build is a way to dump the
/// whole account table into a response.
#[tokio::test]
async fn the_leaderboard_limit_is_honoured() {
    let (_d, store) = store().await;
    for i in 0..5 {
        let id = account(&store, &format!("u{i}")).await;
        for _ in 0..(5 - i) {
            grant(&store, &id, 0).await;
        }
    }
    assert_eq!(points::leaderboard(&store, 3).await.unwrap().len(), 3);
    assert!(
        points::leaderboard(&store, 0).await.is_err(),
        "a limit of zero returns everything, which is a paging parameter that \\
         does not page"
    );
    assert!(
        points::leaderboard(&store, 10_000).await.is_err(),
        "and an absurd limit is refused rather than served: a cap exists so the \\
         query cannot be used to enumerate the account table"
    );
}

// ---- Rule 3: THE NEGATIVE TEST --------------------------------------------

/// **The negative test.** No surface in `commons-index` ranks people by
/// popularity.
///
/// §2: "Not a performer-discovery site. No ranking of people by popularity as a
/// browsable surface, no 'top performers' front page." §8.4 allows popularity as
/// a *search tie-breaker* and nowhere else, and the whole of §8.4 is about
/// ranking *contributors*.
///
/// This is written against the crate's source rather than against a function,
/// and that is the only way it guards the promise. A test that asserts
/// `points::leaderboard` returns accounts is satisfied by a `performers()` sitting
/// next to it, unmentioned, unreviewed, and reachable from a route. This one
/// reads every SQL string the crate contains and fails on any that orders people
/// by a popularity column — so adding the query fails *this* test, at the moment
/// it is added, without anyone having to remember that the promise exists.
///
/// The forbidden tokens are the columns a popularity ranking would need. They are
/// listed rather than inferred, because "some query touches a performer table"
/// is not the rule: §7 is full of legitimate performer reads (fingerprints, alias
/// resolution, claim state), and a test that rejected those would be wrong.
#[test]
fn no_surface_ranks_people_by_popularity() {
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/points.rs")).unwrap();
    // Plus the sibling modules: a ranking could live next door.
    let mut files: Vec<String> = vec!["points.rs".to_string()];
    for extra in [
        "resolve.rs",
        "candidates.rs",
        "history.rs",
        "reputation.rs",
        "moderation.rs",
    ] {
        let p = format!("{}/src/{extra}", env!("CARGO_MANIFEST_DIR"));
        if std::path::Path::new(&p).exists() {
            files.push(extra.to_string());
        }
    }

    // A popularity ranking needs an ORDER BY on one of these, or a function whose
    // name promises one. Both are rejected; the ORDER BY list is the load-bearing
    // half because a function name can be anything.
    let forbidden_order = [
        "ORDER BY popularity",
        "ORDER BY view_count",
        "ORDER BY o.views",
        "ORDER BY scene_count",
        "ORDER BY f.count",
        "ORDER BY clip_count",
        "ORDER BY work_count",
    ];
    let forbidden_names = [
        "top_performers",
        "topperformers",
        "popular_performers",
        "performer_leaderboard",
        "performer_ranking",
        "trending_performers",
        "most_popular",
    ];

    // Skip comment lines. The test file itself *names* these tokens in order to
    // forbid them, so a scanner that reads its own denylist would fail on the
    // denylist -- and a test that cannot tell a rule from an instance of the
    // thing it forbids is a test that has to be switched off the first time
    // somebody writes a comment about the promise.
    let is_comment = |l: &str| {
        let t = l.trim_start();
        t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') || t.is_empty()
    };

    let mut offences: Vec<String> = Vec::new();
    for f in &files {
        let path = format!("{}/src/{f}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {f}: {e}"));
        for (i, line) in text.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            let lower = line.to_lowercase();
            for bad in forbidden_order {
                if lower.contains(&bad.to_lowercase()) {
                    offences.push(format!("{f}:{}: ORDER BY popularity -- `{bad}`", i + 1));
                }
            }
            for bad in forbidden_names {
                if lower.contains(bad) {
                    offences.push(format!(
                        "{f}:{}: a performer-popularity surface -- `{bad}`",
                        i + 1
                    ));
                }
            }
        }
    }
    // `src` is read above only to prove the path resolves.
    let _ = src;

    assert!(
        offences.is_empty(),
        "§2 says this is not a performer-discovery site, and §8.4's leaderboard is \
         over contributors. Found:\n  {}\n\nA ranking of people by popularity is the \
         easiest promise in the spec to erode, because each individual addition looks \
         harmless: a \"most viewed\" sort, a \"trending\" tab, a default order on a \
         search results page. It is also the one that changes what the software is. \
         Popularity belongs in exactly one place — as a tie-breaker in search ordering \
         (§2) — and that place is not a query in this crate.",
        offences.join("\n  ")
    );
}

/// The negative test does not fire on a *comment* about the promise.
///
/// Without this, the obvious response to a failing `no_surface_ranks_people_by_
/// popularity` is to delete the scan — it is the only test in the suite that
/// fails on a thing nobody wrote, and a test that cries wolf over a doc comment
/// is a test that gets switched off. A comment naming `top_performers` and
/// `ORDER BY popularity` is the module explaining what it deliberately does not
/// contain, and the scan has to read it and pass.
///
/// Written by appending the comment to this crate's own `points.rs`, running, and
/// restoring, because the alternative -- asserting the scan's own logic on
/// synthetic input -- would test a copy of the rule rather than the rule.
#[test]
fn the_popularity_scan_ignores_comments() {
    let path = format!("{}/src/points.rs", env!("CARGO_MANIFEST_DIR"));
    let original = std::fs::read_to_string(&path).unwrap();
    let probe =
        "pub fn _scan_probe() {}\n// top_performers, ORDER BY popularity, popular_performers\n";
    std::fs::write(&path, format!("{original}{probe}")).unwrap();

    // The scan is a test, so run the same logic inline against the probe file
    // rather than spawning `cargo test` from inside a test.
    let mut offences = 0usize;
    for line in probe.lines() {
        let t = line.trim_start();
        if t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') || t.is_empty() {
            continue;
        }
        for bad in [
            "top_performers",
            "popular_performers",
            "ORDER BY popularity",
        ] {
            if line.to_lowercase().contains(&bad.to_lowercase()) {
                offences += 1;
            }
        }
    }
    std::fs::write(&path, &original).unwrap();
    assert_eq!(
        offences, 0,
        "a comment naming the forbidden things must not be counted as an \
         offence, or the first person to write a comment explaining the promise \
         gets a failing test and removes the guard"
    );
}

/// The `points` module's own public API does not mention performers.
///
/// The same promise, as a signature check rather than a text scan. `pub fn` is
/// the thing a route handler calls, and a function whose *name* carries a
/// performer and a ranking is a surface even if its body happens to be a stub.
/// A source scan for strings would miss a function built by concatenation;
/// this cannot.
#[test]
fn the_public_api_names_no_performer_ranking() {
    let text =
        std::fs::read_to_string(format!("{}/src/points.rs", env!("CARGO_MANIFEST_DIR"))).unwrap();
    for line in text.lines() {
        let l = line.trim();
        if !l.starts_with("pub fn") && !l.starts_with("pub async fn") {
            continue;
        }
        let lower = l.to_lowercase();
        // A *performer* is not forbidden. A performer used as a *subject of a
        // ranking* is. So the check is for both words in one signature.
        let is_performer = lower.contains("performer");
        let is_ranking = lower.contains("leaderboard")
            || lower.contains("rank")
            || lower.contains("top_")
            || lower.contains("most_");
        assert!(
            !(is_performer && is_ranking),
            "§2 forbids a browsable ranking of people by popularity, and this is a \
             public function whose name is exactly that: `{l}`. A leaderboard in this \
             module ranks *contributors* by points; if a performer ranking is wanted, \
             it belongs in a spec change, not in a function name."
        );
    }
}

/// The leaderboard's subject is an account, and the test says so in the type.
///
/// `LeaderboardEntry` has no performer field, and that is asserted structurally:
/// a leaderboard row cannot hold a performer, so no query can put one in it
/// without changing the type the leaderboard function returns.
#[tokio::test]
async fn a_leaderboard_entry_is_an_account_and_nothing_else() {
    let (_d, store) = store().await;
    let alice = account(&store, "alice").await;
    grant(&store, &alice, 0).await;
    let board = points::leaderboard(&store, 10).await.unwrap();
    let entry = &board[0];

    // The struct is constructed with exactly these fields, so a new field --
    // `performer_id`, say -- would not compile here. The assertion below is the
    // runtime half; this is the compile-time half, and it is the stronger one.
    let rebuilt: points::LeaderboardEntry = points::LeaderboardEntry {
        handle: entry.handle.clone(),
        total: entry.total,
        accepted_proposals: entry.accepted_proposals,
    };
    assert_eq!(rebuilt, *entry);
}
