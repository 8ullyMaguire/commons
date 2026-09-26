//! §10.7, §14.1 — consent-checked bulk writes.
//!
//! # Why this module exists
//!
//! [`Store::query`](crate::db::Store::query) is documented as the only way to
//! *read* objects, and it takes a [`CallerId`] by reference with no default and
//! no other constructor. The stated mechanism is the whole argument: a filter
//! that can be omitted is a filter that will be omitted, and the call site that
//! omits it is the one nobody reviews.
//!
//! [`Store::apply_tag`](crate::db::Store::apply_tag) has never had that
//! guarantee, which was survivable when every write named exactly one object.
//! It stops being survivable the moment a caller names a set: a bulk write with
//! no consent check does not leak one object, it rewrites four thousand. So the
//! clause goes in the data layer, where §14.1 says it belongs, rather than in
//! the modal that happens to call it.
//!
//! # Two shapes, because the selection has two shapes
//!
//! A selection is either a set of ids or a query — see `ui/src/lib/api/
//! selection.ts`, where `query` is a *mode* rather than an operation. Both
//! shapes have to work, and the id shape is the one that will be forgotten: it
//! "obviously" needs no filter, because the caller already said which objects.
//! It does.
//!
//! # What the return value is for
//!
//! [`BulkOutcome::applied`] is the count the *server* computed. Not the length
//! of the list the client sent, and not the client's own estimate: the three
//! differ the moment a row changed between the selection and the write, and
//! this is the number a user will trust about what just happened. A modal that
//! reports "12 tagged" when 12 were sent and 11 matched is a lie with a
//! progress bar.
//!
//! [`BulkOutcome::skipped_invisible`] is not an error. An object with no consent
//! record is `unverified` and invisible to the public tiers, so a caller
//! selecting 10 of which 3 are unverified has done nothing wrong — the 3 are
//! simply not theirs to see. Reporting that as a failure tells the user
//! something is wrong with their library when their library is fine.

use crate::db::{Store, StoreError};
use crate::filter_ast::{CallerId, Filter, FilterError, Value};

/// What a bulk write actually did, as the server counted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BulkOutcome {
    /// Rows the write reached. The server's number, never the request's.
    pub applied: usize,
    /// Rows the caller named that the consent filter hid.
    ///
    /// A separate counter rather than an error, per the module docs: an
    /// invisible object is not a broken one.
    pub skipped_invisible: usize,
    /// How many the caller asked for, before filtering.
    ///
    /// Kept so a caller can tell "nothing matched" from "everything you named is
    /// hidden from you" — the second is a permissions shape and the first is an
    /// empty result, and they need different messages.
    ///
    /// 0 for a filter target: the caller never named a set, so there is no
    /// request size to report, and a `COUNT(*)` here would be the *match* count
    /// wearing a request's name.
    pub requested: usize,
}

impl BulkOutcome {
    /// Did the write reach anything at all?
    pub fn wrote_anything(&self) -> bool {
        self.applied > 0
    }

    /// Were rows hidden from this caller?
    ///
    /// Distinct from "the request was empty": a caller with a stale selection
    /// gets `requested == 0` and `skipped_invisible == 0`, and a caller who
    /// cannot see the rows gets `skipped_invisible > 0`. Same blank screen, very
    /// different situations.
    pub fn hid_something(&self) -> bool {
        self.skipped_invisible > 0
    }
}

/// A batch of ids, or a query that names them.
///
/// Not a struct with an `Option` because the two shapes have different risk and
/// the caller should have to say which it is: an id list is bounded by what the
/// client sent, a filter is bounded by the library. Making the caller name the
/// shape is what stops a `Vec` of one from quietly becoming "everything
/// matching".
#[derive(Debug, Clone)]
pub enum Target<'a> {
    /// Exactly these objects. Still consent-filtered — see the module docs.
    Ids(&'a [String]),
    /// Everything matching this filter, consent-filtered like everything else.
    Filter(&'a Filter),
}

/// Errors from a bulk write.
///
/// [`BulkError::ConsentCheck`] is separated from [`BulkError::Store`] so a
/// caller can treat "the filter would not compile" as a client error and "the
/// database refused" as a server error. Merging them would make a malformed
/// filter look like a disk failure.
#[derive(Debug)]
pub enum BulkError {
    /// The consent clause or the user's filter would not compile.
    ConsentCheck(FilterError),
    /// The tag does not exist.
    NoSuchTag(String),
    /// The store refused.
    Store(StoreError),
}

impl std::fmt::Display for BulkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BulkError::ConsentCheck(e) => write!(f, "bulk target would not compile: {e}"),
            BulkError::NoSuchTag(id) => write!(f, "no such tag: {id}"),
            BulkError::Store(e) => write!(f, "bulk write failed: {e}"),
        }
    }
}

