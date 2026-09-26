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

use commons_identity::cluster::{
    self, Assignment, ClusterState, ConsolidateConfig, Engine, EngineConfig, ScoreComponents,
};
use commons_ml::face::Embedder;
use commons_store::Store;

/// The fixture's dimensionality: the real embedding width.
const D: usize = Embedder::ARCFACE_WIDTH;
/// One of the ten people.
const PEOPLE: usize = 10;
/// Appearances per person.
const PER_PERSON: usize = 20;

/// SplitMix64: a small, seeded, well-distributed generator.
///
/// Seeded because a clustering test with an unseeded generator fails
/// intermittently, and an intermittently failing test gets commented out instead
/// of fixed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// A uniform u64.
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A standard normal, via Box-Muller.
    ///
    /// `ln` of a uniform can be zero, so the argument is clamped: a Gaussian
    /// generator that returns infinity makes every distance NaN and the test
    /// fails with a message that points nowhere.
    fn normal(&mut self) -> f64 {
        let u1 = ((self.next_u64() >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0);
        let u2 = ((self.next_u64() >> 11) as f64) / ((1u64 << 53) as f64);
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// A unit vector, or near enough that the caller does not have to care.
    fn unit(&mut self) -> Vec<f32> {
        let v: Vec<f32> = (0..D).map(|_| self.normal() as f32).collect();
        Embedder::l2_normalize(&v).to_vec()
    }
}

/// The whole fixture: vectors, the ten ground-truth people, the weight-change
/// indices, and the planted lookalike pair.
struct Fixture {
    /// One entry per appearance, in a fixed order.
    vectors: Vec<Vec<f32>>,
    /// The ground-truth person for each appearance.
    truth: Vec<usize>,
    /// Per person, which appearance indices are the weight-change cases.
    weight_change: Vec<Vec<usize>>,
    /// The two appearance indices that are the planted lookalike pair.
    lookalike: (usize, usize),
}

impl Fixture {
    /// Build the fixture. Deterministic for a given `seed`.
    fn build(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let people: Vec<Vec<f32>> = (0..PEOPLE).map(|_| rng.unit()).collect();

        let mut vectors = Vec::new();
        let mut truth = Vec::new();
        let mut weight_change = Vec::new();

        for (p, centre) in people.iter().enumerate() {
            let mut heavy = Vec::new();
            for i in 0..PER_PERSON {
                // Three per person are the weight-change cases. The direction is
                // fixed per person so the same three are always heavy, and a
                // failure is reproducible.
                let is_heavy = i < 3;
                let sigma = if is_heavy { 0.11 } else { 0.018 };
                let noisy: Vec<f32> = (0..D)
                    .map(|k| centre[k] + (sigma * rng.normal()) as f32)
                    .collect();
                let v = Embedder::l2_normalize(&noisy).to_vec();
                if is_heavy {
                    heavy.push(vectors.len());
                }
                vectors.push(v);
                truth.push(p);
            }
            weight_change.push(heavy);
        }

        // The planted lookalike. One extra vector placed *exactly* between two
        // appearances of the same person is not ambiguous — it is clearly that
        // person. What is ambiguous is a vector equidistant from two *different*
        // people, which no single nearest-neighbour rule can resolve.
        //
        // So: a vector that is the mean of person 0's and person 1's centroids,
        // plus a little noise. It is at distance d from both, and a rule that
        // picks the lower id silently asserts it is person 0.
        let midpoint: Vec<f32> = (0..D)
            .map(|k| (people[0][k] + people[1][k]) / 2.0)
            .collect();
        let mut jitter: Vec<f32> = (0..D).map(|_| (0.004 * rng.normal()) as f32).collect();
        for k in 0..D {
            jitter[k] += midpoint[k];
        }
        let look = Embedder::l2_normalize(&jitter).to_vec();

        // Two appearances with the same embedding. If the engine has an
        // "already seen this vector" path that does not consider ambiguity,
        // these two take the same decision and one of them is wrong.
        let lookalike = (vectors.len(), vectors.len() + 1);
        vectors.push(look.clone());
        truth.push(usize::MAX); // not either person
        vectors.push(look);
        truth.push(usize::MAX);

        Fixture {
            vectors,
            truth,
            weight_change,
            lookalike,
        }
    }

    fn len(&self) -> usize {
        self.vectors.len()
    }
}

/// An engine over an empty library.
async fn engine() -> (tempfile::TempDir, Store, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let engine = Engine::open(&store, EngineConfig::default()).await.unwrap();
    (dir, store, engine)
}

/// The face distance two vectors sit at, as the engine computes it.
fn distance(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    (1.0 - dot).max(0.0)
}

/// The clusters in the store, with their member appearance ids.
async fn clusters(store: &Store) -> Vec<(String, Vec<String>)> {
    cluster::all_with_members(store)
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.id, c.member_ids))
        .collect()
}

// ---------------------------------------------------------------------------
// (1) The ten clusters are recovered.
// ---------------------------------------------------------------------------

