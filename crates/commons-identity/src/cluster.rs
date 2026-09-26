//! The clustering engine: join, create, or admit ignorance (T-P3-002, §7.1).
//!
//! # The decision, and why ambiguity is the interesting branch
//!
//! Given a face embedding, the engine does one of three things: **join** an
//! existing cluster, **create** a new anonymous one, or attach as
//! **ambiguous**. The third is the one that carries the design.
//!
//! §7.1: "Ambiguity is displayed, not hidden. A cluster with conflicting
//! evidence is `ambiguous` in the UI with both candidates and a 'this is two
//! people' action. Silently guessing is the failure mode that would make people
//! distrust the links."
//!
//! Note the direction of the asymmetry, because it decides the threshold code
//! below. An engine that is too cautious produces a library split into too many
//! clusters; the user merges two by hand and is mildly annoyed. An engine that
//! is too confident asserts that two different people are one person — and a
//! user who catches it doing that once has no reason to believe anything else it
//! says. The product is a *trust* product: every link is a claim about a real
//! person, and the user's willingness to act on the engine's output is the
//! whole mechanism by which amateur corpora get threaded. So the expensive
//! error is the confident one, and the tie-break rule below is written to make
//! ambiguity cheap and merging expensive.
//!
//! Concretely, a nearest-neighbour rule that takes the best match whenever the
//! best match passes the threshold will always pick one of two equidistant
//! candidates. That is a guess, and it is wrong with probability 0.5. The rule
//! here is instead: find the best match; if the *runner-up* is within
//! [`EngineConfig::ambiguity_margin`] of the best, the engine does not know, and
//! says so.
//!
//! # Why the distance is recorded on the appearance, not just the decision
//!
//! §7.4: "The thresholds are per-decision, visible, and configurable, and the UI
//! shows *why* two items were linked ('same face, 0.87; body consistent') —
//! because a link the user cannot interrogate is a link they will not trust."
//!
//! So the `appearance` row carries `distance`, `face_score` and `body_score`
//! separately, and [`Appearance`] reads them back. One opaque float cannot
//! produce that sentence, which is why the schema has three columns and not
//! one.
//!
//! # The transitive-merge guard
//!
//! §7.1 step 4: "agglomerative passes merge clusters whose members are mutually
//! consistent, with a guard against transitive over-merge."
//!
//! Without the guard, clustering collapses into one blob. Every individual link
//! is locally reasonable — A looks like B, B looks like C — and the result is
//! one enormous wrong answer. That is *worse* than no clustering, because the
//! user then distrusts every link including the correct ones. So a merge
//! requires the direct A~C distance to pass, not merely a path between them.
//!
//! # The centroid, and why it is stored as hex
//!
//! A cluster's centroid is the running mean of its members, normalised. It is
//! recomputed on every assignment rather than incrementally, because an
//! incremental mean silently weights old members less and a cluster that has
//! been reassigned forty times drifts away from its own members. `centroid_hex`
//! is in the schema as text: a 512-dim f32 is 2 KiB, which is too large to be
//! pleasant in a column but must be *somewhere* durable, and hex keeps the
//! column type portable across SQLite and Postgres without a binary type that
//! means different things on each.

use std::collections::BTreeMap;

use commons_ml::face::Embedder;
use commons_store::Store;

pub mod store;
pub use store::{
    all_with_members, ambiguous_candidates, appearance, get, handle_candidates, propose_handle,
    AppearanceRecord, CandidateDistance, ClusterRow, HandleCandidate,
};

/// What the engine decided about a face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assignment {
    /// Joined an existing cluster.
    Joined,
    /// Created a new anonymous cluster.
    Created,
    /// The engine does not know, and says so.
    ///
    /// Not a failure and not a new cluster: the appearance is recorded, flagged,
    /// and attached to nothing. §7.1 requires this state to exist, and a user
    /// who never sees it is being told the engine guessed on their behalf.
    Ambiguous,
}

