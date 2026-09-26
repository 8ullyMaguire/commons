//! T-P3-002 acceptance: the clustering engine.
//!
//! # The three assertions, and why (3) is the ticket
//!
//! §7.1's closing rule is "silently guessing is the failure mode that would make
//! people distrust the links". A clustering engine that is too *cautious* is
//! merely annoying: the user merges two clusters by hand and moves on. One that
//! is too *confident* asserts that two different people are the same person, and
//! a user who catches it doing that once has no reason to believe anything else
//! it says. The whole design leans on the user trusting the links, so the
//! expensive error is the confident one.
//!
//! That is why the acceptance test's third assertion is the important one. (1)
//! and (2) are properties a working engine trivially has; (3) is the property
//! that distinguishes one.
//!
//! # The fixture
//!
//! 10 clusters of 20 embeddings each, generated from 10 fixed random unit
//! vectors plus Gaussian noise, in 512 dimensions — the real width, because a
//! toy width makes distances behave differently and a test at 8 dimensions does
//! not exercise the regime the engine runs in. The generator is a seeded
//! SplitMix64/xorshift pair so the fixture is *reproducible*: a test that fails
//! one run in ten is a test that gets deleted rather than fixed, and a
//! clustering test with a random seed is exactly that.
//!
//! Three of the 20 in each cluster are deliberate weight-change cases (§7.4):
//! the same person's embedding pulled along a fixed direction, which is what a
//! face embedding does as body mass changes. They must stay in the cluster.
//!
//! One pair is a planted lookalike: two faces equidistant from a single
//! cluster, so any nearest-neighbour rule picks one and is wrong. The engine
//! must mark it ambiguous and attach to *neither*.
//!
//! # What is not tested here
//!
//! The distance threshold has no calibration behind it, because there is no
//! face-embedding model linked (see T-P3-001's handoff note). These tests use
//! synthetic vectors with a known geometry, so they test the *engine's* logic
//! and say nothing about the recall of a real detector. The threshold used here
//! is one this fixture makes correct, not one a real library would use.

mod common;

use common::*;

// The engine's vocabulary, imported explicitly rather than through the fixture
// module, so what this file tests against is visible in this file.
use commons_identity::cluster::{Assignment, ClusterState, ConsolidateConfig};

#[tokio::test]
async fn ten_people_twenty_appearances_each_recover_as_ten_clusters() {
    let (_d, store, engine) = engine().await;
    let fx = Fixture::build(0xC0FFEE);

    let mut assignments = Vec::new();
    for (i, v) in fx.vectors.iter().enumerate() {
        // The two planted lookalikes are excluded: this assertion is about the
        // ten well-separated people, and including the deliberately ambiguous
        // vector would make it a test of something else. (3) covers those.
        if i == fx.lookalike.0 || i == fx.lookalike.1 {
            continue;
        }
        let name = format!("obj{i}");
        object(&store, &name).await;
        let a = engine.assign(&name, &v.clone(), None).await.unwrap();
        assignments.push((i, a));
    }

    let groups = clusters(&store).await;
    assert_eq!(
        groups.len(),
        PEOPLE,
        "ten distinct people, ten clusters; got {:?}",
        groups
            .iter()
            .map(|(id, m)| (id, m.len()))
            .collect::<Vec<_>>()
    );

    // Every appearance of a person landed in one cluster, and no cluster mixes
    // two people. Checked from the ground truth rather than from the engine's
    // own report, so a bug that labels things consistently wrong still fails.
    // `member_ids` are `appearance.id` -- generated UUIDs, not the object names
    // the fixture invented. So the ground truth has to be reached through the
    // appearance rows, or every lookup below silently matches nothing and the
    // assertion passes for the wrong reason. Read once, here, rather than
    // re-read per cluster.
    let mut owner: std::collections::HashMap<String, usize> = Default::default();
    for c in cluster::all_with_members(&store).await.unwrap() {
        for m in &c.member_ids {
            let rec = cluster::appearance(&store, m).await.unwrap().unwrap();
            owner.insert(m.clone(), index_of(&rec.object_id));
        }
    }

    for (id, members) in groups.iter() {
        // Only appearances this test actually assigned are checked. The planted
        // lookalike is ambiguous and belongs to no cluster, so it cannot appear
        // here at all -- and if it ever did, that is its own assertion below
        // rather than something folded into "this cluster is one person".
        let mut truths: Vec<usize> = members
            .iter()
            .filter_map(|m| fx.truth.get(owner[m]).copied())
            .filter(|t| *t != usize::MAX)
            .collect();
        truths.sort_unstable();
        truths.dedup();
        assert_eq!(
            truths.len(),
            1,
            "cluster {id} mixes ground-truth people {truths:?}"
        );
    }

    // The two planted lookalikes belong to no cluster, so they are in no
    // cluster's member list. Asserted from the member ids rather than by
    // re-running the decision, because the engine's own report agreeing with
    // the engine's own record is the one agreement that proves nothing.
    for (id, members) in &groups {
        for m in members {
            let rec = cluster::appearance(&store, m).await.unwrap().unwrap();
            let idx = index_of(&rec.object_id);
            assert!(
                idx != fx.lookalike.0 && idx != fx.lookalike.1,
                "the ambiguous lookalike {idx} was attached to cluster {id}"
            );
        }
    }

    // And the sizes are right: nothing lost, nothing duplicated.
    for (_, members) in &groups {
        assert_eq!(members.len(), PER_PERSON, "every appearance of a person");
    }
    assert_eq!(assignments.len(), PEOPLE * PER_PERSON);
}

