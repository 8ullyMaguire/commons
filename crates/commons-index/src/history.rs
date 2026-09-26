//! §8.6 — edit history, and the rule that makes it correct.
//!
//! # The rule
//!
//! **A field's score is recomputed from the accepted-edit set, never maintained
//! as a counter.**
//!
//! Everything in this module follows from that sentence, so it is worth being
//! precise about what it forbids. There is no `score` column on a subject and no
//! `total_votes` on a field. `field_score` reads the accepted set and reduces it.
//! Nothing in this file adds to a number that was already there.
//!
//! The reason is two real upstream bugs, and they are worth naming because both
//! were counter bugs:
//!
//! * **stash-box#943** — merging two entities silently lost edits. A merge moved
//!   rows; a score is a counter over rows; so a merge that forgot a row produced
//!   a score that was quietly wrong, and nothing downstream could tell.
//! * **stash-box#9** — `null` and *unset* were the same thing, so a deliberately
//!   cleared field was indistinguishable from one nobody ever filled in.
//!
//! A recomputation is immune to the first because there is nothing to drift: the
//! number is a function of the rows, so any change to the rows changes the number
//! and the number cannot be *stale*. The second is a separate fix, and it is a
//! modelling one — see [`FieldState`].
//!
//! # The three states
//!
//! [`FieldState`] has three variants, not two, and the third is the point of
//! §8.6.2:
//!
//! | state | meaning | how it is stored |
//! |---|---|---|
//! | `Unset` | nobody has set this field | no row in `field_edit` |
//! | `Set` | a value | a row whose `value_json` is that value |
//! | `Null` | deliberately set to null | a row whose `value_json` is `null` |
//!
//! `Null` needs to exist separately from `Unset` because a user who cleared a
//! field has done something, and a UI that cannot tell that from a field nobody
//! touched cannot show them what they did. It also needs to exist separately from
//! `Set`, because a reader asking "what is this field" and a reader asking "is
//! this field known" are different questions.

use commons_core::{ts, FieldProposal, SubjectType};
use commons_store::{Store, StoreError};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("edit {0} is not in this field's history, so it cannot be reverted to")]
    NotAnEditOfThisField(Uuid),
    #[error("edit {0} does not exist")]
    NoSuchEdit(Uuid),
    #[error("edit {entry} belongs to {author}, and {who} did not make it")]
    NotYourEdit {
        entry: Uuid,
        author: String,
        who: String,
    },
    #[error("an object cannot be merged into itself")]
    SelfMerge,
    #[error("edit {0} is no longer in the accepted set, so there is nothing to retract")]
    NotAccepted(Uuid),
}

impl From<HistoryError> for StoreError {
    fn from(e: HistoryError) -> Self {
        match e {
            HistoryError::Store(s) => s,
            other => StoreError::Invalid {
                what: "edit history",
                why: other.to_string(),
            },
        }
    }
}

/// Whether a field has a value, is deliberately null, or has never been set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldState {
    /// No accepted edit: nobody has said anything about this field.
    Unset,
    /// An accepted edit with a value.
    Set,
    /// An accepted edit whose value is JSON `null`: somebody deliberately cleared
    /// this. Distinct from `Unset` (§8.6.2) and from `Set`.
    Null,
}

impl FieldState {
    /// Is there a value to show?
    ///
    /// False for `Unset` and `Null` alike, which is what a `Display` should do
    /// and *not* what a reader asking "was this cleared" should do — that is
    /// `is_set`, below. Both exist because conflating them is the bug.
    pub fn has_value(self) -> bool {
        matches!(self, FieldState::Set)
    }

    /// Is this field known to somebody, whatever the value?
    pub fn is_set(self) -> bool {
        !matches!(self, FieldState::Unset)
    }
}