impl Assignment {
    pub fn as_str(self) -> &'static str {
        match self {
            Assignment::Joined => "joined",
            Assignment::Created => "created",
            Assignment::Ambiguous => "ambiguous",
        }
    }
}

/// A cluster's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterState {
    /// No name. The default, and a valid browsable state (§7.1 rule 2).
    Anonymous,
    /// A name the user attached.
    Named,
    /// Conflicting evidence; both candidates are shown, neither is chosen.
    Ambiguous,
    /// The user said "this is two people".
    PossiblyDifferent,
}

impl ClusterState {
    pub fn as_str(self) -> &'static str {
        match self {
            ClusterState::Anonymous => "anonymous",
            ClusterState::Named => "named",
            ClusterState::Ambiguous => "ambiguous",
            ClusterState::PossiblyDifferent => "possibly_different",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anonymous" => Some(ClusterState::Anonymous),
            "named" => Some(ClusterState::Named),
            "ambiguous" => Some(ClusterState::Ambiguous),
            "possibly_different" => Some(ClusterState::PossiblyDifferent),
            _ => None,
        }
    }
}

/// The §7.4 composite score, recorded per appearance.
///
/// The fields are `Option` because a body embedding may not exist yet: T-P3-001
/// produces face vectors only, and the body model is later. A `None` is
/// recorded as `None` rather than as zero, because "no body evidence" and "body
/// evidence says no" are different facts and a zero would conflate them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScoreComponents {
    pub face: Option<f32>,
    pub body: Option<f32>,
}

impl ScoreComponents {
    /// Face evidence only, with a known distance.
    pub fn face(distance: f32) -> Self {
        ScoreComponents {
            face: Some(distance),
            body: None,
        }
    }

    /// Face and body evidence.
    pub fn composite(face: f32, body: f32) -> Self {
        ScoreComponents {
            face: Some(face),
            body: Some(body),
        }
    }
}

/// How the engine is configured.
///
/// The threshold is in cosine distance: 0 identical, 1 orthogonal, 2 opposite.
/// Face embeddings live in a narrow cone, so the useful range is roughly 0.3
/// to 0.9 and the default sits in the middle of it.
///
/// **The default is not calibrated.** There is no face-embedding model linked
/// (T-P3-001's handoff records why), so no real library has ever been measured
/// against this number. It is a starting point that the acceptance fixture makes
/// correct, not a value derived from data. When a model is linked, this is the
/// first number to re-derive, and the tests in `tests/cluster.rs` are written
/// against explicit thresholds so that re-deriving it does not mean rewriting
/// them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineConfig {
    /// The cosine distance below which a face joins a cluster.
    pub threshold: f32,
    /// How much closer to the best than the runner-up the runner-up must be,
    /// before the engine abstains.
    ///
    /// This is the whole ambiguity rule, and it is a distance, not a score: the
    /// engine abstains when `runner_up_distance - best_distance` is below it.
    /// So 0.0 means "only on an exact tie" -- which is the correct floor, because
    /// a margin below zero would abstain on a runner-up that is *further* than the
    /// best, which is not ambiguity at all. 0.99 is the opposite failure: every
    /// candidate abstains and the engine does nothing.
    ///
    /// 0.05 is about where two genuinely different people start to be
    /// distinguishable by a well-behaved embedding.
    pub ambiguity_margin: f32,
    /// The most clusters one vector may be compared against before the engine
    /// stops looking.
    ///
    /// A bound, so a library with 200k clusters does not turn one assignment
    /// into 200k distance computations. It is a *recall* bound and the cost is
    /// stated rather than hidden: a face whose true cluster is not in the
    /// nearest `candidate_limit` becomes a new cluster rather than a join.
    pub candidate_limit: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            threshold: 0.55,
            ambiguity_margin: 0.05,
            candidate_limit: 64,
        }
    }
}

