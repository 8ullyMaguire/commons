//! §8.4 — leaderboards, badges, points.
//!
//! # What this module is for
//!
//! An economy over *curation*, and nothing else. §8.4: "Points are earned for
//! *accepted* proposals, which ties the economy to §8.1."
//!
//! # Rule 1 — the ledger, and why there is no balance column
//!
//! There is no `points_total` on `account` and no `points` column on anything
//! else. A balance is a **sum over live rows of `points_award`**, recomputed on
//! read.
//!
//! This is T-P4-004's rule applied to money, and the reason transfers directly: a
//! stored number has to be kept in step with the evidence, and §8.4's evidence is
//! §8.1's output, which changes every time a vote is cast or retracted. A balance
//! column would need a writer on every one of those paths, and a writer that
//! forgets one is a balance that is wrong in a way nothing can detect — because
//! nothing in the system knows the right answer.
//!
//! The ledger knows the right answer. `SUM(points) WHERE withdrawn_at IS NULL` is
//! the definition, and `points_award` is the evidence.
//!
//! # Rule 2 — the award is keyed on the proposal, so reconcile is idempotent
//!
//! `points_award` is unique on `(account_id, proposal_id)`. That single index
//! does two jobs:
//!
//!   * re-running [`reconcile`] over an unchanged field pays nothing twice,
//!   * a proposal that stops winning and then wins again returns to *the same
//!     row* rather than forking an account's history into two awards.
//!
//! Both fall out of the key. Neither would if the key were an autoincrement,
//! which is the obvious choice and the wrong one.
//!
//! # Rule 3 — no performer-popularity surface, anywhere
//!
//! §2: "Not a performer-discovery site. No ranking of people by popularity as a
//! browsable surface, no 'top performers' front page." §8.4's leaderboard is over
//! *contributors*. Popularity is allowed as a search tie-breaker and nowhere
//! else.
//!
//! The promise is guarded by `no_surface_ranks_people_by_popularity` in the test
//! module, which scans this crate's SQL for popularity orderings. It is worth
//! saying *why* it is a scan and not a function test: a test asserting
//! `leaderboard()` returns accounts is satisfied by a `popular_performers()`
//! sitting beside it, unreviewed and reachable from a route. A scan fails when
//! the query is *added*, which is the moment the decision is made.
//!
//! [`LeaderboardEntry`] carries no performer field, so the type makes the same
//! promise structurally: no query can put a performer in a leaderboard row
//! without changing the row type.

use commons_core::{ts, ProposalSource, Role, SubjectType};
use commons_store::{Store, StoreError};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

/// One accepted proposal, in points.
///
/// A constant rather than a literal at a call site so a test can sit exactly on a
/// boundary that is defined in one place, and so "how much is a curation worth"
/// is a single answer a deployment can change.
pub const POINTS_PER_ACCEPTED_PROPOSAL: i64 = 10;

/// Badge thresholds, named for the same reason.
pub const BADGE_FIRST: usize = 1;
pub const BADGE_HUNDRED: usize = 100;

/// Default reward for an invited contributor (§8.4 #600).
pub const INVITE_REWARD_POINTS: i64 = 25;

/// Default cap on outstanding invite keys (stash-box#551, configurable).
pub const DEFAULT_MAX_INVITE_KEYS: i64 = 25;

/// The most rows a leaderboard read will return.
///
/// A cap, not a default, and it is here rather than in the handler because the
/// handler is not the only caller. A leaderboard with no ceiling is a way to
/// enumerate the account table one page at a time.
pub const MAX_LEADERBOARD_ROWS: i64 = 1_000;

#[derive(Debug, Error)]
pub enum PointsError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Resolve(#[from] crate::resolve::ResolveError),
    /// A paging or configuration value outside its permitted range.
    #[error("out of range: {0}")]
    OutOfRange(String),
    /// The account may not do this.
    #[error("not permitted: {0}")]
    NotPermitted(String),
}

impl From<PointsError> for StoreError {
    fn from(e: PointsError) -> Self {
        match e {
            PointsError::Store(s) => s,
            other => StoreError::Invalid {
                what: "points",
                why: other.to_string(),
            },
        }
    }
}

type Result<T> = std::result::Result<T, PointsError>;

