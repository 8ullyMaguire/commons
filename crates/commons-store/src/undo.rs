//! Undo for destructive actions.
//!
//! §10.7 (#3221), §8.6 (revert), §10.10 (bulk editing, right-click paste). The
//! storage half of a feature whose other half is the command registry from
//! T-P5-006 item 5. Migration `0019_undo.sql`.
//!
//! # An undo is a record, not a re-derivation
//!
//! The tempting design is to not store anything: remember the action, and when
//! the user presses undo, work out what to reverse. That is wrong in three
//! ways, and each one is a bug rather than a shortcoming.
//!
//! **Which objects.** The undo must restore exactly the objects the write
//! touched. Re-running the same target later can return a different set: an
//! object may have been added, removed, or started matching for a different
//! reason. A `Target::Filter` is explicitly unbounded at request time, so
//! "the same selection" is not even a well-defined phrase.
//!
//! **What was there before.** `bulk_apply_tag` uses `ON CONFLICT DO UPDATE`, so
//! an object that already carried the tag has its `confidence` and `source`
//! *replaced* rather than gaining a second row. Its inverse is therefore
//! "put back what was there", and what was there is not recoverable from the
//! row afterwards. Every one of those columns is nullable — `0016_tag_attributes`
//! added them to a table where they did not exist — so "the row was absent" and
//! "the row was there with NULLs" are different states, and the first version
//! of this module had no way to tell them apart.
//!
//! **Whether undo is still allowed.** A write that has since been superseded
//! must not be undoable into a state nobody was ever in. That is a comparison
//! against the recorded prior state, so it needs the record.
//!
//! # The staleness check is the interesting part
//!
//! Restoring `before` when the object has since changed does not put the object
//! back; it puts it into a state that never existed, and silently discards the
//! edit that superseded the one being undone. So [`Store::undo`] verifies before
//! it writes: every object in the record must still be in the record's `after`
//! state. One that is not is reported by name, with its id, and *nothing* is
//! written — a partial undo is worse than a refused one, because the user cannot
//! tell which half happened.
//!
//! The check is a single query, not a read-then-write: between reading the
//! current state and writing the prior state, another writer can commit. The
//! whole operation is one transaction for the same reason.
//!
//! # Expiry is enforced on read
//!
//! `expires_at` is checked by [`Store::undoable`] and by [`Store::undo`], and by
//! nothing else. A sweeper is a second thing to run, to schedule, and to notice
//! has stopped running; a record past its expiry is simply not offered and not
//! accepted, whether or not anything has swept.
//!
//! What that does *not* do is delete anything. Rows are never removed — the
//! history of what was done and undone is itself history, and §8.6 is about not
//! losing it. An expired record is invisible and inert, not gone.

use serde::{Deserialize, Serialize};

use crate::db::{Store, StoreError};

/// How long an undo stays available.
///
/// A day is long enough for "I did not mean that" to be the reason and short
/// enough that a record is not a permanent liability: the alternative is a
/// record that can restore a state from a week ago over an edit made since.
///
/// The value is in the module rather than a column because it is a policy, and
/// two different actions wanting different windows is a plausible future; making
/// it a column now would mean a per-action value that nothing reads.
pub const UNDO_WINDOW_SECS: i64 = 24 * 60 * 60;

/// Why an undo was refused.
///
/// Every variant names the thing that made the undo wrong, because the user
/// needs to know whether to retry, pick a different action, or accept that the
/// edit is final. "Undo failed" tells them none of that.
#[derive(Debug, thiserror::Error)]
pub enum UndoError {
    #[error("no undo record with id {0:?}")]
    NoSuchRecord(String),

    #[error("the undo record has expired")]
    Expired,

    #[error("this undo has already been applied")]
    AlreadyUndone,

