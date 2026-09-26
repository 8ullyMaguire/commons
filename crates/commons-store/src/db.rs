//! Connection and migration runner (T-P0-004).
//!
//! One logical schema, two engines. `Mode` is the single place that decides
//! which: a local library gets SQLite in the data directory, a public index
//! gets Postgres from the environment. Nothing above this crate chooses.

use std::path::{Path, PathBuf};

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;
use sqlx::{Executor, PgPool, SqlitePool};
use std::str::FromStr;

use crate::filter_ast::Engine;

/// Embedded migrations, compiled in. Embedding rather than reading from disk
/// means a binary cannot be run against a schema it does not carry, which is
/// what makes `migrations/` a package asset instead of a deployment step.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations/sqlite");

/// Which engine a store is running on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Local library. SQLite in a data directory the user chose.
    Library,
    /// Public or shared index. Postgres, from the environment.
    Index,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot open the library at {path}: {source}")]
    OpenLibrary {
        path: PathBuf,
        #[source]
        source: sqlx::Error,
    },
    #[error("cannot connect to the index database: {0}")]
    ConnectIndex(#[source] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("query failed: {0}")]
    Query(#[source] sqlx::Error),
    #[error("data directory {0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("the index database needs DATABASE_URL to be set")]
    NoDatabaseUrl,
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// A handle to the database. The two variants are deliberately not collapsed
/// into a trait object: every call site knows which engine it is on at compile
/// time, and the portable-SQL rules in the schema mean the queries are the
/// same text either way.
#[derive(Clone)]
pub enum Store {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

impl Store {
    /// Open a local library at `data_dir/commons.sqlite`, creating the
    /// directory if needed, and migrate it.
    pub async fn open_library(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref();
        if !data_dir.exists() {
            std::fs::create_dir_all(data_dir).map_err(|e| StoreError::OpenLibrary {
                path: data_dir.to_path_buf(),
                source: sqlx::Error::Io(std::io::Error::other(e)),
            })?;
        }
        if !data_dir.is_dir() {
            return Err(StoreError::NotADirectory(data_dir.to_path_buf()));
        }

        let path = data_dir.join("commons.sqlite");
        let url = format!("sqlite://{}", path.display());

        let mut opts = SqliteConnectOptions::from_str(&url)
            .map_err(|e| StoreError::OpenLibrary {
                path: path.clone(),
                source: sqlx::Error::Configuration(e.into()),
            })?
            // Foreign keys are off by default in SQLite and the schema relies on
            // them for ON DELETE CASCADE. Without this, deleting an object would
            // orphan its files, segments, tags and votes.
            .foreign_keys(true)
            // WAL lets the watcher write while a query reads, which is the
            // normal state of a scanning library.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            // NORMAL is the documented safe pairing with WAL: durable across a
            // process crash, and only at risk from an OS-level crash, which the
            // generated-file cache can rebuild from anyway.
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .create_if_missing(true);

        // A library can be browsed and scanned from more than one process (the
        // server, the CLI, a Tauri shell). Cap the pool so a dozen clients do
        // not serialise behind one writer.
        opts = opts.pragma("cache_size", "-16000"); // ~16MB, part of the §4.3 budget

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(|e| StoreError::OpenLibrary {
                path: path.clone(),
                source: e,
            })?;

        let store = Store::Sqlite(pool);
        store.migrate().await?;
        Ok(store)
    }

    /// Connect to the index database named by `DATABASE_URL` and migrate it.
    pub async fn open_index() -> Result<Self> {
        let url = std::env::var("DATABASE_URL").map_err(|_| StoreError::NoDatabaseUrl)?;
        let pool = PgPool::connect(&url)
            .await
            .map_err(StoreError::ConnectIndex)?;
        let store = Store::Postgres(pool);
        store.migrate().await?;
        Ok(store)
    }

    /// Open a throwaway in-memory SQLite, for tests. Migrations still run, so a
    /// test that passes here is testing the real schema.
    pub async fn open_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|e| StoreError::ConnectIndex(sqlx::Error::Configuration(e.into())))?
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(StoreError::ConnectIndex)?;
        let store = Store::Sqlite(pool);
        store.migrate().await?;
        Ok(store)
    }

    pub fn engine(&self) -> Engine {
        match self {
            Store::Sqlite(_) => Engine::Sqlite,
            Store::Postgres(_) => Engine::Postgres,
        }
    }

    pub fn mode(&self) -> Mode {
        match self {
            Store::Sqlite(_) => Mode::Library,
            Store::Postgres(_) => Mode::Index,
        }
    }

    /// Apply pending migrations. Idempotent: a second call is a no-op, which
    /// is what lets the server call it on every start.
    pub async fn migrate(&self) -> Result<()> {
        match self {
            Store::Sqlite(pool) => MIGRATOR.run(pool).await?,
            // The Postgres tree is byte-identical to the SQLite one, so the
            // checksummed migration table applies to both and stays in step.
            Store::Postgres(pool) => MIGRATOR.run(pool).await?,
        }
        Ok(())
    }

    /// The schema version this build expects, for `/healthz` (§15.9).
    pub async fn schema_version(&self) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations")
            .fetch_one(self.pool())
            .await
            .map_err(StoreError::Query)?;
        Ok(row.0)
    }

    /// True when nothing is queued or running: what `/readyz` reports.
    pub async fn pending_jobs(&self) -> Result<i64> {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM job WHERE state IN ('queued', 'running')")
                .fetch_one(self.pool())
                .await
                .map_err(StoreError::Query)?;
        Ok(row.0)
    }

    /// The `?`-placeholder SQL from a compiled filter, rewritten to `$n` when
    /// the target is Postgres. The AST emits one marker style; this is the only
    /// place that knows the difference.
    pub fn bind_sql(sql: &str) -> String {
        let mut out = String::with_capacity(sql.len() + 8);
        let mut n = 1usize;
        for c in sql.chars() {
            if c == '?' {
                out.push('$');
                out.push_str(&n.to_string());
                n += 1;
            } else {
                out.push(c);
            }
        }
        out
    }

    pub fn pool(&self) -> &sqlx::SqlitePool {
        match self {
            Store::Sqlite(p) => p,
            // A Postgres pool is not a SQLite pool. Rather than launder the
            // type, expose it as a generic executor and let callers pick.
            Store::Postgres(_) => panic!("pool() is SQLite-only; use pg_pool() or exec()"),
        }
    }

    pub fn pg_pool(&self) -> Option<&PgPool> {
        match self {
            Store::Postgres(p) => Some(p),
            Store::Sqlite(_) => None,
        }
    }

    pub fn sqlite_pool(&self) -> Option<&SqlitePool> {
        match self {
            Store::Sqlite(p) => Some(p),
            Store::Postgres(_) => None,
        }
    }
}

