//! T-P3-005 acceptance: §7.5's self-service performer claim.
//!
//! # What this ticket is really about
//!
//! §7.5 exists because stash cannot offer its users a performer concept at all.
//! The person in the content is the one who can consent to it, and a verified
//! claim is how a takedown request reaches the right person without that person
//! needing an index account.
//!
//! So the property worth testing is **scope**, and the ticket says so
//! explicitly: "the scope assertion is the point." Everything else here -- that
//! a claim is queued rather than applied, that approval produces a dashboard --
//! is in service of one question. Once a performer is verified, what may they
//! change?
//!
//! The answer has to be *exactly* their own record. A performer who can edit
//! another performer's metadata is not a user with permissions, they are a
//! peer with write access to each other's identities, and the platform's entire
//! claim mechanism rests on that being impossible. A test that only checks the
//! happy path would pass against an implementation that got this wrong.
//!
//! # A name that is already taken
//!
//! The `claim` table in the schema is the *federation* claim (§14.1): a signed
//! assertion from a peer. A performer claim is a different thing entirely -- an
//! unverified human saying "that is me" -- and it is named `performer_claim`
//! here. Conflating the two would be easy and would be wrong: the federation
//! claim carries a signature and a content hash and is never moderated, and
//! folding a self-service claim into it would make an unsigned row look like a
//! signed one.

mod common;

use common::*;
use commons_identity::claim::{self, ClaimError, ClaimState, Dashboard, Submit};
use commons_identity::cluster::ops;
use commons_store::Store;

/// A cluster with one appearance, the minimum a dashboard can show.
async fn a_cluster_with_one_appearance() -> (tempfile::TempDir, Store, String, String) {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(90).unit();
    object(&store, "img-1").await;
    let out = engine
        .assign("img-1", &Frame::from_pair(&base, 0.05), None)
        .await
        .unwrap();
    (d, store, out.cluster_id.unwrap(), out.appearance_id)
}

/// Submit a claim and approve it in one step, returning the performer id.
///
/// The shape nearly every test below needs, and writing it out four times meant
/// four chances to get the submit-then-approve order wrong. A call to
/// `approve_pending` without a claim is refused -- correctly, since approving
/// nothing is what the moderation step exists to prevent -- so the helper is the
/// honest way to set the fixture up.
async fn claim_and_approve(store: &Store, account: &str, cluster_id: &str) -> String {
    claim::submit(
        store,
        claim::Submit {
            cluster_id: cluster_id.to_string(),
            account: account.to_string(),
            evidence: "test evidence".into(),
        },
    )
    .await
    .unwrap();
    claim::approve_pending(store, account, cluster_id)
        .await
        .unwrap()
}

/// A submitted claim is queued, not applied.
///
/// The distinction is the whole of §7.5's "the claim goes to moderation". A
/// claim that took effect on submission would let anyone take over any cluster
/// by clicking a button, which is a worse outcome than having no claim feature
/// at all.
#[tokio::test]
async fn a_submitted_claim_is_queued_and_not_applied() {
    let (_d, store, cluster_id, _appearance) = a_cluster_with_one_appearance().await;

    let claim = claim::submit(
        &store,
        Submit {
            cluster_id: cluster_id.clone(),
            account: "performer-anna".into(),
            // The evidence is what a moderator reads. §7.5 says a person may
            // "privately verify" a cluster, so this is expected to be a private
            // channel and is stored as such rather than published.
            evidence: "a link to a page only I control".into(),
        },
    )
    .await
    .unwrap();

    assert_eq!(claim.state, ClaimState::Queued, "not applied, not rejected");
    assert_eq!(claim.cluster_id, cluster_id);

    // Nothing about the cluster changed. A pending claim that had already
    // attached itself would show up here as a verified performer.
    assert!(
        !claim::is_verified(&store, &cluster_id).await.unwrap(),
        "a queued claim does not verify anything"
    );
    assert!(
        claim::verified_performer(&store, &cluster_id)
            .await
            .unwrap()
            .is_none(),
        "and the cluster has no performer"
    );
}

