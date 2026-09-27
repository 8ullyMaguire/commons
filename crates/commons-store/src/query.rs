//! §14.1 — the only sanctioned way to read objects.
//!
//! # Why this module exists
//!
//! §14.1: "Rules, all enforced in the data layer, not the UI."
//!
//! [`Filter::consent_clause`] has existed since T-P0-005 and is correct in
//! intent, but for its whole life it was a `pub fn` called by nothing outside
//! its own test module — and, as it turned out, could not have been called by
//! anything, because it emitted `o.consent_tier` and `object` has no such
//! column. The tier is on `consent_record.tier`. So the guarantee the ticket
//! asked for was not missing; it was unreachable, and a clause that produces an
//! `SQLITE_ERROR` fails closed in a way that is easy to mistake for "the filter
//! is strict".
//!
//! The three obligations of this module:
//!
//! 1. **There is one way in.** [`Store::query`] takes a [`CallerId`] and has no
//!    overload without one, so no call site can forget the filter. Object reads
//!    go through `query`; anything else is caught by a test in
//!    `crates/commons-consent/tests/policy.rs` that scans the workspace for
//!    `SELECT ... FROM object` outside this file.
//! 2. **The clause names a real column.** The join to `consent_record` is an
//!    inner join, which is itself a decision: an object with *no* consent record
//!    is `unverified` (the column's default) and so invisible to the public
//!    tiers, and an inner join is how that is expressed without a second table
//!    read. §14.1: unverified is "private by default" — the default has to be
//!    private, and it is only private if its absence is invisible.
//! 3. **A missing consent record cannot be read as consent.** `INNER JOIN` is
//!    the whole of it. A `LEFT JOIN` with a `COALESCE` would give a missing
//!    record the tier `unverified`, which is the same answer — but it would do
//!    so by *defaulting*, and a default is a value somebody can change in one
//!    place and have every object with no record follow. The inner join has no
//!    default to change.

use crate::db::{Store, StoreError};

/// Bind one AST value onto a query builder, returning the builder.
///
/// A macro rather than a function because `QueryAs` carries the row type as a
/// generic parameter and naming it in a signature means naming sqlx's argument
/// type too, which is an associated type behind a `Database` bound that moves
/// between sqlx minor versions. Expanding at the call site keeps the types
/// whatever the builder already has.
///
/// A list reaching here is a bug in the filter compiler, not a caller error:
/// `to_sql` expands a list into one placeholder per element, so a surviving
/// list means a placeholder and its value disagree -- which would bind the
/// *text* of a list to a single `?` and match nothing, looking like an empty
/// library rather than a bug.
macro_rules! bind_value {
    ($qb:expr, $v:expr) => {
        match $v {
            crate::filter_ast::Value::Str(s) => $qb.bind(s.clone()),
            crate::filter_ast::Value::Int(i) => $qb.bind(*i),
            crate::filter_ast::Value::Float(f) => $qb.bind(*f),
            crate::filter_ast::Value::Bool(b) => $qb.bind(*b),
            crate::filter_ast::Value::Null => $qb.bind(Option::<String>::None),
            crate::filter_ast::Value::List(items) => {
                panic!(
                    "filter_ast emitted a list as a single bind, which no \
                     engine accepts: {items:?}. This is a compiler bug in \
                     `to_sql`, not a caller error."
                )
            }
        }
    };
}
use crate::filter_ast::{CallerId, Filter};
use crate::sort::Sort;
use std::collections::HashMap;
use std::fmt;

/// A row as it came out of the consent-filtered query.
///
/// Deliberately not `#[derive(Serialize)]`-and-`pub` on every column: a struct
/// that mirrors the table is a struct that grows a `title` field the day
/// somebody needs one, and the growth is invisible at every call site. This one
/// is the shape the API is allowed to return, and adding to it is a decision
/// somebody has to make on purpose.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectRow {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub tier: String,
    pub redistribution_permitted: bool,
}

impl fmt::Display for ObjectRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}, {})", self.id, self.kind, self.tier)
    }
}

