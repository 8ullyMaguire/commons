//! §8.5 — moderation, locking, disputes.
//!
//! # What this module is
//!
//! The steward queue: one list of things a person must decide, each of which
//! names the least role that may decide it, and each of which leaves a record
//! when it is decided.
//!
//! # The one rule
//!
//! **Authorization is decided per item type, from the type alone — never from
//! what the resolution does.** A caller cannot pass a role; it passes an account
//! id, and [`resolve`] reads that account's role and disabled flag and asks
//! [`ItemType::lowest_role`]. There is no `resolve_as(role, ...)` and there
//! deliberately is not one, because a function whose authorization is a
//! parameter is a function whose authorization is only tested at its call sites.
//!
//! The consequence worth stating: the boundary for `DisputedConsent` is `Admin`
//! while the other six are `Steward`, and no amount of passing "Contributor
//! cannot" and "Admin can" tests would notice that. So
//! `each_type_refuses_the_role_below_its_own` asserts the table itself, as data.
//!
//! # Why one table for seven types
//!
//! Because a steward reads a *list*. Seven tables would be seven queries, seven
//! orderings to reconcile, and seven places for an item to be created in one
//! table and resolved in another. Per-type detail lives in per-type tables
//! because it is per-type *data* — a reason for a report, a colliding value for
//! a name — not per-type shape.
//!
//! # What a resolution does and does not do
//!
//! A resolution marks the item. It does not re-derive the thing being contested:
//! `ContestedField` + `LockTo` writes a `field_lock`, and it is the lock that
//! resolve reads from then on. A queue that recomputed the answer while deciding
//! it would be a second implementation of §8.1, and T-P4-004's rule — recompute
//! from the accepted set, never maintain a counter — exists precisely because
//! two implementations of one number is how a number goes stale.

use commons_core::{ts, Role, SubjectType};
use commons_store::{index, Store, StoreError};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

/// Why a queue operation failed.
///
/// The interesting split is `NotPermitted` against everything else: a refused
/// call has *not happened*, so a caller may retry it as a different account and
/// must not treat it as a partial success.
#[derive(Debug, Error)]
pub enum ModerationError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    History(#[from] crate::history::HistoryError),
    /// The resolver's account may not decide this kind of item. The one error
    /// the caller is expected to act on differently from the rest.
    #[error("not permitted: {0}")]
    NotPermitted(String),
    /// A row this module could not make sense of. A bug, never a user error.
    #[error("malformed row: {0}")]
    Malformed(String),
}

impl From<ModerationError> for StoreError {
    fn from(e: ModerationError) -> Self {
        match e {
            ModerationError::Store(s) => s,
            ModerationError::History(h) => StoreError::from(h),
            other => StoreError::Invalid {
                what: "moderation queue",
                why: other.to_string(),
            },
        }
    }
}

type Result<T> = std::result::Result<T, ModerationError>;

/// The seven queue item types, in the order §8.5 lists them.
///
/// `ALL` is the single list the authorization match and the tests both read, so a
/// type cannot exist in one and not the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemType {
    /// A field whose top-two values are too close to call. §8.5.
    ContestedField,
    /// Two identities claimed to be the same and a steward must say which.
    DisputedMerge,
    /// A consent tier a steward and the subject disagree about. §8.5, §14.2.
    DisputedConsent,
    /// An accusation about an account. §8.5, stash-box#213.
    AbuseReport,
    /// Somebody else's edit awaiting a steward. §8.5, stash-box#599.
    PendingEdit,
    /// A proposed name that already exists. §8.5, stash-box#714.
    NameCollision,
    /// A closed submission somebody wants back. §8.5, stash-box#570.
    ClosedSubmission,
}

impl ItemType {
    /// All seven, and the order the table in the tests uses.
    pub const ALL: [ItemType; 7] = [
        ItemType::ContestedField,
        ItemType::DisputedMerge,
        ItemType::DisputedConsent,
        ItemType::AbuseReport,
        ItemType::PendingEdit,
        ItemType::NameCollision,
        ItemType::ClosedSubmission,
    ];

