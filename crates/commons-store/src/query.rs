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
}

impl ObjectPage {
    /// The ids, for a caller that only wants to know what is there.
    pub fn ids(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.id.clone()).collect()
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
    pub async fn query(
        &self,
        filter: &Filter,
        caller: &CallerId,
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

        // `limit + 1` for `has_more`, and the extra row is dropped afterwards
        // rather than trimmed in SQL, because a `LIMIT` that is off by one
        // between the count and the page is the classic way to get a page that
        // says "no more" while one exists.
        let sql = format!(
            "SELECT o.id, o.kind, o.title, c.tier, c.redistribution_permitted
               FROM object o
               INNER JOIN consent_record c ON c.object_id = o.id
              WHERE {consent} AND ({body})
              ORDER BY o.date DESC, o.id
              LIMIT {}",
            limit + 1
        );

        let mut qb = sqlx::query_as::<_, (String, String, Option<String>, String, bool)>(&sql);
        for p in &params {
            qb = bind_value!(qb, p);
        }
        let raw = qb.fetch_all(self.pool()).await.map_err(StoreError::Query)?;

        let has_more = raw.len() > limit;
        Ok(ObjectPage {
            rows: raw
                .into_iter()
                .take(limit)
                .map(|(id, kind, title, tier, redistribution)| ObjectRow {
                    id,
                    kind,
                    title,
                    tier,
                    redistribution_permitted: redistribution,
                })
                .collect(),
            has_more,
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