/// A page of results, and whether more exist.
///
/// `has_more` is computed with a window one larger than the limit rather than by
/// counting, because a second `COUNT(*)` on a filtered query is a second full
/// scan of the same join, and the whole reason this query is shaped as an inner
/// join is that it is on the hot path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectPage {
    pub rows: Vec<ObjectRow>,
    pub has_more: bool,
    /// Where to resume, when there is more to resume from.
    ///
    /// `None` whenever `has_more` is false, including for a short page that
    /// happens to be the last one -- so a caller cannot seek past the end and
    /// receive an empty page that looks like a filter that matched nothing.
    #[doc(hidden)]
    pub next_cursor: Option<crate::sort::Cursor>,
}

impl ObjectPage {
    /// The ids, for a caller that only wants to know what is there.
    pub fn ids(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.id.clone()).collect()
    }

    /// Where to resume this page's sort, or `None` at the end.
    pub fn next_cursor(&self) -> Option<crate::sort::Cursor> {
        self.next_cursor.clone()
    }
}

/// A query that could not be run.
///
/// No `StoreError` passthrough, because every failure here is either a bad
/// filter (the caller's fault, and the message should say which part) or a
/// store failure (not the caller's, and the message should not pretend it is).
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("bad filter: {0}")]
    Filter(#[from] crate::filter_ast::FilterError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("a limit of {0} is not allowed; the maximum is {1}")]
    LimitTooLarge(usize, usize),
}

/// The largest page this store will produce.
///
/// Not a memory-safety limit — a page of a million rows is a denial of service
/// against the operator's own disk, and §5.16's shareable URLs are
/// attacker-controllable, so the limit is part of the trust boundary rather than
/// a tuning knob.
pub const MAX_PAGE: usize = 500;

impl Store {
    /// Run a consent-filtered object query. **The only way to read objects.**
    ///
    /// Takes a `CallerId` by reference with no default and no other
    /// constructor, so every call site has to say who is asking. That is the
    /// entire mechanism: a filter that can be omitted is a filter that will be
    /// omitted, and the call site that omits it is the one nobody reviews.
    /// Newest first, first page only. Kept for the callers that do not care
    /// about the order, and defined in terms of [`Self::query_sorted`] so there
    /// is one query in this file rather than two that can drift.
    pub async fn query(
        &self,
        filter: &Filter,
        caller: &CallerId,
        limit: usize,
    ) -> Result<ObjectPage, QueryError> {
        self.query_sorted(filter, caller, Sort::date_desc(), None, limit)
            .await
    }