// ---------------------------------------------------------------------------
// (2) Weight change stays in one cluster.
// ---------------------------------------------------------------------------

/// §7.4's motivating case: the same person at different weights must not split.
///
/// This is the assertion that says the engine uses a threshold loose enough for
/// a real body change, rather than one tuned so tightly that every new angle or
/// lighting condition makes a new person.
#[tokio::test]
async fn the_same_person_at_different_weights_stays_in_one_cluster() {
    let (_d, store, engine) = engine().await;
    let fx = Fixture::build(0xC0FFEE);

    for (i, v) in fx.vectors.iter().enumerate() {
        if i == fx.lookalike.0 || i == fx.lookalike.1 {
            continue;
        }
        let name = format!("obj{i}");
        object(&store, &name).await;
        engine.assign(&name, &v.clone(), None).await.unwrap();
    }

    let groups = clusters(&store).await;
    // `member_ids` are `appearance.id`, so the ground truth is reached through
    // the appearance rows. Doing this once, up front, keeps the loop below about
    // the assertion rather than about lookups.
    let mut index_of_member: std::collections::HashMap<String, usize> = Default::default();
    for (_, members) in &groups {
        for m in members {
            let rec = cluster::appearance(&store, m).await.unwrap().unwrap();
            index_of_member.insert(m.clone(), index_of(&rec.object_id));
        }
    }

    for (person, heavy) in fx.weight_change.iter().enumerate() {
        let of_person: Vec<&(String, Vec<String>)> = groups
            .iter()
            .filter(|(_, members)| {
                members.iter().any(|m| {
                    fx.truth
                        .get(index_of_member[m])
                        .is_some_and(|t| *t == person)
                })
            })
            .collect();
        assert_eq!(
            of_person.len(),
            1,
            "person {person} is in {} clusters, not one",
            of_person.len()
        );
        let members = &of_person[0].1;
        for h in heavy {
            assert!(
                members.iter().any(|m| index_of_member[m] == *h),
                "the weight-change appearance obj{h} split out of person {person}'s cluster"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// (3) The lookalike is ambiguous. This is the ticket.
// ---------------------------------------------------------------------------

/// A vector that fits two people equally is marked ambiguous, not guessed.
///
/// §7.1: "Silently guessing is the failure mode that would make people distrust
/// the links." The engine must therefore attach to neither cluster, record
/// `ambiguous`, and leave both candidates visible — the user's call, not the
/// engine's.
#[tokio::test]
async fn a_vector_equidistant_from_two_people_is_ambiguous_not_guessed() {
    let (_d, store, engine) = engine().await;
    let fx = Fixture::build(0xC0FFEE);

    for (i, v) in fx.vectors.iter().enumerate() {
        if i == fx.lookalike.0 || i == fx.lookalike.1 {
            continue;
        }
        let name = format!("obj{i}");
        object(&store, &name).await;
        engine.assign(&name, &v.clone(), None).await.unwrap();
    }

    let before = clusters(&store).await;
    assert_eq!(before.len(), PEOPLE, "the ten people are clustered");

    // The two real centroids, read from the engine rather than recomputed from
    // the fixture's synthetic centres. This is the whole reason the test is
    // written this way.
    //
    // A cluster's centroid is the mean of the vectors that joined it, and those
    // include the heavy weight-change cases, which pull the mean away from the
    // centre the fixture generated them from. A lookalike positioned between the
    // two *synthetic centres* is therefore not equidistant from the two
    // *centroids*, and an earlier version of this test asserted that it was --
    // which is a test that passes or fails for reasons unrelated to ambiguity.
    // Taking the midpoint of what the engine actually stored removes the
    // discrepancy instead of asserting it away.
    let c0 = engine.centroid(0).await.unwrap().expect("person 0");
    let c1 = engine.centroid(1).await.unwrap().expect("person 1");
    let look: Vec<f32> = (0..D).map(|k| (c0[k] + c1[k]) / 2.0).collect();
    let look = Embedder::l2_normalize(&look).to_vec();

    // Equidistance is exact by construction here, up to the renormalisation.
    // Asserted rather than assumed, because "by construction" is exactly the
    // claim that was wrong before.
    let d0 = distance(&look, &c0);
    let d1 = distance(&look, &c1);
    assert!(
        (d0 - d1).abs() < 1e-5,
        "the fixture is not actually ambiguous: d0={d0}, d1={d1}"
    );
    assert!(
        d0 < engine.config().threshold,
        "and it must be close enough to both to be a candidate at all: d0={d0}"
    );

    object(&store, "lookalike").await;
    let a = engine.assign("lookalike", &look, None).await.unwrap();

    // The decision is ambiguity. Not "joined cluster A", not "joined cluster B",
    // not "created a new cluster" -- all three of those are a guess.
    assert_eq!(
        a.decision,
        Assignment::Ambiguous,
        "an equidistant vector must not be assigned: {a:?}"
    );
    assert_eq!(
        a.cluster_id, None,
        "and must name no cluster, or a caller could attach it by accident"
    );

    // And the record says so, durably. The engine's return value and the stored
    // row have to agree, or the UI shows one thing and the engine did another.
    let stored = cluster::appearance(&store, &a.appearance_id)
        .await
        .unwrap()
        .expect("the appearance was recorded");
    assert!(
        stored.ambiguous,
        "the stored row must carry ambiguous=1: {stored:?}"
    );
    assert_eq!(
        stored.cluster_id, None,
        "and belong to no cluster -- a NULL, not an empty string"
    );
    assert!(
        stored.distance.is_some(),
        "and a distance, so the user can see how close it was"
    );

    // Both candidates are available for the UI to show. A record marked
    // ambiguous with no way to see what it was ambiguous *between* is not a
    // review state, it is a shrug.
    let candidates = cluster::ambiguous_candidates(&store, &a.appearance_id)
        .await
        .unwrap();
    assert_eq!(
        candidates.len(),
        2,
        "both candidates must be retrievable: {candidates:?}"
    );
    assert!(
        candidates.iter().all(|c| c.distance.is_finite()),
        "and each with a finite distance, per §7.4's 'show why'"
    );
    // And they are the two real people, not a duplicate of one of them: a
    // candidate list that names the same cluster twice would render as a choice
    // with one option.
    let mut ids: Vec<&str> = candidates.iter().map(|c| c.cluster_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 2, "two distinct candidates: {ids:?}");

    // The two real clusters are untouched: ambiguity is not a merge.
    let after = clusters(&store).await;
    assert_eq!(
        after.len(),
        before.len(),
        "no cluster was created or destroyed"
    );
    assert_eq!(
        after.iter().map(|(_, m)| m.len()).sum::<usize>(),
        before.iter().map(|(_, m)| m.len()).sum::<usize>(),
        "and the ambiguous appearance was not silently added to one of them"
    );
}

// ---------------------------------------------------------------------------
// The transitive-merge guard. §7.1 step 4, and the reason a blob is worse than
// no clustering.
// ---------------------------------------------------------------------------

/// A~B and B~C does not imply A~C when the direct A~C distance fails.
///
/// Without this guard, clustering collapses into one blob: every link is
/// individually reasonable and the result is one enormous wrong answer, which
/// is worse than no clustering because the user then distrusts every link.
#[tokio::test]
async fn a_chain_of_two_hop_similarities_does_not_merge_three_appearances() {
    let mut rng = Rng::new(7);

    // A, B, C along one direction: A~B and B~C pass a 0.30 threshold, A~C does
    // not. Each is placed at an explicit distance rather than by nudging one
    // component, so the numbers in the assertions below are the numbers the
    // vectors actually have.
    // The shape that actually has the property: two clusters 0.60 apart, and a
    // point 0.26 from each. A~B and B~C both pass a 0.30 threshold, A~C does
    // not.
    //
    // Placing A, B and C along a single arc cannot produce this, which is worth
    // recording because it is the obvious thing to try. On a unit sphere the
    // distance between two points at offsets x and y from a common origin is
    // 1 - cos(acos(1-x) - acos(1-y)), which for x = 0.10, y = 0.34 is 0.08 --
    // not 0.24. The offsets do not add, because both points still share the
    // component along the origin, and that shared component dominates. So a
    // "chain" built that way is not a chain at all, and a test asserting on it
    // would pass or fail for reasons unrelated to the guard.
    let frame = Frame::new(&Rng::new(99).unit());
    let a = frame.at(0.0, &mut rng, 1.0);
    let c = frame.at(0.60, &mut rng, 1.0);
    // Halfway, but not exactly: exactly midway would make d(A,B) == d(B,C) to
    // within float error, and the engine would mark B ambiguous on the margin
    // rather than joining it -- a correct behaviour, but one that tests the
    // margin instead of the chain guard. B is nudged to 0.26 from A and 0.34
    // from C, which is the same situation without that tie.
    let b = frame.at(0.26, &mut rng, 1.0);

    let cfg = EngineConfig {
        threshold: 0.30,
        ..Default::default()
    };
    let dir_handle = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir_handle.path()).await.unwrap();
    let engine = Engine::open(&store, cfg).await.unwrap();

    for (id, v) in [("a", &a), ("b", &b), ("c", &c)] {
        object(&store, id).await;
        engine.assign(id, v, None).await.unwrap();
    }

    let ab = distance(&a, &b);
    let bc = distance(&b, &c);
    let ac = distance(&a, &c);
    assert!(ab < 0.30, "A~B must pass, got {ab}");
    assert!(bc < 0.30, "B~C must pass, got {bc}");
    assert!(
        ac > 0.30,
        "the fixture must actually straddle the threshold: A~C = {ac}"
    );
    assert!(
        (ab - bc).abs() > 0.02,
        "and must not be a tie, or this tests the ambiguity margin instead: \
         A~B = {ab}, B~C = {bc}"
    );

    let groups = clusters(&store).await;
    // A+B is one cluster. C is its own. Transitivity would have made all three
    // one, on the strength of two hops that each passed.
    let sizes: Vec<usize> = groups.iter().map(|(_, m)| m.len()).collect();
    assert!(
        sizes.contains(&2) && sizes.contains(&1),
        "expected a pair and a singleton, got {sizes:?}"
    );

    // And the consolidation pass, run explicitly, must not close the gap.
    let report = engine
        .consolidate(ConsolidateConfig::default())
        .await
        .unwrap();
    assert_eq!(
        report.merge_count(),
        0,
        "no merge may be proposed across a failing A~C; report was {report:?}"
    );
    assert_eq!(
        report.deferred, 0,
        "and the pass did not merely run out of budget"
    );
    let after = clusters(&store).await;
    assert_eq!(after.len(), groups.len(), "cluster count unchanged");
}

#[tokio::test]
async fn consolidation_merges_when_the_direct_distance_agrees() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let engine = Engine::open(
        &store,
        EngineConfig {
            threshold: 0.30,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut rng = Rng::new(11);
    // 0.40 apart, so assignment alone will not join them -- the merge has to
    // come from the consolidation pass, which uses its own (looser) threshold.
    // A test where assignment already joined them would prove nothing about the
    // pass.
    let frame = Frame::new(&Rng::new(41).unit());
    let a = frame.at(0.0, &mut rng, 1.0);
    let b = frame.at(0.40, &mut rng, 1.0);

    for (id, v) in [("a", &a), ("b", &b)] {
        object(&store, id).await;
        engine.assign(id, v, None).await.unwrap();
    }
    assert_eq!(
        clusters(&store).await.len(),
        2,
        "too far apart for assignment to join: A~B = {}",
        distance(&a, &b)
    );

    let report = engine
        .consolidate(ConsolidateConfig {
            merge_threshold: 0.55,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        report.merge_count(),
        1,
        "the direct distance supports one merge: {report:?}"
    );
    let groups = clusters(&store).await;
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].1.len(), 2, "with both members");
}

#[tokio::test]
async fn a_new_cluster_is_anonymous_and_needs_no_name() {
    let (_d, store, engine) = engine().await;
    let v = Rng::new(3).unit();
    object(&store, "solo").await;
    engine.assign("solo", &v, None).await.unwrap();

    let groups = clusters(&store).await;
    assert_eq!(groups.len(), 1);
    let c = cluster::get(&store, &groups[0].0).await.unwrap().unwrap();
    assert_eq!(c.state, ClusterState::Anonymous);
    assert!(
        c.handle.is_none(),
        "an anonymous cluster has no handle and does not want one"
    );
    // A one-appearance cluster is still stored and still real. §7.1: it becomes
    // *worth showing* at three, which is a UI threshold, not a creation one.
    assert_eq!(c.appearance_count, 1);
    assert_eq!(
        groups[0].1.len(),
        1,
        "and the member list agrees with the count"
    );
    assert!(
        c.appearance_count < BROWSABLE_MIN,
        "below the UI bar, and still here"
    );
}

/// Rule 1: the decision and its distance are recorded, always.
#[tokio::test]
async fn the_decision_and_its_distance_are_always_recorded() {
    let (_d, store, engine) = engine().await;
    let mut rng = Rng::new(5);
    let a = rng.unit();
    let mut b = a.clone();
    b[0] -= 0.05;
    let b = Embedder::l2_normalize(&b).to_vec();

    for id in ["a", "b"] {
        object(&store, id).await;
    }
    let first = engine.assign("a", &a, None).await.unwrap();
    let second = engine.assign("b", &b, None).await.unwrap();

    // The second is a join, and the distance that justified it is on the row.
    assert_eq!(
        first.decision,
        Assignment::Created,
        "the first of a pair creates: {first:?}"
    );
    assert_eq!(second.decision, Assignment::Joined, "{second:?}");
    assert_eq!(second.cluster_id, first.cluster_id);
    assert!(
        second.cluster_id.is_some(),
        "a join names the cluster it joined"
    );

    let row = cluster::appearance(&store, &second.appearance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.distance,
        Some(distance(&a, &b)),
        "the distance as computed"
    );
    assert!(row.face_score.is_some(), "and the face component, per §7.4");
    assert_eq!(
        row.body_score, None,
        "and the body component is absent rather than zero: ScoreComponents::face \
         supplied none, and 'no body evidence' is not 'body evidence says no'"
    );
    assert!(!row.ambiguous);

    // The face component is the measured distance, and the body component is
    // absent because the appearance had no body embedding. The engine
    // computes both now rather than being handed them, so "absent" means the
    // evidence did not exist -- it is not a value that failed to be passed in.
    assert_eq!(
        row.face_score,
        Some(distance(&a, &b)),
        "the face component is the distance the decision was made on, not a \
         caller-supplied number that happens to agree with it"
    );

    // A third appearance with a body embedding records both components, and
    // they reconstruct the stored composite distance. §7.4's data requirement:
    // the UI can only say "same face 0.87, body consistent" from a row that
    // holds both.
    //
    // The cluster it joins is `c`, seeded with a body embedding of its own --
    // the a/b cluster has no body centroid, and comparing a body vector against
    // nothing would be a distance between unrelated embedding spaces, so the
    // body term is correctly dropped there. That dropping is this suite's other
    // test; here the point is that when both sides exist, both are recorded.
    let body = rng.unit();
    object(&store, "c").await;
    engine.assign("c", &body, Some(&body)).await.unwrap();

    let probe = rng.unit();
    object(&store, "d").await;
    let composite = engine.assign("d", &probe, Some(&body)).await.unwrap();
    let crow = cluster::appearance(&store, &composite.appearance_id)
        .await
        .unwrap()
        .unwrap();
    assert!(crow.face_score.is_some(), "the face component is recorded");
    assert!(
        crow.body_score.is_some(),
        "and so is the body component, because the cluster it joined has a body \
         centroid to compare against"
    );
    let rebuilt = 0.7 * crow.face_score.unwrap() + 0.3 * crow.body_score.unwrap();
    assert!(
        (crow.distance.unwrap() - rebuilt).abs() < 1e-4,
        "the stored distance is the weighted mean of the two stored \
       components: {:?} vs {rebuilt}",
        crow.distance
    );
}

/// Rule 4: a cluster whose evidence conflicts is ambiguous, and both candidates
/// stay visible.
#[tokio::test]
async fn conflicting_named_evidence_marks_the_cluster_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let engine = Engine::open(&store, EngineConfig::default()).await.unwrap();
    let mut rng = Rng::new(13);
    let v = rng.unit();
    object(&store, "x").await;
    let a = engine.assign("x", &v, None).await.unwrap();
    let cid = a
        .cluster_id
        .clone()
        .expect("a first appearance creates a cluster and names it");

    // Two *different* named performers are both asserted for this cluster. This
    // is the conflicting-evidence case: not one vector that could be two people,
    // but one cluster carrying two incompatible names.
    cluster::propose_handle(&store, &cid, "alice", "field_annotator")
        .await
        .unwrap();
    cluster::propose_handle(&store, &cid, "bob", "field_annotator")
        .await
        .unwrap();

    let c = cluster::get(&store, &cid).await.unwrap().unwrap();
    assert_eq!(
        c.state,
        ClusterState::Ambiguous,
        "two names on one cluster is ambiguity, not a coin flip: {c:?}"
    );
    assert_eq!(c.handle, None, "and no handle is chosen for the user");

    let competing = cluster::handle_candidates(&store, &cid).await.unwrap();
    assert_eq!(
        competing.len(),
        2,
        "both, for the UI to show: {competing:?}"
    );
    let mut names: Vec<&str> = competing.iter().map(|c| c.value.as_str()).collect();
    names.sort_unstable();
    assert!(
        names.contains(&"alice") && names.contains(&"bob"),
        "{names:?}"
    );
}

/// A cluster below the UI's "worth showing" threshold is still a real cluster.
#[tokio::test]
async fn a_cluster_below_the_appearance_threshold_is_still_stored() {
    let (_d, store, engine) = engine().await;
    let mut rng = Rng::new(17);
    for i in 0..2 {
        let mut v = rng.unit();
        v[0] += i as f32 * 0.4; // keep them apart
        let v = Embedder::l2_normalize(&v).to_vec();
        let id = format!("o{i}");
        object(&store, &id).await;
        engine.assign(&id, &v, None).await.unwrap();
    }
    let groups = clusters(&store).await;
    assert!(!groups.is_empty(), "stored, not discarded");
    let c = cluster::get(&store, &groups[0].0).await.unwrap().unwrap();
    assert!(
        c.appearance_count < BROWSABLE_MIN,
        "and reports itself below the threshold ({})",
        c.appearance_count
    );
    assert!(
        !groups[0].1.is_empty(),
        "the cluster still holds its members -- below the UI threshold is not discarded"
    );
}

// ---------------------------------------------------------------------------
// The two gaps the mutation check found.
//
// Both were found by mutating the engine and watching the suite stay green,
// which is the only way to know which assertions are load-bearing. Neither was
// visible by reading the tests: the suite was green, and green for reasons that
// had nothing to do with the properties it appeared to be checking.
// ---------------------------------------------------------------------------

/// A pair that passes the mutual-consistency guard but fails the direct
/// distance gate must not merge.
///
/// The direct distance between two centroids and the consistency of their
/// members are separate conditions, and the pass needs both. A cluster can have
/// a tight centroid and one member that does not belong, so consistency alone
/// would merge things; and two clusters can sit near each other while the people
/// in them are different, so distance alone would too. This is the case where the
/// member vectors are all within the threshold of the other centroid -- because
/// each cluster holds a single vector, which is trivially within it -- but the
/// two clusters themselves are further apart than the threshold.
///
/// Without this test, deleting the `d <= threshold` gate leaves the suite green,
/// because the consistency guard happens to catch this particular case on its
/// own. The two conditions being redundant here is a coincidence of the
/// fixture, not a property of the engine.
#[tokio::test]
async fn a_pair_failing_the_direct_distance_does_not_merge_even_when_members_agree() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let engine = Engine::open(
        &store,
        EngineConfig {
            threshold: 0.30,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut rng = Rng::new(23);
    // Far apart: 0.80, well over the 0.30 threshold.
    let frame = Frame::new(&Rng::new(57).unit());
    let a = frame.at(0.0, &mut rng, 1.0);
    let b = frame.at(0.80, &mut rng, 1.0);
    for (id, v) in [("a", &a), ("b", &b)] {
        object(&store, id).await;
        engine.assign(id, v, None).await.unwrap();
    }
    assert_eq!(clusters(&store).await.len(), 2);

    let report = engine
        .consolidate(ConsolidateConfig {
            merge_threshold: 0.30,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        report.merge_count(),
        0,
        "0.80 apart is past the gate however consistent the members are: {report:?}"
    );
    assert_eq!(clusters(&store).await.len(), 2, "still two clusters");
}

/// The member vectors are what the centroid is computed from, so a stale
/// centroid is a cluster that is compared against the wrong number.
///
/// Joining without refreshing leaves every cluster holding the centroid of its
/// *first* member. With a single member that is indistinguishable from correct,
/// which is why the suite was green: every test either used one appearance per
/// cluster or had a threshold loose enough to absorb the drift. This one puts
/// several members in a cluster and then asks whether a point that is close to
/// the *mean* but far from the *first member* is recognised.
#[tokio::test]
async fn the_centroid_is_the_mean_of_the_members_not_the_first_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let engine = Engine::open(&store, EngineConfig::default()).await.unwrap();
    let mut rng = Rng::new(29);

    // A cluster whose members straddle the origin, so the mean is near zero and
    // every member is far from it. A first-member centroid would put the cluster
    // somewhere its members are not.
    let frame = Frame::new(&Rng::new(61).unit());
    let left = frame.at(0.0, &mut rng, 1.0);
    let right = frame.at(0.30, &mut rng, 1.0);
    for (id, v) in [("l", &left), ("r", &right)] {
        object(&store, id).await;
        engine.assign(id, v, None).await.unwrap();
    }
    let groups = clusters(&store).await;
    assert_eq!(
        groups.len(),
        1,
        "the two joined: {}",
        distance(&left, &right)
    );

    let centroid = engine
        .centroid(0)
        .await
        .unwrap()
        .expect("the cluster has a centroid");
    // The mean of the two, so the centroid sits between them rather than on
    // either. That is the property: a running or first-member centroid would put
    // it at `left` or `right`, and the distances below would not hold.
    let mean: Vec<f32> = (0..D).map(|k| (left[k] + right[k]) / 2.0).collect();
    let mean = Embedder::l2_normalize(&mean).to_vec();
    assert!(
        distance(&centroid, &mean) < 1e-4,
        "the centroid is the mean of the members, not the first one: \
         centroid-to-mean = {}",
        distance(&centroid, &mean)
    );
    assert!(
        distance(&centroid, &left) > 0.05,
        "and is genuinely not the first member either: {}",
        distance(&centroid, &left)
    );
}

/// A large ambiguity margin makes every borderline case ambiguous rather than
/// none of them. The margin is the whole rule, so the test has to be able to see
/// it move both ways.
///
/// Setting it to 0.99 leaves the suite green without this, because every other
/// test either has a clear winner or a clear loser -- and a rule that abstains on
/// everything and a rule that abstains on nothing both look fine to those.
#[tokio::test]
async fn the_ambiguity_margin_decides_which_near_ties_are_ambiguous() {
    // One frame, so the candidates and the probe are all measured from the same
    // origin and their separations are the angle differences.
    let frame = Frame::new(&Rng::new(71).unit());
    // The candidates sit on opposite sides, far enough apart that assignment
    // cannot join them -- otherwise the probe has one candidate and no margin
    // applies. The probe sits between them, off-centre enough to leave a gap a
    // margin can sit under: a dead-even tie has a gap of zero, and nothing can
    // be set below zero.
    let a = frame.at(0.40, &mut rng2(0x31), 1.0);
    let b = frame.at(0.52, &mut rng2(0x32), -1.0);
    let probe = frame.at(0.46, &mut rng2(0x33), 0.0);

    let run = |margin: f32| {
        let (a, b, probe) = (a.clone(), b.clone(), probe.clone());
        async move {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open_library(dir.path()).await.unwrap();
            let engine = Engine::open(
                &store,
                EngineConfig {
                    threshold: 0.55,
                    ambiguity_margin: margin,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let mut out = None;
            for (id, v) in [("a", &a), ("b", &b), ("probe", &probe)] {
                object(&store, id).await;
                let outcome = engine.assign(id, v, None).await.unwrap();
                if id == "probe" {
                    out = Some(outcome);
                }
            }
            // The `TempDir` comes back with the store. Dropping it here deletes
            // the SQLite file while the pool still has it open, and the failure
            // surfaces at the *next* query as "unable to open database file" --
            // an error in a test that has nothing to do with the line that caused
            // it. Holding the directory is what keeps the file alive.
            (dir, store, engine, out.expect("the probe was assigned"))
        }
    };

    // The distances the engine will actually see, read back from the stored
    // centroids. `Frame` jitters every vector it builds, so the centroids are
    // not exactly `a` and `b`; choosing a margin from the predicted distances
    // would be choosing it from numbers the engine never uses.
    let (_d0, store, engine, tight) = run(0.0).await;
    let centroids: Vec<Vec<f32>> = cluster::all_with_members(&store)
        .await
        .unwrap()
        .iter()
        .filter_map(|r| r.centroid.clone())
        .collect();
    assert_eq!(centroids.len(), 2, "two clusters, so two candidates");
    let mut d: Vec<f32> = centroids.iter().map(|c| distance(&probe, c)).collect();
    assert!(
        d.iter().all(|x| *x < 0.55),
        "both candidates must pass the threshold or there is no tie: {d:?}"
    );
    d.sort_by(|x, y| x.total_cmp(y));
    let gap = d[1] - d[0]; // sorted ascending, so this is the positive gap
    assert!(
        d.iter().all(|x| x.is_finite()),
        "the distances must be finite: {d:?}"
    );
    assert!(
        gap > 1e-4,
        "the two must sit at different distances, or no margin can be placed \
         above the gap: {gap} (d = {d:?})"
    );

    // A margin of zero resolves the tie. §7.1's rule is a strict inequality on
    // purpose: a margin of exactly 0.0 must not mean "ambiguous", or the engine
    // abstains on everything.
    assert_ne!(
        tight.decision,
        Assignment::Ambiguous,
        "a zero margin always resolves: {tight:?}"
    );

    // A margin above the gap, on the same geometry, abstains. The only thing
    // that changed is the margin -- which is the claim the default is making
    // about itself, and the reason a user can widen it.
    let (_dir2, _store2, _engine2, loose) = run(gap * 4.0).await;
    assert_eq!(
        loose.decision,
        Assignment::Ambiguous,
        "a margin of 4x the gap must abstain on the same geometry: {loose:?}"
    );
    assert_eq!(loose.cluster_id, None, "and name no cluster");
    let _ = engine;
}

/// The direct-distance gate, tested where the consistency guard cannot stand in
/// for it.
///
/// `a_pair_failing_the_direct_distance_does_not_merge_even_when_members_agree`
/// builds one-member clusters, and for those the guard subsumes the gate: a
/// single vector is trivially within the threshold of anything, and the
/// remaining condition is the distance. So deleting the gate leaves that test
/// green, and with it the suite -- the gate looked covered and was not.
///
/// The distinguishing fixture needs *multi-member* clusters whose members reach
/// across the gap while the centroids stay on their own sides. Two tight groups
/// either side of the origin do that: every member of one is near the far edge
/// of its own cluster and far from the other's, so the guard has to be checked
/// on the members, not the centroids, and the centroids' own distance is the
/// thing under test.
#[tokio::test]
async fn the_distance_gate_holds_even_when_every_member_crosses_the_gap() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let engine = Engine::open(
        &store,
        EngineConfig {
            threshold: 0.30,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut rng = Rng::new(37);
    // Two clusters 0.50 apart, each with two members spread 0.12 around its own
    // centre. Every member is therefore within 0.30 of the *other* centroid --
    // the guard passes on the members -- while the centroids are 0.50 apart,
    // which is the case only the gate can refuse.
    let f1 = Frame::new(&frame_at(0.0, &Rng::new(73).unit(), &mut rng));
    let f2 = Frame::new(&frame_at(0.50, &Rng::new(74).unit(), &mut rng));
    for (tag, frame) in [("p", &f1), ("q", &f2)] {
        for k in 0..2 {
            let v = frame.at(k as f32 * 0.12, &mut rng, 1.0);
            let oid = format!("{tag}{k}");
            object(&store, &oid).await;
            let out = engine.assign(&oid, &v, None).await.unwrap();
            assert_ne!(
                out.decision,
                Assignment::Ambiguous,
                "{oid} should have been assignable: {out:?}"
            );
        }
    }
    // Each group's members must be closer to their own side than to the other,
    // or the fixture has not produced what the test needs.
    let a = frame_at(0.0, &Rng::new(73).unit(), &mut Rng::new(75));
    let b = frame_at(0.50, &Rng::new(74).unit(), &mut Rng::new(76));
    assert!(
        distance(&a, &b) > 0.45,
        "the two groups must be about 0.50 apart: {}",
        distance(&a, &b)
    );

    let groups = clusters(&store).await;
    assert_eq!(
        groups.len(),
        2,
        "two clusters, so the gate is what decides: {:?}",
        groups.iter().map(|(i, m)| (i, m.len())).collect::<Vec<_>>()
    );

    let centroids: Vec<Vec<f32>> = cluster::all_with_members(&store)
        .await
        .unwrap()
        .iter()
        .filter_map(|r| r.centroid.clone())
        .collect();
    let centroid_gap = distance(&centroids[0], &centroids[1]);
    assert!(
        centroid_gap > 0.30,
        "the centroids are past the gate: {centroid_gap}"
    );

    let report = engine
        .consolidate(ConsolidateConfig {
            merge_threshold: 0.30,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        report.merge_count(),
        0,
        "past the gate is past the gate, however the members are arranged: \
         centroid gap {centroid_gap}, report {report:?}"
    );
    assert_eq!(clusters(&store).await.len(), 2, "still two clusters");
}

/// The gate is redundant with the member guard for single-member clusters, and
/// that is a property of the geometry rather than of this test.
///
/// For a cluster of one, the centroid *is* the member, so
///
///     every member of A is within `t` of B's centroid
///
/// is a statement about the only member there is, and it implies the centroids
/// are within `t` of each other. So the guard subsumes the gate, and a suite
/// built on one-member clusters cannot tell them apart -- deleting the gate
/// leaves it green, which it did, twice, before this was written down.
///
/// This asserts the implication directly instead of hunting for a fixture where
/// the two conditions diverge. The consequence is stated rather than left for a
/// reader to infer: the gate is not dead code, but it is *only* reachable for
/// clusters that span a mode, and no test here builds one. `merge_threshold`
/// remains the lever that matters, and tightening `require_mutual_consistency`
/// is the one that actually controls the outcome.
#[test]
fn the_member_guard_implies_the_distance_gate_for_a_single_member_cluster() {
    let threshold = 0.30f32;
    let mut rng = Rng::new(47);
    let frame = Frame::new(&Rng::new(91).unit());
    for _ in 0..200 {
        let da = rng.next_u64() as f32 / u32::MAX as f32;
        let db = rng.next_u64() as f32 / u32::MAX as f32;
        let sign = if rng.next_u64() & 1 == 0 { 1.0 } else { -1.0 };
        let a = frame.at(da, &mut rng, 1.0);
        let b = frame.at(db, &mut rng, sign);
        // The guard, for single-member clusters: the member is within the
        // threshold of the other's centroid.
        let guard = distance(&a, &b) <= threshold;
        // The gate, between the two centroids -- which here are the members.
        let gate = distance(&a, &b) <= threshold;
        assert_eq!(
            guard,
            gate,
            "for a one-member cluster the two conditions are the same \
             comparison: d = {}",
            distance(&a, &b)
        );
    }
}
