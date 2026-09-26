//! T-P3-003 acceptance: the §7.4 composite score.
//!
//! # What this ticket is really about
//!
//! §7.4 names the failure the composite exists to prevent: "A face-only system
//! splits one person into three clusters, which is worse than no clustering
//! because the user then has to merge manually and distrusts the result."
//!
//! The temptation, reading that, is to "just average the two distances". That
//! version is broken, and broken in a way every other test here still passes. An
//! appearance with a good face and *no silhouette at all* — a close-up, which is
//! most of a professional shoot — has no body distance. If absence is scored as
//! 0.0, that appearance is measured at `w_face × d_face`: its face distance is
//! silently scaled by 0.7, so a face at 0.70 reads as 0.49 and joins a cluster it
//! should have missed. The composite then *causes* the over-merging it was added
//! to stop, and does it only for the images with the least evidence — the
//! opposite of where the risk is.
//!
//! So the weights are **renormalised over the evidence that exists**. That is the
//! first assertion here and the one the rest of the file leans on.
//!
//! # What is pinned
//!
//! 1. A missing component is renormalised away, not counted as zero.
//! 2. A missing component on the *cluster* side is dropped for that candidate
//!    only, and the absence is recorded rather than fabricated.
//! 3. Both components reach the row and reconstruct the stored distance. §7.4
//!    requires the UI to say "same face 0.87, body consistent", and that
//!    sentence is impossible from a single opaque float.
//! 4. The weights are configuration, and a zero weight disables a source rather
//!    than producing a division by zero or a `NaN`.
//!
//! The numbers are synthetic. No body-embedding model is linked, so these
//! fixtures are vectors with a known geometry and they test the *arithmetic*,
//! not the recall of any real detector.

mod common;

use common::{distance, object, tuned, Frame, Rng, Store};
use commons_identity::cluster::{
    self, Assignment, ClusterError, Engine, EngineConfig, ScoreComponents, ScoreWeights,
};

/// The recorded components of the appearance belonging to `object_id`.
///
/// Read back from the row rather than taken from the engine's return value:
/// §7.4's promise is about what the UI can read, and a field on a struct is not
/// what the UI reads.
async fn recorded(store: &Store, object_id: &str) -> (Option<f32>, Option<f32>, Option<f32>) {
    let all = cluster::all_appearances(store).await.unwrap();
    let row = all
        .into_iter()
        .find(|a| a.object_id == object_id)
        .unwrap_or_else(|| panic!("no appearance recorded for {object_id}"));
    (row.distance, row.face_score, row.body_score)
}

/// An engine with a threshold and weights chosen by the test.
/// Is `got` within a hair of `want`?
///
/// The fixtures build vectors by rotating in float32, so a distance the test
/// wrote as 0.70 reads back as 0.6999998. That is the fixture being right and
/// the literal being inexact; an `assert_eq!` on it would fail forever and the
/// response would be to loosen the fixture, which is the wrong fix.
fn close_to(got: Option<f32>, want: f32) -> bool {
    matches!(got, Some(g) if (g - want).abs() < 1e-3)
}

/// The weighted mean of two stored components, under the given weights.
///
/// The assertion throughout is the *relation* between the three stored numbers,
/// not that a component equals a number the fixture asked for. `Frame::at` adds
/// a small random perturbation so vectors from the same frame are not identical,
/// and a test that asserted `face_score == 0.70` would be asserting that the
/// fixture's noise was zero. The relation is what §7.4 promises and it holds
/// whatever the noise was.
fn weighted(face: Option<f32>, body: Option<f32>, wf: f32, wb: f32) -> Option<f32> {
    let (present, sum) = match (face, body) {
        (Some(f), Some(b)) => (wf + wb, wf * f + wb * b),
        (Some(f), None) => (wf, wf * f),
        (None, Some(b)) => (wb, wb * b),
        (None, None) => return None,
    };
    if present <= 0.0 {
        return None;
    }
    Some(sum / present)
}

/// `d == weighted(face, body)`, to a tolerance that noise cannot cross.
fn components_agree(
    d: Option<f32>,
    face: Option<f32>,
    body: Option<f32>,
    wf: f32,
    wb: f32,
) -> bool {
    match (d, weighted(face, body, wf, wb)) {
        (Some(d), Some(w)) => (d - w).abs() < 1e-3,
        _ => false,
    }
}