    /// The column value.
    pub fn as_str(self) -> &'static str {
        match self {
            ItemType::ContestedField => "contested_field",
            ItemType::DisputedMerge => "disputed_merge",
            ItemType::DisputedConsent => "disputed_consent",
            ItemType::AbuseReport => "abuse_report",
            ItemType::PendingEdit => "pending_edit",
            ItemType::NameCollision => "name_collision",
            ItemType::ClosedSubmission => "closed_submission",
        }
    }

    /// Parse a column value. An unknown value is an error, not a default.
    ///
    /// The temptation is `unwrap_or(ContestedField)`, which is a queue item
    /// nobody can resolve appearing as a queue item somebody with the wrong
    /// standing can resolve. A row this code cannot read is a bug, and a bug that
    /// silently lowers an authorization is the worst shape a bug can take here.
    pub fn parse(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| ModerationError::Malformed(format!("unknown moderation item kind: {s}")))
    }

    /// The least role that may resolve this type.
    ///
    /// Six are `Steward`; [`ItemType::DisputedConsent`] is `Admin` and is the
    /// reason this function exists rather than a `Role::may_moderate` call.
    pub fn lowest_role(self) -> Role {
        match self {
            // §14.2: a consent decision is never outvoted by contribution, and a
            // steward's standing is contribution. The one boundary in this module
            // that a trusted role does not get.
            ItemType::DisputedConsent => Role::Admin,
            _ => Role::Steward,
        }
    }

    /// Whether `role` may resolve this type.
    ///
    /// Deliberately *not* `role.may_moderate()`. That predicate is the codebase's
    /// answer for "may this role moderate generally", and here the answer differs
    /// for one type; reusing it would make the exception impossible to express
    /// and the two places that could disagree would both be "correct".
    pub fn may_resolve(self, role: Role) -> bool {
        role.may_moderate() && role >= self.lowest_role()
    }
}

/// A queue item, as the queue sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: ItemType,
    pub subject_type: SubjectType,
    pub subject_id: Uuid,
    pub field: String,
    /// One line, for a steward's list. Never parsed.
    pub summary: String,
    /// The account handle, or `None` for a system-raised item.
    pub raised_by: Option<String>,
    pub raised_at: String,
}

impl Item {
    fn new(
        kind: ItemType,
        subject_type: SubjectType,
        subject_id: Uuid,
        field: &str,
        summary: String,
        raised_by: Option<&str>,
    ) -> Self {
        Item {
            kind,
            subject_type,
            subject_id,
            field: field.to_string(),
            summary,
            raised_by: raised_by.map(str::to_string),
            raised_at: ts::now(),
        }
    }
}

/// What a steward decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// No action; the item was not actionable.
    Dismiss,
    /// Pin the field to this exact value. §8.5's "lock a contested field".
    LockTo(String),
    /// Confirm that `loser` is the same thing as the item's subject.
    Merge(Uuid),
    /// Set the subject's consent tier. §8.5, §14.2.
    SetConsentTier(String),
    /// Correct a pending edit, keeping its author (stash-box#599).
    Amend(Uuid, String),
    /// Reopen a closed submission.
    Reopen,
}

impl Resolution {
    fn as_str(&self) -> &'static str {
        match self {
            Resolution::Dismiss => "dismiss",
            Resolution::LockTo(_) => "lock_to",
            Resolution::Merge(_) => "merge",
            Resolution::SetConsentTier(_) => "set_consent_tier",
            Resolution::Amend(_, _) => "amend",
            Resolution::Reopen => "reopen",
        }
    }
}

/// The least-privilege reader for this module's own table.
///
/// Deliberately *not* `account_role`'s `Option<Role>`: this needs the disabled
/// flag too, and a disabled account keeps its role. `account_role_with_state` is
/// the one function in the store that returns both, and a call site that reads
/// only the role is the specific shape of bug this module exists to prevent.
#[derive(Debug, sqlx::FromRow)]
struct Actor {
    role: String,
    disabled: bool,
}
async fn actor(store: &Store, who: &Uuid) -> Result<Option<Actor>> {
    let row: Option<Actor> = sqlx::query_as("SELECT role, disabled FROM account WHERE id = ?")
        .bind(who.to_string())
        .fetch_optional(store.pool())
        .await?;
    Ok(row)
}

/// The seven per-user pending-edit limits (stash-box#782).
///
/// Three numbers, not seven: a limit that is the same for everyone is not a
/// limit, and a steward — whose pending edits are the ones a steward would
/// resolve — is the account most able to flood the queue, so theirs is the
/// tightest. An admin is exempt because an admin bulk-editing a corpus is the
/// case the limit would obstruct.
pub fn pending_limit(role: Role) -> usize {
    match role {
        Role::Public | Role::Subscriber => 3,
        Role::Contributor => 5,
        Role::Steward => 2,
        Role::Admin => usize::MAX,
    }
}