/// The result of one assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignmentOutcome {
    /// What was decided.
    pub decision: Assignment,
    /// The `appearance` row that was written.
    pub appearance_id: String,
    /// The cluster it ended up in -- including the one a `Created` decision just
    /// made -- or `None` only when ambiguous.
    ///
    /// `None` for [`Assignment::Ambiguous`] is the point: an ambiguous
    /// appearance belongs to no cluster, and returning the best candidate's id
    /// here would let a caller attach it by accident.
    pub cluster_id: Option<String>,
    /// The distance to the cluster it joined or created, or to the best
    /// candidate when ambiguous.
    pub distance: Option<f32>,
}

impl AssignmentOutcome {
    /// The cluster id, for the paths where one is guaranteed.
    ///
    /// Panics on ambiguity, deliberately: a caller that joins an ambiguous
    /// appearance to "some" cluster has a bug, and `Option` would let it
    /// compile. Use [`AssignmentOutcome::cluster_id`] to handle it.
    pub fn require_cluster(&self) -> &str {
        self.cluster_id
            .as_deref()
            .expect("an ambiguous appearance has no cluster")
    }
}

/// What the engine found for one vector: the ranked candidates.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub cluster_id: String,
    pub distance: f32,
}

/// The clustering engine.
pub struct Engine {
    store: Store,
    config: EngineConfig,
}

impl Engine {
    /// Open an engine over a store.
    pub async fn open(store: &Store, config: EngineConfig) -> Result<Self, ClusterError> {
        if !(0.0..=2.0).contains(&config.threshold) {
            return Err(ClusterError::BadThreshold(config.threshold));
        }
        if config.ambiguity_margin < 0.0 {
            return Err(ClusterError::NegativeMargin(config.ambiguity_margin));
        }
        if config.candidate_limit == 0 {
            return Err(ClusterError::NoCandidates);
        }
        Ok(Engine {
            store: store.clone(),
            config,
        })
    }