/// Tunable parts of the economy (§8.4 and stash-box#551 both say configurable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EconomyConfig {
    /// Points for a first accepted proposal.
    pub points_per_accepted_proposal: i64,
    /// Points for redeeming an invite.
    pub invite_reward_points: i64,
    /// Cap on outstanding invite keys.
    pub max_invite_keys: i64,
}

impl Default for EconomyConfig {
    fn default() -> Self {
        EconomyConfig {
            points_per_accepted_proposal: POINTS_PER_ACCEPTED_PROPOSAL,
            invite_reward_points: INVITE_REWARD_POINTS,
            max_invite_keys: DEFAULT_MAX_INVITE_KEYS,
        }
    }
}

/// A badge a contributor can hold.
///
/// Not stored anywhere. Each is a predicate over the award ledger, evaluated on
/// read — see [`badges`]. A `has_badge` column would be the counter this whole
/// module refuses to keep, and it would go stale the moment a vote is retracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Badge {
    /// At least [`BADGE_FIRST`] accepted proposals.
    FirstAcceptedProposal,
    /// At least [`BADGE_HUNDRED`] accepted proposals.
    HundredAcceptedProposals,
}

impl Badge {
    /// Every badge, in the order the ladder ascends.
    pub const ALL: [Badge; 2] = [
        Badge::FirstAcceptedProposal,
        Badge::HundredAcceptedProposals,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Badge::FirstAcceptedProposal => "first_accepted_proposal",
            Badge::HundredAcceptedProposals => "hundred_accepted_proposals",
        }
    }

    /// The count of accepted proposals this badge needs.
    ///
    /// `>=`, and inclusive on purpose. A threshold written `>` against the same
    /// number is a one-off bug that only shows at exactly the boundary, which is
    /// the single number nobody tests.
    pub const fn threshold(self) -> usize {
        match self {
            Badge::FirstAcceptedProposal => BADGE_FIRST,
            Badge::HundredAcceptedProposals => BADGE_HUNDRED,
        }
    }
}

/// One award, live or withdrawn.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Award {
    pub id: Uuid,
    pub proposal_id: Uuid,
    pub points: i64,
    pub awarded_at: String,
    pub withdrawn_at: Option<String>,
    pub withdraw_reason: Option<String>,
}

/// An account's standing: a sum, not a stored number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Balance {
    /// Sum of live awards.
    pub total: i64,
    /// How many live awards. Separate from `total` because a badge is a
    /// function of the *count*, and a badge computed from points would be
    /// reachable twice over for the same thing.
    pub accepted_proposals: i64,
}

/// One row of the leaderboard.
///
/// An account, a number, and a count — and no performer field, which is the
/// structural half of §2's promise. Adding one would not compile at the test
/// that reconstructs this struct field by field.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LeaderboardEntry {
    pub handle: String,
    pub total: i64,
    pub accepted_proposals: i64,
}

/// An account's balance, recomputed.
pub async fn balance(store: &Store, who: &Uuid) -> Result<Balance> {
    let row: (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(points), 0) AS total, COUNT(*) AS n
           FROM points_award
          WHERE account_id = ? AND withdrawn_at IS NULL",
    )
    .bind(who.to_string())
    .fetch_one(store.pool())
    .await?;
    Ok(Balance {
        total: row.0,
        accepted_proposals: row.1,
    })
}

/// The balance of the field's winning proposal's proposer, or zeroes.
///
/// A field can pay at most one proposer at a time, so "the balance this
/// reconcile affected" is a single account or none. The `Option` is the
/// difference between "nobody is paid for this field" and "we do not know yet",
/// and the two should not look alike.
pub async fn field_balance(
    store: &Store,
    subject_type: SubjectType,
    subject_id: &Uuid,
    field: &str,
) -> Result<Balance> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT a.account_id
           FROM points_award a
           JOIN field_proposal p ON p.id = a.proposal_id
          WHERE a.withdrawn_at IS NULL
            AND p.subject_type = ? AND p.subject_id = ? AND p.field = ?
          LIMIT 1",
    )
    .bind(subject_type.as_str())
    .bind(subject_id.to_string())
    .bind(field)
    .fetch_optional(store.pool())
    .await?;
    match row {
        Some(id) => balance(store, &Uuid::parse_str(&id).unwrap_or_default()).await,
        None => Ok(Balance {
            total: 0,
            accepted_proposals: 0,
        }),
    }
}

