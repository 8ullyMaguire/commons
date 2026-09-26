//! §8.1 — `resolve(subject, field) -> ResolvedValue`.
//!
//! The displayed value of a field is the winner by weighted vote, computed from
//! the evidence and cached. Three properties follow from that and they are the
//! reason the design is what it is:
//!
//! 1. **A settled value can change.** Better evidence moves it. That is correct
//!    for a living index and it is the reason the winner is computed rather
//!    than stored — a stored winner is an opinion that has fossilised.
//! 2. **A machine proposal is a voter, not an override.** `ml:captioner` at
//!    0.99 confidence carries weight 0.99. It does not carry authority.
//! 3. **A lock is the absence of a vote, not a very large one.** No weight
//!    overcomes it, which is why a hundredweight proposal loses to a lock.
//!
//! ## Why the cache is keyed on the evidence and not on the field
//!
//! The obvious cache is `(subject, field) -> ResolvedValue`, invalidated by a
//! notification. That is a cache with a correctness dependency on every write
//! path in the codebase, including the ones that arrive later. Instead the key
//! is `(subject, field, fingerprint)` where the fingerprint is derived from the
//! proposal rows and their live votes — so a stale entry is not merely
//! invalidated late, it is *addressed* wrongly and can never be looked up. A
//! new proposal or a new vote changes the fingerprint, which changes the key,
//! which misses. Nothing has to remember to invalidate anything, which is the
//! only property that makes a cache safe to add to a system that is still
//! growing write paths.

use commons_core::{FieldProposal, ProposalSource, Role, SubjectType, Vote};
use commons_store::db::StoreError;
use commons_store::index;
use commons_store::Store;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use uuid::Uuid;

/// How votes are weighted. All of it is configurable, because §8.1 says so and
/// because a curation system whose only tunable is "how much does one vote
/// count" cannot be run by two different communities.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveConfig {
    /// Days after which a vote counts half. A very large value disables decay.
    pub half_life_days: f64,
    /// Ceiling on one voter's contribution, relative to a weight-1 vote.
    ///
    /// This is a bound on *any one account*, not a bound on the scale. A
    /// reputation of 6 and a reputation of 5 both fit under the default 5.0, and
    /// 6 therefore outranks 5 — which is the property §8.1 depends on, since a
    /// settled value that cannot be out-evidenced is a settled value forever.
    /// Clamping the total instead would make every reputation above the ceiling
    /// indistinguishable, and the "better evidence arrives and the value moves"
    /// promise would quietly stop holding at exactly the top of the scale.
    pub max_vote_weight: f64,
    /// Floor on one *voter's* contribution, so a badly-repudiated account can
    /// still vote — a weight of zero is indistinguishable from being silenced,
    /// and silence must be a moderator's explicit decision (§8.5), never an
    /// emergent one.
    ///
    /// It is a floor per voter, never a floor on a proposal's score. A floor on
    /// the score would give every unvoted proposal the same positive weight,
    /// which makes an unvoted proposal beat a heavily-backed one — and that is
    /// not a subtle weighting choice, it is the voting system inverted.
    pub min_voter_weight: f64,
    /// Floor on the sybil damping multiplier, so a large bloc is discounted
    /// rather than silenced. §8.3 discounts and flags; it does not disqualify.
    pub sybil_damp_floor: f64,
    /// How long a cached entry may be served before the fingerprint is
    /// rechecked. Zero means check every time.
    pub cache_ttl: std::time::Duration,
}