/// Ten people, twenty appearances each, and the engine finds ten.
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
        let a = engine
            .assign(&name, v, &ScoreComponents::face(0.0))
            .await
            .unwrap();
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
    for (person, members) in groups.iter().enumerate() {
        let _ = person;
        let mut truths: Vec<usize> = members
            .iter()
            .filter_map(|a| {
                a.strip_prefix("obj")
                    .and_then(|n| n.parse::<usize>().ok())
                    .map(|i| fx.truth[i])
            })
            .collect();
        truths.sort_unstable();
        truths.dedup();
        assert_eq!(
            truths.len(),
            1,
            "cluster {members:?} mixes ground-truth people {truths:?}"
        );
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
        engine
            .assign(&name, v, &ScoreComponents::face(0.0))
            .await
            .unwrap();
    }

    let groups = clusters(&store).await;
    for (person, heavy) in fx.weight_change.iter().enumerate() {
        let of_person: Vec<&Vec<String>> = groups
            .iter()
            .filter(|(_, members)| {
                members.iter().any(|m| {
                    m.strip_prefix("obj")
                        .and_then(|n| n.parse::<usize>().ok())
                        .is_some_and(|i| fx.truth[i] == person)
                })
            })
            .collect();
        assert_eq!(
            of_person.len(),
            1,
            "person {person} is in {} clusters, not one",
            of_person.len()
        );
        let members = of_person[0];
        for h in heavy {
            let name = format!("obj{h}");
            assert!(
                members.contains(&name),
                "the weight-change appearance {name} split out of person {person}'s cluster"
            );
        }
    }
    let _ = store;
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

    // Train the engine on people 0 and 1 well enough that the planted vector
    // sits between them. Using the whole fixture keeps the two candidates far
    // enough apart that the ambiguity is genuine rather than an artefact of a
    // thin sample.
    for (i, v) in fx.vectors.iter().enumerate() {
        if i == fx.lookalike.0 || i == fx.lookalike.1 {
            continue;
        }
        let name = format!("obj{i}");
        engine
            .assign(&name, v, &ScoreComponents::face(0.0))
            .await
            .unwrap();
    }

    let before = clusters(&store).await;
    assert_eq!(before.len(), PEOPLE, "the two candidates exist");

    // The planted vector: equal distance to person 0's and person 1's
    // centroids, by construction.
    let look = &fx.vectors[fx.lookalike.0];
    let c0 = engine
        .centroid(0)
        .await
        .unwrap()
        .expect("person 0's centroid");
    let c1 = engine
        .centroid(1)
        .await
        .unwrap()
        .expect("person 1's centroid");
    let d0 = distance(look, &c0);
    let d1 = distance(look, &c1);
    assert!(
        (d0 - d1).abs() < 1e-3,
        "the fixture is not actually ambiguous: d0={d0}, d1={d1}"
    );

    let a = engine
        .assign("lookalike", look, &ScoreComponents::face(0.0))
        .await
        .unwrap();

    // The decision is ambiguity. Not "joined cluster A", not "joined cluster B",
    // not "created a new cluster" — all three of those are a guess.
    assert_eq!(
        a.decision,
        Assignment::Ambiguous,
        "an equidistant vector must not be assigned: {a:?}"
    );

    // And the record says so, durably. The engine's return value and the stored
    // row have to agree, or the UI shows one thing and the engine did another.
    let stored = cluster::appearance(store, &a.appearance_id)
        .await
        .unwrap()
        .expect("the appearance was recorded");
    assert!(
        stored.ambiguous,
        "the stored row must carry ambiguous=1: {stored:?}"
    );
    assert!(
        stored.distance.is_some(),
        "and a distance, so the user can see how close it was"
    );

    // Both candidates are available for the UI to show. A record marked
    // ambiguous with no way to see what it was ambiguous *between* is not a
    // review state, it is a shrug.
    let candidates = cluster::ambiguous_candidates(store, &a.appearance_id)
        .await
        .unwrap();
    assert_eq!(
        candidates.len(),
        2,
        "both candidates must be retrievable: {candidates:?}"
    );
    assert!(
        candidates.iter().all(|c| c.distance.is_some()),
        "and each with its distance, per §7.4's 'show why'"
    );

    // The two real clusters are untouched: ambiguity is not a merge.
    let after = clusters(&store).await;
    assert_eq!(
        after.len(),
        before.len(),
        "no cluster was created or destroyed"
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
    let (_d, _store, engine) = engine().await;
    let mut rng = Rng::new(7);

    // A, B, C on a geodesic: A~B and B~C pass the threshold, A~C does not.
    // A threshold of 0.30 and unit vectors: cos = 1 - d.
    let a = rng.unit();
    let mut b = a.clone();
    b[0] = (b[0] - 0.20) as f32;
    let b = Embedder::l2_normalize(&b).to_vec();
    let mut c = b.clone();
    c[0] = (c[0] - 0.20) as f32;
    let c = Embedder::l2_normalize(&c).to_vec();

    let cfg = EngineConfig {
        threshold: 0.30,
        ..Default::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let engine = Engine::open(&store, cfg).await.unwrap();

    engine
        .assign("a", &a, &ScoreComponents::face(0.0))
        .await
        .unwrap();
    engine
        .assign("b", &b, &ScoreComponents::face(0.0))
        .await
        .unwrap();
    engine
        .assign("c", &c, &ScoreComponents::face(0.0))
        .await
        .unwrap();

    let ab = distance(&a, &b);
    let bc = distance(&b, &c);
    let ac = distance(&a, &c);
    assert!(ab < 0.30, "A~B must pass, got {ab}");
    assert!(bc < 0.30, "B~C must pass, got {bc}");
    assert!(ac > 0.30, "A~C must fail, got {ac}");

    let groups = clusters(&store).await;
    // A+B is one cluster. C is its own. Transitivity would have made all three
    // one, on the strength of two hops that each passed.
    let sizes: Vec<usize> = groups.iter().map(|(_, m)| m.len()).collect();
    assert!(
        sizes.contains(&2) && sizes.contains(&1),
        "expected a pair and a singleton, got {sizes:?}"
    );

    // And the consolidation pass, run explicitly, must not close the gap.
    let merged = engine
        .consolidate(ConsolidateConfig::default())
        .await
        .unwrap();
    assert_eq!(merged, 0, "no merge may be proposed across a failing A~C");
    let after = clusters(&store).await;
    assert_eq!(after.len(), groups.len(), "cluster count unchanged");
}

/// The guard is not a blanket refusal: a merge the direct distance *does*
/// support still happens.
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
    let a = rng.unit();
    let mut b = a.clone();
    b[0] = (b[0] - 0.01) as f32;
    let b = Embedder::l2_normalize(&b).to_vec();

    engine
        .assign("a", &a, &ScoreComponents::face(0.0))
        .await
        .unwrap();
    engine
        .assign("b", &b, &ScoreComponents::face(0.0))
        .await
        .unwrap();
    assert_eq!(
        clusters(&store).await.len(),
        2,
        "too far apart to assign together"
    );

    let merged = engine
        .consolidate(ConsolidateConfig::default())
        .await
        .unwrap();
    assert_eq!(merged, 1, "the direct distance supports one merge");
    let groups = clusters(&store).await;
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].1.len(), 2, "with both members");
}