/// An account's awards, newest first, withdrawn ones included.
///
/// Withdrawn rows are in the list on purpose: "why does this account have fewer
/// points than it did" is a question a person will ask, and the answer is a row.
pub async fn awards(store: &Store, who: &Uuid) -> Result<Vec<Award>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        proposal_id: String,
        points: i64,
        awarded_at: String,
        withdrawn_at: Option<String>,
        withdraw_reason: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, proposal_id, points, awarded_at, withdrawn_at, withdraw_reason
           FROM points_award
          WHERE account_id = ?
          ORDER BY awarded_at DESC, id DESC",
    )
    .bind(who.to_string())
    .fetch_all(store.pool())
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| Award {
            id: Uuid::parse_str(&r.id).unwrap_or_default(),
            proposal_id: Uuid::parse_str(&r.proposal_id).unwrap_or_default(),
            points: r.points,
            awarded_at: r.awarded_at,
            withdrawn_at: r.withdrawn_at,
            withdraw_reason: r.withdraw_reason,
        })
        .collect())
}

/// The badges an account currently holds.
///
/// A predicate over the ledger, evaluated now. `COUNT(*) >= threshold` rather
/// than `>`, and the boundary is tested exactly.
pub async fn badges(store: &Store, who: &Uuid) -> Result<Vec<Badge>> {
    let n = balance(store, who).await?.accepted_proposals as usize;
    Ok(Badge::ALL
        .into_iter()
        .filter(|b| n >= b.threshold())
        .collect())
}

/// Bring the ledger in line with what §8.1 currently decides for one field.
///
/// The only writer of `points_award` that creates or withdraws a row, which is
/// what makes the invariants checkable: an award exists iff its proposal is the
/// field's winner and the field is neither tied nor locked.
///
/// Three cases, and each is a withdrawal or a creation for a *reason*:
///
///   * the winning proposal is a machine's, or has no proposer → **no award**.
///     §8.1 says a machine proposal's weight carries no authority, and a field
///     that nobody proposed pays nobody.
///   * the field is tied, or locked → **withdraw**. A tie is not a decision
///     (§8.1 reports it as one needing a human) and a lock is the absence of a
///     vote, so in both cases no proposal "won".
///   * the field has a winner with a proposer → **award**, or restore a
///     previously withdrawn row.
pub async fn reconcile(
    store: &Store,
    subject_type: SubjectType,
    subject_id: &Uuid,
    field: &str,
) -> Result<Balance> {
    let config = EconomyConfig::default();
    let resolved = crate::resolve::resolve(store, *subject_id, field).await?;

    // Whether there is *any* award this field should carry, and why not.
    let no_award_reason: Option<&str> = if resolved.locked {
        Some("the field is locked, and a lock is the absence of a vote")
    } else if resolved.tied {
        Some("the field is tied, and a tie is not a decision")
    } else if resolved.proposal_id.is_none() {
        Some("the field has no winner, so nothing was accepted")
    } else {
        None
    };

    // The proposal that should hold an award, if any.
    //
    // Only a *human* proposal pays, and only one with a proposer. §8.1 is
    // explicit that a machine proposal's weight "does not carry authority", and
    // §8.4 pays for curation. A scan that paid points would make bulk import the
    // cheapest way to earn them, and the leaderboard would rank who imported most
    // rather than who curated best.
    let should_award: Option<(String, Uuid)> = match (no_award_reason, resolved.proposal_id) {
        (Some(_), _) => None,
        (None, Some(winner)) => {
            let row: Option<(Option<String>, bool)> =
                sqlx::query_as("SELECT proposer_id, source = ? FROM field_proposal WHERE id = ?")
                    .bind(ProposalSource::User.as_str())
                    .bind(winner.to_string())
                    .fetch_optional(store.pool())
                    .await?;
            match row {
                Some((Some(proposer), true)) => Some((proposer, winner)),
                _ => None,
            }
        }
        (None, None) => None,
    };

    // Withdraw every live award for this field that is not the one that should
    // stand. Scoped by the field's proposals rather than by a stored field
    // column, because the award table records *what was accepted*, not where.
    //
    // The `!=` is the whole point and the bug this shape exists to prevent: an
    // earlier version withdrew only when the field had *no* winner, so a rival
    // that simply took over left the old award live and the account was paid for
    // two proposals on one field. "Which award should stand" is not the same
    // question as "is there an award", and the second one is the wrong question.
    let keep: Option<String> = should_award.as_ref().map(|(_, pid)| pid.to_string());
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT a.proposal_id
           FROM points_award a
           JOIN field_proposal p ON p.id = a.proposal_id
          WHERE a.withdrawn_at IS NULL
            AND p.subject_type = ? AND p.subject_id = ? AND p.field = ?",
    )
    .bind(subject_type.as_str())
    .bind(subject_id.to_string())
    .bind(field)
    .fetch_all(store.pool())
    .await?;
    for pid in ids {
        if keep.as_deref() == Some(pid.as_str()) {
            continue;
        }
        let why = no_award_reason.unwrap_or(
            "a different proposal now wins this field, so this one is no longer \
             an accepted proposal",
        );
        sqlx::query(
            "UPDATE points_award SET withdrawn_at = ?, withdraw_reason = ?
              WHERE proposal_id = ? AND withdrawn_at IS NULL",
        )
        .bind(ts::now())
        .bind(why)
        .bind(&pid)
        .execute(store.pool())
        .await?;
    }

    if let Some((proposer, proposal)) = should_award {
        award(
            store,
            &proposer,
            &proposal,
            config.points_per_accepted_proposal,
        )
        .await?;
    }

    // What this reconcile changed, read back from the ledger rather than
    // assembled from the arithmetic above. A reconcile that reports its own
    // arithmetic while the stored rows say otherwise is a bug the caller cannot
    // see, and the whole point of a ledger is that the read is the truth.
    field_balance(store, subject_type, subject_id, field).await
}