async fn body_centroid(store: &Store, cluster_id: &str) -> Option<Vec<f32>> {
    cluster::all_with_members(store)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.id == cluster_id)
        .unwrap()
        .body_centroid
}

async fn face_centroid(store: &Store, cluster_id: &str) -> Vec<f32> {
    cluster::all_with_members(store)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.id == cluster_id)
        .unwrap()
        .centroid
        .expect("a cluster the engine assigned to has a centroid")
}

// ---------------------------------------------------------------------------
// (1) A missing component is renormalised, not scored as zero.
// ---------------------------------------------------------------------------

/// A face-only appearance is judged on its face, at full weight.
///
/// With weights 0.7/0.3 and a 0.60 threshold, scoring the absent body as 0.0
/// would put a face at 0.70 at 0.49 — inside the threshold, a join. On the
/// evidence, 0.70 is outside it. This is the whole ticket in one assertion.
#[tokio::test]
async fn a_face_only_appearance_is_not_diluted_by_an_absent_body() {
    let (_d, store, engine) = tuned(0.60, 0.7, 0.3).await;
    let base = Rng::new(101).unit();

    let seed = base.clone();
    object(&store, "seed").await;
    engine.assign("seed", &seed, None).await.unwrap();

    // 0.70 from the seed: past a 0.60 threshold on the face alone.
    let probe = Frame::from_pair(&base, 0.70);
    object(&store, "probe").await;
    let _out = engine.assign("probe", &probe, None).await.unwrap();

    let (d, face, body) = recorded(&store, "probe").await;
    assert_eq!(
        body, None,
        "no body evidence is recorded as absent, not as 0.0 -- the UI has to be \
         able to tell 'no body evidence' from 'body evidence says no'"
    );
    let face = face.expect("the face distance is recorded as measured");
    assert!(
        (face - 0.70).abs() < 1e-3,
        "and it is the real face distance, past the threshold on its own: {face}"
    );
    assert_eq!(
        d,
        Some(face),
        "the composite of a single component is that component, unweighted -- \
         not 0.7 x {face} = {}",
        0.7 * face
    );
}

/// The mirror: the body term is used when there is a body, and dropped when the
/// cluster has no body centroid to compare against.
///
/// Renormalising is not "ignore the weight that does not apply" — it is "spread
/// the whole budget over what does". A cluster seeded by a close-up has no body
/// centroid at all, and comparing an incoming body vector against nothing would
/// be a distance between unrelated embedding spaces.
#[tokio::test]
async fn a_body_evidence_with_no_cluster_body_centroid_is_dropped_not_faked() {
    let (_d, store, engine) = tuned(0.60, 0.7, 0.3).await;
    let base = Rng::new(102).unit();

    // The seed has a face and no body, so its cluster has no body centroid.
    let seed = base.clone();
    object(&store, "seed").await;
    engine.assign("seed", &seed, None).await.unwrap();

    let probe_body = Frame::from_pair(&base, 0.05);
    let probe_face = Frame::from_pair(&base, 0.70);
    object(&store, "probe").await;
    let out = engine
        .assign("probe", &probe_face, Some(&probe_body))
        .await
        .unwrap();

    assert_eq!(
        out.decision,
        Assignment::Created,
        "the face alone decides, because the cluster has no body centroid to \\
         compare against: {out:?}"
    );
    let (d, face, body) = recorded(&store, "probe").await;
    assert_eq!(
        body, None,
        "the body distance is recorded as absent -- it was never compared against \
         anything, and recording 0.0 would claim the body matched"
    );
    let face = face.expect("the face distance is recorded");
    assert!(
        (face - 0.70).abs() < 1e-3,
        "and it is the real face distance: {face}"
    );
    assert_eq!(
        d,
        Some(face),
        "so the composite is the face distance alone, and body evidence that \
         could not be compared contributes nothing rather than something small"
    );
    assert!(
        close_to(d, 0.70),
        "so the composite is the face distance alone"
    );
}

// ---------------------------------------------------------------------------
// (2) Both components, and the weighted mean.
// ---------------------------------------------------------------------------

