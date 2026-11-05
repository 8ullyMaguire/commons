//! Bounties — **pay for identifying a half-remembered thing**.
//!
//! "I read it in 2019, there was a scene with a broken tanto, the ending was on a rooftop in the rain" is a
//! real query that search cannot answer. A bounty is that query written down, with credits behind it, and it is
//! the same shape on every product in the family: a fanfiction archive finding a fic, an imageboard finding the
//! source of a repost, a video library finding a scene, a directory finding the blog somebody half-remembers.
//!
//! # What is shared, and what is not
//!
//! **Shared, here:** the state machine, the bounds that make a bounty a promise rather than a transfer channel,
//! and the refusals worded so a person can act on them. All of it is pure — no clock of its own, no store, no
//! HTTP — so every rule is testable without a database.
//!
//! **Not shared, deliberately:** the money. An escrow is twenty lines on top of whatever ledger a host already
//! has, and the family's ledgers differ (credits, XP, holds, invoices). [`Escrow`] names the four operations a
//! host has to provide; a host that does not want the trait can ignore it and call its own ledger directly, as
//! long as it honours the order written in [`settle_the_winner`].

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// The smallest budget worth posting.
pub const MIN_BUDGET: i64 = 1;
/// The largest budget a single bounty may carry.
pub const MAX_BUDGET: i64 = 500;
/// Minimum useful description length.
pub const MIN_DESCRIPTION: usize = 60;
/// Maximum description length.
pub const MAX_DESCRIPTION: usize = 2000;
/// The longest a bounty may stay open.
pub const MAX_DAYS_OPEN: i64 = 90;

/// Where a bounty is in its life.
///
/// One-way: `open` goes to `accepted`, `cancelled` or `expired`, and never back. A bounty that could reopen
/// would let an offer be withdrawn after somebody did the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BountyState {
    Open,
    Accepted,
    Cancelled,
    Expired,
}

impl BountyState {
    /// The wire word, which is also the word a database column stores.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Accepted => "accepted",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    /// Parse a wire word. An unknown state is `None`, never a default: guessing a state would let a corrupted
    /// row behave like an open bounty.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "open" => Some(Self::Open),
            "accepted" => Some(Self::Accepted),
            "cancelled" => Some(Self::Cancelled),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }

    /// Whether credits are still committed.
    #[must_use]
    pub fn holds_credits(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// A bounty as stored.
///
/// `subject` is whatever a host's things are grouped by — a fandom on a fiction archive, a studio on a video
/// library, a source on an imageboard. It is optional because a half-remembered thing often comes with no
/// category at all, and that must still be postable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bounty {
    pub id: String,
    /// The account paying.
    pub asker_account_id: String,
    pub description: String,
    pub subject: Option<String>,
    pub budget: i64,
    pub state: BountyState,
    pub closes_at: String,
    pub created_at: String,
    /// The proposal that was paid, when one was.
    pub accepted_proposal_id: Option<String>,
    /// The credit hold standing behind the promise.
    pub hold_id: Option<String>,
}

/// One answer: "it is this thing".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub bounty_id: String,
    pub proposer_account_id: String,
    /// The identifier of the candidate in the host's own namespace (a work, a scene, a post).
    pub subject_id: String,
    pub note: String,
    /// `proposed`, `accepted`, `rejected` or `withdrawn`.
    pub state: String,
    pub created_at: String,
}

impl Proposal {
    /// Whether this answer is still waiting for a decision.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.state == "proposed"
    }
}

/// The refusal sentence, so a caller can show it verbatim.
pub type Refusal = String;

/// Check a new bounty before anything is reserved or written.
///
/// `now` and `closes_at` are RFC-3339 strings in the format a host stores, so the rule can be tested without a
/// clock and without agreeing on a timestamp type across products.
///
/// # Errors
/// The refusal sentence.
pub fn validate_new(
    description: &str,
    budget: i64,
    now: &str,
    closes_at: &str,
) -> Result<(), Refusal> {
    let trimmed = description.trim();
    let len = trimmed.chars().count();
    if len < MIN_DESCRIPTION {
        return Err(format!(
            "description: {len} characters is not enough to find anything — describe it in at least \
             {MIN_DESCRIPTION} (where you saw it, what happens, anything you remember)"
        ));
    }
    if len > MAX_DESCRIPTION {
        return Err(format!(
            "description: {len} characters is longer than the {MAX_DESCRIPTION} a bounty may carry — the detail \
             that identifies it fits"
        ));
    }
    if !(MIN_BUDGET..=MAX_BUDGET).contains(&budget) {
        return Err(format!(
            "budget: {budget} is outside {MIN_BUDGET}..={MAX_BUDGET} — a bounty is a tip-sized promise, not a \
             transfer between accounts"
        ));
    }
    let now_ts = parse(now).ok_or_else(|| "now: not an RFC-3339 instant".to_owned())?;
    let closes_ts =
        parse(closes_at).ok_or_else(|| "closes_at: not an RFC-3339 instant".to_owned())?;
    if closes_ts <= now_ts {
        return Err(
            "closes_at: a bounty has to be open for some time — it is already in the past"
                .to_owned(),
        );
    }
    if closes_ts - now_ts > Duration::days(MAX_DAYS_OPEN) {
        return Err(format!(
            "closes_at: more than {MAX_DAYS_OPEN} days away — an unanswered bounty should expire rather than sit \
             on someone's credits forever"
        ));
    }
    Ok(())
}

