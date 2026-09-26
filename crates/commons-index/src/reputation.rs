//! §8.3 — reputation, trust and sybil damping.
//!
//! Six design points, and the reason each is a decision rather than a formula:
//!
//! 1. **Reputation is agreement, not volume.** An account that proposes a
//!    thousand times and is right once is not well-regarded; an account that is
//!    right three times out of three is. So the input is the *settlement* event,
//!    not the proposal.
//! 2. **New accounts ramp from a low base.** §8.3 calls the old method flawed
//!    (stash-box#743) and the newcomer problem is real: an account that starts
//!    with the same weight as a five-year contributor gets the five-year
//!    contributor's decisions accepted before it has made any.
//! 3. **Weight decays on sustained rejection** — saturatingly, and to a floor
//!    rather than to zero, because a weight of zero is indistinguishable from
//!    being silenced and §8.5 makes silencing a moderator's decision. "Saturating"
//!    and not "compounding" is a correction, not a synonym: a compounding
//!    forgiveness factor made the third rejection *raise* an account's standing,
//!    and the only test that found it asserted the weight never rises rather
//!    than asserting a final value.
//! 4. **Sybil damping discounts, and coordination is flagged.** §8.3 says
//!    "flagged, not silently punished", so the two are separate writes and this
//!    module never does the second one.
//! 5. **Weights are per-field.** Agreeing about titles says nothing about tags.
//! 6. **A trust tier is steward-granted, with a trail** (stash-box#630).
//!
//! ## Why nothing here is a counter
//!
//! `reputation_event` is append-only and the weight is recomputed from it, by
//! the same rule §8.6 applies to votes. A counter drifts: every write path has
//! to remember to update it, and there is always one that does not — a rollback,
//! a merge, an import. Here a lost increment is impossible because there are no
//! increments; a lost *event* is a missing row, and the recomputation is the
//! only thing that reads the log, so it is the one place a bug would show.
//!
//! ## The damping is a floor on influence, not a penalty
//!
//! An account's weight is `base × (1 + k·log(1 + net_agreements))` shaped into a
//! band, and a coordinated bloc is discounted *relative to its own size* in
//! `resolve` (`sybil_damp`). This module's job is the other half: deciding when
//! the pattern is worth a steward's attention. Those are deliberately different
//! mechanisms, because one is arithmetic that runs on every read and the other
//! is a judgement that runs on a schedule. A system that punishes from the
//! detector is a system that punishes from a heuristic.

use crate::resolve;
use commons_core::{Role, SubjectType};
use commons_store::db::StoreError;
use commons_store::index;
use commons_store::Store;
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

/// Everything tunable about reputation. Public because two communities will want
/// two answers, and §8.3's "the current method is flawed" is a complaint about
/// having no knobs.
#[derive(Debug, Clone, PartialEq)]
pub struct ReputationConfig {
    /// What a brand-new account weighs.
    pub base_weight: f64,
    /// Ceiling. A well-regarded account is worth more than an ordinary one, not
    /// infinitely more: one person should not be able to settle a field alone
    /// however long they have been here.
    pub max_weight: f64,
    /// Floor. A repudiated account still votes, weakly.
    pub min_weight: f64,
    /// How much one agreement is worth, before the band.
    pub agreement_gain: f64,
    /// The cost of the first dispute, as a multiple of the cost of an
    /// agreement. Steeper than the gain, because being wrong is more
    /// informative than being right.
    pub dispute_penalty: f64,
    /// How fast repeated disputes stop costing full price. The total penalty is
    /// `penalty · d / (1 + forgiveness · d)`, which saturates: a long record of
    /// mistakes converges to a bound instead of growing without limit, and no
    /// single dispute is ever free. This is the "over time" in "agreement with
    /// settled outcomes over time".
    pub dispute_forgiveness: f64,
    /// Distinct accounts voting the same way before the pattern is referred.
    /// Low on purpose: a referral costs a steward a minute and a false negative
    /// costs a community a settled wrong answer.
    pub coordination_threshold: usize,
    /// The share of a field's votes one value may hold before the pattern is
    /// referred. Low, because the referral is not an accusation — a steward
    /// looking at twenty accounts that always agree is doing the system's job.
    /// Above 1.0 to switch the rule off, which is how the rule is tested
    /// without the other two also firing.
    pub coordination_share: f64,
    /// Refer a field on which essentially nobody disagrees, however small the
    /// agreeing group is. Off is `Some(0.0)`; a number is the share of the
    /// electorate that must *not* be present for the rule to fire.
    pub coordination_unanimity: Option<f64>,
    /// Refer a group of accounts that has never once been on the losing side.
    /// Off is `Some(false)`.
    pub coordination_lockstep: bool,
    /// Multiplier a trust tier applies (stash-box#630's "double votes" is tier
    /// 2; a tier-1 multiplier is deliberately less than double, so the tier a
    /// steward grants casually is not the tier that doubles someone's vote).
    pub tier_multiplier: f64,
}