/// One accepted edit, as history shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    /// The edit's own id. A stable handle for blame, revert and removal, and
    /// *not* the proposal id: one proposal can be accepted, retracted and
    /// accepted again, and each acceptance is a separate event.
    pub id: Uuid,
    /// The proposal this edit came from, if it came from one. Null for an edit
    /// with no proposal behind it — a merge record's moved edits keep their
    /// proposal, but a re-derivation need not have one.
    pub proposal_id: Option<Uuid>,
    pub field: String,
    pub value: Value,
    /// The author, or `None` once they asked to be unlinked (§8.6.5).
    pub author: Option<String>,
    /// The weight this edit carries, from when it was accepted. Frozen, like a
    /// vote's: an edit's weight is the judgement made at the time, and
    /// re-deriving it from today's reputation would rewrite history every time
    /// somebody's standing changed.
    pub weight: f64,
    pub accepted_at: Option<String>,
    pub retracted_at: Option<String>,
    /// When the attribution was removed, if it was.
    pub removed: Option<String>,
    /// Why this edit exists, when that is not obvious — a revert says so.
    pub justification: Option<String>,
}

/// A merge of two object identities.
#[derive(Debug, Clone, PartialEq)]
pub struct Merge {
    pub id: Uuid,
    pub winner: Uuid,
    pub loser: Uuid,
    /// How many edits moved. Zero is a legitimate value and is recorded rather
    /// than treated as failure: a merge with nothing to move still happened, and
    /// a silent success is indistinguishable from a merge that worked.
    pub moved_edits: i64,
    pub actor: Option<String>,
    pub created_at: String,
}

/// A report about a history entry (stash-box#656).
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub id: Uuid,
    pub entry: Uuid,
    /// The reporter, kept even after the entry's attribution is removed.
    pub reporter: String,
    pub reason: String,
    pub created_at: String,
}

// ---- accepting and retracting ---------------------------------------------

