//! Persistence for §8.1's proposal set, votes and field locks.
//!
//! This is the only place that touches `field_proposal`, `vote` and
//! `field_lock`. The reason it is a separate module from `resolve` is that
//! every caller of the voting machinery needs the same rows, and a `resolve`
//! that owned its SQL would be resolvable only by itself.
//!
//! ## Why values are stored as text and ids as text
//!
//! The `field_proposal.id` column is `TEXT` in the schema rather than a native
//! uuid column, because a peer index (§13) proposes on subjects whose ids come
//! from another system. Keeping ids as opaque text here means the federation
//! path does not need a parallel id type. `commons-core` still types them as
//! `Uuid` at the API boundary, and these functions are the one place the two
//! meet, so the conversion is written once.

use crate::db::{Store, StoreError};
use commons_core::{FieldProposal, ProposalSource, ProposerKind, SubjectType, Vote};
use sqlx::Row;
use uuid::Uuid;

/// A field's lock, read back from `field_lock`.
///
/// `value_json` is nullable in the schema: a lock with no value means "this field
/// is closed", which is different from "this field is pinned to this value".
/// Both are legitimate and the difference is what a steward needs to see when
/// deciding whether to unlock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLock {
    pub value_json: Option<String>,
    pub locked_by: Option<String>,
    pub locked_at: String,
}

/// Store a proposal, refusing a confidence outside `0.0..=1.0`.
///
/// The refusal is here rather than at the read because a confidence of 1.5
/// means the caller is confused about what the number means, and clamping it
/// would hide that. The schema accepts any REAL; the check is ours to make.
pub async fn insert_proposal(store: &Store, p: &FieldProposal) -> Result<Uuid, StoreError> {
    if let Some(c) = p.confidence {
        if !(0.0..=1.0).contains(&c) {
            return Err(StoreError::Invalid {
                what: "proposal confidence",
                why: format!("{c} is outside 0.0..=1.0 for field {}", p.field),
            });
        }
    }
    sqlx::query(
        "INSERT INTO field_proposal
           (id, subject_type, subject_id, field, value_json, source,
            proposer_kind, proposer_id, confidence, created_at, justification)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(p.id.to_string())
    .bind(p.subject_type.as_str())
    .bind(p.subject_id.to_string())
    .bind(&p.field)
    .bind(&p.value_json)
    .bind(p.source.as_str())
    .bind(p.proposer_kind.as_str())
    .bind(&p.proposer_id)
    .bind(p.confidence)
    .bind(&p.created_at)
    .bind(None::<String>)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(p.id)
}

/// Insert a proposal unless an identical one already exists.
///
/// Returns the proposal's id and whether a row was written. Both, because a
/// caller generating candidates wants the count and a caller addressing a
/// proposal wants the id — and returning only the id would make the second
/// caller do a second query to find out which case it was in. `field_proposal_uniq_idx` covers
/// `(subject_type, subject_id, field, value_json, source, proposer_id)`, so
/// "identical" means the same value from the same source about the same field —
/// which is exactly the definition §8.2's candidate generation needs to be
/// re-runnable. A rescan re-proposes every value from every proposer, and
/// without this the proposal table grows a duplicate pile on every pass.
///
/// The check is `INSERT OR IGNORE` rather than select-then-insert: the unique
/// index is the authority, and a select-then-insert races with a concurrent
/// scanner. `OR IGNORE` is also the only version that cannot fail the scan, and
/// a rescan that can fail is a rescan that gets retried.
pub async fn insert_proposal_if_absent(
    store: &Store,
    p: &FieldProposal,
) -> Result<(Uuid, bool), StoreError> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT OR IGNORE INTO field_proposal
           (id, subject_type, subject_id, field, value_json, source,
            proposer_kind, proposer_id, confidence, created_at, justification)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(p.subject_type.as_str())
    .bind(p.subject_id.to_string())
    .bind(&p.field)
    .bind(&p.value_json)
    .bind(p.source.as_str())
    .bind(p.proposer_kind.as_str())
    .bind(&p.proposer_id)
    .bind(p.confidence)
    .bind(&p.created_at)
    .bind(&p.justification)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    // `rows_affected == 0` means the unique index rejected it, so this proposal
    // is already on file. The *existing* id is what a caller should learn, so
    // read it back rather than returning an id that was never written.
    if sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM field_proposal WHERE id = ?")
        .bind(id.to_string())
        .fetch_one(store.pool())
        .await
        .map_err(StoreError::Query)?
        == 1
    {
        return Ok((id, true));
    }
    let existing: String = sqlx::query_scalar(
        "SELECT id FROM field_proposal
          WHERE subject_type = ? AND subject_id = ? AND field = ? AND value_json = ?
            AND source = ? AND COALESCE(proposer_id, '') = COALESCE(?, '')",
    )
    .bind(p.subject_type.as_str())
    .bind(p.subject_id.to_string())
    .bind(&p.field)
    .bind(&p.value_json)
    .bind(p.source.as_str())
    .bind(&p.proposer_id)
    .fetch_one(store.pool())
    .await
    .map_err(StoreError::Query)?;
    let parsed = Uuid::parse_str(&existing).map_err(|_| StoreError::Invalid {
        what: "field_proposal.id",
        why: format!("not a uuid: {existing}"),
    })?;
    Ok((parsed, false))
}