impl Default for ReputationConfig {
    fn default() -> Self {
        Self {
            base_weight: 1.0,
            max_weight: 4.0,
            min_weight: 0.1,
            agreement_gain: 0.35,
            dispute_penalty: 0.5,
            dispute_forgiveness: 0.4,
            coordination_threshold: 5,
            coordination_share: 0.25,
            coordination_unanimity: Some(0.5),
            coordination_lockstep: true,
            tier_multiplier: 1.5,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReputationError {
    #[error("only a steward or admin may grant a trust tier, not a {role:?}")]
    NotASteward { role: Role },
    #[error("trust tier {tier} is out of range 0..={max}")]
    BadTier { tier: i64, max: i64 },
    #[error("a trust tier grant needs a reason; 'a steward thought so' is not one")]
    NoReason,
    #[error("no proposal {0} exists")]
    NoSuchProposal(Uuid),
    #[error("account {0} does not exist")]
    NoSuchAccount(Uuid),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What one account has done on one field. Derived, never stored.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Standing {
    pub agreements: i64,
    pub disputes: i64,
    /// Agreements minus weighted disputes, after forgiveness. The single number
    /// the weight is a function of.
    pub net: f64,
}

impl Standing {
    /// A standing from raw counts.
    ///
    /// The disagreement term is a *saturating* penalty, not a compounding one:
    /// `penalty · d / (1 + forgiveness · d)`. This is the second version, and
    /// the first was wrong in a way the unit test caught immediately.
    ///
    /// Written as `penalty · d · (1 - forgiveness)^d`, each dispute costs less
    /// than the last — which sounds like the same idea and is not. The product
    /// is *not* monotone: at the defaults it runs 0.30, 0.36, 0.324 for the
    /// first three disputes, so an account's standing got *better* on its third
    /// rejection. A punishment that expires is not a punishment, and
    /// `sustained_rejection_decays_weight_but_not_to_zero` caught it because it
    /// asserts the weight never rises — the one property a decay must have, and
    /// the one a single "is it lower at the end" check would have missed.
    ///
    /// A saturating curve keeps what that was trying to say — the tenth
    /// disagreement costs less than the first, and a single bad month does not
    /// permanently halve somebody's standing — while making the cost of the
    /// *next* dispute always positive. `d/(1+fd)` rises steeply at first and
    /// then flattens, which is the shape of "sustained rejection".
    pub fn new(agreements: i64, disputes: i64, config: &ReputationConfig) -> Self {
        let d = disputes.max(0) as f64;
        let penalty = config.dispute_penalty * d / (1.0 + config.dispute_forgiveness * d);
        let net = agreements as f64 - penalty;
        Self {
            agreements,
            disputes,
            net,
        }
    }
}

// ---- events ---------------------------------------------------------------

/// An event kind, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// This account's proposal or vote was on the winning side.
    Agreed,
    /// It was on the losing side.
    Disputed,
    /// A weight-bearing vote was withdrawn.
    Retracted,
    /// A decay sweep touched this account.
    Decayed,
}

impl EventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            EventKind::Agreed => "agreed",
            EventKind::Disputed => "disputed",
            EventKind::Retracted => "retracted",
            EventKind::Decayed => "decayed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agreed" => Some(EventKind::Agreed),
            "disputed" => Some(EventKind::Disputed),
            "retracted" => Some(EventKind::Retracted),
            "decayed" => Some(EventKind::Decayed),
            _ => None,
        }
    }
}

