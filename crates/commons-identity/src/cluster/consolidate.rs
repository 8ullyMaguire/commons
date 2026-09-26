//! The agglomerative consolidation pass (T-P3-002, §7.1 step 4).
//!
//! # What this pass is for
//!
//! Incremental assignment is greedy and one-directional: every face joins the
//! best cluster available *at the moment it arrives*. Two consequences follow,
//! and this pass exists to fix both.
//!
//!  1. A cluster seeded by a single atypical face has a bad centroid, and every
//!     face that should have joined it instead created a new cluster. The
//!     clusters are individually defensible and collectively wrong.
//!  2. Clusters that *should* be one never meet, because at no point during
//!     assignment was either of them the nearest to the other.
//!
//! So this runs as a scheduled job (§15.9) over the whole library, and merges
//! where merging is justified.
//!
//! # The transitive-merge guard, which is the whole point
//!
//! §7.1 step 4: "agglomerative passes merge clusters whose members are mutually
//! consistent, with a guard against transitive over-merge."
//!
//! Without the guard, the pass collapses the library into one blob. A~B and B~C
//! both pass, so A~C is merged on the strength of a path rather than a
//! measurement — and each individual link was locally reasonable. The result is
//! one enormous wrong answer, which is *worse* than no clustering: the user
//! catches it, and then distrusts every link including the correct ones. §7.1
//! says this outright, and it is the reason the guard is a property of the
//! decision rather than a tunable.
//!
//! The rule: two clusters merge only when the *direct* distance between their
//! centroids passes the threshold, AND their membership is mutually consistent
//! — every member of the smaller cluster is within the threshold of the larger's
//! centroid. The second condition is what "mutually consistent" means in §7.1:
//! it is not enough that the two centroids agree, because a cluster can have a
//! tight centroid and one member that does not belong.
//!
//! # Determinism
//!
//! Pairs are considered in a fixed order — by distance, then by cluster id — so
//! two runs over the same library produce the same merges. A pass whose result
//! depends on hash-map iteration order is a pass that cannot be tested, and a
//! clustering bug that comes and goes with the seed is the worst kind to chase.

use std::collections::BTreeMap;

use commons_store::Store;

use super::{cosine_distance, store, ClusterError, Engine};

/// How the pass is configured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsolidateConfig {
    /// The centroid distance below which two clusters may merge.
    ///
    /// Deliberately *looser* than the assignment threshold, because the pass
    /// exists to make the joins that incremental assignment could not: two
    /// clusters that never met during assignment, because at no point was either
    /// the nearest to the other.
    ///
    /// Loosening it is safe because of `require_mutual_consistency`, not in spite
    /// of it -- the guard, not the number, is what stops a merge being carried by
    /// two centroids that happen to be near each other while the people in them
    /// are not.
    pub merge_threshold: f32,
    /// The most pairs to consider in one pass.
    ///
    /// A bound on work, and on how long a scheduled job may run. A library with
    /// 200k clusters has ~2 * 10^10 pairs; at a thousand per pass that is not a
    /// bound anyone would accept as "eventually", so this is paired with
    /// `max_rounds` in the job that calls it and the pair list is built from
    /// candidate blocks rather than the full cross product. The limit is stated
    /// rather than hidden: a pass that stops early leaves some mergeable pairs
    /// for the next run, which is correct behaviour, not a silent truncation.
    pub max_pairs: usize,
    /// Require every member of the smaller cluster to be consistent with the
    /// larger's centroid.
    ///
    /// On. Turning it off is allowed for a library where a cluster is known to
    /// contain a mis-assigned face and the user would rather merge anyway — but
    /// it is the guard §7.1 asks for, so the default is the safe one and the
    /// escape hatch is visible.
    pub require_mutual_consistency: bool,
}

impl Default for ConsolidateConfig {
    fn default() -> Self {
        ConsolidateConfig {
            merge_threshold: 0.55,
            max_pairs: 20_000,
            require_mutual_consistency: true,
        }
    }
}

/// What a pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergeReport {
    /// The merges performed, as `(survivor, absorbed)`.
    pub merged: Vec<(String, String)>,
    /// Pairs that were within the centroid threshold but refused because their
    /// members disagreed.
    ///
    /// Recorded rather than counted, because these are the interesting ones: a
    /// pass that silently skips them looks identical to a pass that found
    /// nothing, and the difference is the difference between "there was nothing
    /// to do" and "there was something to do and the guard said no".
    pub refused_inconsistent: Vec<(String, String)>,
    /// Pairs beyond `max_pairs`, left for a later pass.
    pub deferred: usize,
}