    /// The record belongs to somebody else. Its own variant rather than
    /// `NoSuchRecord`: a caller probing for the existence of another account's
    /// records should not be able to distinguish "exists but not yours" from
    /// "does not exist", and collapsing them into one error is how a caller
    /// learns that a record exists.
    #[error("this undo belongs to another caller")]
    NotYourRecord,

    /// The write has been superseded. `object_id` is named because "somebody
    /// edited one of forty objects since" is not actionable and "object 7f3a
    /// was edited since" is.
    #[error("object {object_id:?} was changed after this undo was recorded")]
    Superseded { object_id: String },

    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Two undo failures are equal when they are the same refusal.
///
/// `StoreError` is not `PartialEq` -- it holds a `PathBuf` and a `sqlx::Error` --
/// so the derive is unavailable and this is written by hand. Every store
/// failure compares equal to every other, which is deliberate: a test that
/// pinned a `sqlx` error string would break on a driver upgrade without
/// anything about undo having changed. The refusals a caller *acts* on are the
/// ones compared exactly.
impl PartialEq for UndoError {
    fn eq(&self, other: &Self) -> bool {
        use UndoError::*;
        match (self, other) {
            (Store(_), Store(_)) => true,
            (NoSuchRecord(a), NoSuchRecord(b)) => a == b,
            (Expired, Expired) | (AlreadyUndone, AlreadyUndone) => true,
            (NotYourRecord, NotYourRecord) => true,
            (Superseded { object_id: a }, Superseded { object_id: b }) => a == b,
            _ => false,
        }
    }
}

/// One object's state on one side of a write.
///
/// The three states are the ones `history.rs` defines, and reusing them is the
/// point rather than a convenience: §8.6 makes NULL-versus-unset a correctness
/// rule, and a looser second representation here would collapse them and make
/// "restore the prior state" restore the wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldState {
    /// No row: nobody has said anything about this field.
    Unset,
    /// A row with a value.
    Set,
    /// A row whose value is deliberately null.
    Null,
}

impl FieldState {
    /// Classify a row's columns.
    ///
    /// `row_existed` comes from the record, not from the columns, and this is
    /// the reason the column exists: all three of `confidence`, `source` and
    /// `created_at` are nullable, so three NULLs cannot tell "no row" from "a
    /// row of NULLs" — and the second is the state every `object_tag` row
    /// written before 0016 is in.
    pub fn of(row_existed: bool, has_value: bool) -> Self {
        match (row_existed, has_value) {
            (false, _) => FieldState::Unset,
            (true, true) => FieldState::Set,
            (true, false) => FieldState::Null,
        }
    }
}

/// The state of one object's tag row, on one side of a write.
#[derive(Debug, Clone, PartialEq)]
pub struct TagState {
    /// Whether the row existed. Not derivable from the fields below.
    pub row_existed: bool,
    pub confidence: Option<f64>,
    pub source: Option<String>,
    pub created_at: Option<String>,
}

impl TagState {
    /// The state of an object that had no such row.
    pub fn absent() -> Self {
        Self {
            row_existed: false,
            confidence: None,
            source: None,
            created_at: None,
        }
    }

    /// Does this state equal the row as it is now?
    ///
    /// Exact on all four fields, including `row_existed`. Comparing only the
    /// non-NULL ones would make "the row is gone" look the same as "the row is
    /// there with NULLs", which is the distinction the whole check turns on.
    pub fn matches(&self, other: &TagState) -> bool {
        self.row_existed == other.row_existed
            && self.confidence == other.confidence
            && self.source == other.source
            && self.created_at == other.created_at
    }
}

/// One object's inverse, as recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct UndoEntry {
    pub object_id: String,
    /// The state the write replaced.
    pub before: TagState,
    /// The state the write wrote.
    pub after: TagState,
}