/// A borrowed executor, so a caller can run a statement on whichever engine it
/// holds without matching on the enum at every call site.
impl Store {
    pub async fn fetch_affected(&self, sql: &str) -> Result<i64> {
        match self {
            Store::Sqlite(p) => {
                let r = p.execute(sql).await.map_err(StoreError::Query)?;
                Ok(r.rows_affected() as i64)
            }
            Store::Postgres(p) => {
                let r = p.execute(sql).await.map_err(StoreError::Query)?;
                Ok(r.rows_affected() as i64)
            }
        }
    }
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Store::Sqlite(p) => f.debug_tuple("Sqlite").field(&p.size()).finish(),
            Store::Postgres(p) => f.debug_tuple("Postgres").field(&p.size()).finish(),
        }
    }
}

// ---------------------------------------------------------------- file rows

/// A row of the `file` table, as the scanner needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub id: String,
    pub object_id: String,
    pub path: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub hash_blake3: Option<String>,
}

/// Every `file` row, in whatever order the database returns.
///
/// The caller must not depend on the order. The first version of the
/// reconciler did, and picked whichever row a hash lookup returned first --
/// which made whether two identical files got merged depend on the query
/// planner.
pub async fn file_rows(store: &Store) -> Result<Vec<StoredFile>> {
    let rows =
        sqlx::query("SELECT id, object_id, path, size_bytes, mtime_ns, hash_blake3 FROM file")
            .fetch_all(store.pool())
            .await
            .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .map(|r| StoredFile {
            id: r.get("id"),
            object_id: r.get("object_id"),
            path: r.get("path"),
            size_bytes: r.get("size_bytes"),
            mtime_ns: r.get("mtime_ns"),
            hash_blake3: r.get("hash_blake3"),
        })
        .collect())
}

