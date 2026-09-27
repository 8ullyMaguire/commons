//! Funscript rows: the store layer (T-P6-003, spec §5.6).
//!
//! # What is stored, and what is deliberately not
//!
//! The `funscript` table carries the **path**, the **axis count** and the
//! script's **metadata** as JSON. It does not carry the actions.
//!
//! That is the decision worth defending, because the obvious design is the
//! other one. A funscript is a 20,000-action timeline and a library is a
//! hundred thousand of them; storing the actions means a table that grows by
//! gigabytes, a re-parse on every read to rebuild an index, and a migration
//! every time the format grows a field. Storing the path means the script
//! stays where the user's tool put it — next to the video, in a directory, or
//! wherever a plugin fetched it — and `axis_count` is enough for a library
//! view to say "this is a 2-axis script" without opening a single file.
//!
//! The cost is real and stated: a timeline is computed per player load rather
//! than read from the database. For a 20,000-action script that is about a
//! millisecond, once, when the user opens the player — and it is the only
//! place the actions are needed at all.
//!
//! # `metadata` is a JSON blob and that is the right call here
//!
//! The fields are the script's own (`title`, `author`, `version`, `source`) and
//! they are *read-only projections* of a file Commons does not own. A new
//! exporter field therefore needs no migration, which is the property that
//! matters: the funscript format is a de-facto standard other tools write, and
//! a schema that must be migrated every time it grows is a schema that will
//! always be one version behind.
//!
//! The trade is that a typo in a metadata key is invisible to the database. It
//! is caught at the type boundary instead — [`FunscriptMetadata`] is what
//! serialises into this column, so the shape is checked in Rust and the column
//! is only ever read back through it.
//!
//! # The same rules as every other store module
//!
//! `INTEGER` decodes as `i32` (Postgres INT4, SQLite INT8 — an `i64` read
//! passes on one engine and fails on the other), the query is built inside each
//! match arm because `sqlx::Query` is monomorphic, and the placeholder style
//! comes from the engine rather than being guessed.

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::db::{placeholders, Store, StoreError};

/// A funscript's read-only metadata, as stored in the `metadata` column.
///
/// Mirrors `commons_scan::funscript::FunscriptMetadata` field for field. It is
/// a separate type rather than a re-export because the store must not depend on
/// `commons-scan` (layering), and a struct that duplicated a domain type would
/// drift the first time one of them gained a field. A test asserts the two
/// serialise identically, which is the cheapest thing that catches the drift.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FunscriptMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    /// Kept as text: the format's own version arrives as a number in some
    /// exporters and a string in others, and the store does not care which.
    pub version: Option<String>,
    /// `sidecar` | `directory` | `provided`. See
    /// `commons_scan::funscript::FunscriptSource`.
    pub source: Option<String>,
}

macro_rules! script_from_row {
    ($r:expr, $row:ty) => {{
        let r: &$row = $r;
        FunscriptRow {
            id: r.get("id"),
            object_id: r.get("object_id"),
            path: r.get("path"),
            // `axis_count` is INTEGER: i32 on both engines, never i64.
            axis_count: r.get::<i32, _>("axis_count"),
            metadata: r.get("metadata"),
            created_at: r.get("created_at"),
        }
    }};
}

/// One funscript row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunscriptRow {
    pub id: String,
    pub object_id: String,
    /// Where the script is. Absolute or library-relative, whatever the scanner
    /// recorded; the reader does not care and the writer does not decide.
    pub path: String,
    /// How many named axes the script has. `1` for a classic script, N for
    /// #6339. Stored rather than counted because a library view must not open
    /// a file to render a row.
    pub axis_count: i32,
    /// The JSON metadata, or None for a script whose file carried none.
    pub metadata: Option<String>,
    pub created_at: String,
}

impl FunscriptRow {
    /// The metadata, decoded. `None` when the column is NULL **or** when the
    /// stored text will not parse.
    ///
    /// The two are deliberately collapsed. A row whose metadata blob is
    /// corrupt is a row whose metadata the user never sees, and a route that
    /// 500s on it would take the whole funscript list down for one bad row —
    /// while a route that 500s on nothing still needs to answer "does this
    /// object have a script".
    pub fn metadata(&self) -> Option<FunscriptMetadata> {
        self.metadata
            .as_deref()
            .and_then(|t| serde_json::from_str(t).ok())
    }
}