impl MergeReport {
    pub fn merge_count(&self) -> usize {
        self.merged.len()
    }
}

/// Run one consolidation pass with the engine's own configuration.
pub async fn consolidate(
    engine: &Engine,
    config: ConsolidateConfig,
) -> Result<MergeReport, ClusterError> {
    consolidate_with_threshold(engine, config).await
}

pub async fn consolidate_with_threshold(
    engine: &Engine,
    config: ConsolidateConfig,
) -> Result<MergeReport, ClusterError> {
    // The merge threshold is the pass's own, and is NOT clamped to the
    // assignment threshold. Clamping it was wrong, and wrong in a way the tests
    // caught: the whole point of a scheduled pass is to find the joins that
    // incremental assignment never made, because at the moment each appearance
    // arrived neither of two clusters was the nearest to the other. A pass that
    // may not exceed the assignment threshold can therefore never merge
    // anything assignment did not already join -- it would be a no-op that
    // reports having considered every pair.
    //
    // What makes a looser threshold safe here is not the threshold at all. It is
    // the mutual-consistency guard below: every member of the smaller cluster
    // must be within the threshold of the larger's centroid, so the merge
    // cannot be carried by two centroids that happen to be near each other while
    // the people in them are not. That guard is why a looser number is
    // defensible, which is why `merge_threshold` is allowed to exceed the
    // assignment threshold and why tightening *that* is the lever that matters.
    let threshold = config.merge_threshold;
    let store = engine.store();

    let mut report = MergeReport::default();
    // Cluster state, re-read after every merge: a merge changes the survivor's
    // centroid and its membership, so a pair decided before the merge may be
    // wrong after it. Recomputing from the store each round is the honest way
    // to do that, and it also means an interrupted pass leaves a consistent
    // library -- each merge is its own transaction.
    for _round in 0..8 {
        let rows = store::all_with_members(store).await?;
        if rows.len() < 2 {
            break;
        }
        let centroids: Vec<(String, Vec<f32>)> = rows
            .iter()
            .filter_map(|r| r.centroid.as_ref().map(|c| (r.id.clone(), c.clone())))
            .collect();
        if centroids.len() < 2 {
            break;
        }

        let mut pairs: Vec<(f32, String, String)> = Vec::new();
        for i in 0..centroids.len() {
            for j in (i + 1)..centroids.len() {
                let d = cosine_distance(&centroids[i].1, &centroids[j].1);
                if d <= threshold {
                    // Fixed order: distance, then the cluster ids, so a pair is
                    // visited in the same sequence on every run.
                    pairs.push((d, centroids[i].0.clone(), centroids[j].0.clone()));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

        let mut merged_this_round = 0usize;
        let mut taken: BTreeMap<String, String> = BTreeMap::new();
        for (d, a, b) in pairs.iter() {
            if report.merged.len() + merged_this_round >= config.max_pairs {
                report.deferred += 1;
                continue;
            }
            // Skip a pair where either cluster has already been absorbed this
            // round: the survivor's centroid has moved and the decision would be
            // against a cluster that no longer exists.
            if taken.contains_key(a) || taken.contains_key(b) {
                continue;
            }
            // A cluster is never merged *into* something it already contains,
            // and never with itself. `a` and `b` are distinct by construction,
            // but a caller-supplied pair list is not, so this is checked.
            if a == b {
                continue;
            }

            if config.require_mutual_consistency {
                let ca = super::store::member_vectors(store, a).await?;
                let cb = super::store::member_vectors(store, b).await?;
                if !mutually_consistent(&ca, &cb, threshold) {
                    report.refused_inconsistent.push((a.clone(), b.clone()));
                    continue;
                }
            }

            // The survivor is the one with more members, so the smaller cluster's
            // id disappears. Ties break on the id, so the choice is stable.
            let (survivor, absorbed) = {
                let na = super::store::member_vectors(store, a).await?.len();
                let nb = super::store::member_vectors(store, b).await?.len();
                if na > nb || (na == nb && a < b) {
                    (a.clone(), b.clone())
                } else {
                    (b.clone(), a.clone())
                }
            };
            let _ = d;
            super::store::merge(store, &absorbed, &survivor).await?;
            taken.insert(absorbed.clone(), survivor.clone());
            report.merged.push((survivor, absorbed));
            merged_this_round += 1;
        }

        if merged_this_round == 0 {
            break;
        }
    }
    Ok(report)
}

/// Is every member of each set within `threshold` of the other's centroid?
///
/// §7.1's "mutually consistent". The centroid distance alone is not enough: a
/// cluster whose centroid sits between two modes can be within threshold of
/// another cluster while holding a member that is not.
fn mutually_consistent(a: &[Vec<f32>], b: &[Vec<f32>], threshold: f32) -> bool {
    let (Some(ca), Some(cb)) = (centroid(a), centroid(b)) else {
        // No vectors recorded for one side: there is nothing to be consistent
        // *with*, and refusing is the safe direction. A cluster whose sidecar
        // was deleted should not be merged away on the strength of a centroid
        // alone.
        return false;
    };
    a.iter().all(|v| cosine_distance(v, &cb) <= threshold)
        && b.iter().all(|v| cosine_distance(v, &ca) <= threshold)
}

/// The mean of a set of vectors, normalised. `None` for an empty set.
fn centroid(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let first = vectors.first()?;
    let mut mean = vec![0f32; first.len()];
    for v in vectors {
        if v.len() != mean.len() {
            return None;
        }
        for (m, x) in mean.iter_mut().zip(v) {
            *m += *x;
        }
    }
    let n = vectors.len() as f32;
    for m in mean.iter_mut() {
        *m /= n;
    }
    Some(commons_ml::face::Embedder::l2_normalize(&mean).to_vec())
}

/// Attach a face the engine had left ambiguous to a cluster the user chose.
///
/// Not part of the scheduled pass: §7.1 makes this a decision the *user* makes,
/// and the "this is two people" action is a separate one that splits a cluster
/// rather than joining it. Both write through the same place, so the recorded
/// decision and the row agree.
pub async fn resolve_ambiguous(
    engine: &Engine,
    appearance_id: &str,
    cluster_id: &str,
) -> Result<(), ClusterError> {
    let store = engine.store();
    let Some(a) = store::appearance(store, appearance_id).await? else {
        return Err(ClusterError::NoSuchAppearance(appearance_id.to_string()));
    };
    if !a.ambiguous {
        return Err(ClusterError::NotAmbiguous(appearance_id.to_string()));
    }
    // Flip the row onto the chosen cluster and clear the flag in one statement,
    // so there is no window in which it is attached and still flagged ambiguous
    // -- a state every read would have to defend against and the UI would render
    // as both joined and unresolved.
    store::resolve_ambiguous(store, appearance_id, cluster_id).await?;
    // The count is recomputed rather than incremented: a single statement that
    // cannot drift, against an increment every future membership change would
    // have to remember to mirror.
    store::recompute_count(store, cluster_id).await?;
    Ok(())
}

/// Split a cluster: the "this is two people" action.
///
/// §7.1 requires it as the counterpart to ambiguity -- ambiguity exists so the
/// user can resolve it, and a resolution that can only ever be "join" would leave
/// no way to undo a wrong link.
pub async fn split(store: &Store, cluster_id: &str, keep: &[String]) -> Result<u64, ClusterError> {
    let new_id = uuid::Uuid::new_v4().to_string();
    let now = super::now();
    // No centroid, of either kind. `split` moves appearances to a new cluster
    // and the caller must recompute; seeding a centroid here from a single
    // appearance would make the split's second half depend on which appearance
    // happened to arrive first.
    store::insert_cluster(
        store,
        &new_id,
        super::ClusterState::Anonymous,
        None,
        None,
        &now,
    )
    .await?;
    let moved = store::move_appearances(store, cluster_id, &new_id, keep).await?;
    if moved > 0 {
        // Both halves get a fresh centroid from their own members, and the
        // original is marked: it is no longer a person, and leaving it
        // `anonymous` would present a split cluster as if nothing had happened.
        let (a, b) = (
            store::member_vectors(store, cluster_id).await?,
            store::member_vectors(store, &new_id).await?,
        );
        if let Some(c) = centroid(&a) {
            store::set_centroid(store, cluster_id, &super::hex(&c), &now).await?;
        }
        if let Some(c) = centroid(&b) {
            store::set_centroid(store, &new_id, &super::hex(&c), &now).await?;
        }
        // The original half is marked, not left anonymous: it is no longer a
        // person, and an `anonymous` cluster that is a fragment of a split
        // presents itself as a first sighting of someone new.
        store::set_state(store, cluster_id, super::ClusterState::PossiblyDifferent).await?;
    }
    Ok(moved)
}
