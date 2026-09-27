//! Where an object's bytes are, and who is allowed to be told.
//!
//! T-P5-006 item 9, spec §10.6. The vertical view needs to load media, and
//! there was no way to ask this question before: the store could tell you an
//! object exists and could tell you its consent tier, but not "give me the path
//! of the file behind this object, if this caller is allowed to have it".
//!
//! # Why the gate is here and not at the route
//!
//! For the same reason `locator.rs` puts its gate in the data layer, and the
//! same sentence applies: **a check that lives at the call site is a check that
//! some future call site forgets.** A media route is the most attractive target
//! in the codebase for getting that wrong — it is the one route whose whole job
//! is handing out file paths, so "I already checked" feels like a formality
//! rather than the security boundary it is.
//!
//! So the function returns `Option<MediaLocation>` and there is no path in the
//! type for a caller to obtain without passing a `CallerId`. There is no
//! `media_path_unchecked`, and adding one would be the moment the design stops
//! working — which is the point of writing it down.
//!
//! # Absent and denied are the same answer
//!
//! `None` means "not for you", and it covers all three of: the object does not
//! exist, the file is not present on disk, and the caller's tiers do not include
//! this one. Differentiating them would turn this function into an oracle for
//! what is in the library, and on a consent-first platform **whether an id
//! exists is itself consent information** — §14.1 is not only about the bytes,
//! it is about the fact that there is something to have bytes about.
//!
//! The cost is real and worth stating: a caller debugging a missing file cannot
//! tell "denied" from "gone" from this API. That is the intended trade, and the
//! route's 404 is the same answer for the same reason.

use crate::db::{Store, StoreError};
use crate::filter_ast::{CallerId, Filter, FilterError, Value};
use std::path::PathBuf;

/// Why a media lookup could not be run.
///
/// Its own type rather than a `StoreError` variant, and for the reason
/// `RelationError::Consent` gives: a caller that gets `Consent` has a broken
/// `CallerId`, not a broken database, and folding the two together makes them
/// the same log line — which is how a caller bug ends up investigated as an
/// outage.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("media query failed: {0}")]
    Query(#[from] StoreError),
    #[error("consent clause could not be built: {0}")]
    Consent(#[from] FilterError),
}

type MediaResult<T> = std::result::Result<T, MediaError>;

/// Where an object's bytes are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaLocation {
    /// The file's path, as the index recorded it.
    ///
    /// **Not resolved, not canonicalised, and not checked for existence.** The
    /// route opens it; this function does not touch the disk, because a store
    /// that stats the filesystem on every query is a store whose tests need a
    /// real disk and whose behaviour depends on mount state. Existence is
    /// `file.state`, which the index maintains and which is what this reads.
    pub path: PathBuf,
    /// The recorded size, so a route can answer `Content-Length` without
    /// opening the file and can refuse a range that cannot exist.
    pub size_bytes: u64,
    /// `file.state`. Only `present` is a path to bytes; the others are a row
    /// without a file, and a mid-rescan library is full of them.
    pub state: String,
}

impl MediaLocation {
    /// Whether this row is a path to bytes right now.
    pub fn is_present(&self) -> bool {
        self.state == "present"
    }
}