/// Record an accepted edit.
///
/// Takes the proposal rather than the subject, field and value separately,
/// because those three *are* the proposal: passing them alongside its id allowed
/// them to disagree, and a caller that passed the right proposal with the wrong
/// field would write an edit to a field no ballot ever voted on. The eight
/// arguments this had were four fields of one struct and three ways to be wrong.
///
/// This is the only way into the accepted set, and it is the whole of §8.6's
/// correctness rule on the write side: the set is a set of rows, and a score over
/// it is a fold. Accepting the same proposal twice is refused by a partial unique
/// index rather than by a check here, so the invariant holds even if a second
/// caller appears.
///
/// `weight` stays a separate argument, because it is the one thing about an
/// acceptance that is *not* in the proposal: the proposal says what, the weight
/// says how much the accepted set should count it at, and the two are chosen at
/// different moments by different code.
pub async fn accept(
    store: &Store,
    proposal: &FieldProposal,
    author: Option<&str>,
    weight: f64,
) -> Result<Uuid, HistoryError> {
    let id = Uuid::new_v4();
    // `seq` and the insert are one transaction, so a sequence value is never
    // allocated and then lost -- which would leave a hole, and a hole in a total
    // order is not a hole in a sequence but a pair of rows whose relative order
    // is now decided by whatever falls out of the sort.
    let mut tx = store.pool().begin().await.map_err(StoreError::Query)?;
    let seq = next_seq(&mut tx).await?;
    sqlx::query(
        "INSERT INTO field_edit
           (id, proposal_id, subject_type, subject_id, field, value_json, author,
            weight, accepted_at, created_at, seq)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(proposal.id.to_string())
    .bind(proposal.subject_type.as_str())
    .bind(proposal.subject_id.to_string())
    .bind(&proposal.field)
    .bind(&proposal.value_json)
    .bind(author)
    .bind(weight)
    .bind(ts::now())
    .bind(ts::now())
    .bind(seq)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;
    tx.commit().await.map_err(StoreError::Query)?;
    Ok(id)
}

/// Take the next value from the table-wide sequence.
///
/// One row, updated in place. See migration 0010 for why this is not a
/// `BIGSERIAL` on one side and an `AUTOINCREMENT` on the other: SQLite cannot
/// declare two primary keys, so the autoincrement is unavailable beside a text
/// `id`, and two different mechanisms on two backends is two different meanings.
///
/// The insert-if-absent is inside the transaction, so two writers racing here
/// serialise on the row rather than both inserting and one of them losing its
/// sequence to a constraint violation at commit.
async fn next_seq(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<i64, HistoryError> {
    sqlx::query("INSERT OR IGNORE INTO field_edit_seq (id, next) VALUES (1, 1)")
        .execute(&mut **tx)
        .await
        .map_err(StoreError::Query)?;
    sqlx::query("UPDATE field_edit_seq SET next = next + 1 WHERE id = 1")
        .execute(&mut **tx)
        .await
        .map_err(StoreError::Query)?;
    let next: i64 = sqlx::query_scalar("SELECT next FROM field_edit_seq WHERE id = 1")
        .fetch_one(&mut **tx)
        .await
        .map_err(StoreError::Query)?;
    Ok(next - 1)
}

/// Take an edit out of the accepted set.
///
/// The row stays. A retracted edit is not a proposal that never happened, and a
/// hard delete would make "rejected" and "reverted" the same absence. Every
/// score query filters on `retracted_at IS NULL`, so the row is excluded by
/// being marked rather than by not existing — which is also what makes
/// re-accepting it possible later.
pub async fn retract(store: &Store, edit: Uuid) -> Result<(), HistoryError> {
    let changed = sqlx::query(
        "UPDATE field_edit SET retracted_at = ?
          WHERE id = ? AND accepted_at IS NOT NULL AND retracted_at IS NULL",
    )
    .bind(ts::now())
    .bind(edit.to_string())
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    if changed.rows_affected() == 0 {
        return Err(HistoryError::NotAccepted(edit));
    }
    Ok(())
}

// ---- reading ---------------------------------------------------------------

/// The accepted edits for one field, in acceptance order.
///
/// Order is `seq`, a monotonic per-table counter, and not `id`.
///
/// This was found by `a_fields_history_is_the_accepted_edits_in_order`, which
/// read back `["Second", "First"]`. Two edits accepted in the same millisecond --
/// which is every pair of edits in these fixtures, and most pairs in a real scan
/// of a few hundred thousand rows -- tie on `accepted_at`, and the fallback was
/// `id`, a random uuid. So the tie was broken at random and the history came out
/// in an order that had nothing to do with time. "The first one anybody proposed"
/// is a fact about time, and a uuid sort gets it wrong about half the time.
///
/// `seq` is a real ordering: a rowid-backed autoincrement in SQLite and a
/// `BIGSERIAL` in Postgres, both monotonic within the table, so it breaks a
/// timestamp tie in the order the rows were written. It is not a score and not
/// anything §8.6 recomputes; it is a sequence, and a sequence cannot drift.
pub async fn field_history(
    store: &Store,
    subject_id: Uuid,
    field: &str,
) -> Result<Vec<HistoryEntry>, HistoryError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, proposal_id, field, value_json, author, weight, accepted_at,
                retracted_at, removed_at, justification
           FROM field_edit
          WHERE subject_id = ? AND field = ? AND retracted_at IS NULL
            AND removed_at IS NULL
           ORDER BY seq",
    )
    .bind(subject_id.to_string())
    .bind(field)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(rows.into_iter().map(Row::into_entry).collect())
}

/// Every accepted edit for a subject, whatever the field.
pub async fn subject_history(
    store: &Store,
    subject_id: Uuid,
) -> Result<Vec<HistoryEntry>, HistoryError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, proposal_id, field, value_json, author, weight, accepted_at,
                retracted_at, removed_at, justification
           FROM field_edit
          WHERE subject_id = ? AND retracted_at IS NULL AND removed_at IS NULL
           ORDER BY seq",
    )
    .bind(subject_id.to_string())
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(rows.into_iter().map(Row::into_entry).collect())
}