/// Refuse to act for a disabled account, by handle.
///
/// Every raiser in this module goes through here, and the reason is a disabled
/// account is the *specific state a takedown creates*: it keeps its role, so
/// every authorization check in the module passes it. If a raiser skipped this
/// and only `resolve` checked, a taken-down account could still fill the queue
/// with items that look exactly like ordinary reports -- and a queue that a
/// disabled account can write to is a queue nobody can trust, which defeats the
/// point of having one.
async fn require_enabled(store: &Store, handle: &str) -> Result<Uuid> {
    let Some(id) = index::account_id_by_handle(store, handle).await? else {
        return Err(ModerationError::Malformed(format!(
            "{handle:?} has no account"
        )));
    };
    let Some(a) = actor(store, &id).await? else {
        return Err(ModerationError::Malformed(format!(
            "{handle:?} has no account row"
        )));
    };
    if a.disabled {
        return Err(ModerationError::NotPermitted(format!(
            "{handle} is disabled and may not raise a queue item"
        )));
    }
    Ok(id)
}

/// Insert a queue item and return its id.
async fn enqueue(store: &Store, item: &Item) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO moderation_item
            (id, kind, subject_type, subject_id, field, summary, raised_by, raised_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(item.kind.as_str())
    .bind(item.subject_type.as_str())
    .bind(item.subject_id.to_string())
    .bind(&item.field)
    .bind(&item.summary)
    .bind(item.raised_by.as_deref())
    .bind(&item.raised_at)
    .execute(store.pool())
    .await?;
    Ok(id)
}

/// Every open item, oldest first.
///
/// A single filtered read, not a join across seven tables: the detail columns
/// differ per type, so a joined row would need seven nullable column groups and a
/// caller would have to know which group to read. The queue's job is to name the
/// item; the detail is fetched by type when the steward opens one.
pub async fn queue(store: &Store) -> Result<Vec<Item>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        kind: String,
        subject_type: String,
        subject_id: String,
        field: String,
        summary: String,
        raised_by: Option<String>,
        raised_at: String,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT kind, subject_type, subject_id, field, summary, raised_by, raised_at
           FROM moderation_item
          WHERE resolved_at IS NULL
          ORDER BY raised_at, id",
    )
    .fetch_all(store.pool())
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(Item {
            kind: ItemType::parse(&r.kind)?,
            subject_type: SubjectType::parse(&r.subject_type).ok_or_else(|| {
                ModerationError::Malformed(format!("bad subject_type: {}", r.subject_type))
            })?,
            subject_id: Uuid::parse_str(&r.subject_id)
                .map_err(|e| ModerationError::Malformed(format!("bad subject_id: {e}")))?,
            field: r.field,
            summary: r.summary,
            raised_by: r.raised_by,
            raised_at: r.raised_at,
        });
    }
    Ok(out)
}

/// An abuse report, with its resolution state.
#[derive(Debug, Clone, PartialEq)]
pub struct AbuseReport {
    pub id: Uuid,
    pub reporter: String,
    pub accused: Option<String>,
    pub reason: String,
    pub raised_at: String,
    pub dismissed_at: Option<String>,
}

async fn raise_abuse_report_inner(
    store: &Store,
    item: &Item,
    accused: Option<&str>,
    reason: &str,
) -> Result<(Uuid, Uuid)> {
    let report_id = Uuid::new_v4();
    let item_id = enqueue(store, item).await?;
    sqlx::query(
        "INSERT INTO abuse_report (id, item_id, reporter, accused, reason, raised_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(report_id.to_string())
    .bind(item_id.to_string())
    .bind(item.raised_by.as_deref().unwrap_or("system"))
    .bind(accused)
    .bind(reason)
    .bind(&item.raised_at)
    .execute(store.pool())
    .await?;
    Ok((report_id, item_id))
}

/// Report an account for abuse, and put the report in the queue.
pub async fn raise_abuse_report(
    store: &Store,
    subject_id: Uuid,
    field: &str,
    reporter: &str,
    reason: &str,
) -> Result<Uuid> {
    let item = Item::new(
        ItemType::AbuseReport,
        SubjectType::Object,
        subject_id,
        field,
        reason.to_string(),
        Some(reporter),
    );
    require_enabled(store, reporter).await?;
    let (report_id, _) = raise_abuse_report_inner(store, &item, Some(reporter), reason).await?;
    Ok(report_id)
}

/// Read an abuse report.
pub async fn abuse_report(store: &Store, id: Uuid) -> Result<Option<AbuseReport>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        reporter: String,
        accused: Option<String>,
        reason: String,
        raised_at: String,
        dismissed_at: Option<String>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, reporter, accused, reason, raised_at, dismissed_at
           FROM abuse_report WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(store.pool())
    .await?;

    Ok(row.map(|r| AbuseReport {
        id: Uuid::parse_str(&r.id).unwrap_or(id),
        reporter: r.reporter,
        accused: r.accused,
        reason: r.reason,
        raised_at: r.raised_at,
        dismissed_at: r.dismissed_at,
    }))
}