impl Store {
    /// The file behind an object, if this caller may have it.
    ///
    /// `Ok(None)` is the whole of the gate: no path, no size, nothing to leak.
    /// The consent clause is ANDed into the `WHERE` — it is a predicate over
    /// data, not a filter applied to rows that were already read — and it is the
    /// first arm of the query for the same reason `Store::query` puts it first:
    /// its parameters are pushed first so they cannot interleave with anything
    /// else's.
    ///
    /// The consent join is an INNER join, and mutating it to LEFT survives every
    /// test in the file. That is not a test gap: `consent_clause` emits
    /// `c.tier IN (...)`, a LEFT JOIN with no match leaves `c.tier` NULL, and
    /// `NULL IN (...)` is NULL, which is not TRUE — so the row is filtered out
    /// either way. The INNER is there so the query says what it means, and the
    /// WHERE clause happens to enforce the same thing. A test that claimed to
    /// pin it would be claiming coverage it does not have.
    ///
    /// One file per object is the *precondition*, so that join is an inner join
    /// too. The opposite choice is right in `all_tags_with_counts` and wrong
    /// here: a tag with no objects is a real row worth showing, while an object
    /// with no file is an object with nothing to serve, and listing it would
    /// produce a tile that 404s when tapped.
    pub async fn media_path(
        &self,
        object_id: &str,
        caller: &CallerId,
    ) -> MediaResult<Option<MediaLocation>> {
        let mut params: Vec<Value> = Vec::new();
        let consent = Filter::consent_clause(caller, &mut params)?;
        params.push(Value::Str(object_id.to_string()));

        // The `present` filter is in the `WHERE` and not in Rust. An absent
        // row is a file the index knows about and the disk does not have, and
        // routing it would 500 for a library that is merely being reindexed --
        // a mid-rescan library is entirely absent rows, so this is the common
        // case during indexing and not an edge case.
        //
        // `f.state` is still selected: a caller that wants to distinguish "no
        // file row" from "a file that is not there" needs the value, and
        // hiding it would conflate the two one level below the conflation this
        // function already accepts.
        //
        // **No `ORDER BY path`.** This is the `search.rs` rule, arrived at
        // again: `ORDER BY` on a free-text field is alphabetical on one engine
        // and collation-dependent on the other, which is the class of
        // disagreement §3.5 exists to prevent. I wrote `ORDER BY f.path LIMIT 1`
        // first and the parity test caught it immediately -- with
        // `/library/a.mp4` and `/library/a-preview.mp4`, SQLite's BINARY order
        // put the preview second ('-' 0x2D > '.' 0x2E) and Postgres's
        // en_US.UTF-8 order put it FIRST, because that collation ignores
        // punctuation at the primary comparison level. Two engines, one
        // library, two different files served, and the first one to get it
        // wrong is a served preview where a clip was asked for.
        //
        // So the choice is made in Rust, on the same rule `search.rs` uses: a
        // `String`'s own `Ord` is Unicode code-point order, identical on both
        // engines, and the query returns every candidate.
        let sql = format!(
            "SELECT f.path, f.size_bytes, f.state
               FROM file f
               INNER JOIN object o ON o.id = f.object_id
               INNER JOIN consent_record c ON c.object_id = o.id
              WHERE {consent} AND f.object_id = ? AND f.state = 'present'"
        );

        // The bind loop is hand-written, and duplicated from `relations.rs`,
        // for the reason that file gives: `sqlx::Query` and `sqlx::QueryAs` are
        // unrelated types with no common trait, so a shared helper would have to
        // be generic over a parameter neither exposes. Two lines beats a
        // signature nobody can write.
        macro_rules! bound {
            ($qb:expr) => {{
                let mut q = $qb;
                for v in &params {
                    q = match v {
                        Value::Str(s) => q.bind(s.clone()),
                        Value::Int(i) => q.bind(*i),
                        Value::Float(f) => q.bind(*f),
                        Value::Bool(b) => q.bind(*b),
                        // `consent_clause` emits only scalars, and a silent
                        // stringification here would compare a tier against the
                        // literal text `["a"]`.
                        Value::Null | Value::List(_) => {
                            panic!("consent_clause emitted a non-scalar parameter: {v:?}")
                        }
                    };
                }
                q
            }};
        }

        type R = (String, i64, String);
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

        // `min_by` on the path alone. The order is total because `path` is
        // `NOT NULL`, so no two candidates can tie, and a tie would not matter
        // anyway: the answer is one path, and either is the same bytes to
        // nobody. Ordering in Rust rather than in SQL is what makes the two
        // engines agree -- `String`'s `Ord` is code-point order on both, and
        // `ORDER BY path` is BINARY on one and en_US.UTF-8 on the other.
        let chosen = raw.into_iter().min_by(|a, b| a.0.cmp(&b.0));

        Ok(chosen.map(|(path, size, state)| MediaLocation {
            path: PathBuf::from(path),
            // A negative recorded size is not a size. `size_bytes` is NOT NULL
            // but a hand-edited or migrated row can hold one, and a route that
            // puts a negative number in `Content-Length` fails in a way that
            // looks like a browser bug.
            size_bytes: size.max(0) as u64,
            state,
        }))
    }
}