    /// A consent-filtered object page, in a given order, from a given position.
    ///
    /// The order is a tuple, and it is always total: [`Sort`] appends `o.id` as
    /// the last key so that a page boundary between two rows that tie on
    /// everything the user named still has a defined answer. Without that, a
    /// cursor is a guess and the list repeats and skips rows.
    ///
    /// `cursor` is the keyset position, or `None` for the first page. It is the
    /// last row's own key values, produced by [`ObjectPage::next_cursor`] — a
    /// caller cannot assemble one by hand, so a cursor of the wrong arity for
    /// the sort is a type error rather than a page that is quietly wrong.
    ///
    /// # What is NOT here
    ///
    /// No `totalCount`. A `COUNT(*)` over the same filter is a second scan of
    /// the same inner join, on the hot path, to produce a number the UI can
    /// show approximately. §5.16 does not require an exact count, and a keyset
    /// page cannot produce one honestly anyway -- the honest number is "at least
    /// this many", which is what `has_more` already says.
    pub async fn query_sorted(
        &self,
        filter: &Filter,
        caller: &CallerId,
        sort: Sort,
        cursor: Option<crate::sort::Cursor>,
        limit: usize,
    ) -> Result<ObjectPage, QueryError> {
        if limit > MAX_PAGE {
            return Err(QueryError::LimitTooLarge(limit, MAX_PAGE));
        }

        let mut params: Vec<crate::filter_ast::Value> = Vec::new();

        // The consent conjunction, always. Its parameters are pushed first, and
        // the user filter's after, so the two never interleave: an ordering bug
        // in one would otherwise silently bind the other's values.
        let consent = Filter::consent_clause(caller, &mut params)?;
        // The filter's own params come back with its sql, already in the order
        // the placeholders appear. Concatenating the two lists is only safe
        // because the consent placeholders were pushed *first* and appear
        // first in the query -- which is why the two are pushed in that order
        // and not the other way round.
        let compiled = filter.to_sql(self.engine(), caller)?;
        let body = compiled.sql;
        params.extend(compiled.params);

        // The keyset predicate, when there is a cursor. Its binds go LAST
        // because its placeholders appear last in the assembled string -- the
        // same rule the two pushes above obey, and the same reason the order
        // is not a coincidence.
        if let Some(c) = &cursor {
            params.extend(sort.after_binds(c));
        }
        let after_clause = match &cursor {
            Some(_) => format!(
                " AND ({})",
                sort.after_sql().expect("a sort always has a predicate")
            ),
            None => String::new(),
        };

        // The key columns ride along in the SELECT so the last row can become
        // the next cursor without a second round trip.
        //
        // ALL of them, always, in a fixed order -- not just the ones this sort
        // uses, and not in the sort's order. Two reasons, and the first is the
        // one that forced it: `sqlx::query_as` maps to a tuple type, so the
        // column count is fixed at compile time and cannot follow the number of
        // sort keys. The second: a page's cursor is a *value*, and a value that
        // could have been assembled in a different order depending on which keys
        // the sort happened to use is a value whose meaning depends on the sort
        // that produced it. This one has one meaning.
        let sql = format!(
            "SELECT o.id, o.kind, o.title, c.tier, c.redistribution_permitted, \
                    o.date, o.rating_sum, o.title, o.kind, o.created_at
               FROM object o
               INNER JOIN consent_record c ON c.object_id = o.id
              WHERE {consent} AND ({body}){after_clause}
              ORDER BY {}
              LIMIT {}",
            sort.order_by(),
            limit + 1
        );

        // (id, kind, title, tier, redistribution, date, rating_sum, title, kind, created_at)
        //
        // `redistribution_permitted` is an INTEGER column in BOTH schemas, and
        // sqlx decodes a Postgres INTEGER as `i64`, not `bool`. This tuple said
        // `bool`, so the query PANICKED on Postgres: `mismatched types; Rust type
        // bool (as SQL type BOOL) is not compatible with SQL type INT4`. The
        // consent-filtered object query has therefore only ever run against
        // SQLite, and no test noticed because every test reaching this path used
        // the SQLite harness.
        //
        // Decoding as `i32` and reading "non-zero" as true handles both engines
        // without a branch: SQLite's INTEGER is a 64-bit value that sqlx will
        // widen to i32 for a column declared INTEGER, and Postgres's INTEGER is
        // INT4, which is i32 exactly.
        //
        // `locator.rs` reads this same column as `i64` and its tests pass only
        // on SQLite, for the same reason this one failed: sqlx is strict about
        // INT4 vs INT8 and does not widen. So there is no "existing idiom to
        // follow" here -- there is one idiom that happens to be correct on one
        // engine. Reading it by name with `try_get` is the fix that would work
        // either way; `i32` is the fix that works for the value this column
        // actually holds, and the column is a 0/1 flag, so the narrower type is
        // the right one to assert.
        type R = (
            String,         // id
            String,         // kind
            Option<String>, // title
            String,         // tier
            i32,            // redistribution_permitted: Postgres INTEGER is INT4
            Option<String>, // date
            i64,            // rating_sum (NOT NULL in the schema)
            Option<String>, // title (as a sort key)
            String,         // kind (as a sort key)
            String,         // created_at
        );
        // Two arms, not one generic executor.
        //
        // `self.pool()` is SQLite-only and panics on Postgres -- and the code
        // this replaced CALLED IT, so the consent-filtered object query has
        // only ever run against SQLite. No test caught it because every test
        // that exercises this path used the SQLite harness. The match is what
        // the rest of this crate already does (see `relations.rs`), and writing
        // it out is the price of two engines: the `?` placeholders have to
        // become `$1..$n` on the Postgres side, which is not something a shared
        // executor can hide, because the number of placeholders is not known
        // until the statement is assembled.
        macro_rules! bound {
            ($qb:expr) => {{
                let mut qb = $qb;
                for p in &params {
                    qb = bind_value!(qb, p);
                }
                qb
            }};
        }
        let raw: Vec<R> = match self {
            Store::Sqlite(p) => bound!(sqlx::query_as::<_, R>(&sql))
                .fetch_all(p)
                .await
                .map_err(StoreError::Query)?,
            Store::Postgres(p) => {
                let pg = Store::bind_sql(&sql);
                bound!(sqlx::query_as::<_, R>(&pg))
                    .fetch_all(p)
                    .await
                    .map_err(StoreError::Query)?
            }
        };

        let has_more = raw.len() > limit;
        // The cursor is the LAST ROW THE CALLER SEES -- index `limit - 1` --
        // and not the extra `limit + 1`-th row, which exists only to prove
        // `has_more` and is dropped.
        //
        // I wrote the extra row first, and it was a silent data-loss bug: seeking
        // from a row that was never returned skips that row on the next page, so
        // every page boundary would drop exactly one object and the list would
        // be quietly missing 1-in-N of the library with no error anywhere. The
        // `limit + 1` window is what makes this mistake easy to write, because
        // the row carrying the key values is right there in the loop.
        let visible = &raw[..limit.min(raw.len())];
        let rows: Vec<ObjectRow> = visible
            .iter()
            .map(|(id, kind, title, tier, redistribution, ..)| ObjectRow {
                id: id.clone(),
                kind: kind.clone(),
                title: title.clone(),
                tier: tier.clone(),
                redistribution_permitted: *redistribution != 0,
            })
            .collect();
        let next_cursor = if has_more {
            visible.last().map(
                |(id, _, _, _, _, date, rating_sum, sk_title, sk_kind, added)| {
                    crate::sort::Cursor::from_row(
                        &sort, id, date, rating_sum, sk_title, sk_kind, added,
                    )
                },
            )
        } else {
            None
        };

        Ok(ObjectPage {
            rows,
            has_more,
            next_cursor,
        })
    }