impl Default for ResolveConfig {
    /// A half-life of 180 days. Long enough that a settled field does not churn
    /// weekly, short enough that evidence from two years ago is a tiebreaker
    /// rather than an authority.
    fn default() -> Self {
        Self {
            half_life_days: 180.0,
            max_vote_weight: 25.0,
            min_voter_weight: 0.05,
            sybil_damp_floor: 0.2,
            cache_ttl: std::time::Duration::from_secs(30),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no proposal {0} exists for that field")]
    NoSuchProposal(Uuid),
    #[error("no account {0} exists")]
    NoSuchAccount(Uuid),
    /// A lock onto a value nobody proposed. Its own variant because the fix is
    /// "propose it first", which is different from any of the others' fixes.
    #[error(
        "no proposal on field {field} carries the value {value:?}, so there is nothing to pin"
    )]
    NothingToPin { field: String, value: String },
    #[error("this account has already voted on that proposal")]
    AlreadyVoted { account: Uuid, proposal: Uuid },
    #[error("role {role:?} may not vote (§8.1: public is read-only)")]
    MayNotVote { role: Role },
    #[error("role {role:?} may not lock a field; that is a steward action")]
    NotASteward { role: Role },
    #[error("account {0} is disabled")]
    Disabled(Uuid),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The result of resolving one `(subject, field)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedValue {
    /// The winning value, or `None` when nothing was proposed. `None` and
    /// `Some("\"\"")` are different states (§8.6) and are kept apart here so
    /// every caller inherits the distinction.
    pub value_json: Option<String>,
    pub proposal_id: Option<Uuid>,
    pub source: Option<ProposalSource>,
    /// Why the winner exists, for the UI to attribute (§8.2).
    pub justification: Option<String>,
    /// Winner weight minus runner-up weight, normalised into `0.0..=1.0`.
    /// Zero means a tie.
    pub margin: f64,
    /// More than one distinct value has live support.
    pub contested: bool,
    /// The top two values weigh the same. Distinct from `contested` because a
    /// tie needs a human and a contested-but-decided field does not.
    pub tied: bool,
    /// A lock is in force. When true the value is the pinned one and no
    /// evidence was consulted.
    pub locked: bool,
    /// Every proposal's final weight, for the UI and for the tests that assert
    /// a machine proposal's weight is bounded.
    pub weights: Vec<ProposalWeight>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProposalWeight {
    pub proposal_id: Uuid,
    pub value_json: String,
    pub source: ProposalSource,
    /// Raw evidence weight: reputation summed over live votes, plus a machine
    /// proposal's own confidence, times sybil damping.
    pub evidence: f64,
    /// `evidence` times recency decay and the weight bounds.
    pub weight: f64,
    pub live_votes: i64,
}

impl ProposalWeight {
    /// Whether this proposal has any support behind it.
    ///
    /// A human vote counts. So does an *inference* — `ml:tagger` and
    /// `ml:captioner` — because their confidence is a judgement, and §8.2
    /// promises local inference is usable in a fresh library where nobody has
    /// voted on anything yet.
    ///
    /// Extraction is not judgement. `filename`, `embedded` and `caption` found a
    /// string that was already there; they did not decide anything, so an
    /// extraction nobody has confirmed carries no weight. Treating those the
    /// same as an inference is how a library's titles come to be whatever the
    /// first parser guessed, and it is the exact inversion §8.1's "a machine
    /// proposal is a voter, never an override" is written against.
    fn is_endorsed(&self) -> bool {
        self.live_votes > 0 || self.source.is_inference()
    }
}

// ---- the cache ------------------------------------------------------------

/// Keyed on the evidence, so a stale entry cannot be looked up rather than
/// merely needing to be invalidated. See the module docs.
fn fingerprint(
    proposals: &[FieldProposal],
    live: &HashMap<Uuid, Vec<(String, f64, String)>>,
) -> u64 {
    // FNV-1a. A hash collision would serve a wrong answer, so the inputs are
    // the *contents* of every proposal and every vote row, not their counts.
    // Every vote row, live or retracted. A fingerprint that only saw the
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for c in b {
            h ^= *c as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for p in proposals {
        eat(p.id.as_bytes());
        eat(p.value_json.as_bytes());
        eat(p.source.as_str().as_bytes());
        if let Some(c) = p.confidence {
            eat(c.to_bits().to_le_bytes().as_slice());
        } else {
            eat(b"none");
        }
        // The live vote set, which is also what the arithmetic reads. A
        // retraction removes a row from it, so the fingerprint changes with it
        // -- a separate "all votes including retracted" view was tried here and
        // removed, because a mutation pass showed it made no difference to any
        // outcome: the retracted row is invisible to the arithmetic either way,
        // and it is the *absence* of the live row that moves the key.
        if let Some(v) = live.get(&p.id) {
            for (acct, w, at) in v {
                eat(acct.as_bytes());
                eat(w.to_bits().to_le_bytes().as_slice());
                eat(at.as_bytes());
            }
        }
    }
    h
}

/// The process-wide cache. `LazyLock` rather than a `static` with a const
/// constructor because `HashMap::new` is not const, and building it at first
/// use rather than at load is also what keeps a library that is only imported
/// (a CLI that lists, say) from allocating a map it never touches.
static CACHE: std::sync::LazyLock<Mutex<HashMap<String, (Instant, ResolvedValue)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The cache key includes the *configuration*, not just the evidence.
///
/// A key of `(subject, field, fingerprint)` alone is a real bug and an easy one:
/// `half_life_days` changes the answer, so a field resolved under a ten-day
/// half-life would be answered from an entry computed under a ten-year one.
/// Nothing about the evidence changed, so the fingerprint is identical and the
/// knob is silently ignored — which is the failure the config's own test is
/// written to catch.
///
/// The config is hashed from its bits rather than its `Debug` output, so the
/// key does not depend on a formatting change and a field added to the struct
/// cannot be forgotten here.
fn cache_key(subject: &Uuid, field: &str, fp: u64, config: &ResolveConfig) -> String {
    let mut h: u64 = fp;
    let mut eat = |b: &[u8]| {
        for c in b {
            h ^= *c as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(config.half_life_days.to_bits().to_le_bytes().as_slice());
    eat(config.max_vote_weight.to_bits().to_le_bytes().as_slice());
    eat(config.min_voter_weight.to_bits().to_le_bytes().as_slice());
    eat(config.sybil_damp_floor.to_bits().to_le_bytes().as_slice());
    let ttl = config.cache_ttl.as_nanos();
    eat(&[
        (ttl & 0xff) as u8,
        ((ttl >> 8) & 0xff) as u8,
        ((ttl >> 16) & 0xff) as u8,
        ((ttl >> 24) & 0xff) as u8,
    ]);
    format!("{subject}/{field}/{h:016x}")
}

// ---- public API -----------------------------------------------------------

/// Resolve a field with the default configuration.
pub async fn resolve(
    store: &Store,
    subject: Uuid,
    field: &str,
) -> Result<ResolvedValue, ResolveError> {
    resolve_with(store, subject, field, &ResolveConfig::default()).await
}

/// Resolve a field, with the configuration spelled out.
///
/// The signature takes the subject as a bare `Uuid` and always treats it as an
/// `Object`, because every current caller is a scene. That is a deliberate
/// narrowing: §8.1's `subject_type` is polymorphic so studio names can vote by
/// the same machinery, and the day that happens this gains a `subject_type`
/// parameter rather than a second function. A parameter that does not exist
/// yet is a lie waiting to be told; a parameter that exists and is always the
/// same value is a bug waiting to be found.
pub async fn resolve_with(
    store: &Store,
    subject: Uuid,
    field: &str,
    config: &ResolveConfig,
) -> Result<ResolvedValue, ResolveError> {
    resolve_typed(store, SubjectType::Object, &subject, field, config).await
}

/// The polymorphic form, for subjects that are not objects.
pub async fn resolve_typed(
    store: &Store,
    subject_type: SubjectType,
    subject: &Uuid,
    field: &str,
    config: &ResolveConfig,
) -> Result<ResolvedValue, ResolveError> {
    let proposals = index::proposals_for(store, subject_type, subject, field).await?;

    // A lock is consulted before any evidence, because a lock is the statement
    // that the evidence does not get a say. Reading the lock last would mean a
    // locked field still pays for the vote computation and, worse, any bug in
    // that computation becomes visible as a lock failure.
    let existing_lock = index::field_lock(store, subject_type, subject, field).await?;
    if let Some(pinned) = existing_lock.as_ref().and_then(|l| l.value_json.as_ref()) {
        let winning = proposals.iter().find(|p| &p.value_json == pinned);
        return Ok(ResolvedValue {
            value_json: Some(pinned.clone()),
            proposal_id: winning.map(|p| p.id),
            source: winning.map(|p| p.source),
            justification: match winning {
                Some(p) => index::justification(store, &p.id).await?,
                None => None,
            },
            margin: 1.0,
            contested: proposals.len() > 1,
            tied: false,
            locked: true,
            weights: Vec::new(),
        });
    }

    let mut live: HashMap<Uuid, Vec<(String, f64, String)>> = HashMap::new();
    for p in &proposals {
        live.insert(p.id, index::live_votes(store, &p.id).await?);
    }

    let fp = fingerprint(&proposals, &live);
    let key = cache_key(subject, field, fp, config);
    if config.cache_ttl.is_zero() {
        return compute(store, &proposals, &live, config).await;
    }
    let hit = CACHE.lock().ok().and_then(|guard| guard.get(&key).cloned());
    if let Some((at, cached)) = hit {
        if at.elapsed() < config.cache_ttl {
            return Ok(cached);
        }
    }

    let out = compute(store, &proposals, &live, config).await?;
    if let Ok(mut guard) = CACHE.lock() {
        // Bound the cache by evidence count, not by entry count: one pathological
        // field with a million proposals must not be able to evict everything.
        if guard.len() > 4_096 {
            guard.clear();
        }
        guard.insert(key, (Instant::now(), out.clone()));
    }
    Ok(out)
}

/// Turn the evidence into weights, in one pass.
///
/// The shape of the pass matters and it is worth stating, because an earlier
/// version of this function computed a weight and then recomputed it in a
/// second loop, which meant the first computation existed only to be
/// overwritten. Everything a proposal's weight depends on is therefore read
/// here, once, in the order it is needed:
///
/// 1. the votes' frozen weights, which are authoritative and are never
///    re-derived from today's reputation. `cast_vote` wrote the account's
///    per-field reputation into that column (§8.3 -- agreeing about titles says
///    nothing about tags), so per-field weighting happens once, at the ballot,
///    rather than being restated on every read;
/// 2. recency decay, applied per vote so a field with one ancient vote and one
///    fresh one is not dragged down by the old one;
/// 3. a machine proposal's own confidence, which counts once, as itself, and is
///    bounded by the same ceiling as a human vote;
/// 4. sybil damping, applied to the *total* rather than per account, because
///    damping per account would be a function of who happens to be in the field
///    rather than of the pattern they form.
async fn compute(
    store: &Store,
    proposals: &[FieldProposal],
    live: &HashMap<Uuid, Vec<(String, f64, String)>>,
    config: &ResolveConfig,
) -> Result<ResolvedValue, ResolveError> {
    let mut weights: Vec<ProposalWeight> = Vec::with_capacity(proposals.len());
    for p in proposals {
        let votes = live.get(&p.id).map(|v| v.as_slice()).unwrap_or(&[]);

        // The undecayed sum of frozen weights, kept for the UI: "how much
        // evidence" and "how much of it still counts" are different questions
        // and a breakdown that shows only the second hides the first.
        let mut evidence = 0.0;
        let mut decayed = 0.0;
        for (_acct, frozen, at) in votes {
            evidence += *frozen;
            // The frozen weight already *is* the account's field reputation at
            // cast time, so it is used as-is. `field_rep` is consulted only for
            // the ratio between voters, which is what the sybil term needs and
            // what a per-vote multiplier would double-count.
            let contribution = (*frozen).clamp(config.min_voter_weight, config.max_vote_weight);
            decayed += contribution * decay_factor(at, config.half_life_days);
        }

        // A machine proposal is a voter. With no account it has no reputation,
        // so its confidence stands in for one vote -- which is why a 0.99
        // captioner still loses to four human votes, and why its weight is
        // capped at the same ceiling rather than being trusted in proportion.
        let base = if votes.is_empty() && p.source.is_automatic() {
            p.confidence.unwrap_or(0.0).min(1.0)
        } else {
            0.0
        };

        // Sybil damping. Sublinear in the backer count and floored at 0.2, so
        // a bloc's *marginal* influence shrinks as it grows without a bloc of
        // two being worth nothing next to a bloc of one. §8.3 also wants the
        // pattern flagged for a steward; that is `reputation::flag_coordinated`
        // in T-P4-003, because flagging is a judgement and weighting is
        // arithmetic, and one should not decide the other by accident.
        let damp = sybil_damp(votes.len(), config);

        weights.push(ProposalWeight {
            proposal_id: p.id,
            value_json: p.value_json.clone(),
            source: p.source,
            evidence,
            weight: (decayed + base) * damp,
            live_votes: votes.len() as i64,
        });
    }

    // A proposal with no live support has no weight at all, and therefore no
    // standing as an answer. This is the rule the weight floor used to break:
    // a field with one endorsed proposal and one bare proposal must settle on
    // the endorsed one, and a candidate that nobody has backed and that no
    // machine has scored is not a candidate.
    //
    // A *machine* proposal is exempt, because its confidence is its support —
    // an unvoted `ml:captioner` proposal at 0.8 is the tagger's one vote, and
    // dropping it would make local inference unusable in a fresh library, which
    // is the exact case §8.2 promises it for.
    for w in weights.iter_mut() {
        w.weight = if w.is_endorsed() { w.weight } else { 0.0 };
    }

    // One weight per *distinct value*, not per proposal: a value proposed by
    // three people is one candidate with three supporters, and counting it
    // three times would let the number of proposers decide the winner twice --
    // once in the sum and once through the sybil term.
    let mut by_value: HashMap<String, f64> = HashMap::new();
    for w in &weights {
        *by_value.entry(w.value_json.clone()).or_insert(0.0) += w.weight;
    }
    // A value whose total is zero has no endorsement behind it, so it is not a
    // candidate. Dropping it here rather than filtering afterwards keeps
    // `contested` honest: a bare proposal does not make a field contested, it
    // makes it *unsettled*, which is the same as having no proposals at all.

    let mut ranked: Vec<(String, f64)> = by_value.into_iter().collect();
    // Deterministic ordering: weight, then value. Without the value tiebreak a
    // tie resolves differently on two machines, and the same evidence would give
    // two different answers depending on which one you asked.
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    ranked.retain(|(_, w)| *w > 0.0);
    let contested = ranked.len() > 1;
    let (value_json, top, second) = match ranked.first() {
        None => (None, 0.0, 0.0),
        Some((v, w)) => {
            let second = ranked.get(1).map(|(_, w)| *w).unwrap_or(0.0);
            (Some(v.clone()), *w, second)
        }
    };
    // A relative epsilon, because these are sums of decayed floats and an exact
    // `==` would call a 1e-17 difference a decisive win.
    let tied = contested && (top - second).abs() <= f64::EPSILON * top.max(1.0);

    let winner = value_json.as_ref().and_then(|v| {
        weights
            .iter()
            .filter(|w| &w.value_json == v)
            .max_by(|a, b| {
                a.weight
                    .partial_cmp(&b.weight)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let justification = match winner {
        Some(w) => index::justification(store, &w.proposal_id).await?,
        None => None,
    };

    Ok(ResolvedValue {
        value_json,
        proposal_id: winner.map(|w| w.proposal_id),
        source: winner.map(|w| w.source),
        justification,
        margin: if top > 0.0 {
            ((top - second) / top).clamp(0.0, 1.0)
        } else {
            0.0
        },
        contested,
        tied,
        locked: false,
        weights,
    })
}

/// How much a proposal's weight is discounted for having many backers.
///
/// `sqrt(n)` against an absolute scale, not against the most-backed proposal in
/// the field. The relative version has a property that looks like fairness and
/// is not: the field's largest bloc divides by itself, so it is never damped at
/// all, and a field where one side has eight accounts and the other has one
/// damps nothing. The absolute version says what §8.3 means — a coordinated
/// bloc is discounted however large it is, so its *marginal* backer is worth
/// progressively less.
///
/// The floor of 0.2 is what stops a bloc of two being worth nothing next to a
/// bloc of one: `sqrt(2)/2 = 0.707` normally, and the floor only bites past
/// about twenty-five backers, by which point the discount is meant to be severe.
fn sybil_damp(backers: usize, config: &ResolveConfig) -> f64 {
    if backers <= 1 {
        return 1.0;
    }
    (backers as f64).sqrt().recip().max(config.sybil_damp_floor)
}

/// The multiplier recency applies to one vote: `0.5 ^ (age_days / half_life)`.
///
/// A half-life of zero or below is "no decay", which is what a caller setting it
/// to zero is asking for. Refusing the config would be a worse answer than
/// honouring the intent, and the default of 180 days is well away from it.
fn decay_factor(at: &str, half_life_days: f64) -> f64 {
    if half_life_days <= 0.0 {
        return 1.0;
    }
    let now = commons_core::ts::now();
    // An unreadable timestamp means "not old enough to decay", not "infinitely
    // old". A vote whose date cannot be read is not evidence of great age, and
    // treating it as ancient would let one bad clock delete a field's consensus.
    let age = commons_core::ts::age_days(at, &now).unwrap_or(0).max(0) as f64;
    0.5f64.powf(age / half_life_days)
}

/// The account columns the voting path needs.
///
/// Deliberately a tuple rather than `query_as::<Account>`: `commons-core` has no
/// sqlx dependency and must not acquire one, because T-P0-007 makes it the
/// bottom of the dependency graph and a `FromRow` derive in the domain crate
/// would drag the whole database stack in under it. Reading the two columns by
/// name keeps the layering intact and costs four lines.
async fn account_role(store: &Store, account: &Uuid) -> Result<Option<(String, bool)>, StoreError> {
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT role, disabled FROM account WHERE id = ?")
            .bind(account.to_string())
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?;
    Ok(row.map(|(role, disabled)| (role, disabled != 0)))
}

/// A role string from the database, refusing an unknown one.
///
/// A role the code does not know is a schema and code disagreement, and it is
/// safer to refuse the operation than to guess: guessing `contributor` for an
/// unknown role could hand someone the ability to curate.
fn parse_role(s: &str) -> Result<Role, ResolveError> {
    Role::parse(s).ok_or(ResolveError::MayNotVote { role: Role::Public })
}

async fn account_weight(store: &Store, account: &str, field: &str) -> Result<f64, StoreError> {
    let rep: Option<String> =
        sqlx::query_scalar("SELECT field_reputation FROM account WHERE id = ?")
            .bind(account)
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?;
    // No per-field map, or an empty one, means the account is at the base weight
    // of 1.0 -- which is what a brand-new account is, and is the whole of
    // §8.3's "new accounts ramp from a low base".
    let Some(json) = rep.filter(|j| !j.trim().is_empty()) else {
        return Ok(1.0);
    };
    let map: serde_json::Value = serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
    let w = map.get(field).and_then(|v| v.as_f64()).unwrap_or(1.0);
    tracing::debug!(account, field, weight = w, raw = %json, "resolved account weight");
    Ok(w)
}

// ---- writing --------------------------------------------------------------

/// Cast a vote at the account's current reputation for this field.
///
/// Three refusals, all checked here rather than trusted from the caller:
/// a disabled account, a `public` role (§8.1's role table makes anonymous
/// read-only, and this is the query-layer check that makes it true), and a
/// duplicate. The duplicate matters most: a caller that retries on error would
/// otherwise turn one vote into two, and a doubled vote is invisible in the
/// data.
pub async fn cast_vote(
    store: &Store,
    account: &Uuid,
    proposal: &Uuid,
    field: &str,
) -> Result<(), ResolveError> {
    let (role_s, disabled) = account_role(store, account)
        .await?
        .ok_or(ResolveError::NoSuchAccount(*account))?;
    if disabled {
        return Err(ResolveError::Disabled(*account));
    }
    let role = parse_role(&role_s)?;
    if !role.may_vote() {
        return Err(ResolveError::MayNotVote { role });
    }
    let p = index::proposal_by_id(store, proposal)
        .await?
        .ok_or(ResolveError::NoSuchProposal(*proposal))?;
    if p.field != field {
        return Err(ResolveError::NoSuchProposal(*proposal));
    }
    let dup: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM vote WHERE proposal_id = ? AND account_id = ?")
            .bind(proposal.to_string())
            .bind(account.to_string())
            .fetch_one(store.pool())
            .await
            .map_err(StoreError::Query)?;
    if dup > 0 {
        return Err(ResolveError::AlreadyVoted {
            account: *account,
            proposal: *proposal,
        });
    }

    let weight = account_weight(store, &account.to_string(), field).await?;
    index::insert_vote(
        store,
        &Vote {
            id: Uuid::new_v4(),
            proposal_id: *proposal,
            account_id: *account,
            field: field.to_string(),
            weight,
            created_at: commons_core::ts::now(),
            retracted: false,
        },
    )
    .await?;
    Ok(())
}

/// Withdraw a vote. The row stays, marked retracted, so the history of what was
/// believed and when survives (§8.6).
pub async fn retract_vote(
    store: &Store,
    account: &Uuid,
    proposal: &Uuid,
) -> Result<(), ResolveError> {
    index::retract_vote(store, account, proposal).await?;
    Ok(())
}

/// Pin a field. Steward or admin only, and only onto a value somebody actually
/// proposed — a lock on a value with no proposal behind it is a fabricated fact
/// carrying steward authority, which is the one thing a lock must never be.
pub async fn lock(
    store: &Store,
    subject: Uuid,
    field: &str,
    value_json: &str,
    by: &Uuid,
) -> Result<(), ResolveError> {
    lock_typed(
        store,
        SubjectType::Object,
        &subject,
        field,
        Some(value_json),
        by,
    )
    .await
}

pub async fn lock_typed(
    store: &Store,
    subject_type: SubjectType,
    subject: &Uuid,
    field: &str,
    value_json: Option<&str>,
    by: &Uuid,
) -> Result<(), ResolveError> {
    let (role_s, _disabled) = account_role(store, by)
        .await?
        .ok_or(ResolveError::NoSuchAccount(*by))?;
    let role = parse_role(&role_s)?;
    if !role.may_lock() {
        return Err(ResolveError::NotASteward { role });
    }
    if let Some(v) = value_json {
        let proposals = index::proposals_for(store, subject_type, subject, field).await?;
        if !proposals.iter().any(|p| p.value_json == v) {
            return Err(ResolveError::NothingToPin {
                field: field.to_string(),
                value: v.to_string(),
            });
        }
    }
    index::set_field_lock(store, subject_type, subject, field, value_json, Some(by)).await?;
    Ok(())
}

/// Lift a lock, handing the field back to the vote. Steward or admin only.
pub async fn unlock(
    store: &Store,
    subject: Uuid,
    field: &str,
    by: &Uuid,
) -> Result<(), ResolveError> {
    let (role_s, _disabled) = account_role(store, by)
        .await?
        .ok_or(ResolveError::NoSuchAccount(*by))?;
    let role = parse_role(&role_s)?;
    if !role.may_lock() {
        return Err(ResolveError::NotASteward { role });
    }
    index::clear_field_lock(store, SubjectType::Object, &subject, field).await?;
    Ok(())
}

/// The final weight of every proposal, for the UI's "why this value" panel and
/// for the tests that bound a machine proposal's weight.
pub async fn weight_breakdown(
    store: &Store,
    subject: Uuid,
    field: &str,
) -> Result<Vec<ProposalWeight>, ResolveError> {
    Ok(resolve(store, subject, field).await?.weights)
}