/// A write to record, as the caller describes it.
///
/// Named rather than passed as seven scalars, because the seven are one
/// sentence about one event and a caller filling them in positionally is a
/// caller that can put `matched` in `requested`'s slot. `entries` is the only
/// required field in practice, and it is the only one the caller has to have
/// computed; the rest are identity and bookkeeping the store fills from the
/// caller's own action.
#[derive(Debug, Clone, PartialEq)]
pub struct Write {
    /// The record's id, and the toast's undo key.
    pub id: String,
    /// Whose write this is. Also whose undo it is.
    pub caller: String,
    /// `bulk.tag.add`, rendered in the toast.
    pub action: String,
    /// What the write was about.
    pub tag_id: String,
    /// How many objects the caller named.
    pub requested: i32,
    /// How many the write actually reached, after consent filtering.
    pub matched: i32,
    /// One per object the write *changed*.
    pub entries: Vec<UndoEntry>,
}

impl Write {
    /// A write with no counts filled in.
    ///
    /// For the common case where the caller knows the identity and the entries
    /// and not much else; the counts are the two numbers a bulk action already
    /// has, and a caller guessing them is worse than one leaving them zero.
    pub fn new(
        id: &str,
        caller: &str,
        action: &str,
        tag_id: &str,
        entries: Vec<UndoEntry>,
    ) -> Self {
        let n = entries.len() as i32;
        Self {
            id: id.to_string(),
            caller: caller.to_string(),
            action: action.to_string(),
            tag_id: tag_id.to_string(),
            requested: n,
            matched: n,
            entries,
        }
    }
}

/// A write, and how to reverse it.
#[derive(Debug, Clone, PartialEq)]
pub struct UndoRecord {
    pub id: String,
    pub caller: String,
    pub action: String,
    /// What the write was about.
    ///
    /// On the record rather than passed to [`Store::undo`], because a caller
    /// that supplies it can supply the wrong one — and an undo that restores a
    /// record's `before` values onto a *different* tag is a silent corruption
    /// that passes every check in `undo`, because the check reads `after` from
    /// the record and compares against the tag the record names.
    pub tag_id: String,
    /// How many objects the caller named.
    ///
    /// `i32`, not `i64`, and that is not a style choice: `INTEGER` is INT4 on
    /// Postgres and INT8 on SQLite, so a decode asking for `i64` succeeds on
    /// SQLite and fails on Postgres. `i32` is what both hand over. The same
    /// rule is stated on `relations.rs`'s row type, and it is the reason this
    /// module's counters are narrower than a count of rows would suggest they
    /// need to be -- a cap of `i32::MAX` objects in one bulk write is not a
    /// limit worth enforcing.
    pub requested: i32,
    /// How many the write actually reached, after consent filtering.
    pub matched: i32,
    pub created_at: String,
    pub expires_at: String,
    /// Set once consumed. `None` is the offerable state.
    pub undone_at: Option<String>,
    pub entries: Vec<UndoEntry>,
}

impl UndoRecord {
    /// Is this record still offerable to its caller?
    ///
    /// `now` is passed rather than read, so a test can state the boundary
    /// exactly. An expiry test that waits for the clock is a test that is slow
    /// when it passes and flaky when the clock is coarse.
    pub fn is_undoable(&self, now: &str) -> bool {
        self.undone_at.is_none() && self.expires_at.as_str() > now
    }

    /// The count a toast should say.
    ///
    /// `entries.len()` and not `matched`, because they are different numbers
    /// and the difference is the bug: `matched` counts the rows the predicate
    /// reached, and a row that already carried the tag was reached but not
    /// changed. Saying "added beach to 40" when 12 of them already had it is
    /// wrong in the direction that makes the user doubt the app.
    pub fn changed_count(&self) -> usize {
        self.entries.len()
    }
}

// ---------------------------------------------------------------- the store