/// Attach the human-readable reason a proposal exists (§8.2).
///
/// Separate from the insert because a caller frequently builds the proposal
/// first and only decides its wording later — and because the wording is the
/// part the UI shows, so it is worth being able to set it without a rewrite.
pub async fn set_justification(
    store: &Store,
    proposal: &Uuid,
    justification: &str,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE field_proposal SET justification = ? WHERE id = ?")
        .bind(justification)
        .bind(proposal.to_string())
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// One column, decoded. Every read goes through here so a schema mismatch is
/// reported as `Row { column }` rather than as a bare sqlx error with the
/// column name somewhere in its source chain.
fn col<'r, T>(r: &'r sqlx::sqlite::SqliteRow, name: &'static str) -> Result<T, StoreError>
where
    T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    r.try_get(name).map_err(|source| StoreError::Row {
        column: name,
        source,
    })
}

fn row_to_proposal(r: &sqlx::sqlite::SqliteRow) -> Result<FieldProposal, StoreError> {
    let s: String = col(r, "subject_type")?;
    let subject_type = SubjectType::parse(&s).ok_or(StoreError::Invalid {
        what: "subject_type",
        why: format!("unknown value {s:?}"),
    })?;
    let s: String = col(r, "source")?;
    let source = ProposalSource::parse(&s).ok_or(StoreError::Invalid {
        what: "proposal source",
        why: format!("unknown value {s:?}"),
    })?;
    let s: String = col(r, "proposer_kind")?;
    let proposer_kind = ProposerKind::parse(&s).ok_or(StoreError::Invalid {
        what: "proposer_kind",
        why: format!("unknown value {s:?}"),
    })?;
    let id: String = col(r, "id")?;
    let subject_id: String = col(r, "subject_id")?;
    Ok(FieldProposal {
        id: Uuid::parse_str(&id).map_err(|e| StoreError::Invalid {
            what: "proposal id",
            why: e.to_string(),
        })?,
        subject_type,
        subject_id: Uuid::parse_str(&subject_id).map_err(|e| StoreError::Invalid {
            what: "subject id",
            why: e.to_string(),
        })?,
        field: col(r, "field")?,
        value_json: col(r, "value_json")?,
        source,
        proposer_kind,
        proposer_id: col(r, "proposer_id")?,
        confidence: col(r, "confidence")?,
        created_at: col(r, "created_at")?,
        justification: col(r, "justification")?,
    })
}

/// Every proposal for one `(subject, field)`, oldest first.
///
/// Oldest first, not by id: two proposals created in the same second have no
/// meaningful order by id, and "the first one anyone proposed" is a fact about
/// time that a uuid sort would get wrong about half the time.
pub async fn proposals_for(
    store: &Store,
    subject_type: SubjectType,
    subject_id: &Uuid,
    field: &str,
) -> Result<Vec<FieldProposal>, StoreError> {
    let rows = sqlx::query(
        "SELECT * FROM field_proposal
          WHERE subject_type = ? AND subject_id = ? AND field = ?
          ORDER BY created_at, id",
    )
    .bind(subject_type.as_str())
    .bind(subject_id.to_string())
    .bind(field)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    rows.iter().map(row_to_proposal).collect()
}