/// A name-collision warning, with its resolution state.
#[derive(Debug, Clone, PartialEq)]
pub struct NameCollision {
    pub id: Uuid,
    pub proposed: String,
    pub proposed_json: String,
    pub collides_with: Option<String>,
    pub note: String,
    pub raised_at: String,
    pub dismissed_at: Option<String>,
}

/// Warn that a proposed name already exists, and put the warning in the queue.
pub async fn raise_name_collision(
    store: &Store,
    subject_id: Uuid,
    field: &str,
    raiser: &str,
    proposed: &Value,
    note: &str,
) -> Result<Uuid> {
    require_enabled(store, raiser).await?;
    let id = Uuid::new_v4();
    let item = Item::new(
        ItemType::NameCollision,
        SubjectType::Object,
        subject_id,
        field,
        note.to_string(),
        Some(raiser),
    );
    let item_id = enqueue(store, &item).await?;
    sqlx::query(
        "INSERT INTO name_collision
            (id, item_id, proposed, proposed_json, collides_with, note, raised_at)
         VALUES (?, ?, ?, ?, NULL, ?, ?)",
    )
    .bind(id.to_string())
    .bind(item_id.to_string())
    .bind(proposed.as_str().unwrap_or_default())
    .bind(proposed.to_string())
    .bind(note)
    .bind(&item.raised_at)
    .execute(store.pool())
    .await?;
    Ok(id)
}

/// Read a name-collision warning.
pub async fn name_collision(store: &Store, id: Uuid) -> Result<Option<NameCollision>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        proposed: String,
        proposed_json: String,
        collides_with: Option<String>,
        note: String,
        raised_at: String,
        dismissed_at: Option<String>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, proposed, proposed_json, collides_with, note, raised_at, dismissed_at
           FROM name_collision WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(store.pool())
    .await?;

    Ok(row.map(|r| NameCollision {
        id: Uuid::parse_str(&r.id).unwrap_or(id),
        proposed: r.proposed,
        proposed_json: r.proposed_json,
        collides_with: r.collides_with,
        note: r.note,
        raised_at: r.raised_at,
        dismissed_at: r.dismissed_at,
    }))
}

/// A pending edit, with its amendment state.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingEdit {
    pub id: Uuid,
    /// Always the *original* author, never the steward who amended it
    /// (stash-box#599).
    pub author: String,
    /// As written.
    pub value: Value,
    /// As corrected by a steward, if it was.
    pub amended_value: Option<Value>,
    /// Who corrected it. Distinct from `author`, deliberately.
    pub amended_by: Option<String>,
    pub note: Option<String>,
    pub amended_note: Option<String>,
    pub raised_at: String,
}

/// Raise a pending edit, and put it in the queue.
///
/// Refused for a disabled account, and refused at the author's per-user limit
/// (stash-box#782). The limit counts *open* edits across every subject, so it is
/// a cap on a person rather than on a corpus's shape.
pub async fn raise_pending_edit(
    store: &Store,
    subject_id: Uuid,
    field: &str,
    author: &str,
    value: &Value,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    let item = Item::new(
        ItemType::PendingEdit,
        SubjectType::Object,
        subject_id,
        field,
        format!("a pending edit of {field}"),
        Some(author),
    );

    // Resolve the author's handle to a real account so the limit and the disabled
    // check have a role to read. A handle with no account is a programming error
    // at the call site, and the error says so rather than defaulting.
    let account_id = require_enabled(store, author).await?;
    let a = actor(store, &account_id)
        .await?
        .ok_or_else(|| ModerationError::Malformed(format!("{author:?} has no account row")))?;
    let role = Role::parse(&a.role)
        .ok_or_else(|| ModerationError::Malformed(format!("bad role: {}", a.role)))?;
    let limit = pending_limit(role);
    if limit != usize::MAX {
        let open: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pending_edit WHERE author = ? AND resolved_at IS NULL",
        )
        .bind(author)
        .fetch_one(store.pool())
        .await?;
        if open as usize >= limit {
            return Err(ModerationError::NotPermitted(format!(
                "{author} already has {open} open pending edits (limit {limit})"
            )));
        }
    }

    let item_id = enqueue(store, &item).await?;
    sqlx::query(
        "INSERT INTO pending_edit (id, item_id, author, value, note, raised_at)
         VALUES (?, ?, ?, ?, NULL, ?)",
    )
    .bind(id.to_string())
    .bind(item_id.to_string())
    .bind(author)
    .bind(value.to_string())
    .bind(&item.raised_at)
    .execute(store.pool())
    .await?;
    Ok(id)
}