/// Award `points` for `proposal`, or restore a withdrawn row.
///
/// The `ON CONFLICT ... DO UPDATE SET withdrawn_at = NULL` is what makes
/// reconcile idempotent *and* restorative in one statement. The alternative —
/// checking for an existing row and branching — is two round trips and a race
/// between them.
async fn award(store: &Store, account: &str, proposal: &Uuid, points: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO points_award (id, account_id, proposal_id, points, awarded_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (account_id, proposal_id) DO UPDATE
            SET withdrawn_at = NULL, withdraw_reason = NULL",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(account)
    .bind(proposal.to_string())
    .bind(points)
    .bind(ts::now())
    .execute(store.pool())
    .await?;
    Ok(())
}

// ---- leaderboard ----------------------------------------------------------

/// The contributor leaderboard: highest points first.
///
/// Refuses a limit of zero and a limit above [`MAX_LEADERBOARD_ROWS`]. A paging
/// parameter that is silently ignored is an enumeration vector.
///
/// Ordered by `total DESC, handle ASC` — the second key is not cosmetic. Without
/// a total order the same library produces a different top-10 on different runs,
/// and a leaderboard that reshuffles on a re-read cannot be cached, screenshotted,
/// or argued about.
pub async fn leaderboard(store: &Store, limit: i64) -> Result<Vec<LeaderboardEntry>> {
    if limit <= 0 {
        return Err(PointsError::OutOfRange(format!(
            "leaderboard limit {limit} must be positive: a limit of zero would \\
             return everything, which is a paging parameter that does not page"
        )));
    }
    if limit > MAX_LEADERBOARD_ROWS {
        return Err(PointsError::OutOfRange(format!(
            "leaderboard limit {limit} exceeds the cap of {MAX_LEADERBOARD_ROWS}"
        )));
    }

    #[derive(sqlx::FromRow)]
    struct Row {
        handle: String,
        total: i64,
        n: i64,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT a.handle,
                COALESCE(SUM(w.points), 0) AS total,
                COUNT(w.id)                  AS n
           FROM account a
           JOIN points_award w ON w.account_id = a.id AND w.withdrawn_at IS NULL
          WHERE a.disabled = 0
          GROUP BY a.handle
          ORDER BY total DESC, a.handle ASC
          LIMIT ?",
    )
    .bind(limit)
    .fetch_all(store.pool())
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| LeaderboardEntry {
            handle: r.handle,
            total: r.total,
            accepted_proposals: r.n,
        })
        .collect())
}

// ---- invites (stash-box#551, #600) ----------------------------------------