/// Whether `proposal` may be paid by `asker` at `now`.
///
/// # Errors
/// The refusal sentence.
pub fn may_accept(
    bounty_state: BountyState,
    closes_at: &str,
    asker_account_id: &str,
    proposer_account_id: &str,
    now: &str,
) -> Result<(), Refusal> {
    match bounty_state {
        BountyState::Open => {}
        other => {
            return Err(format!(
                "bounty: it is {} — only an open bounty can be accepted",
                other.as_str()
            ))
        }
    }
    let Some(closes_ts) = parse(closes_at) else {
        return Err("closes_at: not an RFC-3339 instant".to_owned());
    };
    let Some(now_ts) = parse(now) else {
        return Err("now: not an RFC-3339 instant".to_owned());
    };
    if now_ts >= closes_ts {
        return Err(
            "bounty: it has passed its close date — cancel it so the credits are released"
                .to_owned(),
        );
    }
    if asker_account_id == proposer_account_id {
        return Err(
            "proposal: that is your own — a bounty pays somebody else for finding it; cancel the bounty instead \
             of paying yourself"
                .to_owned(),
        );
    }
    Ok(())
}

/// Whether `proposer` may answer `asker`'s bounty.
///
/// # Errors
/// The refusal sentence.
pub fn may_propose(
    bounty_state: BountyState,
    asker_account_id: &str,
    proposer_account_id: &str,
) -> Result<(), Refusal> {
    if bounty_state != BountyState::Open {
        return Err(format!(
            "bounty: it is {} — only an open bounty takes answers",
            bounty_state.as_str()
        ));
    }
    if asker_account_id == proposer_account_id {
        return Err(
            "bounty: that is your own — answers come from somebody else, or it is already found"
                .to_owned(),
        );
    }
    Ok(())
}

/// The four operations a host's ledger has to provide for a bounty to be payable.
///
/// Implemented by the host, called by the host: this crate does not call it, because the order matters and the
/// order belongs to the code that also owns the state machine's rows. The contract is in
/// [`settle_the_winner`].
pub trait Escrow {
    /// Host error type.
    type Error;

    /// Reserve `amount` for this bounty. Returns a hold id.
    ///
    /// # Errors
    /// Host error.
    fn reserve(
        &self,
        account: &str,
        bounty_id: &str,
        amount: i64,
        ttl_seconds: i64,
    ) -> impl std::future::Future<Output = Result<String, Self::Error>>;

    /// Release a hold, returning the credits to the asker.
    ///
    /// # Errors
    /// Host error.
    fn release(&self, hold_id: &str) -> impl std::future::Future<Output = Result<(), Self::Error>>;

    /// Move `amount` from `from` to `to`, idempotent on `key`.
    ///
    /// # Errors
    /// Host error.
    fn pay(
        &self,
        hold_id: &str,
        from: &str,
        to: &str,
        amount: i64,
        key: &str,
    ) -> impl std::future::Future<Output = Result<String, Self::Error>>;

    /// Credits `account` can commit right now (balance minus standing holds).
    ///
    /// # Errors
    /// Host error.
    fn available(
        &self,
        account: &str,
    ) -> impl std::future::Future<Output = Result<i64, Self::Error>>;
}

/// The order a host must follow when an asker accepts an answer.
///
/// 1. **Compare-and-set the bounty's state** from `open` to `accepted`. Two acceptances must produce one
///    winner; a read-then-write is not enough, because both readers see `open`.
/// 2. **Only then pay**, with an idempotency key derived from the winning proposal, so a retried request re-uses
///    the transaction instead of paying twice.
/// 3. **Capture or release the hold** last, at the amount actually paid.
///
/// Paying first would pay the wrong person whenever the CAS loses; paying outside an idempotency key would pay
/// twice on a retry. This function is documentation with a body so that the order can be cited in code review.
pub fn settle_the_winner(state_after_cas: BountyState, paid: bool) -> Result<(), &'static str> {
    if state_after_cas != BountyState::Accepted {
        return Err("the bounty is not accepted; do not pay");
    }
    if !paid {
        return Err("accepted but unpaid: retry with the same idempotency key");
    }
    Ok(())
}

fn parse(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|value| value.with_timezone(&Utc))
}