/// With both sides present on both sources, the distance is the weighted mean.
///
/// And the two stored components reconstruct it. That round-trip is the test: a
/// composite computed one way and stored another is exactly the bug §7.4's "show
/// the user why" requirement exists to make impossible.
#[tokio::test]
async fn the_stored_components_reconstruct_the_stored_distance() {
    let (_d, store, engine) = tuned(0.60, 0.7, 0.3).await;
    let base = Rng::new(103).unit();

    // Seeded with both sources, so it has both centroids.
    object(&store, "seed").await;
    engine
        .assign(
            "seed",
            &Frame::from_pair(&base, 0.05),
            Some(&Frame::from_pair(&base, 0.05)),
        )
        .await
        .unwrap();

    // Both components 0.40 from the seed: the mean is 0.40, inside a 0.60
    // threshold, so this joins. The threshold sits between the components and
    // the mean on purpose, so a combination that took the maximum would also
    // pass and a combination that took the minimum would not.
    let probe_face = Frame::from_pair(&base, 0.40);
    let probe_body = Frame::from_pair(&base, 0.40);
    object(&store, "probe").await;
    let out = engine
        .assign("probe", &probe_face, Some(&probe_body))
        .await
        .unwrap();
    assert_eq!(
        out.decision,
        Assignment::Joined,
        "a 0.40 mean is inside 0.60"
    );

    let (d, face, body) = recorded(&store, "probe").await;
    assert!(
        face.is_some() && body.is_some(),
        "both components reach the row: face {face:?}, body {body:?}"
    );
    // Not a fixed number. The probe's distance is measured against the cluster's
    // centroid, and the centroid is the mean of the members -- so after the join
    // it is a mean of two vectors, not the seed. 0.40 is the distance to the
    // *seed*; the row records the distance to the centroid that existed when
    // the decision was made. Both components are equal here because the fixture
    // gave the appearance the same vector for both sources, which makes the
    // arithmetic trivial and the round-trip exact -- deliberately, because this
    // test is about the relation between the three stored numbers and the next
    // one is about the weights.
    assert!(
        components_agree(d, face, body, 0.7, 0.3),
        "the stored distance is the weighted mean of the two stored components: \
         distance {d:?}, face {face:?}, body {body:?}"
    );
}

/// Two appearances whose components disagree, so the *weights* decide.
///
/// This is the assertion a `combined()` that returned the face distance alone
/// would fail, and nothing else in the file would: the distances would be
/// plausible, the decisions self-consistent, and only the combination wrong.
#[tokio::test]
async fn the_weights_decide_when_the_components_disagree() {
    // All the weight on the body. The face is 0.70 — outside any threshold — and
    // the body is 0.20, which carries the decision.
    let (_d, store, engine) = tuned(0.60, 0.2, 0.8).await;
    let base = Rng::new(104).unit();

    let seed = Frame::from_pair(&base, 0.00);
    object(&store, "seed").await;
    engine
        .assign("seed", &seed, Some(&Frame::from_pair(&base, 0.00)))
        .await
        .unwrap();

    // 0.2 × 0.70 + 0.8 × 0.20 = 0.30, comfortably inside 0.60. A face-only
    // engine would have created a cluster here, which is the point.
    let probe = Frame::from_pair(&base, 0.70);
    let probe_body = Frame::from_pair(&base, 0.20);
    object(&store, "probe").await;
    let out = engine
        .assign("probe", &probe, Some(&probe_body))
        .await
        .unwrap();

    assert_eq!(
        out.decision,
        Assignment::Joined,
        "0.2x0.70 + 0.8x0.20 = 0.30, which is inside 0.60 -- a face-only engine \\
         would have created a cluster here: {out:?}"
    );
    let (d, face, body) = recorded(&store, "probe").await;
    let face = face.expect("both components are recorded whatever the decision");
    let body = body.expect("both components are recorded whatever the decision");
    assert!(
        face > 0.60,
        "the face is recorded as the far distance it was: {face}"
    );
    assert!(body < 0.30, "and the body as the near one: {body}");
    assert!(
        components_agree(d, Some(face), Some(body), 0.2, 0.8),
        "the distance is the 0.2/0.8 weighted mean, which is what made the \
         decision: distance {d:?}, face {face}, body {body}"
    );
    assert!(
        d.unwrap() < 0.60,
        "and it is inside the threshold, so the join is justified by the \
         recorded numbers rather than asserted"
    );
}

// ---------------------------------------------------------------------------
// (3) The weights are configuration, and a zero weight disables a source.
// ---------------------------------------------------------------------------