pub async fn proposal_by_id(store: &Store, id: &Uuid) -> Result<Option<FieldProposal>, StoreError> {
    let row = sqlx::query("SELECT * FROM field_proposal WHERE id = ?")
        .bind(id.to_string())
        .fetch_optional(store.pool())
        .await
        .map_err(StoreError::Query)?;
    row.as_ref().map(row_to_proposal).transpose()
}

/// Rewrite a machine proposal's confidence.
///
/// A test seam like `age_vote`: the only honest way to see how a field resolves
/// at a different confidence without a model in the loop.
pub async fn set_confidence(
    store: &Store,
    proposal: &Uuid,
    confidence: f64,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE field_proposal SET confidence = ? WHERE id = ?")
        .bind(confidence)
        .bind(proposal.to_string())
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

pub async fn justification(store: &Store, id: &Uuid) -> Result<Option<String>, StoreError> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT justification FROM field_proposal WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?;
    Ok(row.and_then(|r| r.0))
}

/// Record a vote at the account's current reputation, so history does not
/// silently re-weight when reputation later moves.
///
/// The weight is frozen here on purpose (§8.1, `vote.weight`). A vote cast by a
/// newcomer weighs what the newcomer was worth that day; if reputation were
/// looked up at resolve time, a single confirmed proposal would retroactively
/// multiply every vote that newcomer ever cast, which is how a reputation system
/// turns into a leverage multiplier.
pub async fn insert_vote(store: &Store, v: &Vote) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO vote (id, proposal_id, account_id, field, weight, retracted, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(v.id.to_string())
    .bind(v.proposal_id.to_string())
    .bind(v.account_id.to_string())
    .bind(&v.field)
    .bind(v.weight)
    .bind(i64::from(v.retracted))
    .bind(&v.created_at)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// The live votes on one proposal, as `(account, weight, created_at)`.
///
/// Retracted rows are filtered here rather than at the caller. A retraction
/// keeps its row so the history is intact (§8.6's recompute-from-the-edit-set
/// rule), so "is this vote live" is a column, not an absence — and getting that
/// filter wrong is invisible until a retracted vote changes a winner.
pub async fn live_votes(
    store: &Store,
    proposal: &Uuid,
) -> Result<Vec<(String, f64, String)>, StoreError> {
    let rows = sqlx::query(
        "SELECT account_id, weight, created_at FROM vote
          WHERE proposal_id = ? AND retracted = 0
          ORDER BY created_at, id",
    )
    .bind(proposal.to_string())
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<String, _>("account_id"),
                r.get::<f64, _>("weight"),
                r.get::<String, _>("created_at"),
            )
        })
        .collect())
}