impl std::error::Error for BulkError {}

impl From<StoreError> for BulkError {
    fn from(e: StoreError) -> Self {
        BulkError::Store(e)
    }
}

/// The `FROM … WHERE` body shared by the count and the insert.
///
/// One helper for both target shapes *and* both statements, and that is the
/// point twice over:
///
///  - a consent clause written twice is a clause that will be dropped from one
///    of the copies, and the copy that gets dropped is the one nobody reads;
///  - if the count and the insert each built their own `WHERE`, they could
///    drift onto different row sets, and the count is the number the user is
///    shown.
///
/// The id path is the one that must not be special-cased away: an empty list
/// compiles to a *false* predicate rather than to no predicate at all, so a user
/// who selected nothing and clicked apply tags nothing instead of everything.
///
/// `params` is appended to, in placeholder order, so the caller binds them in
/// exactly this order. Pushing the consent parameters first and the user's
/// after is what keeps the two from interleaving — the ordering constraint
/// `Store::query` documents, for the same reason.
fn bulk_from_where(
    store: &Store,
    target: &Target<'_>,
    caller: &CallerId,
    params: &mut Vec<Value>,
) -> Result<String, BulkError> {
    let consent = Filter::consent_clause(caller, params).map_err(BulkError::ConsentCheck)?;
    let user = match target {
        Target::Ids(ids) => {
            if ids.is_empty() {
                "1 = 0".to_string()
            } else {
                let holes = crate::filter_ast::placeholders(ids.len());
                for id in ids.iter() {
                    params.push(Value::Str(id.clone()));
                }
                format!("o.id IN ({holes})")
            }
        }
        Target::Filter(filter) => {
            let compiled = filter
                .to_sql(store.engine(), caller)
                .map_err(BulkError::ConsentCheck)?;
            params.extend(compiled.params);
            compiled.sql
        }
    };
    Ok(format!(
        "FROM object o INNER JOIN consent_record c ON c.object_id = o.id \
         WHERE {consent} AND ({user})"
    ))
}

/// Run a `SELECT COUNT(*)` on the bulk predicate, on either engine.
///
/// A macro rather than a function for the same reason `query.rs`'s `bind_value!`
/// is: the query builder's type moves with the engine, so naming it in a
/// signature means naming an associated type behind a `Database` bound that
/// shifts between sqlx minor versions. Duplicated from `query.rs` rather than
/// exported because a macro that is `#[macro_export]`ed becomes public API of the
/// crate, and a binding helper is not something a downstream caller should be
/// able to reach.
macro_rules! count_visible {
    ($store:expr, $sql:expr, $params:expr) => {{
        let params = $params;
        match $store {
            Store::Sqlite(p) => {
                let mut qb = sqlx::query_scalar::<_, i64>($sql);
                for v in params {
                    qb = match v {
                        Value::Str(s) => qb.bind(s),
                        Value::Int(i) => qb.bind(i),
                        Value::Float(f) => qb.bind(f),
                        Value::Bool(b) => qb.bind(b),
                        Value::Null => qb.bind(Option::<String>::None),
                        // A list reaching a single bind means `to_sql` emitted
                        // one placeholder for a list, which no engine accepts and
                        // which would match nothing -- looking like an empty
                        // library rather than a compiler bug.
                        Value::List(items) => panic!(
                            "filter_ast emitted a list as a single bind in a bulk \
                             target: {items:?}. This is a compiler bug in `to_sql`."
                        ),
                    };
                }
                qb.fetch_one(p).await.map_err(StoreError::Query)?
            }
            Store::Postgres(p) => {
                let bound = Store::bind_sql($sql);
                let mut qb = sqlx::query_scalar::<_, i64>(&bound);
                for v in params {
                    qb = match v {
                        Value::Str(s) => qb.bind(s),
                        Value::Int(i) => qb.bind(i),
                        Value::Float(f) => qb.bind(f),
                        Value::Bool(b) => qb.bind(b),
                        Value::Null => qb.bind(Option::<String>::None),
                        Value::List(items) => panic!(
                            "filter_ast emitted a list as a single bind in a bulk \
                             target: {items:?}. This is a compiler bug in `to_sql`."
                        ),
                    };
                }
                qb.fetch_one(p).await.map_err(StoreError::Query)?
            }
        }
    }};
}