/// One entry by id, including one whose attribution was removed.
///
/// Separate from `field_history` because a removed entry is *not* in any field's
/// history — that is the point of removing it — but it is still a row, and
/// `remove_history_entry` needs to read it back to check authorship.
pub async fn history_entry(store: &Store, id: Uuid) -> Result<Option<HistoryEntry>, HistoryError> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, proposal_id, field, value_json, author, weight, accepted_at,
                retracted_at, removed_at, justification
           FROM field_edit WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(row.map(Row::into_entry))
}

/// Whether a field is unset, set, or deliberately null.
///
/// The three-way answer §8.6.2 requires, and it is derived rather than stored:
/// the question is "what is the current accepted value, and is its absence
/// meaningful", which is a question about the heaviest accepted edit.
pub async fn field_state(
    store: &Store,
    subject_id: Uuid,
    field: &str,
) -> Result<FieldState, HistoryError> {
    match current_entry(store, subject_id, field).await? {
        // No accepted edit at all. Not `Null`: nobody said anything, which is a
        // different fact from somebody saying "nothing".
        None => Ok(FieldState::Unset),
        Some(e) if e.value.is_null() => Ok(FieldState::Null),
        Some(_) => Ok(FieldState::Set),
    }
}

/// The current accepted value, if there is one.
///
/// `None` for both `Unset` and `Null`. That is right for "what do I display" and
/// wrong for "was this cleared" — which is [`field_state`], and the two existing
/// separately for that reason.
pub async fn current_value(
    store: &Store,
    subject_id: Uuid,
    field: &str,
) -> Result<Option<Value>, HistoryError> {
    Ok(current_entry(store, subject_id, field)
        .await?
        .map(|e| e.value))
}

/// The heaviest accepted edit, which is the field's value and its blame.
async fn current_entry(
    store: &Store,
    subject_id: Uuid,
    field: &str,
) -> Result<Option<HistoryEntry>, HistoryError> {
    // Ordered by weight, and then by recency, and the recency tiebreak is what
    // makes a deliberate null visible.
    //
    // The first version ordered by `weight DESC, accepted_at, id` alone, so a
    // field that had been set to a value and then deliberately nulled -- both
    // accepted at the same weight, as two ordinary edits are -- reported `Set`
    // with the *old* value. `a_null_is_not_an_unset` caught it: the whole of
    // stash-box#9 is that clearing a field is something a user did, and a reader
    // that cannot see it is the bug all over again.
    //
    // So the tiebreak is recency, not id. Two edits in the same millisecond are
    // still ordered, because `created_at` is what `accept` wrote and the
    // sequence column is below.
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, proposal_id, field, value_json, author, weight, accepted_at,
                retracted_at, removed_at, justification
           FROM field_edit
          WHERE subject_id = ? AND field = ? AND retracted_at IS NULL
          ORDER BY weight DESC, seq DESC
          LIMIT 1",
    )
    .bind(subject_id.to_string())
    .bind(field)
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(row.map(Row::into_entry))
}

/// The field's score: a fold over the accepted set, and nothing else.
///
/// `max(weight)`, not `sum(weight)`. A field's value *is* the heaviest accepted
/// edit's value, so a sum would be a number with no value behind it — and a
/// `MAX` is the one reduction that cannot drift, because it is a function of the
/// rows and every row is read.
///
/// There is no other implementation of this number in the codebase. That is the
/// enforcement mechanism for §8.6's rule: a counter would be a second
/// implementation, and `a_fields_score_is_the_heaviest_accepted_edit_not_a_sum`
/// exists to make the second one fail.
pub async fn field_score(
    store: &Store,
    subject_id: Uuid,
    field: &str,
) -> Result<f64, HistoryError> {
    recompute(store, subject_id, field).await
}