/// A weight of 0 on a source that is present is a deliberate silence, not a
/// division by zero.
///
/// This is how "body only" and "face only" modes are expressed without a second
/// code path, so it has to be a legal configuration. The face distance is still
/// *recorded* even when it carries no weight: a source weighted out of the
/// decision is not the same as not being measured, and the UI shows both.
#[tokio::test]
async fn a_zero_weight_disables_a_source_without_dividing_by_zero() {
    let (_d, store, engine) = tuned(0.60, 0.0, 1.0).await;
    let base = Rng::new(105).unit();

    let seed = Frame::from_pair(&base, 0.00);
    object(&store, "seed").await;
    engine
        .assign("seed", &seed, Some(&Frame::from_pair(&base, 0.00)))
        .await
        .unwrap();

    let probe = Frame::from_pair(&base, 0.70);
    let probe_body = Frame::from_pair(&base, 0.10);
    object(&store, "probe").await;
    let out = engine
        .assign("probe", &probe, Some(&probe_body))
        .await
        .unwrap();
    assert_eq!(
        out.decision,
        Assignment::Joined,
        "the body is 0.10 and the face is weighted zero, so the body decides: {out:?}"
    );
    let (d, face, body) = recorded(&store, "probe").await;
    assert!(
        (face.expect("recorded") - 0.70).abs() < 1e-3,
        "the face distance is still recorded -- a source weighted out of the \\
         decision is not the same as not being measured, and the UI shows both"
    );
    assert!(close_to(body, 0.10), "and the body distance: {body:?}");
    assert!(
        (d.unwrap() - 0.10).abs() < 1e-4,
        "and the decision is the body distance alone: {d:?}"
    );
}