/// Mint `count` invite keys, refusing to exceed the configured cap.
///
/// The cap is enforced by counting the keys that still have uses left, not the
/// rows that exist: a key with no uses remaining is spent, and a cap counted
/// over spent keys would lock a deployment out of its own corpus.
pub async fn create_invite(store: &Store, by: &Uuid, count: i64) -> Result<Vec<String>> {
    let config = EconomyConfig::default();
    if count <= 0 {
        return Err(PointsError::OutOfRange("mint no invites".to_string()));
    }
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM invite_key WHERE uses < max_uses AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(ts::now())
    .fetch_one(store.pool())
    .await?;
    if live + count > config.max_invite_keys {
        return Err(PointsError::OutOfRange(format!(
            "{live} live keys plus {count} requested exceeds the cap of {} \\
             (stash-box#551 makes this configurable; EconomyConfig carries it)",
            config.max_invite_keys
        )));
    }

    let mut keys = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let key = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO invite_key (key, created_by, max_uses, uses, created_at)
             VALUES (?, ?, 1, 0, ?)",
        )
        .bind(&key)
        .bind(by.to_string())
        .bind(ts::now())
        .execute(store.pool())
        .await?;
        keys.push(key);
    }
    Ok(keys)
}

/// Redeem a key for `who`, and pay the invite reward.
///
/// Exactly-once is enforced by the conditional UPDATE: `WHERE uses < max_uses`
/// means two simultaneous redemptions of the last remaining use see one of them
/// update zero rows, and a zero-row update is a refusal. A read-then-write check
/// would let both through.
///
/// Refused for a disabled account. A disabled account is the state a takedown
/// creates, and one that can still consume keys is a way to keep growing the
/// user base after being removed from it.
pub async fn redeem_invite(store: &Store, key: &str, who: &Uuid) -> Result<Balance> {
    let Some(disabled) = sqlx::query_scalar::<_, bool>("SELECT disabled FROM account WHERE id = ?")
        .bind(who.to_string())
        .fetch_optional(store.pool())
        .await?
    else {
        return Err(PointsError::NotPermitted(format!(
            "{who} is not an account"
        )));
    };
    if disabled {
        return Err(PointsError::NotPermitted(format!(
            "{who} is disabled and may not redeem an invite"
        )));
    }

    let now = ts::now();
    let claimed = sqlx::query(
        "UPDATE invite_key SET uses = uses + 1
          WHERE key = ? AND uses < max_uses AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(key)
    .bind(&now)
    .execute(store.pool())
    .await?;
    if claimed.rows_affected() == 0 {
        return Err(PointsError::NotPermitted(format!(
            "invite key {key} is spent, expired, or unknown"
        )));
    }
    sqlx::query("INSERT INTO invite_redeem (key, account_id, redeemed_at) VALUES (?, ?, ?)")
        .bind(key)
        .bind(who.to_string())
        .bind(&now)
        .execute(store.pool())
        .await?;

    // The reward is a ledger row like any other, keyed on the redemption rather
    // than on a proposal -- so it is never withdrawn by a reconcile, because a
    // withdrawal means "this curation stopped being accepted" and an invite is
    // not curation.
    sqlx::query(
        "INSERT INTO points_award (id, account_id, proposal_id, points, awarded_at, note)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (account_id, proposal_id) DO NOTHING",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(who.to_string())
    .bind(invite_proposal_id(key, who))
    .bind(EconomyConfig::default().invite_reward_points)
    .bind(&now)
    .bind("invite reward")
    .execute(store.pool())
    .await?;

    balance(store, who).await
}

/// A stable pseudo-proposal id for an invite reward, so the ledger's unique key
/// applies to it too.
///
/// Derived rather than random so a re-redeem attempt on the same key for the
/// same account collides on the index instead of paying again. The `0` prefix
/// marks it as not-a-uuid-of-a-real-proposal, which matters if anything ever
/// joins the ledger back to `field_proposal`.
fn invite_proposal_id(key: &str, who: &Uuid) -> String {
    format!("invite:{}:{who}", key)
}

/// An account's role, for a caller that wants to gate on it.
///
/// Thin on purpose: the one authorization decision points makes is the disabled
/// check in [`redeem_invite`], and a helper that reads a role without the flag
/// invites the mistake T-P4-005 just fixed in moderation.
pub async fn is_eligible(store: &Store, who: &Uuid) -> Result<bool> {
    let row: Option<(String, bool)> =
        sqlx::query_as("SELECT role, disabled FROM account WHERE id = ?")
            .bind(who.to_string())
            .fetch_optional(store.pool())
            .await?;
    Ok(match row {
        Some((role, disabled)) => {
            !disabled && Role::parse(&role).is_some_and(|r| r >= Role::Contributor)
        }
        None => false,
    })
}