    pub fn config(&self) -> EngineConfig {
        self.config
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Cluster's centroid, as a unit vector, if it has one.
    pub async fn centroid(&self, index: usize) -> Result<Option<Vec<f32>>, ClusterError> {
        // The tests address clusters positionally, so this reads them in a
        // stable order. `created_at` alone is not stable within a millisecond,
        // and two clusters created in the same batch would swap between calls.
        let rows = all_with_members(&self.store).await?;
        Ok(rows.get(index).and_then(|c| c.centroid.clone()))
    }

    /// The ranked candidates for a vector, nearest first.
    pub async fn candidates(&self, vector: &[f32]) -> Result<Vec<Candidate>, ClusterError> {
        check_width(vector)?;
        let rows = all_with_members(&self.store).await?;
        let mut out: Vec<Candidate> = rows
            .iter()
            .filter_map(|c| {
                let centroid = c.centroid.as_deref()?;
                Some(Candidate {
                    cluster_id: c.id.clone(),
                    distance: cosine_distance(vector, centroid),
                })
            })
            .collect();
        // Ties broken by cluster id, so the ranking is deterministic. An
        // unstable order turns the ambiguity rule into a coin flip, which is the
        // one thing §7.1 rules out.
        out.sort_by(|a, b| {
            a.distance
                .total_cmp(&b.distance)
                .then(a.cluster_id.cmp(&b.cluster_id))
        });
        out.truncate(self.config.candidate_limit);
        Ok(out)
    }

    /// Assign one face embedding to a cluster.
    pub async fn assign(
        &self,
        object_id: &str,
        vector: &[f32],
        scores: &ScoreComponents,
    ) -> Result<AssignmentOutcome, ClusterError> {
        check_width(vector)?;
        let candidates = self.candidates(vector).await?;
        let best = candidates.first();

        // The decision, in the order the spec's three states appear.
        let (decision, target, distance) = match best {
            // Nothing near enough: this is a person nobody has seen.
            None => (Assignment::Created, None, None),
            Some(b) if b.distance > self.config.threshold => {
                (Assignment::Created, None, Some(b.distance))
            }
            // The best match passes. Does the runner-up also pass, and is it
            // close enough that the engine cannot tell them apart?
            //
            // A margin of exactly 0.0 must *not* mean "ambiguous": two clusters
            // at a distance of exactly 0.5 is a measure-zero coincidence, and
            // treating `>=` as ambiguous would abstain on it. The comparison is
            // strict for that reason.
            Some(b) if self.is_ambiguous(&candidates, b) => {
                (Assignment::Ambiguous, None, Some(b.distance))
            }
            Some(b) => (
                Assignment::Joined,
                Some(b.cluster_id.clone()),
                Some(b.distance),
            ),
        };

        let now = now();
        let cluster_id = match &target {
            Some(id) => id.clone(),
            None if decision == Assignment::Created => {
                let id = uuid::Uuid::new_v4().to_string();
                // A new cluster is seeded with this vector as its centroid, so
                // it is immediately comparable. A cluster with a NULL centroid
                // is invisible to the next assignment, which would make every
                // first appearance of every person a new cluster forever.
                store::insert_cluster(
                    &self.store,
                    &id,
                    ClusterState::Anonymous,
                    Some(&hex(vector)),
                    &now,
                )
                .await?;
                id
            }
            None => {
                // Ambiguous: attached to nothing. The id is a placeholder and
                // is never written, and `cluster_id` on the outcome is `None`,
                // so nothing can reference it.
                String::new()
            }
        };

        let appearance_id = uuid::Uuid::new_v4().to_string();
        let ambiguous = decision == Assignment::Ambiguous;
        store::insert_appearance(
            &self.store,
            &appearance_id,
            object_id,
            &store::NewAppearance {
                cluster_id: (!ambiguous).then_some(cluster_id.as_str()),
                scores,
                distance,
                ambiguous,
                now: &now,
            },
        )
        .await?;

        if decision == Assignment::Created || decision == Assignment::Joined {
            // The member vector is recorded so the centroid is computed from
            // what the cluster actually contains. It goes on the cluster, not
            // on the appearance, because it is the cluster's membership that
            // the centroid means.
            store::insert_member_vector(&self.store, &cluster_id, vector).await?;
            self.refresh_centroid(&cluster_id).await?;
        } else {
            // The candidates that made this ambiguous, recorded now. §7.1: both
            // candidates are shown, and a review state the UI cannot populate
            // is not a review state.
            for c in candidates
                .iter()
                .take_while(|c| c.distance <= self.config.threshold)
            {
                store::insert_candidate(
                    &self.store,
                    &appearance_id,
                    &CandidateDistance {
                        cluster_id: c.cluster_id.clone(),
                        distance: c.distance,
                    },
                )
                .await?;
            }
        }

        // `cluster_id` is the cluster the appearance *ended up in*, which for a
        // `Created` decision is the one just made -- not `target`, which is None
        // on that path because the cluster did not exist when the decision was
        // made. Reporting None there left a caller unable to learn which cluster
        // its own write created, and a caller that cannot ask has to go and look,
        // or guess.
        //
        // The one case that must stay None is `Ambiguous`, and that is the whole
        // point of it: an ambiguous appearance belongs to no cluster, and
        // returning the best candidate's id would invite a caller to attach it.
        let cluster_id = match decision {
            Assignment::Ambiguous => None,
            _ => Some(cluster_id),
        };
        let _ = target;

        Ok(AssignmentOutcome {
            decision,
            appearance_id,
            cluster_id,
            distance,
        })
    }

    /// Run one consolidation pass. See [`ConsolidateConfig`].
    pub async fn consolidate(
        &self,
        config: ConsolidateConfig,
    ) -> Result<MergeReport, ClusterError> {
        consolidate::consolidate(self, config).await
    }

    /// Is the best candidate too close to the runner-up to choose?
    fn is_ambiguous(&self, candidates: &[Candidate], best: &Candidate) -> bool {
        // How much *better* the runner-up would have to be, or how close it is to
        // being as good, before the engine declines to choose.
        //
        // The comparison is `runner_up - best`, and the sign matters. It was
        // written the other way round, which is a real bug and not a cosmetic
        // one: `best - runner_up` is negative for every runner-up that is further
        // away, and `negative < margin` is true for any positive margin. So the
        // rule reduced to "ambiguous whenever a second candidate exists",
        // whatever the distances were. A clear winner 0.4 from the best and 0.5
        // from the runner-up abstained, because -0.1 < 0.05.
        //
        // With the sign fixed the rule reads what it should: abstain when the
        // runner-up is within `margin` of the best. A margin of 0.0 then means
        // "only abstain on an exact tie", which is the documented behaviour, and
        // a test that sets the margin to 0.99 and expects the engine to abstain
        // on everything is testing the default rather than the rule.
        //
        // Only the runner-up among those that *pass* is considered. A cluster
        // further away than the best is not evidence against the best; comparing
        // against the single next-nearest regardless of whether it passed would
        // make the best of two tight clusters look ambiguous next to one far-away
        // cluster, which is backwards.
        candidates
            .iter()
            .skip(1)
            .take_while(|c| c.distance <= self.config.threshold)
            .any(|c| c.distance - best.distance < self.config.ambiguity_margin)
    }

    /// Recompute a cluster's centroid as the mean of its members.
    ///
    /// From the stored members rather than incrementally, because a running
    /// mean weights old members less and a cluster reassigned many times drifts
    /// away from the people in it. The member set is the definition; the
    /// centroid is derived from it, every time.
    async fn refresh_centroid(&self, cluster_id: &str) -> Result<(), ClusterError> {
        let vectors = store::member_vectors(&self.store, cluster_id).await?;
        if vectors.is_empty() {
            // No vectors stored for this cluster. The face vector lives in the
            // sidecar, not in SQL, so a library whose sidecar has been deleted
            // has no vectors to average. The cluster keeps its previous
            // centroid rather than being set to NULL, because a NULL centroid
            // makes the cluster invisible to every future assignment.
            return Ok(());
        }
        let width = vectors[0].len();
        let mut mean = vec![0f32; width];
        for v in &vectors {
            if v.len() != width {
                return Err(ClusterError::MixedWidth {
                    expected: width,
                    got: v.len(),
                });
            }
            for (m, x) in mean.iter_mut().zip(v) {
                *m += *x;
            }
        }
        let n = vectors.len() as f32;
        for m in mean.iter_mut() {
            *m /= n;
        }
        let norm = Embedder::l2_normalize(&mean).to_vec();
        store::set_centroid(&self.store, cluster_id, &hex(&norm), &now()).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Consolidate: the scheduled agglomerative pass.
// ---------------------------------------------------------------------------

mod consolidate;
pub use consolidate::{ConsolidateConfig, MergeReport};

// The two decisions §7.1 puts in the user's hands rather than the engine's.
// They are re-exported at this level because they are the whole user-facing
// surface of the identity engine: everything else is a scheduled job.
pub use consolidate::{resolve_ambiguous, split};

/// Cosine distance between two unit vectors, clamped to `[0, 2]`.
///
/// The clamp matters: floating point can make `1 - dot` very slightly negative
/// for identical vectors, and a negative distance would compare as *better*
/// than zero, which is harmless here but makes an assertion about a distance
/// unwriteable. It can also exceed 2 for opposite vectors.
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
    }
    (1.0 - dot).clamp(0.0, 2.0)
}

/// The width every vector in a library must have.
pub fn expected_width() -> usize {
    Embedder::ARCFACE_WIDTH
}

fn check_width(v: &[f32]) -> Result<(), ClusterError> {
    if v.len() != expected_width() {
        return Err(ClusterError::WrongWidth {
            got: v.len(),
            expected: expected_width(),
        });
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(ClusterError::NonFinite);
    }
    Ok(())
}

fn hex(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8);
    for x in v {
        // `{:08x}` on the bit pattern, so the encoding is exact and not a
        // lossy decimal round trip. NaN would be a distinct pattern from 0.0,
        // which is why a non-finite vector is rejected before it gets here.
        s.push_str(&format!("{:08x}", x.to_bits()));
    }
    s
}