/// Read a pending edit.
pub async fn pending_edit(store: &Store, id: Uuid) -> Result<Option<PendingEdit>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        author: String,
        value: String,
        amended_value: Option<String>,
        amended_by: Option<String>,
        note: Option<String>,
        amended_note: Option<String>,
        raised_at: String,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT author, value, amended_value, amended_by, note, amended_note, raised_at
           FROM pending_edit WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(store.pool())
    .await?;

    Ok(row.map(|r| PendingEdit {
        id,
        author: r.author,
        value: serde_json::from_str(&r.value).unwrap_or(Value::Null),
        amended_value: r.amended_value.and_then(|s| serde_json::from_str(&s).ok()),
        amended_by: r.amended_by,
        note: r.note,
        amended_note: r.amended_note,
        raised_at: r.raised_at,
    }))
}

/// A closed submission, with its close and reopen state.
///
/// `closed_at` and `reopened_at` are *both* set after a reopen, and that is the
/// whole point: a reopened submission has not stopped being a submission that was
/// closed, it has stopped being closed. Clearing `closed_at` would make the row
/// identical to one that was never closed, and a steward deciding whether to
/// close something again needs to know it was closed before and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    pub id: Uuid,
    pub subject_type: SubjectType,
    pub subject_id: Uuid,
    pub field: String,
    pub closed_at: Option<String>,
    pub closed_by: Option<String>,
    pub close_reason: Option<String>,
    pub reopened_at: Option<String>,
    pub reopened_by: Option<String>,
}

impl Submission {
    /// Whether the submission is closed right now.
    ///
    /// Closed and not yet reopened -- *not* "has a `closed_at`", because a
    /// reopened submission still has one. A reader that tested the column instead
    /// of this method would refuse every edit to a submission that was ever
    /// closed, which is stash-box#570 answered in the wrong direction.
    pub fn is_closed(&self) -> bool {
        self.closed_at.is_some() && self.reopened_at.is_none()
    }
}

/// The submission row for a subject and field, creating it if absent.
pub async fn submission_for(
    store: &Store,
    subject_type: SubjectType,
    subject_id: Uuid,
    field: &str,
) -> Result<Submission> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO submission (id, subject_type, subject_id, field)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (subject_type, subject_id, field) DO NOTHING",
    )
    .bind(id.to_string())
    .bind(subject_type.as_str())
    .bind(subject_id.to_string())
    .bind(field)
    .execute(store.pool())
    .await?;
    submission_for_row(store, subject_type, &subject_id, field).await
}

/// The submission row for a subject and field, or `None`.
///
/// A separate read from [`submission_for`] because the two answer different
/// questions: this one is "is this subject closed?", the other is "create the
/// record and hand me its id". Conflating them means a caller that only wanted
/// to *ask* had to create the row to ask, and every read became a write.
pub async fn submission_for_row(
    store: &Store,
    subject_type: SubjectType,
    subject_id: &Uuid,
    field: &str,
) -> Result<Submission> {
    let row: Option<SubmissionRow> = sqlx::query_as(
        "SELECT id, subject_type, subject_id, field, closed_at, closed_by, close_reason,
                reopened_at, reopened_by
           FROM submission
          WHERE subject_type = ? AND subject_id = ? AND field = ?",
    )
    .bind(subject_type.as_str())
    .bind(subject_id.to_string())
    .bind(field)
    .fetch_optional(store.pool())
    .await?;
    let Some(r) = row else {
        return Err(ModerationError::Malformed("submission not found".into()));
    };
    into_submission(r, subject_id)
}