/// Bind a statement's `?` markers as the engine needs, run it, discard the
/// result.
///
/// A macro rather than a function for the same reason `bulk.rs`'s
/// `count_visible!` is: the query builder's type moves with the engine, so a
/// function signature would have to name an associated type behind a `Database`
/// bound that shifts between sqlx minor versions.
///
/// Each arm builds its *own* query. Sharing one builder across both arms does
/// not work: the builder's type is fixed by its first bind, so a builder that
/// can be executed against a SQLite pool cannot also be executed against a
/// Postgres one. That is the whole reason the bind list is repeated below, and
/// it is why this file has three such macros rather than one helper.
macro_rules! undo_exec {
    ($store:expr, $sql:expr, $($v:expr),* $(,)?) => {{
        let sql: &str = $sql;
        match $store {
            Store::Sqlite(p) => {
                let mut qb = sqlx::query(sql);
                $( qb = qb.bind($v); )*
                qb.execute(p).await.map_err(StoreError::Query)?;
            }
            Store::Postgres(p) => {
                // Bound to a local, not passed to `query` inline: the rewritten
                // string is a temporary, and the builder borrows it.
                let bound = Store::bind_sql(sql);
                let mut qb = sqlx::query(&bound);
                $( qb = qb.bind($v); )*
                qb.execute(p).await.map_err(StoreError::Query)?;
            }
        }
    }};
}