/// A field's score, recomputed from the accepted-edit set from scratch.
///
/// The name is the point. This is the whole of §8.6: not "update the score" but
/// "compute the score". A caller that has just moved rows calls this, and the
/// value it gets cannot disagree with [`field_score`] because it *is*
/// `field_score` — the test that matters,
/// `a_score_after_a_merge_is_identical_to_a_fresh_recomputation`, calls both and
/// compares, which is a check that no cached or maintained value has drifted.
pub async fn recompute(store: &Store, subject_id: Uuid, field: &str) -> Result<f64, HistoryError> {
    let best: Option<f64> = sqlx::query_scalar(
        "SELECT MAX(weight) FROM field_edit
          WHERE subject_id = ? AND field = ? AND retracted_at IS NULL",
    )
    .bind(subject_id.to_string())
    .bind(field)
    .fetch_one(store.pool())
    .await
    .map_err(StoreError::Query)?;
    // `MAX` over no rows is NULL, and NULL is not 0: a field nobody has edited
    // has no score, and 0 would be indistinguishable from an edit of weight 0 --
    // which is a real thing, since a zero-weight vote is a real vote.
    Ok(best.unwrap_or(0.0))
}

/// Who set the value a reader is looking at.
///
/// The author of the *current* value, which is the heaviest accepted edit — not
/// the most recent editor, who may have been overridden. `None` for a field with
/// no accepted edit: reporting the last person who touched it would attribute a
/// value to somebody who did not set it.
pub async fn blame(
    store: &Store,
    subject_id: Uuid,
    field: &str,
) -> Result<Option<String>, HistoryError> {
    Ok(current_entry(store, subject_id, field)
        .await?
        .and_then(|e| e.author))
}

// ---- revert ----------------------------------------------------------------

/// Revert a field to an earlier accepted edit.
///
/// A revert *adds an edit* whose value is the earlier one, and it is marked as a
/// revert in its justification. The alternative — deleting the later edits — is
/// the counter bug's cousin: it loses the fact that they were ever proposed, and
/// makes a revert indistinguishable from nobody having made them.
///
/// Refuses an edit that is not in this field's history. Without that check a
/// caller could name any edit id in the database and write its value here, which
/// puts a value in a field that no accepted edit ever supported.
pub async fn revert(
    store: &Store,
    subject_id: Uuid,
    field: &str,
    to: Uuid,
    by: &str,
) -> Result<HistoryEntry, HistoryError> {
    let Some(target) = history_entry(store, to).await? else {
        return Err(HistoryError::NoSuchEdit(to));
    };
    if target.field != field {
        return Err(HistoryError::NotAnEditOfThisField(to));
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, proposal_id, field, value_json, author, weight, accepted_at,
                retracted_at, removed_at, justification
           FROM field_edit
          WHERE subject_id = ? AND field = ? AND retracted_at IS NULL
            AND removed_at IS NULL AND id = ?",
    )
    .bind(subject_id.to_string())
    .bind(field)
    .bind(to.to_string())
    .fetch_optional(store.pool())
    .await
    .map_err(StoreError::Query)?;
    if row.is_none() {
        return Err(HistoryError::NotAnEditOfThisField(to));
    }

    // The revert's weight is the target's, not the author's standing today. It is
    // a statement that the earlier value was right, and weighting it by who is
    // asking would make a revert by a newcomer weaker than the value it restores.
    // A revert is an acceptance of an earlier edit, not a new proposal, so it
    // carries a proposal-shaped value with no `field_proposal` row behind it --
    // nothing voted on it, and inventing a row would put a ballot result in front
    // of a proposal that never existed. The unique index on `proposal_id` is
    // satisfied by the fresh id, and the id is not the target's precisely
    // because accepting the same proposal twice is refused.
    let revert = FieldProposal {
        id: Uuid::new_v4(),
        subject_type: SubjectType::Object,
        subject_id,
        field: field.to_string(),
        value_json: target.value.to_string(),
        source: commons_core::ProposalSource::User,
        proposer_kind: commons_core::ProposerKind::User,
        proposer_id: Some(by.to_string()),
        confidence: None,
        created_at: ts::now(),
        justification: None,
    };
    let id = accept(store, &revert, Some(by), target.weight).await?;
    sqlx::query("UPDATE field_edit SET justification = ? WHERE id = ?")
        .bind("reverted to an earlier accepted edit")
        .bind(id.to_string())
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    let entry = history_entry(store, id)
        .await?
        .ok_or(HistoryError::NoSuchEdit(id))?;
    Ok(entry)
}