    /// The tiers a caller may see, as the list a query actually binds.
    ///
    /// `pub` so that a test can assert what a query binds without running one.
    /// A test that has to open a database to check a constant is a test that
    /// stops being run.
    pub fn consent_tiers_for(
        &self,
        caller: &CallerId,
    ) -> Result<Vec<String>, crate::filter_ast::FilterError> {
        let mut params = Vec::new();
        Filter::consent_clause(caller, &mut params)?;
        Ok(params
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect())
    }
}

/// A convenience for the very common "which ids match, ignoring the rest".
///
/// Kept separate from [`Store::query`] so the page's row type stays the query's
/// row type. A convenience that returned a different shape would be a second
/// surface, and the whole ticket is about there being one.
impl Store {
    /// Ids only. Same filter, same caller, same guarantees.
    pub async fn query_ids(
        &self,
        filter: &Filter,
        caller: &CallerId,
        limit: usize,
    ) -> Result<Vec<String>, QueryError> {
        Ok(self.query(filter, caller, limit).await?.ids())
    }
}

/// A map of id to tier, for callers that need several objects at once and
/// would otherwise query per id.
///
/// Takes the ids in and returns the tiers *of the ones the caller may see*,
/// with a missing entry meaning "not visible". A missing entry is not
/// distinguishable from a missing object, and that is deliberate: §14.1 does not
/// want the existence of a `denied` object disclosed to somebody who cannot see
/// it, and a distinct error for "exists but hidden" is exactly that disclosure.
pub async fn tiers_for(
    store: &Store,
    ids: &[String],
    caller: &CallerId,
) -> Result<HashMap<String, String>, QueryError> {
    let mut params: Vec<crate::filter_ast::Value> = Vec::new();
    let consent = Filter::consent_clause(caller, &mut params)?;
    let mut placeholders = Vec::new();
    for id in ids {
        placeholders.push("?");
        params.push(crate::filter_ast::Value::Str(id.clone()));
    }
    if placeholders.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = format!(
        "SELECT c.object_id, c.tier FROM consent_record c INNER JOIN object o ON o.id = c.object_id
          WHERE {consent} AND c.object_id IN ({})",
        placeholders.join(", ")
    );
    let mut qb = sqlx::query_as::<_, (String, String)>(&sql);
    for p in &params {
        qb = bind_value!(qb, p);
    }
    let rows = qb
        .fetch_all(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(rows.into_iter().collect())
}