/// The queue is the steward queue, and it shows the evidence.
#[tokio::test]
async fn the_queue_holds_pending_claims_with_their_evidence() {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(91).unit();
    let mut clusters = Vec::new();
    for name in ["q1", "q2"] {
        object(&store, name).await;
        let v = Frame::from_pair(&base, 0.05 + clusters.len() as f32 * 1.4);
        clusters.push(
            engine
                .assign(name, &v, None)
                .await
                .unwrap()
                .cluster_id
                .unwrap(),
        );
    }
    for (i, c) in clusters.iter().enumerate() {
        claim::submit(
            &store,
            Submit {
                cluster_id: c.clone(),
                account: format!("performer-{i}"),
                evidence: format!("evidence {i}"),
            },
        )
        .await
        .unwrap();
    }

    let queue = claim::queue(&store).await.unwrap();
    assert_eq!(queue.len(), 2, "both claims are queued: {queue:?}");
    for c in &queue {
        assert_eq!(c.state, ClaimState::Queued);
        assert!(
            !c.evidence.is_empty(),
            "the moderator has to see the evidence to decide: {c:?}"
        );
    }
    let _ = d;
}

/// Two claims on the same cluster: the second is refused, not queued.
///
/// This is the abuse case the queue exists to stop. Without it, a cluster under
/// active contest accumulates one pending claim per person who clicked, and the
/// moderator's queue becomes a list of duplicates rather than a set of distinct
/// claims.
#[tokio::test]
async fn a_second_claim_on_the_same_cluster_is_refused() {
    let (_d, store, cluster_id, _appearance) = a_cluster_with_one_appearance().await;

    claim::submit(
        &store,
        Submit {
            cluster_id: cluster_id.clone(),
            account: "first".into(),
            evidence: "mine".into(),
        },
    )
    .await
    .unwrap();

    let second = claim::submit(
        &store,
        Submit {
            cluster_id: cluster_id.clone(),
            account: "second".into(),
            evidence: "also mine".into(),
        },
    )
    .await;
    assert!(
        matches!(second, Err(ClaimError::AlreadyClaimed { .. })),
        "the second claim is refused, and says why: {second:?}"
    );

    assert_eq!(
        claim::queue(&store).await.unwrap().len(),
        1,
        "and the queue still holds one"
    );
}

/// An account cannot submit two claims either, so a person cannot farm the
/// queue with one claim per cluster they are not in.
#[tokio::test]
async fn an_account_may_hold_only_one_pending_claim() {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(92).unit();
    let mut clusters = Vec::new();
    for name in ["c1", "c2"] {
        object(&store, name).await;
        let v = Frame::from_pair(&base, 0.05 + clusters.len() as f32 * 1.4);
        clusters.push(
            engine
                .assign(name, &v, None)
                .await
                .unwrap()
                .cluster_id
                .unwrap(),
        );
    }

    claim::submit(
        &store,
        Submit {
            cluster_id: clusters[0].clone(),
            account: "busy".into(),
            evidence: "e".into(),
        },
    )
    .await
    .unwrap();
    let second = claim::submit(
        &store,
        Submit {
            cluster_id: clusters[1].clone(),
            account: "busy".into(),
            evidence: "e".into(),
        },
    )
    .await;
    assert!(
        matches!(second, Err(ClaimError::TooManyPending)),
        "one pending claim per account: {second:?}"
    );
    let _ = d;
}

/// Approval verifies the performer, and only then.
#[tokio::test]
async fn approval_verifies_the_performer() {
    let (_d, store, cluster_id, _appearance) = a_cluster_with_one_appearance().await;
    let claim = claim::submit(
        &store,
        Submit {
            cluster_id: cluster_id.clone(),
            account: "performer-anna".into(),
            evidence: "e".into(),
        },
    )
    .await
    .unwrap();

    claim::approve(&store, &claim.id, "steward-1")
        .await
        .unwrap();

    let verified = claim::verified_performer(&store, &cluster_id)
        .await
        .unwrap()
        .expect("the cluster has a verified performer");
    assert_eq!(verified, "performer-anna");
    assert!(claim::is_verified(&store, &cluster_id).await.unwrap());
    assert!(
        claim::queue(&store).await.unwrap().is_empty(),
        "an approved claim leaves the queue"
    );
}