/// Mark a vote retracted. The row stays, so the history of what was believed
/// and when is intact — a deleted vote is indistinguishable from a vote never
/// cast, and §8.6 needs the difference.
pub async fn retract_vote(
    store: &Store,
    account: &Uuid,
    proposal: &Uuid,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE vote SET retracted = 1 WHERE proposal_id = ? AND account_id = ?")
        .bind(proposal.to_string())
        .bind(account.to_string())
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// Backdate a vote, for testing decay without waiting a year.
///
/// This is a test seam, not an API: it is the only way to get a year-old vote
/// into a fixture, and a fixture that cannot express the case cannot test it.
/// It lives here rather than in the test module because the test module has no
/// SQL of its own by design.
///
/// The arithmetic is done here rather than in SQLite's `datetime()`. That
/// function rewrites a timestamp into its own format, and the schema's format
/// is RFC 3339 (`2026-09-26T17:15:11Z`) -- so an SQL-side shift silently
/// produced `2025-09-26 17:15:11`, which no longer parses as the thing the rest
/// of the code reads. The decay then treated a year-old vote as brand new, and
/// every test that aged a vote passed for the wrong reason.
pub async fn age_vote(store: &Store, proposal: &Uuid, days: i64) -> Result<(), StoreError> {
    let rows = sqlx::query("SELECT id, created_at FROM vote WHERE proposal_id = ?")
        .bind(proposal.to_string())
        .fetch_all(store.pool())
        .await
        .map_err(StoreError::Query)?;
    for r in rows {
        let at: String = r.get("created_at");
        let shifted = commons_core::ts::shift_days(&at, -days).ok_or(StoreError::Invalid {
            what: "vote timestamp",
            why: format!("{at:?} is not a timestamp this code can shift"),
        })?;
        sqlx::query("UPDATE vote SET created_at = ? WHERE id = ?")
            .bind(shifted)
            .bind(r.get::<String, _>("id"))
            .execute(store.pool())
            .await
            .map_err(StoreError::Query)?;
    }
    Ok(())
}

/// The lock on a field, if any.
pub async fn field_lock(
    store: &Store,
    subject_type: SubjectType,
    subject_id: &Uuid,
    field: &str,
) -> Result<Option<FieldLock>, StoreError> {
    let row = sqlx::query(
        "SELECT value_json, locked_by, locked_at FROM field_lock
          WHERE subject_type = ? AND subject_id = ? AND field = ?",
    )
    .bind(subject_type.as_str())
    .bind(subject_id.to_string())
    .bind(field)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(row.map(|r| FieldLock {
        value_json: r.get("value_json"),
        locked_by: r.get("locked_by"),
        locked_at: r.get("locked_at"),
    }))
}

/// Pin a field. An existing lock is replaced, which is how a steward re-pins a
/// field to a different value without a separate "re-lock" verb.
pub async fn set_field_lock(
    store: &Store,
    subject_type: SubjectType,
    subject_id: &Uuid,
    field: &str,
    value_json: Option<&str>,
    locked_by: Option<&Uuid>,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO field_lock (subject_type, subject_id, field, value_json, locked_by, locked_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (subject_type, subject_id, field)
           DO UPDATE SET value_json = excluded.value_json,
                         locked_by  = excluded.locked_by,
                         locked_at  = excluded.locked_at",
    )
    .bind(subject_type.as_str())
    .bind(subject_id.to_string())
    .bind(field)
    .bind(value_json)
    .bind(locked_by.map(|u| u.to_string()))
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

pub async fn clear_field_lock(
    store: &Store,
    subject_type: SubjectType,
    subject_id: &Uuid,
    field: &str,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM field_lock WHERE subject_type = ? AND subject_id = ? AND field = ?")
        .bind(subject_type.as_str())
        .bind(subject_id.to_string())
        .bind(field)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// Accounts that voted the same way on a field, for sybil damping (§8.3).
///
/// Grouped in SQL rather than fetched and counted in Rust because the whole
/// point is to answer "how many distinct accounts back this" for every
/// proposal, and a query per proposal turns resolve into N+1 on a field with
/// many proposals.
pub async fn backer_counts(
    store: &Store,
    field: &str,
    since: &str,
) -> Result<Vec<(String, i64)>, StoreError> {
    let rows = sqlx::query(
        "SELECT p.id AS id, COUNT(DISTINCT v.account_id) AS backers
           FROM field_proposal p
           LEFT JOIN vote v ON v.proposal_id = p.id AND v.retracted = 0
                              AND v.created_at >= ?
          WHERE p.field = ?
          GROUP BY p.id",
    )
    .bind(since)
    .bind(field)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>("id"), r.get::<i64, _>("backers")))
        .collect())
}

/// Every account that has ever voted on a field, with how many live votes each
/// has there. Used by §8.3's coordination check.
pub async fn field_voters(store: &Store, field: &str) -> Result<Vec<(String, i64)>, StoreError> {
    let rows = sqlx::query(
        "SELECT account_id, COUNT(*) AS n FROM vote
          WHERE field = ? AND retracted = 0
          GROUP BY account_id",
    )
    .bind(field)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>("account_id"), r.get::<i64, _>("n")))
        .collect())
}