/// Errors from the funscript store.
#[derive(Debug, thiserror::Error)]
pub enum FunscriptStoreError {
    #[error("funscript query failed: {0}")]
    Query(#[from] StoreError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<sqlx::Error> for FunscriptStoreError {
    fn from(e: sqlx::Error) -> Self {
        Self::Query(StoreError::Query(e))
    }
}

/// Every funscript for an object, in a stable order.
///
/// `ORDER BY axis_count DESC, path` rather than by `created_at`: two scripts
/// for one object are a re-scan and a user-supplied one, and the *interesting*
/// one is the multi-axis script, because that is the one a single-axis player
/// cannot use and a user needs to know exists. `path` breaks the tie so the
/// order is total — a list that reorders between two identical requests makes
/// "which one is first" unanswerable, and a player that picks the first
/// silently changes behaviour.
pub async fn list_for_object(
    store: &Store,
    object_id: &str,
) -> Result<Vec<FunscriptRow>, FunscriptStoreError> {
    const SQL: &str = "SELECT id, object_id, path, axis_count, metadata, created_at \
         FROM funscript WHERE object_id = {p} ORDER BY axis_count DESC, path";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query(&sql)
                .bind(object_id)
                .fetch_all($p)
                .await
                .map_err(|e| FunscriptStoreError::Query(StoreError::Query(e)))
                .map(|rows| rows.iter().map(|r| script_from_row!(r, $row)).collect())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// One funscript by id.
pub async fn get(store: &Store, id: &str) -> Result<Option<FunscriptRow>, FunscriptStoreError> {
    const SQL: &str =
        "SELECT id, object_id, path, axis_count, metadata, created_at FROM funscript WHERE id = {p}";
    macro_rules! go {
        ($p:expr, $numbered:literal, $row:ty) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query(&sql)
                .bind(id)
                .fetch_optional($p)
                .await
                .map_err(|e| FunscriptStoreError::Query(StoreError::Query(e)))
                .map(|row| row.as_ref().map(|r| script_from_row!(r, $row)))
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false, sqlx::sqlite::SqliteRow),
        Store::Postgres(p) => go!(p, true, sqlx::postgres::PgRow),
    }
}

/// The path recorded for an object, if any.
///
/// A single-row convenience for the scanner, which wants "the file I just found"
/// without building a list. Ordered the same way as [`list_for_object`] so
/// "the first script" means the same thing in both.
pub async fn primary_path(
    store: &Store,
    object_id: &str,
) -> Result<Option<String>, FunscriptStoreError> {
    const SQL: &str = "SELECT path FROM funscript WHERE object_id = {p} \
         ORDER BY axis_count DESC, path LIMIT 1";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            sqlx::query_scalar::<_, String>(&sql)
                .bind(object_id)
                .fetch_optional($p)
                .await
                .map_err(|e| FunscriptStoreError::Query(StoreError::Query(e)))
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}

/// Insert a funscript row, or return the existing one for the same path.
///
/// The `UNIQUE (object_id, path)` constraint is the conflict target and it is
/// the whole point: a re-scan finds the same sidecar at the same path, and the
/// row must be replaced rather than duplicated, because a duplicated row shows
/// the user the same script twice and makes "which one does the player load"
/// ambiguous. `axis_count` is updated because a script that gained an axis is a
/// different script at the same path.
///
/// Returns whether a row was written, so a caller can tell a re-scan that
/// changed something from one that found nothing new.
pub async fn put(store: &Store, row: &FunscriptRow) -> Result<bool, FunscriptStoreError> {
    // Written ONCE in the SQLite placeholder style and converted by
    // `Store::bind_sql` for Postgres. The other way round -- two hand-written
    // statements -- is how the two engines drift, and the drift is invisible
    // until one of them refuses a bind count. Note `created_at` is TEXT in
    // this schema on BOTH engines, so there is no `NOW()` divergence to paper
    // over: the caller supplies the timestamp and the row is reproducible.
    const SQL: &str =
        "INSERT INTO funscript (id, object_id, path, axis_count, metadata, created_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (object_id, path) DO UPDATE SET axis_count = excluded.axis_count, \
         metadata = excluded.metadata";
    macro_rules! go {
        ($p:expr, $sql:expr) => {{
            let r = sqlx::query($sql)
                .bind(&row.id)
                .bind(&row.object_id)
                .bind(&row.path)
                .bind(row.axis_count)
                .bind(row.metadata.as_deref())
                .bind(&row.created_at)
                .execute($p)
                .await
                .map_err(|e| FunscriptStoreError::Query(StoreError::Query(e)))?;
            Ok(r.rows_affected() > 0)
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, SQL),
        Store::Postgres(p) => go!(p, &Store::bind_sql(SQL)),
    }
}

/// Delete every funscript for an object, returning how many went.
///
/// Used by a re-scan that found none: a video whose sidecar was deleted must
/// stop offering a script that no longer exists, and leaving the row means the
/// player opens a file that is gone and reports an empty timeline — which a
/// user reads as "my script is broken" rather than "you deleted it".
pub async fn delete_for_object(store: &Store, object_id: &str) -> Result<u64, FunscriptStoreError> {
    const SQL: &str = "DELETE FROM funscript WHERE object_id = {p}";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            let r = sqlx::query(&sql)
                .bind(object_id)
                .execute($p)
                .await
                .map_err(|e| FunscriptStoreError::Query(StoreError::Query(e)))?;
            Ok(r.rows_affected())
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}

/// How many funscripts an object has.
///
/// A count rather than a bool because a library view wants the number, and a
/// bool would make the caller fetch the list to get it.
pub async fn count_for_object(store: &Store, object_id: &str) -> Result<i64, FunscriptStoreError> {
    const SQL: &str = "SELECT COUNT(*) FROM funscript WHERE object_id = {p}";
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let sql = SQL.replace("{p}", &placeholders(1, $numbered));
            // COUNT(*) is BIGINT on both engines, so i64 decodes on both --
            // the INTEGER/i32 trap does not apply here.
            sqlx::query_scalar::<_, i64>(&sql)
                .bind(object_id)
                .fetch_one($p)
                .await
                .map_err(|e| FunscriptStoreError::Query(StoreError::Query(e)))
        }};
    }
    match store {
        Store::Sqlite(p) => go!(p, false),
        Store::Postgres(p) => go!(p, true),
    }
}