/// `expires_at` for a record created at `created`.
///
/// Written out rather than computed by a database function because the two
/// engines spell "now plus one day" differently and the record's expiry is
/// part of its identity: a record written on SQLite and read on Postgres has to
/// expire at the same instant, or the parity the schema otherwise buys is lost
/// at the last step.
fn expires_at(created: &str) -> Result<String, UndoError> {
    chrono::DateTime::parse_from_rfc3339(created)
        .map(|t| {
            (t + chrono::Duration::seconds(UNDO_WINDOW_SECS))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .map_err(|e| StoreError::Invalid {
            what: "undo record",
            why: format!("created_at {created:?} is not RFC 3339: {e}"),
        })
        .map_err(UndoError::Store)
}

/// The current state of one object's tag row.
///
/// Public because a caller building an [`UndoEntry`] needs the current state to
/// record as its `before`, and the alternative is SQL of its own -- which is how
/// the absent-versus-NULL distinction gets re-implemented wrong a second time.
///
/// `TagState::absent()` rather than an `Option` for the row, because the three
/// states are what the inverse needs and the caller should not have to remember
/// that `None` and "a row of NULLs" are different.
pub async fn tag_state(
    store: &Store,
    object_id: &str,
    tag_id: &str,
) -> Result<TagState, UndoError> {
    use sqlx::Row;
    let sql = "SELECT confidence, source, created_at FROM object_tag \
               WHERE object_id = ? AND tag_id = ?";
    // Each arm maps to `TagState` before it leaves, because
    // `Option<SqliteRow>` and `Option<PgRow>` are unrelated types -- the same
    // reason the write macros build per engine.
    macro_rules! read_state {
        ($p:expr, $q:expr) => {
            sqlx::query($q)
                .bind(object_id.to_string())
                .bind(tag_id.to_string())
                .fetch_optional($p)
                .await
                .map_err(StoreError::Query)?
                .map(|r| {
                    Ok(TagState {
                        row_existed: true,
                        // `Option`, because all three columns are nullable: 0016
                        // added them to a table where they did not exist, so every
                        // row written before it has NULLs in all three. That is a
                        // row, not an absent one, and the distinction is the whole
                        // reason `row_existed` is stored separately.
                        confidence: r.try_get::<Option<f64>, _>(0).map_err(StoreError::Query)?,
                        source: r
                            .try_get::<Option<String>, _>(1)
                            .map_err(StoreError::Query)?,
                        created_at: r
                            .try_get::<Option<String>, _>(2)
                            .map_err(StoreError::Query)?,
                    })
                })
                .transpose()
                .map_err(UndoError::Store)?
        };
    }
    let state = match store {
        Store::Sqlite(p) => read_state!(p, sql),
        Store::Postgres(p) => {
            let bound = Store::bind_sql(sql);
            read_state!(p, &bound)
        }
    };
    Ok(state.unwrap_or_else(TagState::absent))
}

impl Store {
    /// Record a write and the state it replaced, so it can be undone.
    ///
    /// [`Write::entries`] is the inverse, one per object the write *changed* — not per
    /// object the write reached. An object that already carried the tag is not
    /// in the list, because "add beach" on an object that has beach is a no-op
    /// and the inverse of a no-op is nothing.
    ///
    /// # Not in a transaction with the write
    ///
    /// `bulk_apply_tag` is a single `INSERT ... SELECT`, so making the two
    /// atomic needs that statement to run on a connection this method also
    /// holds. It is not done here, and the failure it leaves is a write with no
    /// undo — the user is told the write happened, which is the safe direction
    /// of the failure, but it is a gap. Recorded in
    /// `docs/plans/implementation-plan.md` rather than left to be discovered.
    pub async fn record_undo(&self, w: &Write) -> Result<(), UndoError> {
        let (id, caller, action, tag_id) = (&w.id, &w.caller, &w.action, &w.tag_id);
        let (requested, matched) = (w.requested, w.matched);
        let entries = w.entries.as_slice();
        let created = commons_core::ts::now();
        let expires = expires_at(&created)?;

        undo_exec!(
            self,
            "INSERT INTO undo_record
                 (id, caller, action, tag_id, requested, matched, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            id.to_string(),
            caller.to_string(),
            action.to_string(),
            tag_id.to_string(),
            requested,
            matched,
            created.clone(),
            expires,
        );

        for e in entries {
            undo_exec!(
                self,
                "INSERT INTO undo_entry (record_id, object_id, row_existed,
                     before_confidence, before_source, before_created_at,
                     after_confidence, after_source, after_created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                id.to_string(),
                e.object_id.clone(),
                e.before.row_existed as i64,
                e.before.confidence,
                e.before.source.clone(),
                e.before.created_at.clone(),
                e.after.confidence,
                e.after.source.clone(),
                e.after.created_at.clone(),
            );
        }
        Ok(())
    }

    /// Read one record and its entries.
    ///
    /// The entries are the reason this exists as a read: the staleness check in
    /// [`Store::undo`] needs every `before` and every `after`, and fetching them
    /// per-object would be one query per row of a forty-object write.
    pub async fn undo_record(&self, id: &str) -> Result<Option<UndoRecord>, UndoError> {
        use sqlx::Row;
        let sql = "SELECT id, caller, action, tag_id, requested, matched, \
                   created_at, expires_at, undone_at \
                   FROM undo_record WHERE id = ?";
        // Mapped to owned values inside the arm: `Option<SqliteRow>` and
        // `Option<PgRow>` are unrelated types, and a row type the caller has to
        // name is a row type that changes when the schema does.
        macro_rules! go {
            ($p:expr, $q:expr) => {
                sqlx::query($q)
                    .bind(id.to_string())
                    .fetch_optional($p)
                    .await
                    .map_err(StoreError::Query)?
                    .map(|r| {
                        Ok((
                            r.try_get::<String, _>(0).map_err(StoreError::Query)?,
                            r.try_get::<String, _>(1).map_err(StoreError::Query)?,
                            r.try_get::<String, _>(2).map_err(StoreError::Query)?,
                            r.try_get::<String, _>(3).map_err(StoreError::Query)?,
                            r.try_get::<i32, _>(4).map_err(StoreError::Query)?,
                            r.try_get::<i32, _>(5).map_err(StoreError::Query)?,
                            r.try_get::<String, _>(6).map_err(StoreError::Query)?,
                            r.try_get::<String, _>(7).map_err(StoreError::Query)?,
                            r.try_get::<Option<String>, _>(8)
                                .map_err(StoreError::Query)?,
                        ))
                    })
                    .transpose()
                    .map_err(UndoError::Store)?
            };
        }
        let row = match self {
            Store::Sqlite(p) => go!(p, sql),
            Store::Postgres(p) => {
                let bound = Store::bind_sql(sql);
                go!(p, &bound)
            }
        };
        let Some(r) = row else {
            return Ok(None);
        };

        let entry_sql = "SELECT object_id, row_existed, \
                            before_confidence, before_source, before_created_at, \
                            after_confidence, after_source, after_created_at \
                         FROM undo_entry WHERE record_id = ? ORDER BY object_id";
        macro_rules! entries {
            ($p:expr, $q:expr) => {
                sqlx::query($q)
                    .bind(id.to_string())
                    .fetch_all($p)
                    .await
                    .map_err(StoreError::Query)?
                    .into_iter()
                    .map(|e| {
                        Ok(UndoEntry {
                            object_id: e.try_get::<String, _>(0).map_err(StoreError::Query)?,
                            before: TagState {
                                row_existed: e.try_get::<i32, _>(1).map_err(StoreError::Query)?
                                    != 0,
                                confidence: e
                                    .try_get::<Option<f64>, _>(2)
                                    .map_err(StoreError::Query)?,
                                source: e
                                    .try_get::<Option<String>, _>(3)
                                    .map_err(StoreError::Query)?,
                                created_at: e
                                    .try_get::<Option<String>, _>(4)
                                    .map_err(StoreError::Query)?,
                            },
                            // `after.row_existed` is unconditionally true: an
                            // entry is only written for an object the write
                            // *changed*, and a write that changed the row had a
                            // row afterwards. That is the one place the
                            // reconstruction is not literal, and it cannot be
                            // otherwise -- a `false` after would mean the write
                            // removed the row, which is an action that does not
                            // exist yet and would need its own entry kind.
                            after: TagState {
                                row_existed: true,
                                confidence: e
                                    .try_get::<Option<f64>, _>(5)
                                    .map_err(StoreError::Query)?,
                                source: e
                                    .try_get::<Option<String>, _>(6)
                                    .map_err(StoreError::Query)?,
                                created_at: e
                                    .try_get::<Option<String>, _>(7)
                                    .map_err(StoreError::Query)?,
                            },
                        })
                    })
                    .collect::<Result<Vec<_>, StoreError>>()
                    .map_err(UndoError::Store)?
            };
        }
        let entry_rows = match self {
            Store::Sqlite(p) => entries!(p, entry_sql),
            Store::Postgres(p) => {
                let bound = Store::bind_sql(entry_sql);
                entries!(p, &bound)
            }
        };

        Ok(Some(UndoRecord {
            id: r.0,
            caller: r.1,
            action: r.2,
            tag_id: r.3,
            requested: r.4,
            matched: r.5,
            created_at: r.6,
            expires_at: r.7,
            undone_at: r.8,
            entries: entry_rows,
        }))
    }

    /// The caller's undoable records, newest first.
    ///
    /// Expired and already-undone records are filtered out *here* rather than
    /// at the call site, so there is one definition of "offerable" rather than
    /// one per surface that renders a toast.
    pub async fn undoable(&self, caller: &str) -> Result<Vec<UndoRecord>, UndoError> {
        use sqlx::Row;
        let now = commons_core::ts::now();
        let sql = "SELECT id FROM undo_record \
                   WHERE caller = ? AND undone_at IS NULL AND expires_at > ? \
                   ORDER BY created_at DESC, id DESC";
        macro_rules! go {
            ($p:expr, $q:expr) => {
                sqlx::query($q)
                    .bind(caller.to_string())
                    .bind(now.clone())
                    .fetch_all($p)
                    .await
                    .map_err(StoreError::Query)?
                    .into_iter()
                    .map(|r| r.try_get::<String, _>(0).map_err(StoreError::Query))
                    .collect::<Result<Vec<_>, StoreError>>()
                    .map_err(UndoError::Store)?
            };
        }
        let ids = match self {
            Store::Sqlite(p) => go!(p, sql),
            Store::Postgres(p) => {
                let bound = Store::bind_sql(sql);
                go!(p, &bound)
            }
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(rec) = self.undo_record(&id).await? {
                out.push(rec);
            }
        }
        Ok(out)
    }

    /// Reverse a recorded write.
    ///
    /// Verifies first and writes second, and the verification is the whole
    /// reason this is not a `DELETE`. Restoring `before` onto an object that has
    /// since changed does not put that object back — it moves it into a state
    /// nobody was ever in and discards the edit that superseded the one being
    /// undone. So every object in the record must still be in the record's
    /// `after` state, and one that is not aborts the whole undo.
    ///
    /// All-or-nothing is deliberate. A partial undo leaves the user unable to
    /// tell which half happened, and the next undo would then be trying to
    /// reverse a write that was only half applied.
    pub async fn undo(&self, id: &str, caller: &str) -> Result<usize, UndoError> {
        let record = self
            .undo_record(id)
            .await?
            .ok_or_else(|| UndoError::NoSuchRecord(id.to_string()))?;

        if record.caller != caller {
            return Err(UndoError::NotYourRecord);
        }
        if record.undone_at.is_some() {
            return Err(UndoError::AlreadyUndone);
        }
        let now = commons_core::ts::now();
        if !record.is_undoable(&now) {
            return Err(UndoError::Expired);
        }

        // The check, for every object, before any write. `tag_id` is the
        // record's own, which is what makes the check meaningful: `after` was
        // recorded against this tag, so it has to be compared against this tag.
        let tag_id = &record.tag_id;
        for e in &record.entries {
            let now_state = tag_state(self, &e.object_id, tag_id).await?;
            if !e.after.matches(&now_state) {
                return Err(UndoError::Superseded {
                    object_id: e.object_id.clone(),
                });
            }
        }

        for e in &record.entries {
            if e.before.row_existed {
                undo_exec!(
                    self,
                    "UPDATE object_tag
                     SET confidence = ?, source = ?, created_at = ?
                     WHERE object_id = ? AND tag_id = ?",
                    e.before.confidence,
                    e.before.source.clone(),
                    e.before.created_at.clone(),
                    e.object_id.clone(),
                    tag_id.to_string(),
                );
            } else {
                undo_exec!(
                    self,
                    "DELETE FROM object_tag WHERE object_id = ? AND tag_id = ?",
                    e.object_id.clone(),
                    tag_id.to_string(),
                );
            }
        }

        undo_exec!(
            self,
            "UPDATE undo_record SET undone_at = ? WHERE id = ? AND undone_at IS NULL",
            now,
            id.to_string(),
        );

        Ok(record.entries.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(confidence: f64, source: &str) -> TagState {
        TagState {
            row_existed: true,
            confidence: Some(confidence),
            source: Some(source.into()),
            created_at: None,
        }
    }

    /// A row of NULLs and no row at all are the states most easily confused,
    /// and the one the module exists to keep apart.
    #[test]
    fn an_absent_row_does_not_match_a_row_of_nulls() {
        let nulls = TagState {
            row_existed: true,
            confidence: None,
            source: None,
            created_at: None,
        };
        let absent = TagState::absent();
        assert!(!nulls.matches(&absent));
        assert!(!absent.matches(&nulls));
        assert!(absent.matches(&TagState::absent()), "absent matches itself");
    }

    /// The comparison has to be exact on every field, not just the ones a
    /// caller happens to set. Dropping `source` from the comparison would let an
    /// undo silently discard a manual edit to a field the bulk write never
    /// touched.
    #[test]
    fn every_field_is_compared() {
        let base = set(0.9, "bulk");
        for other in [
            set(0.8, "bulk"),   // confidence
            set(0.9, "manual"), // source
            TagState {
                created_at: Some("2026-01-01T00:00:00.000Z".into()),
                ..base.clone()
            }, // created_at
        ] {
            assert!(!base.matches(&other), "a differing field must not match");
        }
    }

    /// `FieldState` is the three-state view. `of` is the classifier, and the
    /// `row_existed` argument is what distinguishes the two None-ish cases.
    #[test]
    fn field_state_classifies_three_ways() {
        assert_eq!(FieldState::of(false, false), FieldState::Unset);
        assert_eq!(FieldState::of(false, true), FieldState::Unset);
        assert_eq!(FieldState::of(true, true), FieldState::Set);
        assert_eq!(FieldState::of(true, false), FieldState::Null);
    }

    fn record(expires: &str) -> UndoRecord {
        UndoRecord {
            id: "r".into(),
            caller: "me".into(),
            action: "bulk.tag.add".into(),
            tag_id: "t".into(),
            requested: 2,
            matched: 2,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            expires_at: expires.into(),
            undone_at: None,
            entries: vec![],
        }
    }

    /// The expiry comparison is a string compare, which is only order-preserving
    /// because every timestamp is the same shape and UTC. So the boundary is
    /// tested on both sides of it exactly.
    #[test]
    fn the_expiry_boundary_is_strict() {
        let r = record("2026-01-02T00:00:00.000Z");
        assert!(r.is_undoable("2026-01-01T23:59:59.999Z"), "one ms inside");
        assert!(
            !r.is_undoable("2026-01-02T00:00:00.000Z"),
            "at the boundary: expired, because the test is `expires_at > now`"
        );
        assert!(!r.is_undoable("2026-01-02T00:00:00.001Z"), "one ms outside");
    }

    /// A consumed record is not offerable whatever its expiry says, and the
    /// window does not reopen it.
    #[test]
    fn a_consumed_record_is_never_undoable() {
        let mut r = record("2999-01-01T00:00:00.000Z");
        r.undone_at = Some("2026-01-01T12:00:00.000Z".into());
        assert!(!r.is_undoable("2026-01-01T00:00:00.000Z"));
    }

    /// The count a toast shows is the number of *changes*, not the number of
    /// objects the predicate reached. A write that reached forty objects and
    /// changed twelve must say twelve.
    #[test]
    fn the_offered_count_is_the_changed_count() {
        let mut r = record("2999-01-01T00:00:00.000Z");
        r.entries = vec![created("a"), created("b"), created("c")];
        assert_eq!(r.changed_count(), 3);
        assert_eq!(r.requested, 2, "the fixture's requested count is unrelated");
    }

    fn created(object_id: &str) -> UndoEntry {
        UndoEntry {
            object_id: object_id.into(),
            before: TagState::absent(),
            after: set(0.9, "bulk"),
        }
    }

    /// `expires_at` writes the offset itself, which is what makes the string
    /// compare above order-preserving. A record created at an instant near
    /// midnight UTC must still land exactly one window later.
    #[test]
    fn expiry_is_exactly_one_window_after_creation() {
        let created = "2026-03-29T01:59:59.500Z";
        let e = expires_at(created).expect("a valid timestamp");
        let before = chrono::DateTime::parse_from_rfc3339(created).expect("ts");
        let after = chrono::DateTime::parse_from_rfc3339(&e).expect("ts");
        assert_eq!(
            (after - before).num_seconds(),
            UNDO_WINDOW_SECS,
            "a day later, to the millisecond"
        );
    }

    /// A malformed timestamp is an error, not a record that silently expires at
    /// the epoch -- which would make the record permanently un-undoable with no
    /// indication why.
    #[test]
    fn a_malformed_timestamp_is_refused() {
        assert!(matches!(
            expires_at("not a timestamp"),
            Err(UndoError::Store(_))
        ));
    }
}
