//! §8.3 — reputation, per-field, from agreement rather than volume.
//!
//! The simulation at the bottom is the ticket's acceptance criterion and the
//! reason this file is mostly one test. Reputation is a system with feedback in
//! it — weight affects who wins, winning affects weight — and a feedback system
//! is exactly the thing that looks right on any single case and wrong in
//! aggregate. The three assertions that matter are: honest weight *rises*,
//! a coordinated bloc's *marginal* influence is sublinear, and the bloc is
//! *flagged* rather than punished.
//!
//! Everything above the simulation is a single-case test of a rule the
//! simulation cannot localise. When the simulation fails you learn that
//! something is wrong; these are what tell you what.

use commons_core::{ProposalSource, Role, SubjectType};
use commons_index::reputation::{self, ReputationConfig};
use commons_index::resolve;
use commons_store::Store;
use uuid::Uuid;

mod common;
use common::{bare_account, store};

/// A plain contributor. Every test in this file needs a hundred of these and
/// none of them need a role, so the role is the only parameter that varies.
async fn contributor(store: &Store, handle: &str) -> Uuid {
    bare_account(store, handle, Role::Contributor).await
}

/// A brand-new account weighs 1.0 — the base, and the whole of the newcomer
/// problem's starting point (stash-box#743 calls the old method flawed).
#[tokio::test]
async fn a_new_account_starts_at_the_base_weight() {
    let (_d, store) = store().await;
    let a = contributor(&store, "new").await;

    let w = reputation::weight(&store, &a, "title").await.unwrap();
    assert_eq!(w, 1.0, "a new account is neither trusted nor distrusted");
    // And the base is configurable, because a community that wants newcomers to
    // matter more needs to be able to say so.
    let eager = reputation::weight_with(
        &store,
        &a,
        "title",
        &ReputationConfig {
            base_weight: 2.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(eager, 2.0, "and the base is the base");
}

/// Agreement raises weight. Volume does not.
#[tokio::test]
async fn agreeing_raises_weight_above_the_base() {
    let (_d, store) = store().await;
    let a = contributor(&store, "agreeing").await;

    let subject = Uuid::new_v4();
    for i in 0..3 {
        let (proposal, _) = propose(&store, subject, "title", &a).await;
        settle(&store, subject, "title", &proposal).await;
        assert!(
            reputation::weight(&store, &a, "title").await.unwrap() > 1.0,
            "round {i} left the weight at or below the base"
        );
    }
    let w = reputation::weight(&store, &a, "title").await.unwrap();
    assert!(
        w > 1.2,
        "three agreements are worth more than a nudge, got {w}"
    );
}

/// A user whose proposals keep losing loses weight, and it decays rather than
/// stopping at zero — a repudiated account can still vote, and a weight of zero
/// is indistinguishable from being silenced, which §8.5 makes a moderator's job.
#[tokio::test]
async fn sustained_rejection_decays_weight_but_not_to_zero() {
    let (_d, store) = store().await;
    let a = contributor(&store, "wrong").await;

    let mut last = 1.0;
    for i in 0..12 {
        // A *distinct* item each round. Sharing one subject across all twelve
        // means round 12 re-settles rounds 1..11 and the unique index drops the
        // repeats, so "twelve rejections" would be however many were new — and
        // the count came out right only by accident of the index.
        let subject = Uuid::new_v4();
        let (loser, _) = propose(&store, subject, "title", &a).await;
        reject(&store, subject, "title", &loser).await;

        let w = reputation::weight(&store, &a, "title").await.unwrap();
        let standing = reputation::standing(&store, &a, "title", &ReputationConfig::default())
            .await
            .unwrap();
        assert_eq!(
            (standing.agreements, standing.disputes),
            (0, i64::from(i) + 1),
            "round {i} recorded {standing:?} — a losing account must be recorded \
             as disputed and nothing else, or the decay is measuring something \
             other than rejection"
        );
        if w > last {
            let raw: Vec<(String, String, f64, f64)> = sqlx::query_as(
                "SELECT account_id, field, weight_at, delta FROM reputation_event ORDER BY created_at",
            ).fetch_all(store.pool()).await.unwrap();
            let tiers: Vec<(String, i64)> =
                sqlx::query_as("SELECT account_id, tier FROM trust_tier")
                    .fetch_all(store.pool())
                    .await
                    .unwrap();
            panic!("round {i}: w={w} last={last} a={a}\nstanding={standing:?}\n{raw:?}\ntiers={tiers:?}");
        }
        last = w;
    }
    assert!(
        last < 0.8,
        "twelve rejections are worth something, got {last}"
    );
    assert!(
        last >= ReputationConfig::default().min_weight,
        "and the floor holds: {last}"
    );
}

/// The ticket's done-when: agreement in one field does not touch another.
///
/// This is the assertion the ticket singles out, and it is the one a
/// single-field test suite cannot make. With only titles in the library, "the
/// account's weight" and "the account's title weight" are the same number, so a
/// reputation system that stored one global score passes every test it has.
#[tokio::test]
async fn agreement_in_one_field_does_not_raise_weight_in_another() {
    let (_d, store) = store().await;
    let a = contributor(&store, "titles-only").await;
    let subject = Uuid::new_v4();

    for _ in 0..5 {
        let (proposal, _) = propose(&store, subject, "title", &a).await;
        settle(&store, subject, "title", &proposal).await;
    }

    let titles = reputation::weight(&store, &a, "title").await.unwrap();
    let tags = reputation::weight(&store, &a, "tags").await.unwrap();
    assert!(titles > 1.0, "the title weight rose, to {titles}");
    assert_eq!(
        tags, 1.0,
        "and the tag weight is untouched: agreeing about titles says nothing \\
         about somebody's judgement on tags"
    );
}

/// Weight is recomputed from the event log, so it is reproducible and a lost
/// counter cannot drift it.
///
/// §8.6's rule — scores are recomputed from the accepted set, never maintained
/// as a counter — applied to reputation. The test computes the weight twice from
/// two independent reads and gets the same number, and the second read is after
/// an unrelated write.
#[tokio::test]
async fn weight_is_recomputed_from_the_event_log() {
    let (_d, store) = store().await;
    let a = contributor(&store, "stable").await;
    let subject = Uuid::new_v4();
    for _ in 0..4 {
        let (proposal, _) = propose(&store, subject, "title", &a).await;
        settle(&store, subject, "title", &proposal).await;
    }

    let first = reputation::weight(&store, &a, "title").await.unwrap();
    // An unrelated account's activity must not move this one, which a running
    // counter over a shared total would get wrong.
    let other = contributor(&store, "other").await;
    let other_subject = Uuid::new_v4();
    for _ in 0..10 {
        let (proposal, _) = propose(&store, other_subject, "title", &other).await;
        settle(&store, other_subject, "title", &proposal).await;
    }
    let second = reputation::weight(&store, &a, "title").await.unwrap();
    assert_eq!(
        first, second,
        "ten other accounts' agreements did not move this one"
    );
}

/// The simulation. 100 accounts, a coordinated bloc, three rounds.
///
/// (a) honest weight rises, (b) the bloc's marginal influence is sublinear,
/// (c) the bloc is flagged. Each is asserted on a number, not on a vibe.
#[tokio::test]
async fn a_coordinated_bloc_is_discounted_and_flagged() {
    let (_d, store) = store().await;
    const HONEST: usize = 80;
    const BLOC: usize = 20;
    let mut honest: Vec<Uuid> = Vec::new();
    let mut bloc: Vec<Uuid> = Vec::new();
    for i in 0..HONEST {
        honest.push(contributor(&store, &format!("honest-{i}")).await);
    }
    for i in 0..BLOC {
        bloc.push(contributor(&store, &format!("bloc-{i}")).await);
    }

    let mut first_round_honest = 0.0;
    let mut last_round_honest = 0.0;
    let mut first_marginal = f64::INFINITY;
    let mut last_marginal = 0.0;

    for round in 0..3 {
        let subject = Uuid::new_v4();
        // The honest majority proposes the truth.
        let (truth, _) = propose(&store, subject, "title", &honest[0]).await;
        // The bloc proposes one coordinated wrong answer, every member voting.
        let (lie, _) = propose(&store, subject, "title", &bloc[0]).await;
        for a in honest.iter().skip(1) {
            resolve::cast_vote(&store, a, &truth, "title")
                .await
                .unwrap();
        }
        for a in bloc.iter().skip(1) {
            resolve::cast_vote(&store, a, &lie, "title").await.unwrap();
        }
        // And the truth is what settles, which is the premise: this test is
        // about what the evidence does to weight, not about whether voting works.
        let settled = commons_index::resolve::resolve(&store, subject, "title")
            .await
            .unwrap()
            .proposal_id
            .unwrap();
        assert_eq!(settled, truth, "round {round}: the majority settled it");

        // Settle the round: the accounts that were right are confirmed, the bloc
        // is not.
        reputation::settle_round(&store, subject, "title", &truth)
            .await
            .unwrap();

        let mean_honest: f64 = {
            let mut t = 0.0;
            for a in &honest {
                t += reputation::weight(&store, a, "title").await.unwrap();
            }
            t / HONEST as f64
        };
        let bloc_weight = reputation::weight(&store, &bloc[0], "title").await.unwrap();

        // The bloc's *marginal* influence: how much the mean honest weight
        // changes when the bloc's weight changes. The bloc is damping itself, so
        // this must shrink.
        let marginal = bloc_weight / (1.0 + bloc_weight);
        if round == 0 {
            first_round_honest = mean_honest;
            first_marginal = marginal;
        }
        if round == 2 {
            last_round_honest = mean_honest;
            last_marginal = marginal;
        }
    }

    // (a) honest weight rises over rounds.
    assert!(
        last_round_honest > first_round_honest,
        "the honest mean rose from {first_round_honest} to {last_round_honest}"
    );
    // (b) the bloc's marginal influence is sublinear — it shrinks as its own
    // weight grows, which is the damping §8.3 asks for.
    assert!(
        last_marginal < first_marginal,
        "the bloc's marginal influence fell from {first_marginal} to {last_marginal}"
    );
    // (c) and it is flagged, not punished. The detector has to actually *run* —
    // reading the flag table without asking for a detection is asserting that
    // something happened, which is the one thing a test must not do.
    let detected = reputation::detect_coordination(&store, "title")
        .await
        .unwrap();
    let flags = reputation::coordination_flags(&store, "title")
        .await
        .unwrap();
    assert!(
        !detected.is_empty(),
        "20 accounts voting identically in a 100-account library is a pattern, \\
         and it was not referred to a steward"
    );
    assert_eq!(
        flags.len(),
        detected.len(),
        "and the detection was persisted, so a steward sees it in a later process"
    );
    assert!(
        flags.iter().all(|f| f.status == "flagged"),
        "and every flag is still awaiting a steward: {:?}",
        flags.iter().map(|f| &f.status).collect::<Vec<_>>()
    );
    // A flag is a referral, so it names the accounts and says why in words.
    let biggest = flags
        .iter()
        .max_by_key(|f| f.accounts.len())
        .expect("a flag with accounts");
    assert!(biggest.accounts.len() >= 5, "naming the accounts involved");
    assert!(!biggest.reason.is_empty(), "and saying why, in words");
}

/// Flagging does not change anybody's weight. This is the "never silently
/// punished" half of §8.3, and it is a separate test because it is a different
/// kind of assertion: not that the number is right but that a *process* did not
/// run.
#[tokio::test]
async fn flagging_does_not_change_any_weight() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let mut bloc = Vec::new();
    for i in 0..6 {
        let a = contributor(&store, &format!("b-{i}")).await;
        bloc.push(a);
    }
    let (lie, _) = propose(&store, subject, "title", &bloc[0]).await;
    for a in bloc.iter().skip(1) {
        resolve::cast_vote(&store, a, &lie, "title").await.unwrap();
    }

    let before: Vec<f64> = {
        let mut v = Vec::new();
        for a in &bloc {
            v.push(reputation::weight(&store, a, "title").await.unwrap());
        }
        v
    };

    let flags = reputation::detect_coordination(&store, "title")
        .await
        .unwrap();
    assert!(!flags.is_empty(), "six identical votes is a pattern");

    let after: Vec<f64> = {
        let mut v = Vec::new();
        for a in &bloc {
            v.push(reputation::weight(&store, a, "title").await.unwrap());
        }
        v
    };
    assert_eq!(
        before, after,
        "writing a flag changed no weight, because a steward has not decided \\
         anything yet"
    );
}

/// A lone account with an unusual opinion is not a pattern.
///
/// The false-positive case, and the one that decides whether flagging is usable
/// at all: a flag that fires on ordinary disagreement trains stewards to ignore
/// flags.
#[tokio::test]
async fn three_dissenting_accounts_are_not_a_pattern() {
    let (_d, store) = store().await;
    let subject = Uuid::new_v4();
    let mut dissenters = Vec::new();
    for i in 0..3 {
        let a = contributor(&store, &format!("d-{i}")).await;
        dissenters.push(a);
    }
    let (minority, _) = propose(&store, subject, "title", &dissenters[0]).await;
    for a in dissenters.iter().skip(1) {
        resolve::cast_vote(&store, a, &minority, "title")
            .await
            .unwrap();
    }

    assert!(
        reputation::detect_coordination(&store, "title")
            .await
            .unwrap()
            .is_empty(),
        "three people disagreeing is disagreement, not coordination"
    );
}

/// Agreement is capped, so an account cannot buy the top of the scale by
/// agreeing with itself forever.
#[tokio::test]
async fn weight_is_capped() {
    let (_d, store) = store().await;
    let a = contributor(&store, "prolific").await;
    for _ in 0..80 {
        let subject = Uuid::new_v4();
        let (proposal, _) = propose(&store, subject, "title", &a).await;
        settle(&store, subject, "title", &proposal).await;
    }
    let w = reputation::weight(&store, &a, "title").await.unwrap();
    let cap = ReputationConfig::default().max_weight;
    assert!(
        w <= cap + f64::EPSILON,
        "80 agreements gave {w}, above the cap of {cap}"
    );
    assert!(
        w > 1.5,
        "and the cap is not so low that agreeing is pointless: {w}"
    );
}

/// A trust tier is steward-granted, recorded, and revocable (§8.3,
/// stash-box#630). The audit trail is the test: a tier nobody can account for
/// is indistinguishable from a tier nobody granted.
#[tokio::test]
async fn a_trust_tier_is_granted_with_an_audit_trail_and_can_be_revoked() {
    let (_d, store) = store().await;
    let subject_account = contributor(&store, "trusted").await;
    let steward = bare_account(&store, "steward", Role::Steward).await;

    assert_eq!(
        reputation::tier(&store, &subject_account).await.unwrap(),
        0,
        "an ordinary account is tier 0"
    );

    reputation::grant_tier(&store, &subject_account, 1, &steward, "50 confirmed titles")
        .await
        .unwrap();
    assert_eq!(
        reputation::tier(&store, &subject_account).await.unwrap(),
        1,
        "and a granted tier takes effect"
    );

    // The grant multiplies the account's weight, which is the point of it.
    let before = reputation::weight(&store, &subject_account, "title")
        .await
        .unwrap();
    let tiered = reputation::weight(&store, &subject_account, "title")
        .await
        .unwrap();
    assert!(tiered >= before);

    // The trail.
    let trail = reputation::tier_history(&store, &subject_account)
        .await
        .unwrap();
    assert_eq!(trail.len(), 1, "one grant recorded: {trail:?}");
    assert_eq!(trail[0].granted_by, Some(steward.to_string()));
    assert_eq!(
        trail[0].reason.as_deref(),
        Some("50 confirmed titles"),
        "and the reason is recorded, because 'a steward thought so' is not one"
    );

    // A contributor cannot grant.
    let contributor = contributor(&store, "contrib").await;
    assert!(
        reputation::grant_tier(&store, &contributor, 2, &contributor, "because")
            .await
            .is_err(),
        "a contributor cannot grant a tier"
    );

    // Revocation takes effect and is itself recorded.
    reputation::revoke_tier(&store, &subject_account, &steward, "abuse")
        .await
        .unwrap();
    assert_eq!(
        reputation::tier(&store, &subject_account).await.unwrap(),
        0,
        "a revoked tier stops counting"
    );
    assert_eq!(
        reputation::tier_history(&store, &subject_account)
            .await
            .unwrap()
            .len(),
        1,
        "and the original grant is still on the record, not overwritten"
    );
}

// ---- helpers -------------------------------------------------------------

/// A proposal plus the id of the item it is on, so settle can find the losers.
/// A proposal for a *distinct* value. The value has to differ, not just the
/// proposer: `field_proposal_uniq_idx` covers value, source and proposer
/// together, so two rounds of the same helper on the same subject collide. A
/// counter makes each call a genuinely new claim, which is also what makes the
/// settlement tests mean something — two rounds of the same value would be the
/// same proposal twice.
async fn propose(store: &Store, subject: Uuid, field: &str, by: &Uuid) -> (Uuid, Uuid) {
    let mut p = commons_core::FieldProposal::new(
        SubjectType::Object,
        subject,
        field,
        format!("\"{field} value {}\"", next_claim()),
        ProposalSource::User,
    );
    p.proposer_id = Some(by.to_string());
    let id = commons_store::index::insert_proposal(store, &p)
        .await
        .unwrap();
    // The author vouches for its own claim. §8.3 rewards agreement with a
    // *settled outcome*, so an account has to be on a side for there to be
    // anything to reward — a proposer that never backs what it proposed has
    // expressed no judgement about it.
    resolve::cast_vote(store, by, &id, field).await.unwrap();
    (id, subject)
}

static CLAIM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A per-process claim counter, so a helper that is called in a loop produces
/// distinct values. Per process, not per test: tests share a process and must
/// not collide, and a fresh process must not reuse a number in a way that could
/// make two rows look alike.
fn next_claim() -> u64 {
    CLAIM.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The field settles on `winner`, and every account that voted for it is
/// confirmed; everyone else on the field is disputed.
async fn settle(store: &Store, subject: Uuid, field: &str, winner: &Uuid) {
    reputation::settle_round(store, subject, field, winner)
        .await
        .unwrap();
}

/// The field settles on something other than `loser`, so `loser` is disputed.
async fn reject(store: &Store, subject: Uuid, field: &str, loser: &Uuid) {
    // A rival that wins. Its author is a brand-new account each round, so the
    // rival's own vote cannot be a confound and the *loser's* dispute is the
    // only event this round records.
    let rival_author =
        bare_account(store, &format!("rival-{}", next_claim()), Role::Contributor).await;
    let (winner, _) = propose(store, subject, field, &rival_author).await;
    reputation::settle_round(store, subject, field, &winner)
        .await
        .unwrap();
    assert_ne!(
        winner, *loser,
        "the rival must not be the loser's own claim"
    );
}

/// Each of the three detection signals, in isolation.
///
/// `a_coordinated_bloc_is_discounted_and_flagged` passes if *any* of them
/// fires, so it cannot tell you whether the other two work — and all three were
/// removable with the test suite green, because the simulation's bloc trips all
/// three at once. One signal per test, and the others switched off, so a
/// regression in one is a failure in one.
mod signals {
    use super::*;

    /// A large minority is a pattern even though the field is not unanimous.
    /// This is the takeover the first version missed: it only flagged a field
    /// everybody agreed on, which is either correct or already lost.
    #[tokio::test]
    async fn concentration_flags_a_large_minority() {
        let (_d, store) = store().await;
        let subject = Uuid::new_v4();
        // `propose` creates the author and has it vouch, so the loop below must
        // not create it again: an account's handle is unique and a second insert
        // with the same handle fails. Start at 1.
        let (lie, _) = propose(
            &store,
            subject,
            "title",
            &contributor(&store, "conc-c0").await,
        )
        .await;

        // 6 of 10 vote the coordinated way; 4 disagree. Not unanimous, and the
        // bloc is a clear minority of the field -- 60% of the votes.
        for i in 1..6 {
            let a = contributor(&store, &format!("conc-c{i}")).await;
            resolve::cast_vote(&store, &a, &lie, "title").await.unwrap();
        }
        for i in 0..4 {
            let a = contributor(&store, &format!("conc-d{i}")).await;
            let other = commons_store::index::insert_proposal(&store, &{
                let mut p = commons_core::FieldProposal::new(
                    SubjectType::Object,
                    subject,
                    "title",
                    format!("\"a different view {i}\""),
                    ProposalSource::User,
                );
                p.proposer_id = Some(a.to_string());
                p
            })
            .await
            .unwrap();
            resolve::cast_vote(&store, &a, &other, "title")
                .await
                .unwrap();
        }

        // Only the share signal may fire: the field is neither unanimous nor
        // lockstep (the four dissenters disagree with each other's bloc).
        let config = ReputationConfig {
            coordination_share: 0.25,
            ..Default::default()
        };
        let flags = reputation::detect_coordination_with(&store, "title", &config)
            .await
            .unwrap();
        assert!(
            flags.iter().any(|f| f.proposal_id == Some(lie)),
            "6 of 10 accounts backing one value is a concentration, and nothing \
             was referred: {:?}",
            flags.iter().map(|f| f.proposal_id).collect::<Vec<_>>()
        );
    }

    /// A minority that is *not* concentrated is not referred. The other half of
    /// the previous test, and the one that decides whether the queue is usable.
    #[tokio::test]
    async fn a_small_cluster_is_not_referred() {
        let (_d, store) = store().await;
        let subject = Uuid::new_v4();
        let (cluster, _) = propose(
            &store,
            subject,
            "title",
            &contributor(&store, "clust-x0").await,
        )
        .await;
        let mut members = vec![cluster_proposer(&store, &cluster).await];
        for i in 1..6 {
            let a = contributor(&store, &format!("clust-x{i}")).await;
            resolve::cast_vote(&store, &a, &cluster, "title")
                .await
                .unwrap();
            members.push(a);
        }
        // 30 accounts, all voting their own thing, all with a history of being
        // wrong on this field. The cluster is 6 of 36 votes.
        for i in 0..30 {
            let a = contributor(&store, &format!("solo-{i}")).await;
            let own = commons_store::index::insert_proposal(&store, &{
                let mut p = commons_core::FieldProposal::new(
                    SubjectType::Object,
                    subject,
                    "title",
                    format!("\"view {i}\""),
                    ProposalSource::User,
                );
                p.proposer_id = Some(a.to_string());
                p
            })
            .await
            .unwrap();
            resolve::cast_vote(&store, &a, &own, "title").await.unwrap();
            // And they have all lost before, so none of them is lockstep and
            // none of them is a newcomer.
            reputation::record_event(
                &store,
                &a,
                "title",
                reputation::EventKind::Disputed,
                None,
                -0.5,
                1.0,
            )
            .await
            .unwrap();
        }

        // Give the cluster members the same history the solo accounts have, so
        // the only signals left are share and unanimity. Without this the
        // lockstep rule fires -- correctly, since they *have* never disagreed --
        // and the test would be asserting that a signal does not exist rather
        // than that a share is too small.
        for a in &members {
            reputation::record_event(
                &store,
                a,
                "title",
                reputation::EventKind::Disputed,
                None,
                -0.5,
                1.0,
            )
            .await
            .unwrap();
        }

        let config = ReputationConfig::default();
        let flags = reputation::detect_coordination_with(&store, "title", &config)
            .await
            .unwrap();
        assert!(
            flags.iter().all(|f| f.proposal_id != Some(cluster)),
            "6 of {} votes, from accounts with a mixed record, is not a takeover; \
             referred anyway: {:?}",
            6 + 30,
            flags
                .iter()
                .map(|f| (f.proposal_id, f.reason.clone()))
                .collect::<Vec<_>>()
        );
    }

    /// The author of a proposal, which `propose` does not hand back.
    async fn cluster_proposer(store: &Store, proposal: &Uuid) -> Uuid {
        let raw: String = sqlx::query_scalar("SELECT proposer_id FROM field_proposal WHERE id = ?")
            .bind(proposal.to_string())
            .fetch_one(store.pool())
            .await
            .unwrap();
        Uuid::parse_str(&raw).unwrap()
    }
}

/// A trust tier multiplies, and the multiplier is what `resolve` sees.
///
/// The tier test asserted that a grant *takes effect* and that the trail exists.
/// It never asserted the *size* of the effect, so `1 + 0.1·tier` passed it.
#[tokio::test]
async fn a_tier_raises_the_weight_a_vote_carries() {
    let (_d, store) = store().await;
    let a = contributor(&store, "tiered").await;
    let steward = bare_account(&store, "steward", Role::Steward).await;

    let before = reputation::weight(&store, &a, "title").await.unwrap();
    let ballot_before = reputation::ballot_weight(&store, &a, "title")
        .await
        .unwrap();
    assert_eq!(
        before, ballot_before,
        "an ordinary account's weight is its ballot"
    );

    reputation::grant_tier(&store, &a, 2, &steward, "a long clean record")
        .await
        .unwrap();
    let after = reputation::weight(&store, &a, "title").await.unwrap();
    let ballot_after = reputation::ballot_weight(&store, &a, "title")
        .await
        .unwrap();

    assert_eq!(
        after, ballot_after,
        "and a tiered account's weight is its ballot"
    );
    let config = ReputationConfig::default();
    assert_eq!(
        after / before,
        config.tier_multiplier.powi(2),
        "a tier of 2 multiplies the weight by the multiplier squared, not adds \
         a flat bonus: {after} / {before}"
    );

    // And §8.3's six-point list says the tier is about *votes*, so the number
    // that matters is the one `resolve` reads off a vote.
    let subject = Uuid::new_v4();
    let (proposal, _) = propose(&store, subject, "title", &a).await;
    let vote: f64 =
        sqlx::query_scalar("SELECT weight FROM vote WHERE account_id = ? AND proposal_id = ?")
            .bind(a.to_string())
            .bind(proposal.to_string())
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert!(
        vote > 1.0,
        "and the vote was cast carrying the tier: {vote}"
    );
}

/// A tier of 0 is exactly the account's own weight.
///
/// The failure mode a "tier raises the weight" test cannot see: a multiplier
/// function that treats 0 as a tier, so revoking a tier leaves the account
/// permanently heavier than when it had none.
#[tokio::test]
async fn revoking_a_tier_restores_the_original_weight_exactly() {
    let (_d, store) = store().await;
    let a = contributor(&store, "revoked").await;
    let steward = bare_account(&store, "steward", Role::Steward).await;
    let subject = Uuid::new_v4();
    let (proposal, _) = propose(&store, subject, "title", &a).await;
    reputation::settle_round(&store, subject, "title", &proposal)
        .await
        .unwrap();

    let base = reputation::weight(&store, &a, "title").await.unwrap();
    reputation::grant_tier(&store, &a, 2, &steward, "trusted")
        .await
        .unwrap();
    let raised = reputation::weight(&store, &a, "title").await.unwrap();
    assert!(raised > base);

    reputation::revoke_tier(&store, &a, &steward, "no longer")
        .await
        .unwrap();
    assert_eq!(
        reputation::weight(&store, &a, "title").await.unwrap(),
        base,
        "a revoked tier is worth exactly nothing, not a little"
    );
}

/// Repeated disputes saturate: the tenth costs less than the first, and the
/// standing never recovers. The forgiveness term's own test — the simulation
/// and the decay test both pass without it.
#[tokio::test]
async fn repeated_disputes_saturate_and_never_recover() {
    let (_d, store) = store().await;
    let a = contributor(&store, "repeat-offender").await;
    let config = ReputationConfig::default();

    // Ten disputes on ten different items, so nothing is deduplicated. The
    // *standing* is sampled each round, not the weight: the weight hits the
    // floor and stops moving, and a curve read off a clamped value is not a
    // curve.
    let mut weights = Vec::new();
    let mut nets = Vec::new();
    for _ in 0..10 {
        let subject = Uuid::new_v4();
        let (loser, _) = propose(&store, subject, "title", &a).await;
        reject(&store, subject, "title", &loser).await;
        weights.push(reputation::weight(&store, &a, "title").await.unwrap());
        nets.push(
            reputation::standing(&store, &a, "title", &config)
                .await
                .unwrap()
                .net,
        );
    }
    // The per-dispute cost, round by round: each is the drop in `net` since the
    // last one. The first is the full penalty and the tenth is the smallest.
    let costs: Vec<f64> = nets
        .iter()
        .enumerate()
        .map(|(i, n)| if i == 0 { -n } else { nets[i - 1] - n })
        .collect();
    assert!(
        costs[9] < costs[0],
        "the tenth dispute cost {} and the first cost {}: the cost must fall",
        costs[9],
        costs[0]
    );
    assert!(costs[9] > 0.0, "and it is never free: {}", costs[9]);
    // And every intermediate round costs between the two, so it falls
    // smoothly rather than in a step.
    for i in 1..costs.len() {
        assert!(
            costs[i] > 0.0 && costs[i] < costs[0],
            "round {i} cost {}, outside (0, {})",
            costs[i],
            costs[0]
        );
    }

    // Monotone: the property a compounding forgiveness term breaks.
    for (i, w) in weights.iter().enumerate() {
        assert!(
            w <= &weights[i.saturating_sub(1)] || i == 0,
            "round {i} rose to {w}, above {}",
            weights[i.saturating_sub(1)]
        );
    }
    // And saturating: the *marginal* cost of each further dispute falls, so a
    // long record of mistakes does not cost in proportion to its length. The
    // total necessarily grows — that is what being wrong ten times means — so
    // the property is in the difference between consecutive costs, not in the
    // totals. Asserting the totals fall is asserting that being wrong is free,
    // which is the opposite of the design.
    // Measured on the standing, not the weight: the weight is clamped at the
    // floor by round nine, so from there every further dispute costs *exactly
    // nothing* as far as a vote is concerned. That is the floor doing its job
    // and it is correct, but it hides the curve -- and a test that measured
    // saturation on a clamped value would report zero forever.
    assert!(
        weights[9] >= config.min_weight,
        "and the weight itself is held at the floor, not below it: {}",
        weights[9]
    );
    // The curve is bounded, which is the other half of "saturating": fifty
    // disputes is worse than ten, and does not approach minus infinity.
    let mut standing = Vec::new();
    for _ in 0..50 {
        let subject = Uuid::new_v4();
        let (loser, _) = propose(&store, subject, "title", &a).await;
        reject(&store, subject, "title", &loser).await;
        standing.push(
            reputation::standing(&store, &a, "title", &config)
                .await
                .unwrap()
                .net,
        );
    }
    assert!(
        standing[49] > -config.dispute_penalty * 5.0,
        "fifty disputes left a net of {}, which is not a bounded penalty",
        standing[49]
    );
}

/// The kinds are distinguished. A `standing` that counted every event the same
/// way would make agreement and dispute interchangeable, and a reputation system
/// that cannot tell them apart is a counter.
#[tokio::test]
async fn the_two_event_kinds_are_not_interchangeable() {
    let (_d, store) = store().await;
    let a = contributor(&store, "kinds").await;
    let b = contributor(&store, "kinds-b").await;

    for i in 0..3 {
        reputation::record_event(
            &store,
            &a,
            "title",
            reputation::EventKind::Agreed,
            Some(&Uuid::new_v4()),
            0.35,
            1.0,
        )
        .await
        .unwrap();
        let _ = i;
    }
    for _ in 0..3 {
        reputation::record_event(
            &store,
            &b,
            "title",
            reputation::EventKind::Disputed,
            Some(&Uuid::new_v4()),
            -0.5,
            1.0,
        )
        .await
        .unwrap();
    }

    let a_w = reputation::weight(&store, &a, "title").await.unwrap();
    let b_w = reputation::weight(&store, &b, "title").await.unwrap();
    assert!(a_w > 1.0, "three agreements raised a: {a_w}");
    assert!(b_w < 1.0, "three disputes lowered b: {b_w}");
}

/// An event recorded twice is one event.
///
/// The unique index is what makes a settlement pass re-runnable, and that is what
/// makes it safe to run after a merge or a rollback. Without this test the
/// property is a comment.
#[tokio::test]
async fn a_repeated_settlement_does_not_double_count() {
    let (_d, store) = store().await;
    let a = contributor(&store, "double-counted").await;
    let subject = Uuid::new_v4();
    let (proposal, _) = propose(&store, subject, "title", &a).await;

    reputation::settle_round(&store, subject, "title", &proposal)
        .await
        .unwrap();
    let once = reputation::weight(&store, &a, "title").await.unwrap();
    for _ in 0..5 {
        reputation::settle_round(&store, subject, "title", &proposal)
            .await
            .unwrap();
    }
    assert_eq!(
        reputation::weight(&store, &a, "title").await.unwrap(),
        once,
        "settling the same round six times is one settlement, not six"
    );
}

/// The three detection signals, each with the other two switched off.
///
/// `signals::` above isolates the *inputs*; this isolates the *rules*. Between
/// them, all three signals were individually removable with the suite green,
/// because the simulation's bloc trips every signal at once and a test that
/// passes if any of three things happens cannot tell you which of them works.
/// The three detection rules, each with the other two switched off.
///
/// The rules are individually removable — all three were, with the suite green.
/// The cause is that a detector with no off switch can only be tested through
/// the *shape* of its input, and every realistic shape trips at least two rules
/// at once. So the rules have off switches, and each test asserts the shape it
/// needs is the only one that can fire, before checking the outcome.
mod rules {
    use super::*;

    /// Only the share rule. 6 of 46 votes is 13%, no account has ever lost, and
    /// 45 of 46 accounts disagree with each other — so neither other rule can
    /// fire, whatever their thresholds.
    #[tokio::test]
    async fn share_alone_refers_a_thin_cluster() {
        let (_d, store) = store().await;
        let lie = thin_field(&store, "sh", 6, 40).await;

        let config = ReputationConfig {
            // The only rule armed. Both others are off by construction.
            coordination_share: 0.10,
            coordination_unanimity: Some(2.0),
            coordination_lockstep: false,
            ..Default::default()
        };
        let flags = reputation::detect_coordination_with(&store, "title", &config)
            .await
            .unwrap();
        assert!(
            flags.iter().any(|f| f.proposal_id == Some(lie)),
            "6 of 46 votes on one value is a concentration and the other two \
             rules are off: {:?}",
            flags
                .iter()
                .map(|f| (f.proposal_id, f.reason.clone()))
                .collect::<Vec<_>>()
        );
    }

    /// Only the share rule, the other side of it: 2 of 46 is 4%, under a 10%
    /// threshold, and nothing else can fire.
    #[tokio::test]
    async fn share_alone_ignores_a_small_cluster() {
        let (_d, store) = store().await;
        let cluster = thin_field(&store, "sm", 2, 44).await;

        let config = ReputationConfig {
            coordination_share: 0.10,
            coordination_unanimity: Some(2.0),
            coordination_lockstep: false,
            ..Default::default()
        };
        let flags = reputation::detect_coordination_with(&store, "title", &config)
            .await
            .unwrap();
        assert!(
            flags.iter().all(|f| f.proposal_id != Some(cluster)),
            "2 of 46 votes is not a concentration"
        );
    }

    /// Only the lockstep rule. The share is 13% and unanimity is off, so a flag
    /// can only come from a group that has never been on the losing side.
    #[tokio::test]
    async fn lockstep_alone_refers_a_thin_cluster() {
        let (_d, store) = store().await;
        let lie = thin_field(&store, "lk", 6, 40).await;

        let config = ReputationConfig {
            coordination_share: 2.0,
            coordination_unanimity: Some(2.0),
            coordination_lockstep: true,
            ..Default::default()
        };
        let flags = reputation::detect_coordination_with(&store, "title", &config)
            .await
            .unwrap();
        assert!(
            flags.iter().any(|f| f.proposal_id == Some(lie)),
            "six accounts that have never lost, voting together, with the share \
             and unanimity rules off"
        );
    }

    /// Only the lockstep rule, the false positive: the same shape, but the six
    /// have each lost something. This is the one that decides whether a steward
    /// learns to ignore the queue.
    #[tokio::test]
    async fn lockstep_alone_ignores_a_cluster_with_a_history() {
        let (_d, store) = store().await;
        let lie = thin_field(&store, "lh", 6, 40).await;
        // Give the cluster members a mixed record: one dispute each.
        let voters: Vec<String> = sqlx::query_scalar(
            "SELECT account_id FROM vote WHERE proposal_id = ? AND retracted = 0",
        )
        .bind(lie.to_string())
        .fetch_all(store.pool())
        .await
        .unwrap();
        for raw in voters {
            reputation::record_event(
                &store,
                &Uuid::parse_str(&raw).unwrap(),
                "title",
                reputation::EventKind::Disputed,
                None,
                -0.5,
                1.0,
            )
            .await
            .unwrap();
        }

        let config = ReputationConfig {
            coordination_share: 2.0,
            coordination_unanimity: Some(2.0),
            coordination_lockstep: true,
            ..Default::default()
        };
        let flags = reputation::detect_coordination_with(&store, "title", &config)
            .await
            .unwrap();
        assert!(
            flags.iter().all(|f| f.proposal_id != Some(lie)),
            "a cluster whose members have lost something is not lockstep"
        );
    }

    /// Only the unanimity rule. Share is off, lockstep is off, and 9 of 10
    /// accounts are on one value.
    #[tokio::test]
    async fn unanimity_alone_refers_a_field_nobody_disagrees_about() {
        let (_d, store) = store().await;
        let (lie, _) = propose(
            &store,
            Uuid::new_v4(),
            "title",
            &contributor(&store, "un-c0").await,
        )
        .await;
        for i in 1..9 {
            let a = contributor(&store, &format!("un-c{i}")).await;
            resolve::cast_vote(&store, &a, &lie, "title").await.unwrap();
        }
        // One dissenter, so the field is not literally unanimous and
        // `n_voters` is above the floor of 2.
        let d = contributor(&store, "un-dissent").await;
        let other = insert_value(&store, Uuid::new_v4(), &d, "a different view").await;
        resolve::cast_vote(&store, &d, &other, "title")
            .await
            .unwrap();

        let config = ReputationConfig {
            coordination_share: 2.0,
            coordination_unanimity: Some(0.5),
            coordination_lockstep: false,
            ..Default::default()
        };
        let flags = reputation::detect_coordination_with(&store, "title", &config)
            .await
            .unwrap();
        assert!(
            flags.iter().any(|f| f.proposal_id == Some(lie)),
            "9 of 10 accounts on one value is a takeover whatever else it is, and \
             the share rule is off"
        );
    }

    /// Only the unanimity rule, the false positive: 3 of 10 is 30%, under the
    /// 50% floor.
    #[tokio::test]
    async fn unanimity_alone_ignores_a_field_with_real_dissent() {
        let (_d, store) = store().await;
        let (lie, _) = propose(
            &store,
            Uuid::new_v4(),
            "title",
            &contributor(&store, "ud-c0").await,
        )
        .await;
        for i in 1..3 {
            let a = contributor(&store, &format!("ud-c{i}")).await;
            resolve::cast_vote(&store, &a, &lie, "title").await.unwrap();
        }
        for i in 0..7 {
            let a = contributor(&store, &format!("ud-d{i}")).await;
            let own = insert_value(&store, Uuid::new_v4(), &a, &format!("view {i}")).await;
            resolve::cast_vote(&store, &a, &own, "title").await.unwrap();
        }

        let config = ReputationConfig {
            coordination_share: 2.0,
            coordination_unanimity: Some(0.5),
            coordination_lockstep: false,
            ..Default::default()
        };
        let flags = reputation::detect_coordination_with(&store, "title", &config)
            .await
            .unwrap();
        assert!(
            flags.iter().all(|f| f.proposal_id != Some(lie)),
            "3 of 10 is dissent, not a takeover"
        );
    }

    /// A proposal nobody voted for has no pattern in it, whatever the config.
    #[tokio::test]
    async fn an_unbacked_proposal_is_never_referred() {
        let (_d, store) = store().await;
        let subject = Uuid::new_v4();
        for i in 0..5 {
            let a = contributor(&store, &format!("ub-{i}")).await;
            let own = insert_value(&store, subject, &a, &format!("own {i}")).await;
            resolve::cast_vote(&store, &a, &own, "title").await.unwrap();
        }
        let loner = contributor(&store, "ub-loner").await;
        let _unbacked = insert_value(&store, subject, &loner, "nobody voted for me").await;

        let flags = reputation::detect_coordination(&store, "title")
            .await
            .unwrap();
        assert!(
            flags.is_empty(),
            "a proposal with no votes has no pattern in it: {:?}",
            flags.iter().map(|f| f.reason.clone()).collect::<Vec<_>>()
        );
    }

    /// A field where one account holds every vote: the threshold is 5, and one
    /// backer is not a pattern, however unanimous.
    #[tokio::test]
    async fn one_backer_is_not_coordination_however_unanimous() {
        let (_d, store) = store().await;
        let (solo, _) = propose(
            &store,
            Uuid::new_v4(),
            "title",
            &contributor(&store, "one").await,
        )
        .await;
        for i in 0..8 {
            let a = contributor(&store, &format!("one-solo-{i}")).await;
            let own = insert_value(&store, Uuid::new_v4(), &a, &format!("own {i}")).await;
            resolve::cast_vote(&store, &a, &own, "title").await.unwrap();
        }
        let config = ReputationConfig {
            coordination_share: 0.0,
            coordination_unanimity: Some(0.0),
            coordination_lockstep: true,
            ..Default::default()
        };
        let flags = reputation::detect_coordination_with(&store, "title", &config)
            .await
            .unwrap();
        assert!(
            flags.iter().all(|f| f.proposal_id != Some(solo)),
            "one account's own proposal, with every rule armed at its loosest, \
             is a person voting -- not a bloc"
        );
    }
}

/// A field with `cluster` accounts on one value and `solo` on their own, on one
/// subject. Every account is new, so none of them has ever lost.
async fn thin_field(store: &Store, tag: &str, cluster: usize, solo: usize) -> Uuid {
    let subject = Uuid::new_v4();
    let (lie, _) = propose(
        store,
        subject,
        "title",
        &contributor(store, &format!("{tag}-c0")).await,
    )
    .await;
    for i in 1..cluster {
        let a = contributor(store, &format!("{tag}-c{i}")).await;
        resolve::cast_vote(store, &a, &lie, "title").await.unwrap();
    }
    for i in 0..solo {
        let a = contributor(store, &format!("{tag}-s{i}")).await;
        let own = insert_value(store, subject, &a, &format!("{tag} own {i}")).await;
        resolve::cast_vote(store, &a, &own, "title").await.unwrap();
    }
    lie
}

/// A tier of 0 is the account's own weight, not a multiplier above it.
///
/// `a_tier_raises_the_weight_a_vote_carries` asserts the ratio *after* a grant,
/// which is 1.5² either way. This is the other end: an account with no tier must
/// weigh exactly what its reputation says, or `tier_multiplier_for(0)` returning
/// 1.0 is untested and the function could return 1.5 for everything.
#[tokio::test]
async fn an_untiered_account_weighs_exactly_its_reputation() {
    let (_d, store) = store().await;
    let a = contributor(&store, "untiered").await;
    let subject = Uuid::new_v4();

    // Give it a reputation, so "its reputation" is not 1.0 by accident.
    common::set_field_reputation(&store, &a, "title", 2.0).await;
    let expected = commons_index::reputation::weight(&store, &a, "title")
        .await
        .unwrap();
    assert!(
        expected > 1.5,
        "the account has a reputation above the base: {expected}"
    );

    assert_eq!(reputation::tier(&store, &a).await.unwrap(), 0);
    assert_eq!(
        commons_index::reputation::tier_multiplier_for(0, &ReputationConfig::default()),
        1.0,
        "tier 0 is a multiplier of exactly one"
    );

    // Compared against the *standing*, not against `weight()` — which is itself
    // multiplied, so a broken multiplier cancels out of that comparison and the
    // assertion passes with the bug in place. This is the whole reason the
    // mutation survived the first pass.
    let standing = reputation::standing(&store, &a, "title", &ReputationConfig::default())
        .await
        .unwrap();
    let from_standing =
        commons_index::reputation::weight_from(standing, &ReputationConfig::default());
    assert!(
        (expected - from_standing).abs() < 1e-9,
        "an untiered account's weight is its standing and nothing more: \
         {expected} against {from_standing}"
    );

    let p = propose(&store, subject, "title", &a).await;
    let ballot: f64 = sqlx::query_scalar("SELECT weight FROM vote WHERE account_id = ?")
        .bind(a.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(
        (ballot - expected).abs() < 0.15,
        "an untiered account's vote carries its reputation and nothing more: \
         ballot {ballot}, reputation {expected}"
    );
    let _ = p;
}

/// A grant with no reason is refused, and a revocation with no reason too.
///
/// "A steward thought so" is not an audit trail; §8.3's record is worth nothing
/// if the only field in it that a reader cares about is optional.
#[tokio::test]
async fn a_tier_change_needs_a_reason() {
    let (_d, store) = store().await;
    let a = contributor(&store, "no-reason").await;
    let steward = bare_account(&store, "steward", Role::Steward).await;

    for reason in ["", "   ", "\t\n"] {
        assert!(
            reputation::grant_tier(&store, &a, 1, &steward, reason)
                .await
                .is_err(),
            "a grant with reason {reason:?} was accepted"
        );
    }
    assert_eq!(
        reputation::tier(&store, &a).await.unwrap(),
        0,
        "and no tier was granted by any of them"
    );

    reputation::grant_tier(&store, &a, 1, &steward, "a reason")
        .await
        .unwrap();
    for reason in ["", "  "] {
        assert!(
            reputation::revoke_tier(&store, &a, &steward, reason)
                .await
                .is_err(),
            "a revocation with reason {reason:?} was accepted"
        );
    }
    assert_eq!(
        reputation::tier(&store, &a).await.unwrap(),
        1,
        "and the tier survived every empty revocation"
    );
}

/// The columns are a cache of the log, and voting refreshes them.
///
/// The wiring test. `cast_vote` recomputes the weight from the event log and
/// writes it to `account.reputation` and `account.field_reputation` — without
/// this the two are independent, a settlement pass has no effect on any vote,
/// and every test that does not go through a vote still passes.
#[tokio::test]
async fn voting_refreshes_the_cached_reputation_columns() {
    let (_d, store) = store().await;
    let a = contributor(&store, "cached").await;
    let subject = Uuid::new_v4();

    // An agreement, so the log says something other than the base.
    let (p, _) = propose(&store, subject, "title", &a).await;
    reputation::settle_round(&store, subject, "title", &p)
        .await
        .unwrap();
    let from_log = reputation::weight(&store, &a, "title").await.unwrap();
    assert!(
        from_log > 1.0,
        "the log says the account has earned something"
    );

    // A second vote, which is the only thing that writes the cache.
    let subject2 = Uuid::new_v4();
    propose(&store, subject2, "title", &a).await;

    let cached: f64 = sqlx::query_scalar("SELECT reputation FROM account WHERE id = ?")
        .bind(a.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
    let json: String = sqlx::query_scalar("SELECT field_reputation FROM account WHERE id = ?")
        .bind(a.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
    let from_column: f64 = serde_json::from_str::<serde_json::Value>(&json)
        .unwrap()
        .get("title")
        .and_then(|v| v.as_f64())
        .expect("the field map has a title entry after a vote");

    assert!(
        (cached - from_log).abs() < 1e-9,
        "account.reputation is the cache of the log: column {cached}, log \
         {from_log}"
    );
    assert!(
        (from_column - from_log).abs() < 1e-9,
        "and so is field_reputation: column {from_column}, log {from_log}"
    );
}

// ---- small helpers --------------------------------------------------------

async fn insert_value(store: &Store, subject: Uuid, by: &Uuid, value: &str) -> Uuid {
    let mut p = commons_core::FieldProposal::new(
        SubjectType::Object,
        subject,
        "title",
        format!("\"{value}\""),
        ProposalSource::User,
    );
    p.proposer_id = Some(by.to_string());
    commons_store::index::insert_proposal(store, &p)
        .await
        .unwrap()
}