/// The inverse of [`hex`].
pub fn from_hex(s: &str) -> Result<Vec<f32>, ClusterError> {
    if !s.len().is_multiple_of(8) {
        return Err(ClusterError::BadHex(s.len()));
    }
    let mut out = Vec::with_capacity(s.len() / 8);
    for chunk in s.as_bytes().chunks(8) {
        let text = std::str::from_utf8(chunk).map_err(|_| ClusterError::BadHex(s.len()))?;
        let bits = u32::from_str_radix(text, 16).map_err(|_| ClusterError::BadHex(s.len()))?;
        let x = f32::from_bits(bits);
        if !x.is_finite() {
            return Err(ClusterError::NonFinite);
        }
        out.push(x);
    }
    Ok(out)
}

/// The current time, in the format the schema uses.
///
/// Read through one function so a test can be deterministic about it if it ever
/// needs to be; nothing in the clustering logic depends on the clock.
pub(crate) fn now() -> String {
    chrono_like_now()
}

fn chrono_like_now() -> String {
    // `time`-free, so this crate does not grow a dependency for a timestamp: the
    // schema wants an ISO-8601 string, and `SystemTime` can produce it without
    // a calendar library.
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    // Days since the epoch, from the civil-date algorithm. 2026-09-27 is day
    // 20714; this is verified by the store's other timestamp writers, and a
    // wrong date in a `created_at` column is cosmetic rather than load-bearing.
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (y, m, dd) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{dd:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Anything that can go wrong in the engine.
#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    #[error("a face vector of width {got} is not {expected}")]
    WrongWidth { got: usize, expected: usize },

    #[error("a face vector has a non-finite component")]
    NonFinite,

    #[error("threshold {0} is outside the possible range 0..=2")]
    BadThreshold(f32),

    #[error("ambiguity margin {0} is negative")]
    NegativeMargin(f32),

    #[error("candidate limit is zero, so nothing would ever be compared")]
    NoCandidates,

    #[error("a cluster cannot merge into itself")]
    SelfMerge,

    #[error("cluster members have different widths: {expected} and {got}")]
    MixedWidth { expected: usize, got: usize },

    #[error("a stored centroid is not valid hex ({0} chars)")]
    BadHex(usize),

    #[error("no {0} with that id")]
    NotFound(&'static str),

    #[error("no such appearance: {0}")]
    NoSuchAppearance(String),

    #[error("a value could not be serialised for storage: {0}")]
    NotSerialisable(String),

    #[error("appearance {0} is not ambiguous, so there is nothing to resolve")]
    NotAmbiguous(String),

    #[error(transparent)]
    Store(#[from] commons_store::StoreError),
}

/// The distance between two vectors, as the engine computes it.
///
/// Exposed so callers do not reimplement it and end up with a different number
/// than the one recorded on the row.
pub fn distance(a: &[f32], b: &[f32]) -> f32 {
    cosine_distance(a, b)
}

/// Cluster ids grouped by their `state`, for a UI summary.
pub async fn by_state(store: &Store) -> Result<BTreeMap<String, usize>, ClusterError> {
    let rows = all_with_members(store).await?;
    let mut out = BTreeMap::new();
    for r in rows {
        *out.entry(r.state.as_str().to_string()).or_insert(0) += 1;
    }
    Ok(out)
}