/// Every weight zero is a configuration error, not a distance.
///
/// The alternative is 0/0, and a `NaN` distance fails every comparison, so the
/// candidate becomes silently unrankable rather than loudly wrong. Refusing is
/// the only answer that surfaces the mistake.
#[tokio::test]
async fn all_zero_weights_is_refused_rather_than_producing_nan() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let engine = Engine::open(
        &store,
        EngineConfig {
            threshold: 0.60,
            score_weights: cluster::ScoreWeights {
                face: 0.0,
                body: 0.0,
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut rng = Rng::new(16);
    let v = rng.unit();

    let err = engine.assign("nothing", &v, None).await.unwrap_err();
    assert!(
        matches!(
            err,
            cluster::ClusterError::ZeroWeight | cluster::ClusterError::NoEvidence
        ),
        "an unusable configuration is refused with a named error, not a NaN \\
         distance that fails every comparison: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// (4) Each source's centroid is maintained differently, on purpose.
// ---------------------------------------------------------------------------

/// A body centroid is written once, when the cluster first has body evidence.
///
/// Not averaged on every join: the body centroid describes the person, and a
/// later body embedding from a different model is not commensurable with it, so
/// averaging across a model change produces a number describing neither. The
/// model version belongs on the row; that is the recorded follow-up.
#[tokio::test]
async fn a_body_centroid_is_written_once_and_not_averaged() {
    let (_d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(106).unit();

    // The cluster starts with no body evidence at all, which is the case that
    // needs the seeding: a cluster created by a close-up stays uncomparable on
    // the body until something seeds it.
    object(&store, "a").await;
    let first = engine.assign("a", &base, None).await.unwrap();
    let id = first
        .cluster_id
        .clone()
        .expect("a created cluster names itself");
    assert_eq!(
        body_centroid(&store, &id).await,
        None,
        "a face-only cluster has no body centroid: there is nothing to have \
         compared one against"
    );

    object(&store, "b").await;
    let body_b = Frame::from_pair(&base, 0.20);
    engine
        .assign("b", &Frame::from_pair(&base, 0.10), Some(&body_b))
        .await
        .unwrap();
    let after_first = body_centroid(&store, &id).await;
    assert!(
        after_first.is_some(),
        "the first body evidence seeds the centroid, or the cluster would stay \
         uncomparable on the body for ever"
    );

    // A later, different body embedding must not move it: averaging across a
    // possible model change produces a number describing neither model. The
    // model version belongs on the row; that is the recorded follow-up.
    object(&store, "c").await;
    engine
        .assign(
            "c",
            &Frame::from_pair(&base, 0.20),
            Some(&Frame::from_pair(&body_b, 0.55)),
        )
        .await
        .unwrap();
    assert_eq!(
        body_centroid(&store, &id).await,
        after_first,
        "a later body embedding does not overwrite the centroid"
    );
}

/// An appearance with neither a face nor a body is refused, not scored as a
/// perfect match.
///
/// The failure this guards is the one that would be hardest to notice: a
/// `0.0` distance means "identical", so an appearance with no evidence would
/// join the nearest cluster with perfect confidence and be recorded as a match
/// stronger than any real one. It is unreachable through `assign` with a valid
/// engine, which is the point -- the test pins the property at the layer that
/// can still be broken.
#[tokio::test]
async fn no_evidence_at_all_is_refused_rather_than_scored_as_identical() {
    let err = ScoreComponents::default()
        .combined(ScoreWeights::default())
        .expect_err("an empty ScoreComponents has no distance under any weights");
    assert!(
        matches!(err, ClusterError::NoEvidence),
        "and it says so by name, rather than producing the 0.0 that would read as \
         a perfect match: {err:?}"
    );
}

/// The face centroid *does* move, because it is a mean of members.
///
/// The contrast with the test above is the design, and it is worth stating
/// rather than leaving to be discovered: the face centroid is recomputed from
/// what the cluster contains (T-P3-002's rule), while the body centroid is a
/// first observation.
#[tokio::test]
async fn the_face_centroid_is_still_a_mean_of_its_members() {
    let (_d, store, engine) = tuned(0.90, 0.7, 0.3).await;
    let base = Rng::new(107).unit();

    object(&store, "a").await;
    let first = engine
        .assign("a", &Frame::from_pair(&base, 0.00), None)
        .await
        .unwrap();
    let id = first.cluster_id.unwrap();
    let before = face_centroid(&store, &id).await;

    object(&store, "b").await;
    engine
        .assign("b", &Frame::from_pair(&base, 0.30), None)
        .await
        .unwrap();
    let after = face_centroid(&store, &id).await;

    assert!(
        distance(&before, &after) > 0.05,
        "the face centroid moved toward the second member: {}",
        distance(&before, &after)
    );
}

/// A body-only cluster is still a cluster: it has a face centroid of `NULL` and
/// must not be invisible to the next assignment.
///
/// The alternative — skipping a cluster with no face centroid — is correct for
/// *ranking* and wrong for *existence*. A person seen only in distant shots has
/// no usable face, and the second such shot has nothing to compare against. The
/// body is the only evidence there is, so the face weight of the cluster is
/// A cluster seeded from a body-bearing appearance is reachable by a later
/// body-dominated one, and the body centroid is what makes it comparable.
///
/// This is the case §7.4 is about: a distant shot has no usable face, and the
/// second distant shot of the same person has to find the first. The face term
/// is measured and recorded; the body term is what the cluster offers to
/// compare against, and without a body centroid on the cluster there would be
/// nothing to offer.
#[tokio::test]
async fn a_later_appearance_joins_a_body_seeded_cluster_on_the_body() {
    let (_d, store, engine) = tuned(0.60, 0.3, 0.7).await;
    let base = Rng::new(108).unit();

    object(&store, "a").await;
    let first = engine
        .assign(
            "a",
            &Frame::from_pair(&base, 0.00),
            Some(&Frame::from_pair(&base, 0.00)),
        )
        .await
        .unwrap();
    assert_eq!(first.decision, Assignment::Created);
    let id = first
        .cluster_id
        .clone()
        .expect("a created cluster names itself");
    assert!(
        body_centroid(&store, &id).await.is_some(),
        "the cluster has a body centroid to compare against"
    );

    // The face is far (0.70, outside a 0.60 threshold on its own) and the body
    // is close (0.15). Weighted 0.3/0.7: 0.3x0.70 + 0.7x0.15 = 0.315, inside.
    object(&store, "b").await;
    let second = engine
        .assign(
            "b",
            &Frame::from_pair(&base, 0.70),
            Some(&Frame::from_pair(&base, 0.15)),
        )
        .await
        .unwrap();
    assert_eq!(
        second.decision,
        Assignment::Joined,
        "the body carried the decision: {second:?}"
    );
    assert_eq!(second.cluster_id.as_deref(), Some(id.as_str()));

    let (d, face, body) = recorded(&store, "b").await;
    let face = face.expect("both components are recorded");
    let body = body.expect("both components are recorded");
    assert!(face > 0.60, "the face is far: {face}");
    assert!(body < 0.30, "and the body is near: {body}");
    assert!(
        components_agree(d, Some(face), Some(body), 0.3, 0.7),
        "the distance is the 0.3/0.7 weighted mean: distance {d:?}, face {face}, \
         body {body}"
    );
}