// ---- removal and reports (stash-box#656) -----------------------------------

/// Remove an author's own entry from history.
///
/// Only the author may do this, and the check is here rather than in the caller
/// because the caller's `by` argument is exactly the thing being checked: a
/// history any user can edit is not a history, and a history nobody can edit
/// keeps a name attached to a contribution somebody asked to be dissociated from.
///
/// What is removed is the *attribution*. The row stays, with `author` null and
/// `removed_at` set, so the next reader can tell "nobody proposed this" from
/// "this was proposed and the author asked to be unlinked". The second is the
/// one a data subject cares about, and a hard delete erases it.
pub async fn remove_history_entry(
    store: &Store,
    entry: Uuid,
    by: &str,
) -> Result<(), HistoryError> {
    let Some(existing) = history_entry(store, entry).await? else {
        return Err(HistoryError::NoSuchEdit(entry));
    };
    // An already-unlinked entry has no author left to check against, so the
    // authorisation question is already settled and re-removal is a no-op rather
    // than an error: the entry is in the state the caller asked for.
    if let Some(author) = existing.author.as_deref() {
        if author != by {
            return Err(HistoryError::NotYourEdit {
                entry,
                author: author.to_string(),
                who: by.to_string(),
            });
        }
    }
    sqlx::query("UPDATE field_edit SET author = NULL, removed_at = ? WHERE id = ?")
        .bind(ts::now())
        .bind(entry.to_string())
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// Report a history entry.
///
/// The reporter is kept, deliberately and against the grain of the previous
/// function: a report is a fact about the reporter, and an anonymous one is
/// nothing the moderation queue can act on. The asymmetry is the point — the
/// author asked for their *edit* to be unlinked, not for their report to vanish,
/// and `a_report_names_its_reporter` says so where a reader will find it.
pub async fn report_entry(
    store: &Store,
    entry: Uuid,
    reporter: &str,
    reason: &str,
) -> Result<Report, HistoryError> {
    if history_entry(store, entry).await?.is_none() {
        return Err(HistoryError::NoSuchEdit(entry));
    }
    let id = Uuid::new_v4();
    let created_at = ts::now();
    sqlx::query(
        "INSERT INTO history_report (id, entry_id, reporter, reason, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(entry.to_string())
    .bind(reporter)
    .bind(reason)
    .bind(&created_at)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(Report {
        id,
        entry,
        reporter: reporter.to_string(),
        reason: reason.to_string(),
        created_at,
    })
}

/// Every report against one entry, including one whose attribution was removed.
pub async fn reports_for(store: &Store, entry: Uuid) -> Result<Vec<Report>, HistoryError> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT id, reporter, reason, created_at FROM history_report
          WHERE entry_id = ? ORDER BY created_at, id",
    )
    .bind(entry.to_string())
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .map(|(id, reporter, reason, created_at)| Report {
            id: parse_id(&id),
            entry,
            reporter,
            reason,
            created_at,
        })
        .collect())
}

// ---- merges ----------------------------------------------------------------