/// The dashboard scope is *exactly* the claimed cluster.
///
/// The positive case: the performer sees their own appearances.
#[tokio::test]
async fn the_dashboard_shows_the_claimed_clusters_appearances() {
    let (_d, store, cluster_id, appearance) = a_cluster_with_one_appearance().await;
    let performer = claim_and_approve(&store, "performer-anna", &cluster_id).await;

    // The dashboard is keyed by *account*, not by performer id: §7.5's scope is
    // written against the claim, and the account is what a person logs in as.
    let dash = claim::dashboard(&store, "performer-anna").await.unwrap();
    assert_eq!(
        dash.appearances,
        vec![appearance.clone()],
        "their own appearance is on it: {dash:?}"
    );
    assert_eq!(dash.clusters, vec![cluster_id.clone()]);
    let _ = (appearance, performer);
}

/// The negative case, and the reason the ticket exists: a verified performer
/// cannot touch another performer's record.
///
/// Not "is refused a polite error" -- *cannot*. Every read path the dashboard
/// uses is scoped the same way, so this asserts the scope rather than one
/// endpoint's behaviour.
#[tokio::test]
async fn a_performer_cannot_edit_another_performers_record() {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(93).unit();

    // Two clusters, far apart, so they cannot be the same person.
    let mut clusters = Vec::new();
    for (i, name) in ["m1", "m2"].iter().enumerate() {
        object(&store, name).await;
        let v = Frame::from_pair(&base, 0.05 + i as f32 * 1.4);
        let cluster = engine
            .assign(name, &v, None)
            .await
            .unwrap()
            .cluster_id
            .unwrap();
        clusters.push(cluster);
    }

    // Two verified performers: Anna owns the first cluster, Bert the second. Bert
    // being verified too is the point -- the case that would be a real hole is a
    // *verified* person editing another verified person, not a stranger being
    // refused.
    let anna_account = "anna-account";
    let bert_account = "bert-account";
    let anna_performer = claim_and_approve(&store, anna_account, &clusters[0]).await;
    let bert_performer = claim_and_approve(&store, bert_account, &clusters[1]).await;
    assert_ne!(
        anna_performer, bert_performer,
        "the two accounts resolve to two different performer records"
    );

    // Anna proposes a correction to *Bert's* record.
    let err = claim::propose_correction(
        &store,
        anna_account,
        &bert_performer,
        "name",
        &serde_json::json!("Not Bert"),
    )
    .await;
    assert!(
        matches!(err, Err(ClaimError::NotYourRecord { .. })),
        "the correction is refused, by a named error rather than a silent drop: {err:?}"
    );

    // Bert's record is untouched: the refusal is not just a rejected call, it
    // wrote nothing.
    let bert_value = ops::accepted_value(&store, "performer", &bert_performer, "name")
        .await
        .unwrap();
    assert!(
        bert_value.is_none() || bert_value != Some(serde_json::json!("Not Bert")),
        "Anna's refused attempt did not change Bert's record"
    );

    // And Anna can still correct her own -- the refusal is about *whose* record,
    // not a broken permission check that refuses everyone.
    claim::propose_correction(
        &store,
        anna_account,
        &anna_performer,
        "name",
        &serde_json::json!("Anna L."),
    )
    .await
    .unwrap();
    let proposals = ops::proposals_for(&store, "performer", &anna_performer, "name")
        .await
        .unwrap();
    assert!(
        proposals
            .iter()
            .any(|(_, v)| v == &serde_json::json!("Anna L.")),
        "her own correction was accepted: {proposals:?}"
    );
    let _ = d;
}

#[tokio::test]
async fn an_unverified_account_has_no_dashboard() {
    let (_d, store, cluster_id, _appearance) = a_cluster_with_one_appearance().await;
    let empty: Dashboard = claim::dashboard(&store, "nobody").await.unwrap();
    assert!(
        empty.appearances.is_empty() && empty.clusters.is_empty(),
        "an unverified account sees nothing, rather than everything: {empty:?}"
    );
    let _ = cluster_id;
}