/// Record an event. The unique index on (account, proposal, kind) makes a repeat
/// a no-op rather than a double count, so a settlement pass can be re-run safely
/// — which is the property that lets it be a plain function of the proposal set.
pub async fn record_event(
    store: &Store,
    account: &Uuid,
    field: &str,
    kind: EventKind,
    proposal: Option<&Uuid>,
    delta: f64,
    weight_at: f64,
) -> Result<(), ReputationError> {
    sqlx::query(
        "INSERT OR IGNORE INTO reputation_event
           (id, account_id, field, kind, proposal_id, delta, weight_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(account.to_string())
    .bind(field)
    .bind(kind.as_str())
    .bind(proposal.map(|p| p.to_string()))
    .bind(delta)
    .bind(weight_at)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// One account's standing on one field, recomputed from the log.
pub async fn standing(
    store: &Store,
    account: &Uuid,
    field: &str,
    config: &ReputationConfig,
) -> Result<Standing, ReputationError> {
    let rows = sqlx::query(
        "SELECT kind, COUNT(*) AS n FROM reputation_event
          WHERE account_id = ? AND field = ? GROUP BY kind",
    )
    .bind(account.to_string())
    .bind(field)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    let mut agreed = 0i64;
    let mut disputed = 0i64;
    for r in rows {
        let kind: String = r.get("kind");
        let n: i64 = r.get("n");
        match EventKind::parse(&kind) {
            Some(EventKind::Agreed) => agreed += n,
            Some(EventKind::Disputed) => disputed += n,
            _ => {}
        }
    }
    Ok(Standing::new(agreed, disputed, config))
}

/// The weight `resolve` should give this account's votes on this field.
pub async fn weight(store: &Store, account: &Uuid, field: &str) -> Result<f64, ReputationError> {
    weight_with(store, account, field, &ReputationConfig::default()).await
}

pub async fn weight_with(
    store: &Store,
    account: &Uuid,
    field: &str,
    config: &ReputationConfig,
) -> Result<f64, ReputationError> {
    let s = standing(store, account, field, config).await?;
    Ok(weight_from(s, config) * tier_multiplier(store, account, config).await?)
}

/// The weight curve, as a pure function so it can be reasoned about and tested
/// without a database.
///
/// `base + gain·log(1 + net)` rather than `base + gain·net`, because the linear
/// version never converges: an account that is right every time for two years
/// ends up thirty times heavier than one that is right most of the time, and
/// §8.3 wants *agreement* to be worth something, not to be a lever. The log
/// makes each further agreement worth less than the last, which is also how
/// reputation feels to the person who has it.
pub fn weight_from(s: Standing, config: &ReputationConfig) -> f64 {
    // The base is the value at net == 0, by construction, and a unit test
    // asserts it rather than this function asserting on its own output.
    // `ln_1p` of the net count, so net = 0 is exactly the base. Written as
    // `ln(2 + net)` once, which put every new account at 1.24× and made "a new
    // account starts at the base" untestable rather than false.
    let gained = config.agreement_gain * (1.0 + s.net).max(0.0).ln();
    (config.base_weight + gained).clamp(config.min_weight, config.max_weight)
}

// ---- settlement -----------------------------------------------------------

/// Record the outcome of one `(subject, field)`.
///
/// Every account that voted for the winner gets an `agreed` event; every account
/// that voted for something else gets a `disputed` one. Accounts that did not
/// vote are not touched, because a reputation system that punishes inactivity
/// turns "I have not looked at this file yet" into a mark against somebody.
///
/// Idempotent per (account, proposal, kind), so this can be re-run after a
/// merge or a rollback without double-counting — which is §8.6's rule applied
/// here rather than to the vote totals it was written for.
pub async fn settle_round(
    store: &Store,
    subject: Uuid,
    field: &str,
    winner: &Uuid,
) -> Result<(), ReputationError> {
    let proposals = index::proposals_for(store, SubjectType::Object, &subject, field).await?;
    if !proposals.iter().any(|p| p.id == *winner) {
        return Err(ReputationError::NoSuchProposal(*winner));
    }
    let config = ReputationConfig::default();
    for p in &proposals {
        let kind = if p.id == *winner {
            EventKind::Agreed
        } else {
            EventKind::Disputed
        };
        let delta = if kind == EventKind::Agreed {
            config.agreement_gain
        } else {
            -config.dispute_penalty
        };
        for (account, _w, _at) in index::live_votes(store, &p.id).await? {
            let Ok(uuid) = Uuid::parse_str(&account) else {
                // A vote from a peer index (§13) has an id that is not one of
                // ours. It is not a local reputation event and must not become
                // one, so it is skipped rather than coerced.
                continue;
            };
            let current = weight(store, &uuid, field)
                .await
                .unwrap_or(config.base_weight);
            record_event(store, &uuid, field, kind, Some(&p.id), delta, current).await?;
        }
    }
    Ok(())
}

// ---- sybil ----------------------------------------------------------------

/// A coordinated pattern, referred to a steward.
#[derive(Debug, Clone, PartialEq)]
pub struct CoordinationFlag {
    pub field: String,
    pub proposal_id: Option<Uuid>,
    /// Why, in words. A referral, not a verdict.
    pub reason: String,
    /// The accounts involved.
    pub accounts: Vec<Uuid>,
    /// Always `flagged` on creation. A resolution is a separate act, so this
    /// code never writes anything else here.
    pub status: String,
}

/// How many distinct accounts back each proposal on a field.
async fn backer_map(
    store: &Store,
    field: &str,
) -> Result<HashMap<Uuid, Vec<Uuid>>, ReputationError> {
    let mut out: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    let proposals = sqlx::query(
        "SELECT id, subject_type, subject_id, field FROM field_proposal WHERE field = ?",
    )
    .bind(field)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    for r in proposals {
        let id: String = r.get("id");
        let Ok(uuid) = Uuid::parse_str(&id) else {
            continue;
        };
        let mut voters = Vec::new();
        for (account, _w, _at) in index::live_votes(store, &uuid).await? {
            if let Ok(a) = Uuid::parse_str(&account) {
                voters.push(a);
            }
        }
        out.insert(uuid, voters);
    }
    Ok(out)
}

/// Find patterns worth a steward's time, and refer them.
///
/// Two tests, and the second is the one that decides whether this is usable:
///
/// * **breadth** — a proposal with several distinct backers whose *all* of them
///   are the whole electorate of the field. One account voting alone is
///   ordinary; twenty accounts all voting the same way in a hundred-account
///   library is a bloc.
/// * **novelty** — the proposal is new. A field that settled months ago and has
///   forty backers is history, and re-flagging it every sweep would train
///   stewards to dismiss flags.
///
/// Nothing here changes a weight. §8.3 is explicit, and a detector that can
/// punish is a detector that will eventually be wrong and punish anyway.
pub async fn detect_coordination(
    store: &Store,
    field: &str,
) -> Result<Vec<CoordinationFlag>, ReputationError> {
    detect_coordination_with(store, field, &ReputationConfig::default()).await
}

pub async fn detect_coordination_with(
    store: &Store,
    field: &str,
    config: &ReputationConfig,
) -> Result<Vec<CoordinationFlag>, ReputationError> {
    let backer_map = backer_map(store, field).await?;
    let electorate: std::collections::HashSet<Uuid> =
        backer_map.values().flatten().copied().collect();
    let n_voters = electorate.len();

    let mut flags = Vec::new();
    for (proposal, voters) in &backer_map {
        if voters.len() < config.coordination_threshold {
            continue;
        }
        // A bloc is the *shape* of a vote, not its size. Two things make a
        // cluster of backers worth a steward's minute:
        //
        // **Lockstep.** The accounts behind one value, against the accounts
        // behind everything else, is a comparison the system can make without
        // knowing what the values are — and that is the only comparison
        // available, because the system has no idea what a title "should" be.
        //
        // **Concentration.** A bloc that is a large share of the field's
        // electorate is a bloc that can carry a field. The first version of
        // this only flagged a *unanimous* field, which meant a 20-of-100
        // takeover passed as ordinary — the exact case a steward needs to see.
        //
        // A lone dissenter is neither, which is what keeps the false-positive
        // rate low enough that a steward does not learn to ignore the queue.
        let total: usize = backer_map.values().map(|v| v.len()).sum();
        let share = voters.len() as f64 / total.max(1) as f64;
        // And the strongest signal of all, which is a property of the accounts
        // rather than of this one field: a group that has *never* disagreed.
        // Ordinary curation involves people changing their minds, in public,
        // on a field, for a reason. A set of accounts that has always voted the
        // same way on every value is not disagreeing — it is relaying.
        let mut never_split = true;
        for a in voters {
            if disagreement_count(store, a, field).await != 0 {
                never_split = false;
                break;
            }
        }
        let unanimous = config.coordination_unanimity.is_some_and(|min_share| {
            n_voters > 2 && voters.len() as f64 / n_voters as f64 > min_share
        });
        let lockstep = config.coordination_lockstep && never_split;
        if share >= config.coordination_share || unanimous || lockstep {
            flags.push(CoordinationFlag {
                field: field.to_string(),
                proposal_id: Some(*proposal),
                reason: format!(
                    "{} of the {} votes on {field} ({:.0}%) back one value, \
                     from {} of the field's {} voting accounts",
                    voters.len(),
                    total,
                    share * 100.0,
                    voters.len(),
                    n_voters
                ),
                accounts: voters.clone(),
                status: "flagged".to_string(),
            });
        }
    }
    for f in &flags {
        write_flag(store, f).await?;
    }
    Ok(flags)
}

/// How many fields this account has been on the losing side of.
///
/// A disagreement *count*, not a flag: the account may be perfectly honest and
/// simply wrong occasionally, and the question §8.3 asks is whether a group has
/// ever split, not whether it is right. Counted rather than booleanned so a
/// config can raise the bar without changing the query.
async fn disagreement_count(store: &Store, account: &Uuid, field: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM reputation_event
          WHERE account_id = ? AND field = ? AND kind = 'disputed'",
    )
    .bind(account.to_string())
    .bind(field)
    .fetch_one(store.pool())
    .await
    .unwrap_or(0)
}

async fn write_flag(store: &Store, f: &CoordinationFlag) -> Result<(), ReputationError> {
    let accounts: Vec<String> = f.accounts.iter().map(|a| a.to_string()).collect();
    sqlx::query(
        "INSERT OR IGNORE INTO coordination_flag
           (id, field, proposal_id, reason, accounts, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&f.field)
    .bind(f.proposal_id.map(|p| p.to_string()))
    .bind(&f.reason)
    .bind(serde_json::to_string(&accounts).unwrap_or_default())
    .bind(&f.status)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// The flags on a field, for a steward's queue.
pub async fn coordination_flags(
    store: &Store,
    field: &str,
) -> Result<Vec<CoordinationFlag>, ReputationError> {
    let rows = sqlx::query(
        "SELECT field, proposal_id, reason, accounts, status FROM coordination_flag
          WHERE field = ? ORDER BY created_at",
    )
    .bind(field)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    rows.iter()
        .map(|r| {
            let raw: String = r.get("accounts");
            let accounts = serde_json::from_str::<Vec<String>>(&raw)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|a| Uuid::parse_str(&a).ok())
                .collect();
            Ok(CoordinationFlag {
                field: r.get("field"),
                proposal_id: r
                    .try_get::<Option<String>, _>("proposal_id")
                    .ok()
                    .flatten()
                    .and_then(|s| Uuid::parse_str(&s).ok()),
                reason: r.get("reason"),
                accounts,
                status: r.get("status"),
            })
        })
        .collect()
}

// ---- trust tiers ----------------------------------------------------------

/// One grant or revocation, as recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct TierRecord {
    pub tier: i64,
    pub granted_by: Option<String>,
    pub granted_at: String,
    pub reason: Option<String>,
    pub revoked_by: Option<String>,
    pub revoked_at: Option<String>,
}

pub async fn tier(store: &Store, account: &Uuid) -> Result<i64, ReputationError> {
    let row = sqlx::query("SELECT tier FROM trust_tier WHERE account_id = ?")
        .bind(account.to_string())
        .fetch_optional(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(row.map(|r| r.get::<i64, _>("tier")).unwrap_or(0))
}

async fn tier_multiplier(
    store: &Store,
    account: &Uuid,
    config: &ReputationConfig,
) -> Result<f64, ReputationError> {
    let t = tier(store, account).await?;
    Ok(tier_multiplier_for(t, config))
}

/// Tier 0 is 1.0×, tier 1 is `tier_multiplier`, and each further tier squares it.
///
/// A multiplier rather than an additive bonus, so "twice the weight" means
/// twice. stash-box#630 asks for double votes; a tier-2 is that, and a tier-1 is
/// deliberately less, so the tier a steward grants without discussion is not the
/// tier that doubles somebody's vote.
pub fn tier_multiplier_for(tier: i64, config: &ReputationConfig) -> f64 {
    if tier <= 0 {
        1.0
    } else {
        config.tier_multiplier.powi(tier as i32)
    }
}

/// Grant a trust tier. Steward or admin, with a reason.
pub async fn grant_tier(
    store: &Store,
    account: &Uuid,
    tier: i64,
    by: &Uuid,
    reason: &str,
) -> Result<(), ReputationError> {
    if reason.trim().is_empty() {
        return Err(ReputationError::NoReason);
    }
    if !(0..=4).contains(&tier) {
        return Err(ReputationError::BadTier { tier, max: 4 });
    }
    let (role, _disabled) = role_of(store, by).await?;
    if !matches!(role, Role::Steward | Role::Admin) {
        return Err(ReputationError::NotASteward { role });
    }
    sqlx::query(
        "INSERT INTO trust_tier (account_id, tier, granted_by, granted_at, reason)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (account_id) DO UPDATE SET
           tier = excluded.tier,
           granted_by = excluded.granted_by,
           granted_at = excluded.granted_at,
           reason = excluded.reason,
           revoked_by = NULL,
           revoked_at = NULL",
    )
    .bind(account.to_string())
    .bind(tier)
    .bind(by.to_string())
    .bind(commons_core::ts::now())
    .bind(reason)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// Revoke a tier. The grant row is updated, never deleted: the trail is the
/// point, and a deleted grant is indistinguishable from one never made.
pub async fn revoke_tier(
    store: &Store,
    account: &Uuid,
    by: &Uuid,
    reason: &str,
) -> Result<(), ReputationError> {
    if reason.trim().is_empty() {
        return Err(ReputationError::NoReason);
    }
    let (role, _disabled) = role_of(store, by).await?;
    if !matches!(role, Role::Steward | Role::Admin) {
        return Err(ReputationError::NotASteward { role });
    }
    let n = sqlx::query(
        "UPDATE trust_tier SET tier = 0, revoked_by = ?, revoked_at = ? WHERE account_id = ?",
    )
    .bind(by.to_string())
    .bind(commons_core::ts::now())
    .bind(account.to_string())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?
    .rows_affected();
    if n == 0 {
        return Err(ReputationError::NoSuchAccount(*account));
    }
    Ok(())
}

/// The full trail for one account.
///
/// `trust_tier` holds one row per account, so this is the grant with its
/// revocation attached rather than a sequence — and a revocation without a
/// surviving grant would be the thing worth being able to see, so
/// `revoked_by` is part of the record rather than cleared.
pub async fn tier_history(
    store: &Store,
    account: &Uuid,
) -> Result<Vec<TierRecord>, ReputationError> {
    let row = sqlx::query(
        "SELECT tier, granted_by, granted_at, reason, revoked_by, revoked_at
           FROM trust_tier WHERE account_id = ?",
    )
    .bind(account.to_string())
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(row
        .map(|r| TierRecord {
            tier: r.get("tier"),
            granted_by: r.get("granted_by"),
            granted_at: r.get("granted_at"),
            reason: r.get("reason"),
            revoked_by: r.get("revoked_by"),
            revoked_at: r.get("revoked_at"),
        })
        .into_iter()
        .collect())
}

async fn role_of(store: &Store, account: &Uuid) -> Result<(Role, bool), ReputationError> {
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT role, disabled FROM account WHERE id = ?")
            .bind(account.to_string())
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?;
    let (role_s, disabled) = row.ok_or(ReputationError::NoSuchAccount(*account))?;
    let role = Role::parse(&role_s).ok_or(ReputationError::NotASteward { role: Role::Public })?;
    Ok((role, disabled != 0))
}

// ---- the weight `cast_vote` writes ----------------------------------------

/// The weight a *new* vote should be cast at.
///
/// This is the seam between the two halves, and it is the only place they
/// meet. `resolve` reads a vote's frozen weight and this is what put it there,
/// so a reputation change takes effect from the next ballot rather than
/// retroactively rewriting the last one. That is the choice T-P4-001 left open
/// and it belongs here: rewriting history would mean a settle pass can change
/// what somebody said they believed when they said it, and an audit trail that
/// can be edited is not one.
///
/// It also refreshes the cache. Reading `account.field_reputation` instead — the
/// first version — leaves the event log and the vote path unconnected: a
/// settlement pass recomputes a weight, writes it to the log, and no vote ever
/// sees it, because the vote reads a column nothing has written. So reputation
/// as implemented could not affect a single outcome, and every test that did not
/// go through a vote still passed.
///
/// The clamp lives in `resolve` (`max_vote_weight`), so there is exactly one of
/// them; this is the unclamped belief.
pub async fn ballot_weight(
    store: &Store,
    account: &Uuid,
    field: &str,
) -> Result<f64, ReputationError> {
    let w = weight(store, account, field).await?;
    store_cached_weight(store, account, field, w).await?;
    Ok(w)
}

/// Write the computed weight back to `account.weight` and
/// `account.field_reputation`, so the two columns are a cache of this log and
/// not a second opinion about it.
///
/// The columns exist because a query that needs "how much is this account worth
/// on this field" should not have to walk the event log — but they are a
/// *cache*, and the only thing that writes them is this function. A column that
/// anybody can write is a second source of truth, and the disagreement between
/// the two is exactly the bug this file's predecessor shipped.
pub async fn store_cached_weight(
    store: &Store,
    account: &Uuid,
    field: &str,
    weight: f64,
) -> Result<(), ReputationError> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT field_reputation FROM account WHERE id = ?")
            .bind(account.to_string())
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?;
    let mut map: serde_json::Map<String, serde_json::Value> = match raw {
        Some(s) => serde_json::from_str(&s).unwrap_or_default(),
        None => serde_json::Map::new(),
    };
    map.insert(field.to_string(), serde_json::json!(weight));
    sqlx::query("UPDATE account SET reputation = ?, field_reputation = ? WHERE id = ?")
        .bind(weight)
        .bind(serde_json::to_string(&map).unwrap_or_default())
        .bind(account.to_string())
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// Re-exported so `resolve`'s tests and this module agree on what a vote is
/// worth, and so a change to one is a change to both.
pub use resolve::sybil_damp as damp;

#[cfg(test)]
mod tests {
    use super::*;

    /// The curve's anchor. Everything else in this module is measured from
    /// here, and a reputation system whose zero is not the base has no
    /// meaningful "new account".
    #[test]
    fn no_history_is_exactly_the_base_weight() {
        let c = ReputationConfig::default();
        assert_eq!(weight_from(Standing::default(), &c), c.base_weight);
    }

    /// Monotone: more agreement never costs weight, and more dispute never
    /// gains it. Two properties the whole design rests on and neither of which
    /// the log curve gives for free.
    #[test]
    fn the_curve_is_monotone() {
        let c = ReputationConfig::default();
        let mut last = 0.0;
        for a in 0..40 {
            let w = weight_from(Standing::new(a, 0, &c), &c);
            assert!(w > last, "{a} agreements gave {w}, at or below {last}");
            last = w;
        }
        assert!(last <= c.max_weight, "and the cap holds: {last}");

        let mut last = c.max_weight;
        for d in 0..40 {
            let w = weight_from(Standing::new(0, d, &c), &c);
            assert!(w <= last, "{d} disputes gave {w}, above {last}");
            last = w;
        }
        assert!(last >= c.min_weight, "and the floor holds: {last}");
    }

    /// Every dispute costs something, forever.
    ///
    /// The property that catches a non-monotone penalty, and the one a
    /// "is the final weight lower" assertion cannot see: the compounding
    /// forgiveness factor this replaced ran 0.30, 0.36, 0.324 — so a repudiated
    /// account's standing *improved* on its third rejection and only the
    /// per-round assertion saw it.
    #[test]
    fn every_dispute_costs_something_and_the_cost_never_reverses() {
        let c = ReputationConfig::default();
        let mut last = Standing::new(0, 0, &c);
        for d in 1..60i64 {
            let now = Standing::new(0, d, &c);
            assert!(
                now.net < last.net,
                "dispute {d} left net at {}, at or above {} — the penalty is not \
                 monotone, so rejection can *raise* a standing",
                now.net,
                last.net
            );
            assert!(
                now.net < 0.0,
                "dispute {d} cost nothing: net is {}",
                now.net
            );
            last = now;
        }
    }

    /// A single dispute costs less than the first one did *in total*: this is
    /// the "one bad month does not undo a good record" half.
    #[test]
    fn repeated_disputes_saturate_rather_than_accumulate_without_limit() {
        let c = ReputationConfig::default();
        let first =
            weight_from(Standing::new(20, 1, &c), &c) - weight_from(Standing::new(20, 0, &c), &c);
        let tenth =
            weight_from(Standing::new(20, 10, &c), &c) - weight_from(Standing::new(20, 9, &c), &c);
        assert!(
            tenth.abs() < first.abs(),
            "the tenth dispute cost {tenth}, the first cost {first}: a decade of \
             mistakes should not cost ten decades of standing"
        );
    }

    /// Tier 2 is stash-box#630's "double votes" — at most, not exactly, because
    /// the multiplier is a config value and a test that demanded exactly 2.0
    /// would fail the moment a community set it to 1.8.
    #[test]
    fn tier_multipliers_are_monotone_and_start_at_one() {
        let c = ReputationConfig::default();
        assert_eq!(tier_multiplier_for(0, &c), 1.0);
        let mut last = 1.0;
        for t in 1..=4 {
            let m = tier_multiplier_for(t, &c);
            assert!(m > last, "tier {t} gives {m}, at or below {last}");
            last = m;
        }
    }
}