/// Rewrite a file row's path, keeping its id, object, and artifacts.
///
/// This is the move. Everything expensive about a file -- extracted metadata,
/// thumbnails, sprites, proxies -- hangs off `file.id` and is therefore
/// untouched by design rather than by remembering not to delete it.
pub async fn set_path(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    file_id: &str,
    path: &str,
) -> Result<()> {
    sqlx::query("UPDATE file SET path = ? WHERE id = ?")
        .bind(path)
        .bind(file_id)
        .execute(&mut **tx)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// Mark a file absent without deleting it.
///
/// A file that stopped being seen is usually on a volume that is not mounted,
/// not deleted. Deleting the row would cascade away its artifacts, so the
/// next mount of the volume would find a directory of files with no
/// thumbnails and no metadata, and re-extract all of it.
pub async fn mark_absent(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    file_id: &str,
) -> Result<()> {
    sqlx::query("UPDATE file SET state = 'absent' WHERE id = ?")
        .bind(file_id)
        .execute(&mut **tx)
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// A new `file` row.
///
/// A struct rather than eight positional arguments: a call site with eight
/// `&str`/`i64`/`Option<&str>` arguments gets two of them the wrong way round
/// and the compiler cannot help, because every one of them is the same type.
#[derive(Debug, Clone)]
pub struct NewFile<'a> {
    pub id: &'a str,
    pub object_id: &'a str,
    pub path: &'a str,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub hash_xxh128: Option<&'a str>,
    pub hash_blake3: Option<&'a str>,
}

impl NewFile<'_> {
    /// The row, as the reconciler reads it back.
    pub fn to_stored(&self) -> StoredFile {
        StoredFile {
            id: self.id.to_string(),
            object_id: self.object_id.to_string(),
            path: self.path.to_string(),
            size_bytes: self.size_bytes,
            mtime_ns: self.mtime_ns,
            hash_blake3: self.hash_blake3.map(str::to_string),
        }
    }
}