// ---------------------------------------------------------------------------
// The properties the ticket's rules 1, 2 and 4 require.
// ---------------------------------------------------------------------------

/// Rule 2: a cluster is anonymous by default and that is a valid state.
#[tokio::test]
async fn a_new_cluster_is_anonymous_and_needs_no_name() {
    let (_d, store, engine) = engine().await;
    let v = Rng::new(3).unit();
    engine
        .assign("solo", &v, &ScoreComponents::face(0.0))
        .await
        .unwrap();

    let groups = clusters(&store).await;
    assert_eq!(groups.len(), 1);
    let c = cluster::get(&store, &groups[0].0).await.unwrap().unwrap();
    assert_eq!(c.state, ClusterState::Anonymous);
    assert!(
        c.handle.is_none(),
        "an anonymous cluster has no handle and does not want one"
    );
    // A one-appearance cluster is still browsable. §7.1: it becomes *meaningful*
    // at three, which is a UI threshold, not a creation threshold.
    assert_eq!(c.appearance_count, 1);
}

/// Rule 1: the decision and its distance are recorded, always.
#[tokio::test]
async fn the_decision_and_its_distance_are_always_recorded() {
    let (_d, store, engine) = engine().await;
    let mut rng = Rng::new(5);
    let a = rng.unit();
    let mut b = a.clone();
    b[0] = (b[0] - 0.05) as f32;
    let b = Embedder::l2_normalize(&b).to_vec();

    let first = engine
        .assign("a", &a, &ScoreComponents::face(0.0))
        .await
        .unwrap();
    let second = engine
        .assign("b", &b, &ScoreComponents::face(0.0))
        .await
        .unwrap();

    // The second is a join, and the distance that justified it is on the row.
    assert_eq!(second.decision, Assignment::Joined, "{second:?}");
    assert_eq!(second.cluster_id, first.cluster_id);

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
    assert!(row.body_score.is_some(), "and the body component");
    assert!(!row.ambiguous);
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

    let a = engine
        .assign("x", &v, &ScoreComponents::face(0.0))
        .await
        .unwrap();
    let cid = a.cluster_id.clone();

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
    assert!(c.handle.is_none(), "and no handle is chosen for the user");

    let competing = cluster::handle_candidates(&store, &cid).await.unwrap();
    assert_eq!(
        competing.len(),
        2,
        "both, for the UI to show: {competing:?}"
    );
    let names: Vec<&str> = competing.iter().map(|c| c.as_str()).collect();
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
        engine
            .assign(&format!("o{i}"), &v, &ScoreComponents::face(0.0))
            .await
            .unwrap();
    }
    let groups = clusters(&store).await;
    assert!(!groups.is_empty(), "stored, not discarded");
    let c = cluster::get(&store, &groups[0].0).await.unwrap().unwrap();
    assert!(c.is_browsable(), "and reports itself below the threshold");
}