impl Store {
    /// How many objects in `target` this caller can see.
    ///
    /// The number [`Store::bulk_apply_tag`] reports as `applied`, exposed
    /// separately because a selection UI needs it *before* the write: "you have
    /// selected 10, 3 of them are not visible to you" is a message about a
    /// selection, and a caller that can only learn it by writing has already
    /// done the thing it was going to warn about.
    pub async fn bulk_count_visible(
        &self,
        target: &Target<'_>,
        caller: &CallerId,
    ) -> Result<usize, BulkError> {
        let mut params: Vec<Value> = Vec::new();
        let from_where = bulk_from_where(self, target, caller, &mut params)?;
        let sql = format!("SELECT COUNT(*) {from_where}");
        let count = count_visible!(self, &sql, params);
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Apply a tag to every object in `target` that this caller can see.
    ///
    /// One `INSERT … SELECT` for the whole batch rather than a loop. 4,000
    /// single-row statements is a bulk edit that takes minutes, and a loop that
    /// fails on row 2,999 leaves a state the user cannot describe — a single
    /// statement is atomic without a transaction anyone has to remember to roll
    /// back.
    ///
    /// `confidence` and `source` are the same two parameters
    /// [`Store::apply_tag`](crate::db::Store::apply_tag) takes, deliberately. A
    /// bulk tag that cannot record where it came from makes the tagger's
    /// provenance unreadable for exactly the rows it touched, and a human tag
    /// and a model tag become indistinguishable after a bulk edit.
    ///
    /// The tag is checked before the write rather than discovered by it. The
    /// foreign key would catch it too, but only after the statement ran, and
    /// "refused before touching anything" is a better error than a constraint
    /// violation on a batch.
    pub async fn bulk_apply_tag(
        &self,
        target: &Target<'_>,
        tag_id: &str,
        confidence: Option<f64>,
        source: Option<&str>,
        caller: &CallerId,
    ) -> Result<BulkOutcome, BulkError> {
        if !self.tag_exists(tag_id).await? {
            return Err(BulkError::NoSuchTag(tag_id.to_string()));
        }

        let requested = match target {
            Target::Ids(ids) => ids.len(),
            Target::Filter(_) => 0,
        };

        // Counted before the write, and the number that becomes `applied`.
        //
        // It could be read back from the insert's `rows_affected`, which would be
        // one statement instead of two. That count is rows *changed*, which is 0
        // for an object that already carried the tag — so a bulk edit that did
        // exactly what was asked would report "0 changed", and a user who just
        // tagged 40 things would see a number that looks like a failure.
        let applied = self.bulk_count_visible(target, caller).await?;
        let skipped_invisible = requested.saturating_sub(applied);

        if applied == 0 {
            return Ok(BulkOutcome {
                applied: 0,
                skipped_invisible,
                requested,
            });
        }

        let mut params: Vec<Value> = Vec::new();
        let from_where = bulk_from_where(self, target, caller, &mut params)?;
        let sql = format!(
            "INSERT INTO object_tag (object_id, tag_id, confidence, source, created_at) \
             SELECT o.id, ?, ?, ?, ? {from_where} \
             ON CONFLICT (object_id, tag_id) DO UPDATE \
                SET confidence = excluded.confidence, source = excluded.source"
        );

        let now = commons_core::ts::now();
        match self {
            Store::Sqlite(p) => {
                let mut qb = sqlx::query(&sql)
                    .bind(tag_id.to_string())
                    .bind(confidence)
                    .bind(source.map(str::to_string))
                    .bind(now);
                for v in &params {
                    qb = match v {
                        Value::Str(s) => qb.bind(s.clone()),
                        Value::Int(i) => qb.bind(*i),
                        Value::Float(f) => qb.bind(*f),
                        Value::Bool(b) => qb.bind(*b),
                        Value::Null => qb.bind(Option::<String>::None),
                        Value::List(items) => panic!(
                            "filter_ast emitted a list as a single bind in a bulk \
                             target: {items:?}. This is a compiler bug in `to_sql`."
                        ),
                    };
                }
                qb.execute(p).await.map_err(StoreError::Query)?;
            }
            Store::Postgres(p) => {
                let bound = Store::bind_sql(&sql);
                let mut qb = sqlx::query(&bound)
                    .bind(tag_id.to_string())
                    .bind(confidence)
                    .bind(source.map(str::to_string))
                    .bind(now);
                for v in &params {
                    qb = match v {
                        Value::Str(s) => qb.bind(s.clone()),
                        Value::Int(i) => qb.bind(*i),
                        Value::Float(f) => qb.bind(*f),
                        Value::Bool(b) => qb.bind(*b),
                        Value::Null => qb.bind(Option::<String>::None),
                        Value::List(items) => panic!(
                            "filter_ast emitted a list as a single bind in a bulk \
                             target: {items:?}. This is a compiler bug in `to_sql`."
                        ),
                    };
                }
                qb.execute(p).await.map_err(StoreError::Query)?;
            }
        }

        Ok(BulkOutcome {
            applied,
            skipped_invisible,
            requested,
        })
    }
}