/// Insert a file row. Used by the scanner for genuinely new files.
pub async fn insert_file(store: &Store, f: &NewFile<'_>) -> Result<()> {
    sqlx::query(
        "INSERT INTO file (id, object_id, path, size_bytes, mtime_ns, hash_xxh128, hash_blake3)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(f.id)
    .bind(f.object_id)
    .bind(f.path)
    .bind(f.size_bytes)
    .bind(f.mtime_ns)
    .bind(f.hash_xxh128)
    .bind(f.hash_blake3)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// Insert a bare object row. Tests and the scanner's new-file path.
pub async fn insert_object(store: &Store, id: &str, kind: &str) -> Result<()> {
    sqlx::query("INSERT INTO object (id, kind, created_at, updated_at) VALUES (?, ?, '', '')")
        .bind(id)
        .bind(kind)
        .execute(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(())
}

/// Insert an artifact row. Used by the media pipeline and by tests.
pub async fn insert_artifact(
    store: &Store,
    id: &str,
    file_id: &str,
    kind: &str,
    path: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO artifact (id, file_id, kind, path, mtime_ns, size_bytes, \
         generator_version, created_at) VALUES (?, ?, ?, ?, 1, 10, 1, '')",
    )
    .bind(id)
    .bind(file_id)
    .bind(kind)
    .bind(path)
    .execute(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(())
}

/// The artifact kinds attached to a file, sorted.
///
/// Exposed as a typed accessor rather than letting callers run their own
/// query, so the "did a move preserve the artifacts" question has one answer
/// that every caller gets the same way.
pub async fn artifact_kinds(store: &Store, file_id: &str) -> Result<Vec<String>> {
    let rows = sqlx::query("SELECT kind FROM artifact WHERE file_id = ? ORDER BY kind")
        .bind(file_id)
        .fetch_all(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(rows.into_iter().map(|r| r.get("kind")).collect())
}

/// One file row's path and state, or `None` if there is no such row.
pub async fn file_path_and_state(store: &Store, file_id: &str) -> Result<Option<(String, String)>> {
    let row = sqlx::query("SELECT path, state FROM file WHERE id = ?")
        .bind(file_id)
        .fetch_optional(store.pool())
        .await
        .map_err(StoreError::Query)?;
    Ok(row.map(|r| (r.get("path"), r.get("state"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fresh_library_migrates_to_the_full_schema() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_library(dir.path()).await.unwrap();

        assert_eq!(store.mode(), Mode::Library);
        assert_eq!(store.engine(), Engine::Sqlite);

        // Every table from 0001_core.sql must exist, not just some of them.
        for table in [
            "object",
            "file",
            "segment",
            "object_relation",
            "person_cluster",
            "appearance",
            "performer",
            "performer_alias",
            "producer",
            "producer_url",
            "tag",
            "object_tag",
            "field_proposal",
            "vote",
            "field_lock",
            "consent_record",
            "consent_event",
            "denied_hash",
            "job",
            "artifact",
            "locator",
            "claim",
            "peer",
            "custom_field",
            "custom_field_value",
            "external_id",
            "list_obj",
            "list_item",
            "marker",
            "subtitle",
            "funscript",
            "account",
            "session",
            "rating",
            "watch",
            "notification",
            "group_obj",
            "group_member",
            "archive",
            "object_tag",
            "producer_alias",
        ] {
            let n: (i64,) = sqlx::query_as(&format!(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
            ))
            .fetch_one(store.pool())
            .await
            .unwrap();
            assert_eq!(n.0, 1, "table {table} is missing from the schema");
        }
    }

    #[tokio::test]
    async fn migrating_twice_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_library(dir.path()).await.unwrap();
        let before = store.schema_version().await.unwrap();
        store.migrate().await.unwrap();
        store.migrate().await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), before);
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced_so_deletes_cascade() {
        // SQLite defaults FKs off. If this regresses, deleting an object leaves
        // its files, segments and tags behind and the library grows forever.
        let store = Store::open_memory().await.unwrap();
        let fk: (i64,) = sqlx::query_as("PRAGMA foreign_keys")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(fk.0, 1, "foreign keys must be on for a library");

        sqlx::query("INSERT INTO object (id, kind, created_at, updated_at) VALUES ('o1', 'scene', 't', 't')")
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO file (id, object_id, path) VALUES ('f1', 'o1', '/tmp/x.mp4')")
            .execute(store.pool())
            .await
            .unwrap();

        sqlx::query("DELETE FROM object WHERE id = 'o1'")
            .execute(store.pool())
            .await
            .unwrap();
        let left: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM file")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(left.0, 0, "file should have cascaded with its object");
    }

    #[tokio::test]
    async fn wal_is_enabled_so_a_scan_does_not_block_browsing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_library(dir.path()).await.unwrap();
        let mode: (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(mode.0.to_lowercase(), "wal");
    }

    #[tokio::test]
    async fn the_data_directory_is_created_if_absent() {
        let base = tempfile::tempdir().unwrap();
        let nested = base.path().join("a").join("b").join("library");
        assert!(!nested.exists());
        let _store = Store::open_library(&nested).await.unwrap();
        assert!(nested.is_dir());
        assert!(nested.join("commons.sqlite").exists());
    }

    #[tokio::test]
    async fn a_file_where_the_data_dir_should_be_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let err = Store::open_library(&file).await.unwrap_err();
        assert!(matches!(err, StoreError::NotADirectory(_)), "{err:?}");
    }

    #[test]
    fn postgres_placeholders_are_numbered_from_one() {
        assert_eq!(
            Store::bind_sql("a = ? AND b IN (?, ?) AND c = ?"),
            "a = $1 AND b IN ($2, $3) AND c = $4"
        );
        assert_eq!(Store::bind_sql("no markers"), "no markers");
    }

    #[tokio::test]
    async fn the_index_needs_a_database_url() {
        // Guard the environment rather than relying on it being unset.
        let prev = std::env::var("DATABASE_URL").ok();
        unsafe { std::env::remove_var("DATABASE_URL") };
        let err = Store::open_index().await;
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("DATABASE_URL", v);
            }
        };
        assert!(matches!(err, Err(StoreError::NoDatabaseUrl)));
    }
}