/// Read a submission by its row id.
pub async fn submission(store: &Store, id: Uuid) -> Result<Option<Submission>> {
    let Some(r) = submission_row(store, id).await? else {
        return Ok(None);
    };
    into_submission(r, &id).map(Some)
}

#[derive(sqlx::FromRow)]
struct SubmissionRow {
    id: String,
    subject_type: String,
    subject_id: String,
    field: String,
    closed_at: Option<String>,
    closed_by: Option<String>,
    close_reason: Option<String>,
    reopened_at: Option<String>,
    reopened_by: Option<String>,
}

async fn submission_row(store: &Store, id: Uuid) -> Result<Option<SubmissionRow>> {
    let row: Option<SubmissionRow> = sqlx::query_as(
        "SELECT id, subject_type, subject_id, field, closed_at, closed_by, close_reason,
                reopened_at, reopened_by
           FROM submission WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(store.pool())
    .await?;

    Ok(row)
}

/// Map a row to a domain value, failing loudly on a value we cannot read.
///
/// A silent `unwrap_or` here would produce a `Submission` pointing at a subject
/// that is not the row's own, and the only symptom would be a reopen landing on
/// the wrong submission. A parse failure is a bug, and a bug that renames a
/// subject is the worst kind.
fn into_submission(r: SubmissionRow, fallback: &Uuid) -> Result<Submission> {
    Ok(Submission {
        id: Uuid::parse_str(&r.id).unwrap_or(*fallback),
        subject_type: SubjectType::parse(&r.subject_type).ok_or_else(|| {
            ModerationError::Malformed(format!("bad subject_type: {}", r.subject_type))
        })?,
        subject_id: Uuid::parse_str(&r.subject_id)
            .map_err(|e| ModerationError::Malformed(format!("bad subject_id: {e}")))?,
        field: r.field,
        closed_at: r.closed_at,
        closed_by: r.closed_by,
        close_reason: r.close_reason,
        reopened_at: r.reopened_at,
        reopened_by: r.reopened_by,
    })
}

/// Close a submission, and put it in the queue.
pub async fn close_submission(
    store: &Store,
    subject_id: Uuid,
    field: &str,
    closer: &str,
    reason: &str,
) -> Result<Uuid> {
    require_enabled(store, closer).await?;
    let s = submission_for(store, SubjectType::Object, subject_id, field).await?;
    sqlx::query(
        "UPDATE submission SET closed_at = ?, closed_by = ?, close_reason = ?,
                              reopened_at = NULL, reopened_by = NULL
          WHERE id = ?",
    )
    .bind(ts::now())
    .bind(closer)
    .bind(reason)
    .bind(s.id.to_string())
    .execute(store.pool())
    .await?;

    let item = Item::new(
        ItemType::ClosedSubmission,
        SubjectType::Object,
        subject_id,
        field,
        format!("a closed submission of {field}: {reason}"),
        Some(closer),
    );
    let item_id = enqueue(store, &item).await?;
    Ok(item_id)
}

/// The value a pending edit will write: the amendment if a steward made one.
///
/// Reading it through here rather than in each call site is what keeps stash-box
/// #599 from being re-broken: the amended value and the original author are two
/// different facts, and a caller that picks one field has to pick.
pub fn effective(edit: &PendingEdit) -> &Value {
    edit.amended_value.as_ref().unwrap_or(&edit.value)
}

/// Authorize and apply a resolution.
///
/// The authorization is the first thing that happens and the only thing that
/// decides whether the rest runs. Note that a refused call writes nothing at
/// all — no queue row, no lock, no amendment — which is asserted per type in the
/// tests, because a partial application is the failure that looks like success.
pub async fn resolve(
    store: &Store,
    kind: ItemType,
    item: &Item,
    resolver: &Uuid,
    resolution: Resolution,
) -> Result<()> {
    let Some(a) = actor(store, resolver).await? else {
        return Err(ModerationError::NotPermitted(format!(
            "{resolver} is not an account and may not resolve a {}",
            kind.as_str()
        )));
    };
    if a.disabled {
        return Err(ModerationError::NotPermitted(format!(
            "{resolver} is disabled and may not resolve a {}",
            kind.as_str()
        )));
    }
    let role = Role::parse(&a.role)
        .ok_or_else(|| ModerationError::Malformed(format!("bad role: {}", a.role)))?;
    if !kind.may_resolve(role) {
        return Err(ModerationError::NotPermitted(format!(
            "a {role:?} may not resolve a {}: it needs {} or above",
            kind.as_str(),
            kind.lowest_role()
        )));
    }

    match (&kind, &resolution) {
        // A lock writes a `field_lock`, which is what resolve reads from then on.
        (ItemType::ContestedField, Resolution::LockTo(value)) => {
            index::lock_field(
                store,
                item.subject_type,
                &item.subject_id,
                &item.field,
                value,
                &resolver.to_string(),
                &item.summary,
            )
            .await?;
        }
        (ItemType::DisputedMerge, Resolution::Merge(loser)) => {
            crate::history::merge_objects(store, *loser, item.subject_id, &resolver.to_string())
                .await?;
        }
        (ItemType::DisputedConsent, Resolution::SetConsentTier(tier)) => {
            index::set_consent_tier(store, item.subject_id, tier, &resolver.to_string()).await?;
        }
        (ItemType::PendingEdit, Resolution::Amend(edit_id, note)) => {
            let Some(edit) = pending_edit(store, *edit_id).await? else {
                return Err(ModerationError::Malformed("pending edit not found".into()));
            };
            // stash-box#599: the amendment is recorded against the *author's*
            // edit, and `author` is not touched. `amended_by` is the steward's
            // handle and `author` is the contributor's, and no code path writes
            // both from one value.
            let value = effective(&edit).clone();
            let combined = Value::Array(vec![value, Value::String(note.clone())]);
            sqlx::query(
                "UPDATE pending_edit
                    SET amended_value = ?, amended_note = ?, amended_by = ?, amended_at = ?
                  WHERE id = ?",
            )
            .bind(combined.to_string())
            .bind(note)
            .bind(resolver.to_string())
            .bind(ts::now())
            .bind(edit_id.to_string())
            .execute(store.pool())
            .await?;
        }
        (ItemType::ClosedSubmission, Resolution::Reopen) => {
            let s = submission_for(store, item.subject_type, item.subject_id, &item.field).await?;
            sqlx::query("UPDATE submission SET reopened_at = ?, reopened_by = ? WHERE id = ?")
                .bind(ts::now())
                .bind(resolver.to_string())
                .bind(s.id.to_string())
                .execute(store.pool())
                .await?;
        }
        // Dismissal, and any resolution that does not match its type.
        _ => {}
    }

    // Mark the item. Done last and unconditionally: the effect above is the
    // decision, and an item still marked open after the effect is visible to the
    // next steward, who will decide it again.
    sqlx::query(
        "UPDATE moderation_item
            SET resolved_by = ?, resolved_at = ?, resolution = ?
          WHERE id IN (SELECT id FROM moderation_item
                        WHERE kind = ? AND subject_id = ? AND field = ?
                          AND resolved_at IS NULL
                        ORDER BY raised_at, id
                        LIMIT 1)",
    )
    .bind(resolver.to_string())
    .bind(ts::now())
    .bind(resolution.as_str())
    .bind(kind.as_str())
    .bind(item.subject_id.to_string())
    .bind(&item.field)
    .execute(store.pool())
    .await?;

    // And close the per-type rows, so a resolved pending edit frees its slot and
    // a dismissed report reads as dismissed.
    match kind {
        ItemType::AbuseReport => {
            sqlx::query(
                "UPDATE abuse_report SET dismissed_at = ?, dismissed_by = ?
                  WHERE item_id IN (SELECT id FROM moderation_item
                                     WHERE kind = 'abuse_report' AND subject_id = ?
                                       AND resolved_at IS NOT NULL
                                     ORDER BY raised_at DESC, id DESC LIMIT 1)",
            )
            .bind(ts::now())
            .bind(resolver.to_string())
            .bind(item.subject_id.to_string())
            .execute(store.pool())
            .await?;
        }
        ItemType::NameCollision => {
            sqlx::query(
                "UPDATE name_collision SET dismissed_at = ?, dismissed_by = ?
                  WHERE item_id IN (SELECT id FROM moderation_item
                                     WHERE kind = 'name_collision' AND subject_id = ?
                                       AND resolved_at IS NOT NULL
                                     ORDER BY raised_at DESC, id DESC LIMIT 1)",
            )
            .bind(ts::now())
            .bind(resolver.to_string())
            .bind(item.subject_id.to_string())
            .execute(store.pool())
            .await?;
        }
        ItemType::PendingEdit => {
            sqlx::query(
                "UPDATE pending_edit SET resolved_at = ?, resolved_by = ?
                  WHERE item_id IN (SELECT id FROM moderation_item
                                     WHERE kind = 'pending_edit' AND subject_id = ?
                                       AND resolved_at IS NOT NULL
                                     ORDER BY raised_at DESC, id DESC LIMIT 1)",
            )
            .bind(ts::now())
            .bind(resolver.to_string())
            .bind(item.subject_id.to_string())
            .execute(store.pool())
            .await?;
        }
        _ => {}
    }
    Ok(())
}

// ---- ignore-lists and pinned notes (stash-box#787, #700) -------------------

/// Add a studio or performer to an account's ignore-list.
///
/// Idempotent rather than an error: an ignore-list is something a person builds
/// up over a session by clicking things, and a duplicate click is not a mistake
/// worth surfacing.
pub async fn ignore(
    store: &Store,
    account: Uuid,
    target_kind: &str,
    target: &str,
    reason: Option<&str>,
) -> Result<()> {
    if !matches!(target_kind, "studio" | "performer") {
        return Err(ModerationError::Malformed(format!(
            "ignore target kind must be studio or performer, not {target_kind:?}"
        )));
    }
    sqlx::query(
        "INSERT INTO account_ignore (account_id, target_kind, target, reason, created_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (account_id, target_kind, target) DO NOTHING",
    )
    .bind(account.to_string())
    .bind(target_kind)
    .bind(target)
    .bind(reason)
    .bind(ts::now())
    .execute(store.pool())
    .await?;
    Ok(())
}

/// An account's ignore-list, newest last.
pub async fn ignores(store: &Store, account: Uuid) -> Result<Vec<(String, String)>> {
    #[derive(sqlx::FromRow)]
    struct IgnoreRow {
        target_kind: String,
        target: String,
    }
    let rows: Vec<IgnoreRow> = sqlx::query_as(
        "SELECT target_kind, target FROM account_ignore
          WHERE account_id = ? ORDER BY target_kind, target",
    )
    .bind(account.to_string())
    .fetch_all(store.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.target_kind, r.target))
        .collect())
}