/// Merge `loser` into `winner`, re-pointing every edit and recording it.
///
/// The re-pointing is the whole of the #943 fix. A merge that re-points nothing
/// would leave the loser's history behind on a retired identity, and a score
/// over the winner's rows would be short by exactly the edits the merge lost —
/// with no way to tell, because the number is still a plausible number.
///
/// No score is updated here, and that is deliberate rather than an omission:
/// there is no score to update. The winner's score is a `MAX` over its rows, and
/// the loser's rows are now the winner's rows, so the answer is correct the
/// instant the move commits. A caller that wants to *report* the change calls
/// [`recompute`], and gets the same number, because there is nothing to reconcile.
///
/// Refuses a self-merge: every merge here is of two *different* identities, and
/// merging one into itself would duplicate its own edits into its own history —
/// arithmetically harmless, historically a lie.
pub async fn merge_objects(
    store: &Store,
    loser: Uuid,
    winner: Uuid,
    actor: &str,
) -> Result<Merge, HistoryError> {
    if loser == winner {
        return Err(HistoryError::SelfMerge);
    }
    let id = Uuid::new_v4();
    let created_at = ts::now();

    // One transaction, because a half-done merge is the exact failure this
    // function exists to prevent: edits re-pointed with no record, or a record
    // with no edits moved. Either leaves a reader unable to tell what happened,
    // and one of them is unrecoverable.
    let mut tx = store.pool().begin().await.map_err(StoreError::Query)?;

    let moved = sqlx::query(
        "UPDATE field_edit SET subject_id = ? WHERE subject_id = ? AND retracted_at IS NULL",
    )
    .bind(winner.to_string())
    .bind(loser.to_string())
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?
    .rows_affected() as i64;

    // The loser's *proposals* move too, so a proposal made against the retired
    // identity is not left pointing at it. Retracted edits are left where they
    // are: they are history of the identity that made them, and a merge does not
    // rewrite history, it moves the live record.
    sqlx::query(
        "UPDATE field_proposal SET subject_id = ?
          WHERE subject_id = ? AND subject_type = 'object'",
    )
    .bind(winner.to_string())
    .bind(loser.to_string())
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;

    sqlx::query(
        "INSERT INTO object_merge (id, winner_id, loser_id, moved_edits, actor, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(winner.to_string())
    .bind(loser.to_string())
    .bind(moved)
    .bind(actor)
    .bind(&created_at)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::Query)?;

    tx.commit().await.map_err(StoreError::Query)?;

    Ok(Merge {
        id,
        winner,
        loser,
        moved_edits: moved,
        actor: Some(actor.to_string()),
        created_at,
    })
}

/// Every merge whose winner is this object, oldest first.
pub async fn merges_into(store: &Store, winner: Uuid) -> Result<Vec<Merge>, HistoryError> {
    let rows: Vec<(String, String, i64, Option<String>, String)> = sqlx::query_as(
        "SELECT id, loser_id, moved_edits, actor, created_at FROM object_merge
          WHERE winner_id = ? ORDER BY created_at, id",
    )
    .bind(winner.to_string())
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .map(|(id, loser, moved_edits, actor, created_at)| Merge {
            id: parse_id(&id),
            winner,
            loser: parse_id(&loser),
            moved_edits,
            actor,
            created_at,
        })
        .collect())
}

// ---- row mapping -----------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct Row {
    id: String,
    proposal_id: Option<String>,
    field: String,
    value_json: String,
    author: Option<String>,
    weight: f64,
    accepted_at: Option<String>,
    retracted_at: Option<String>,
    removed_at: Option<String>,
    justification: Option<String>,
}

impl Row {
    fn into_entry(self) -> HistoryEntry {
        HistoryEntry {
            id: parse_id(&self.id),
            proposal_id: self.proposal_id.as_deref().map(parse_id),
            field: self.field,
            // A row whose `value_json` will not parse is a corrupt row, and the
            // honest thing is to show the raw text rather than drop the entry:
            // silently omitting a row from a history is how a history becomes
            // wrong without anybody noticing.
            value: serde_json::from_str(&self.value_json)
                .unwrap_or(Value::String(self.value_json.clone())),
            author: self.author,
            weight: self.weight,
            accepted_at: self.accepted_at,
            retracted_at: self.retracted_at,
            removed: self.removed_at,
            justification: self.justification,
        }
    }
}

fn parse_id(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap_or_else(|_| Uuid::new_v4())
}