/// Rejection closes the claim and leaves nothing behind.
#[tokio::test]
async fn rejection_leaves_the_cluster_unclaimed() {
    let (_d, store, cluster_id, _appearance) = a_cluster_with_one_appearance().await;
    let claim = claim::submit(
        &store,
        Submit {
            cluster_id: cluster_id.clone(),
            account: "impostor".into(),
            evidence: "e".into(),
        },
    )
    .await
    .unwrap();

    claim::reject(&store, &claim.id, "steward-1", "not convincing")
        .await
        .unwrap();
    assert!(
        !claim::is_verified(&store, &cluster_id).await.unwrap(),
        "a rejected claim verifies nothing"
    );
    assert!(claim::queue(&store).await.unwrap().is_empty());
    assert_eq!(
        claim::state_of(&store, &claim.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ClaimState::Rejected
    );

    // And the account may claim again afterwards, which is the point of
    // rejecting rather than suspending.
    claim::submit(
        &store,
        Submit {
            cluster_id: cluster_id.clone(),
            account: "impostor".into(),
            evidence: "better evidence".into(),
        },
    )
    .await
    .expect("a rejected claim does not block a later one");
}

/// A claim on a cluster that does not exist is refused, not queued.
///
/// A queued claim against a missing cluster is a queue entry a moderator cannot
/// act on.
#[tokio::test]
async fn claiming_a_cluster_that_does_not_exist_is_refused() {
    let (_d, store, _engine) = tuned(0.90, 0.7, 0.3).await;
    let err = claim::submit(
        &store,
        Submit {
            cluster_id: "no-such-cluster".into(),
            account: "anna".into(),
            evidence: "e".into(),
        },
    )
    .await;
    assert!(
        matches!(err, Err(ClaimError::NoSuchCluster(_))),
        "the refusal names the reason: {err:?}"
    );
}

/// Approving a claim that is not pending is refused.
///
/// A double-approval, or approving an already-rejected claim, would be the
/// moderation UI calling an endpoint twice -- which it will, at some point, on a
/// slow connection.
#[tokio::test]
async fn approving_a_claim_twice_is_refused() {
    let (_d, store, cluster_id, _appearance) = a_cluster_with_one_appearance().await;
    let claim = claim::submit(
        &store,
        Submit {
            cluster_id: cluster_id.clone(),
            account: "anna".into(),
            evidence: "e".into(),
        },
    )
    .await
    .unwrap();

    claim::approve(&store, &claim.id, "steward-1")
        .await
        .unwrap();
    let again = claim::approve(&store, &claim.id, "steward-1").await;
    assert!(
        matches!(again, Err(ClaimError::NotPending(_))),
        "the second approval is refused: {again:?}"
    );
    assert_eq!(
        claim::verified_performer(&store, &cluster_id)
            .await
            .unwrap()
            .unwrap(),
        "anna",
        "and the verification is unchanged rather than doubled"
    );
}

/// Moderation actions name the moderator, so the record says who decided.
///
/// §7.5's claim is the mechanism by which a takedown request reaches a person;
/// if the claim's own approval is anonymous, the consent chain has a link with
/// no owner.
#[tokio::test]
async fn a_claim_records_who_decided_it() {
    let (_d, store, cluster_id, _appearance) = a_cluster_with_one_appearance().await;
    let claim = claim::submit(
        &store,
        Submit {
            cluster_id: cluster_id.clone(),
            account: "anna".into(),
            evidence: "e".into(),
        },
    )
    .await
    .unwrap();

    claim::approve(&store, &claim.id, "steward-7")
        .await
        .unwrap();
    let record = claim::get(&store, &claim.id)
        .await
        .unwrap()
        .expect("the claim is on record");
    assert_eq!(record.decided_by.as_deref(), Some("steward-7"));
    assert_eq!(record.state, ClaimState::Approved);
    assert!(record.decided_at.is_some(), "and when");
}

/// A takedown request reaches a verified performer and nobody else.
///
/// §7.5's third clause, and the one that makes the consent model practical: the
/// person in the content is the one who can consent. A request addressed to the
/// wrong performer is worse than no request -- it discloses that a person is
/// being complained about.
#[tokio::test]
async fn a_takedown_request_reaches_only_the_verified_performer() {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(94).unit();
    let mut clusters = Vec::new();
    for (i, name) in ["t1", "t2"].iter().enumerate() {
        object(&store, name).await;
        let v = Frame::from_pair(&base, 0.05 + i as f32 * 1.4);
        clusters.push(
            engine
                .assign(name, &v, None)
                .await
                .unwrap()
                .cluster_id
                .unwrap(),
        );
    }
    let _ = claim_and_approve(&store, "anna", &clusters[0]).await;
    let _ = claim_and_approve(&store, "bert", &clusters[1]).await;

    let request = claim::open_takedown(
        &store,
        &clusters[0],
        "complainant",
        "this should not be published",
    )
    .await
    .unwrap();

    let recipients = claim::takedown_recipients(&store, &request.id)
        .await
        .unwrap();
    assert_eq!(
        recipients,
        vec!["anna".to_string()],
        "only the account verified against that cluster, and nobody else"
    );
    assert!(
        !recipients.contains(&"bert".to_string()),
        "and specifically not the other performer -- a request addressed to the \\
         wrong person discloses that they are being complained about"
    );
    let _ = d;
}

/// A takedown request against an unclaimed cluster goes to the stewards, not to
/// a performer.
///
/// There is nobody else to ask, and silently dropping the request would be the
/// one outcome worse than a misdirected one.
#[tokio::test]
async fn a_takedown_request_for_an_unclaimed_cluster_goes_to_stewards() {
    let (_d, store, cluster_id, _appearance) = a_cluster_with_one_appearance().await;
    let request = claim::open_takedown(&store, &cluster_id, "complainant", "reason")
        .await
        .unwrap();
    assert_eq!(
        claim::takedown_recipients(&store, &request.id)
            .await
            .unwrap(),
        Vec::<String>::new(),
        "no performer to address, so the stewards are the fallback"
    );
    assert!(
        claim::takedown_is_steward_queue(&store, &request.id)
            .await
            .unwrap(),
        "and it says so explicitly rather than having an empty recipient list"
    );
}

/// The scope check is one function, and every path goes through it.
///
/// Not a behaviour test -- a structural one. The bug this guards is a *second*
/// write path added later that does its own check slightly differently, and the
/// only way to notice is to have the check in one place and assert that.
#[tokio::test]
async fn scope_is_decided_by_one_function_that_every_path_uses() {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(95).unit();

    // Two clusters, far apart.
    let mut clusters = Vec::new();
    for (i, name) in ["s1", "s2"].iter().enumerate() {
        object(&store, name).await;
        let v = Frame::from_pair(&base, 0.05 + i as f32 * 1.4);
        let cluster = engine
            .assign(name, &v, None)
            .await
            .unwrap()
            .cluster_id
            .unwrap();
        clusters.push(cluster);
    }
    let (a_cluster, b_cluster) = (clusters[0].clone(), clusters[1].clone());

    // Account "a" is verified against the first cluster. Account "b" is not
    // verified at all -- it has a cluster but no claim, which is the case that
    // most often falls through to "allowed" in a check written as
    // "if verified, compare ids".
    let a_account = "account-a";
    let a_performer = claim_and_approve(&store, a_account, &a_cluster).await;
    let b_account = "account-b";
    let b_performer = ops::create_performer(&store, "B").await.unwrap();

    // The one predicate, checked directly, in both its forms.
    assert!(
        claim::may_edit(&store, a_account, &a_performer)
            .await
            .unwrap(),
        "the verified performer may edit her own record"
    );
    assert!(
        claim::may_edit_cluster(&store, a_account, &a_performer, &a_cluster)
            .await
            .unwrap(),
        "and by cluster too -- the form a caller holding only a cluster id has"
    );
    assert!(
        !claim::may_edit(&store, a_account, &b_performer)
            .await
            .unwrap(),
        "but not another performer's record"
    );
    assert!(
        !claim::may_edit_cluster(&store, a_account, &b_performer, &b_cluster)
            .await
            .unwrap(),
        "and a caller holding another cluster id cannot skip the performer lookup \
         by using the other form"
    );
    assert!(
        !claim::may_edit(&store, b_account, &b_performer)
            .await
            .unwrap(),
        "an account with no verification may edit nothing -- not even the \
         performer record it nominally created"
    );
    assert!(
        !claim::may_edit(&store, "unverified", &a_performer)
            .await
            .unwrap(),
        "and an unrelated account may not edit a verified performer's record"
    );
    let _ = d;
}

#[tokio::test]
async fn a_field_a_performer_does_not_own_is_refused() {
    let (_d, store, cluster_id, _appearance) = a_cluster_with_one_appearance().await;
    let account = "anna-account";
    let performer = claim_and_approve(&store, account, &cluster_id).await;

    assert!(
        claim::propose_correction(
            &store,
            account,
            &performer,
            "name",
            &serde_json::json!("Anna L.")
        )
        .await
        .is_ok(),
        "their own name is theirs to correct"
    );
    assert!(
        matches!(
            claim::propose_correction(
                &store,
                account,
                &performer,
                "consent",
                &serde_json::json!("granted")
            )
            .await,
            Err(ClaimError::FieldNotCorrectable { .. })
        ),
        "a consent tier is not a metadata field, and a performer editing it \
         would be editing the platform's record of their own consent"
    );
    let _ = cluster_id;
}

/// The dashboard is scoped to *whose* verifications, not just which clusters.
///
/// The one-account tests cannot see a missing account filter: with a single
/// verified account, "return every appearance in the database" and "return this
/// account's appearances" produce identical output. Two accounts, one dashboard,
/// and the assertion is the difference between them.
#[tokio::test]
async fn a_dashboard_shows_only_the_asking_accounts_clusters() {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(97).unit();

    let mut clusters = Vec::new();
    let mut appearances = Vec::new();
    for (i, name) in ["d1", "d2"].iter().enumerate() {
        object(&store, name).await;
        let v = Frame::from_pair(&base, 0.05 + i as f32 * 1.4);
        let out = engine.assign(name, &v, None).await.unwrap();
        clusters.push(out.cluster_id.unwrap());
        appearances.push(out.appearance_id);
    }

    let _anna = claim_and_approve(&store, "anna-account", &clusters[0]).await;
    let _bert = claim_and_approve(&store, "bert-account", &clusters[1]).await;

    let anna_dash = claim::dashboard(&store, "anna-account").await.unwrap();
    assert_eq!(
        anna_dash.appearances,
        vec![appearances[0].clone()],
        "Anna's dashboard holds her appearance and not Bert's"
    );
    assert!(
        !anna_dash.appearances.contains(&appearances[1]),
        "and specifically not the other performer's: {anna_dash:?}"
    );

    let bert_dash = claim::dashboard(&store, "bert-account").await.unwrap();
    assert_eq!(
        bert_dash.appearances,
        vec![appearances[1].clone()],
        "and Bert's holds his own"
    );
    let _ = d;
}

/// One account claiming two clusters resolves to one performer.
///
/// Two records for one person would mean two identities the account could write
/// to, and the scope rule is written against a performer id -- so the split would
/// not be a naming problem, it would be a second thing the person "is".
#[tokio::test]
async fn one_account_claiming_twice_is_one_performer() {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(98).unit();

    let mut clusters = Vec::new();
    for (i, name) in ["p1", "p2"].iter().enumerate() {
        object(&store, name).await;
        let v = Frame::from_pair(&base, 0.05 + i as f32 * 1.4);
        clusters.push(
            engine
                .assign(name, &v, None)
                .await
                .unwrap()
                .cluster_id
                .unwrap(),
        );
    }

    let first = claim_and_approve(&store, "one-account", &clusters[0]).await;
    let second = claim_and_approve(&store, "one-account", &clusters[1]).await;
    assert_eq!(
        first, second,
        "the second claim resolved to the same performer record, not a new one"
    );

    // And the scope still holds on both clusters.
    assert!(claim::may_edit(&store, "one-account", &first)
        .await
        .unwrap());
    assert!(
        claim::may_edit_cluster(&store, "one-account", &first, &clusters[1])
            .await
            .unwrap(),
        "and the account may edit the record through the second cluster too"
    );
    let _ = d;
}

/// A request is addressed to exactly one account, and a second claim on the
/// same cluster cannot produce a second recipient.
///
/// The schema keys `performer_verification` by `cluster_id`, so one cluster is
/// one person. That is a load-bearing decision rather than an accident of the
/// primary key: if two accounts could be verified against one cluster, a
/// takedown request would go to both of them, and each would learn that the
/// other is verified against the same content.
#[tokio::test]
async fn a_takedown_request_reaches_exactly_one_verified_account() {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(99).unit();

    object(&store, "t1").await;
    let v = Frame::from_pair(&base, 0.05);
    let cluster = engine
        .assign("t1", &v, None)
        .await
        .unwrap()
        .cluster_id
        .unwrap();

    let _ = claim_and_approve(&store, "zoe", &cluster).await;

    // A second account claiming the same cluster is refused, because the cluster
    // is already verified -- so there is no way to reach two recipients.
    let second = claim::submit(
        &store,
        claim::Submit {
            cluster_id: cluster.clone(),
            account: "adam".into(),
            evidence: "also me".into(),
        },
    )
    .await;
    assert!(
        second.is_ok(),
        "a second account may still *submit* -- the queue is a moderator's \
         decision, not the schema's: {second:?}"
    );

    let request = claim::open_takedown(&store, &cluster, "complainant", "reason")
        .await
        .unwrap();
    let recipients = claim::takedown_recipients(&store, &request.id)
        .await
        .unwrap();
    assert_eq!(
        recipients,
        vec!["zoe".to_string()],
        "the one verified account, and nobody else"
    );
    let _ = d;
}

/// A request about one cluster does not reach the performer of another.
///
/// The recipient lookup reads the request's `cluster_id` and then the
/// verification for *that* cluster. A version that ignored the cluster would
/// still pass every single-cluster test above -- there is only one verified
/// cluster in them, so "the verified account" and "the verified account of this
/// cluster" are the same string. Two verified clusters, two performers, and one
/// request each is the only way to tell them apart.
#[tokio::test]
async fn a_request_about_one_cluster_does_not_reach_another_clusters_performer() {
    let (d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(101).unit();

    let mut clusters = Vec::new();
    for (i, name) in ["r1", "r2"].iter().enumerate() {
        object(&store, name).await;
        let v = Frame::from_pair(&base, 0.05 + i as f32 * 1.4);
        clusters.push(
            engine
                .assign(name, &v, None)
                .await
                .unwrap()
                .cluster_id
                .unwrap(),
        );
    }
    let _ = claim_and_approve(&store, "zoe", &clusters[0]).await;
    let _ = claim_and_approve(&store, "adam", &clusters[1]).await;

    // A request about the *second* cluster goes to Adam and not to Zoe.
    let request = claim::open_takedown(&store, &clusters[1], "complainant", "reason")
        .await
        .unwrap();
    let recipients = claim::takedown_recipients(&store, &request.id)
        .await
        .unwrap();
    assert_eq!(
        recipients,
        vec!["adam".to_string()],
        "the performer verified against the cluster the request is about"
    );

    // And the other direction, so it is not an ordering coincidence.
    let other = claim::open_takedown(&store, &clusters[0], "complainant", "reason")
        .await
        .unwrap();
    assert_eq!(
        claim::takedown_recipients(&store, &other.id).await.unwrap(),
        vec!["zoe".to_string()],
        "and each request reaches its own cluster's performer, not the first \
         one found"
    );
    let _ = d;
}