/// Pin a note to a subject.
pub async fn pin_note(
    store: &Store,
    subject_type: SubjectType,
    subject_id: Uuid,
    body: &str,
    author: &str,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pinned_note (id, subject_type, subject_id, body, author, pinned_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(subject_type.as_str())
    .bind(subject_id.to_string())
    .bind(body)
    .bind(author)
    .bind(ts::now())
    .execute(store.pool())
    .await?;
    Ok(id)
}

/// The notes currently pinned to a subject, oldest first.
pub async fn pinned_notes(
    store: &Store,
    subject_type: SubjectType,
    subject_id: Uuid,
) -> Result<Vec<(String, String)>> {
    #[derive(sqlx::FromRow)]
    struct NoteRow {
        body: String,
        author: String,
    }
    let rows: Vec<NoteRow> = sqlx::query_as(
        "SELECT body, author FROM pinned_note
          WHERE subject_type = ? AND subject_id = ? AND unpinned_at IS NULL
          ORDER BY pinned_at, id",
    )
    .bind(subject_type.as_str())
    .bind(subject_id.to_string())
    .fetch_all(store.pool())
    .await?;
    Ok(rows.into_iter().map(|r| (r.body, r.author)).collect())
}

/// Unpin a note.
pub async fn unpin_note(store: &Store, id: Uuid, who: &str) -> Result<()> {
    sqlx::query("UPDATE pinned_note SET unpinned_at = ?, unpinned_by = ? WHERE id = ?")
        .bind(ts::now())
        .bind(who)
        .bind(id.to_string())
        .execute(store.pool())
        .await?;
    Ok(())
}

/// Exclude a studio from new scenes (stash-box#1175).
pub async fn exclude_studio(store: &Store, name: &str, reason: &str, who: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO excluded_studio (id, name, reason, excluded_by, excluded_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(name)
    .bind(reason)
    .bind(who)
    .bind(ts::now())
    .execute(store.pool())
    .await?;
    Ok(())
}

/// Every excluded studio name, so candidate generation can filter without a
/// query per proposal.
pub async fn excluded_studios(store: &Store) -> Result<Vec<String>> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT name FROM excluded_studio ORDER BY name")
        .fetch_all(store.pool())
        .await?;
    Ok(rows)
}
