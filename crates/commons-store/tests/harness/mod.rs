//! The two-engine test harness, shared by the parity test files.
//!
//! §3.5 makes "the same behaviour on SQLite and on Postgres" a property of the
//! product, so the tests have to run against both and compare. That harness was
//! written once in `search_parity.rs` and then copied into `fuzzy.rs`, which is
//! exactly the arrangement that lets two copies disagree — and a disagreement
//! between two copies of a harness is indistinguishable from a disagreement
//! between the two engines. So it lives here and is included by path.
//!
//! Included rather than imported as a module because a `tests/common/mod.rs`
//! is compiled as its own test binary by `cargo test`, and a harness binary
//! with no tests in it is noise in every run's output. `#[path]` keeps it a
//! header of the file that uses it.
//!
//! # The Postgres migration tree cannot build a schema from nothing
//!
//! [`postgres_store`] works around two pre-existing defects, and the reasoning
//! is preserved here because it is the only place it is written down:
//!
//!   1. `0001_core.sql` creates `performer_alias` with a foreign key to
//!      `producer` and creates `producer` five statements later. SQLite does not
//!      check foreign keys until a write, so the SQLite tree has always applied
//!      cleanly. Postgres checks at `CREATE TABLE`.
//!   2. `0002_appearance_nullable_cluster.sql` creates `appearance_cluster_idx`
//!      and `appearance_ambiguous_idx` with names `0001` already used. The
//!      SQLite mirror rebuilds the table instead, so SQLite never collides.
//!
//! Both are worked around here rather than by editing the files: both
//! migrations are applied and immutable, and the plan's migration rules say a
//! fix is a new migration, not a reorder. **A new migration cannot fix these** —
//! the failure happens while the old ones run — so the fix has to be in the
//! runner, and the follow-up is recorded under T-P5-001 in
//! `docs/plans/implementation-plan.md`.
//!
//! What this harness does *not* prove: that the tree applies for the right
//! reasons. It proves the tree applies. A migration that applies for the wrong
//! reason still applies.

#![allow(dead_code)]

use commons_store::Store;
use sqlx::postgres::PgPool;
use uuid::Uuid;

/// `producer` in the exact shape `0001` would have created it, so nothing
/// downstream can tell the difference.
pub const PRODUCER_STUB: &str = "CREATE TABLE IF NOT EXISTS producer (\
  id            TEXT PRIMARY KEY,\
  kind          TEXT NOT NULL DEFAULT 'unknown',\
  name          TEXT NOT NULL,\
  career_start  TEXT,\
  career_end    TEXT,\
  defunct       INTEGER NOT NULL DEFAULT 0,\
  created_at    TEXT NOT NULL,\
  updated_at    TEXT NOT NULL)";

/// The indexes `0001` creates that `0002` re-creates under the same name.
pub fn index_drops() -> Vec<String> {
    [
        "DROP INDEX IF EXISTS appearance_cluster_idx",
        "DROP INDEX IF EXISTS appearance_ambiguous_idx",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Apply the migration tree, honouring the `--:sqlite <name>` sidecar markers
/// the way the real migrator does, with the two `0001` defects worked around.
///
/// Each file is run as one multi-statement script through a *simple* protocol,
/// which is what lets several statements travel in one round trip. That is a
/// deliberate choice over splitting on `;` in Rust: a real SQL splitter has to
/// understand dollar-quoting, string literals and `$$` bodies, and a
/// hand-rolled one is wrong in a way that only shows up on a file nobody
/// happens to be reading. The sidecar markers are stripped first, because
/// running SQLite's `ALTER TABLE ... ADD CONSTRAINT` is the exact statement
/// SQLite cannot do -- and that was the first false positive this harness
/// produced.
pub async fn apply_migrations(conn: &PgPool) {
    let dir = format!("{}/migrations/postgres", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".sql") && !n.ends_with(".sqlite.sql"))
        .collect();
    names.sort();

    for name in names {
        let text = std::fs::read_to_string(format!("{dir}/{name}")).unwrap();
        let mut script = strip_sidecars(&text);
        // `producer` comes from the stub, so 0001's own `CREATE TABLE` is
        // elided. Done as a string removal rather than by skipping a parsed
        // statement, so the rest of the file is untouched and the edit is
        // visible in the diff of what ran.
        if name.starts_with("0001") {
            script = remove_create_table(&script, "producer");
        }
        // The two index names 0001 already used. Both, not one: the first
        // version looped with a `break`, dropped `appearance_cluster_idx`,
        // and then failed on `appearance_ambiguous_idx` -- which is the shape
        // of a fix that looks applied and is not.
        if name.starts_with("0002") {
            for idx in ["appearance_cluster_idx", "appearance_ambiguous_idx"] {
                script = format!("DROP INDEX IF EXISTS {idx};\n{script}");
            }
        }
        sqlx::raw_sql(&script)
            .execute(conn)
            .await
            .unwrap_or_else(|e| panic!("applying {name}:\n  {e}"));
    }
}

/// Remove one `CREATE TABLE <name> (...)` statement, up to its closing `;`.
///
/// Offset-based so the caller can see exactly which statement was dropped in
/// the failure message if the tree changes shape.
pub fn remove_create_table(script: &str, table: &str) -> String {
    let needle = format!("CREATE TABLE {table} (");
    let Some(start) = script.find(&needle) else {
        return script.to_string();
    };
    // The statement ends at the first `;` at or after the opening paren. A `;`
    // inside a string literal would end it early; none of these files have one
    // inside a `CREATE TABLE`, and the panic below is how that assumption is
    // checked rather than assumed.
    let rel_end = script[start..]
        .find(';')
        .expect("CREATE TABLE with no terminator");
    let end = start + rel_end + 1;
    format!(
        "{}-- (CREATE TABLE {table} elided: provided by the test stub)\n{}",
        &script[..start],
        &script[end..]
    )
}

/// Remove the `--:sqlite <name> ... --:end` blocks.
///
/// The markers are how one migration carries two engine-specific forms: the
/// sidecar file is substituted for the block on SQLite, and the block is
/// dropped on Postgres.
/// Strip the `--:sqlite <sidecar>` marker lines, keeping the statement between.
///
/// The markers bracket a *pair*: the sidecar path, then the statement Postgres
/// runs, then the closer. So the marker and the closer go and the statement
/// stays — the harness is running the Postgres tree.
///
/// The first version dropped the whole block, statement included, and that was
/// wrong for every block except one: `0017` needs its two `ALTER ... DROP
/// CONSTRAINT` to run, and dropping them left `search_term` with a foreign key
/// to `object(id)`, so a tag could not be indexed. The error named the
/// constraint, which is how it was found.
///
/// `0006` is the one block whose statement is a verbatim *duplicate* of one
/// stated outside the block — `0006_attr_types_alters.sqlite.sql` opens with
/// "Nothing to run" for exactly that reason, and the same `ADD COLUMN
/// created_at` sits above the marker. A faithful `strip_sidecars` must not run
/// it twice.
///
/// `0006` is applied and immutable, so the duplicate cannot be deleted from it.
/// It is detected rather than named: a block statement that is byte-identical to
/// a statement already in the file is a duplicate by definition, and dropping it
/// is right whatever the file is called. That also means a *future* migration
/// that copies a statement into a block by accident gets the same repair for
/// free, which is a better property than a list of known-bad filenames.
pub fn strip_sidecars(text: &str) -> String {
    // Whitespace-collapsed text, so a statement wrapped over two lines compares
    // equal to the same statement on one. The first version compared line by
    // line, which meant a two-line `ALTER TABLE x` + `ADD COLUMN y` could never
    // match its own copy -- and the case this function exists for is a *wrapped*
    // statement, so the check was never true when it was needed.
    let key = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut out: Vec<String> = Vec::new();
    let mut in_block = false;
    let mut block: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with("--:sqlite") {
            in_block = true;
            block.clear();
            continue;
        }
        if line.trim() == "--:end" {
            in_block = false;
            let joined = block.join("\n");
            // Compared against the whole text seen so far, not line by line:
            // the duplicate is a *statement*, which can be several lines, and
            // `out` holds lines. The first version compared per line, so a
            // two-line statement never matched anything and the duplicate was
            // always kept -- which is what kept 0006 failing.
            let seen = key(&out.join("\n"));
            let already = key(&joined).is_empty() || seen.contains(&key(&joined));
            if !already {
                out.push(joined);
            }
            block.clear();
            continue;
        }
        if in_block {
            block.push(line.to_string());
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n")
}

// ------------------------------------------------------------------ engines

/// A private schema on the index engine, so two tests cannot see each other.
///
/// `search_path` is a connection setting, so a `SET` on one pooled connection
/// does not reach the next: the store is opened at a URL carrying the schema
/// rather than configured after the fact. Each test gets its own, because the
/// corpus is written twice and a shared table would make the second run depend
/// on whichever test ran first.
///
/// # `producer` has to exist first, and that is a real finding
///
/// Migration 0001 creates `performer_alias` with a foreign key to `producer`
/// and creates `producer` five statements later. SQLite tolerates the forward
/// reference — it does not check foreign keys until a write — so the SQLite
/// migration tree has always applied cleanly. **Postgres does not**, and
/// migration 0001 has therefore never been applied to an empty Postgres
/// database; every existing index was built by a run that got past it some
/// other way.
///
/// 0001 is applied and immutable (§ the plan's migration rules), so the fix is
/// not to reorder it. The fix is to create `producer` in the private schema
/// before migrating, which is the state every real deployment is in, and to
/// leave the ordering bug for a migration that can address it without editing
/// history. See `docs/plans/implementation-plan.md`, T-P5-001.
/// A semaphore held for the whole of a Postgres schema build.
///
/// The server is shared and small. Every test that calls [`postgres_store`]
/// creates a fresh schema and applies the *entire* migration tree to it, and
/// `0001_core.sql` is large enough that a dozen concurrent applications exhaust
/// the server's shared memory — which surfaces as
/// `applying 0001_core.sql: error returned from database: out of shared memory`
/// and looks, from the failure alone, exactly like a broken migration.
///
/// It was never a broken migration, and it is not rare: this ran green in
/// isolation for months and failed in the full run roughly one time in three,
/// which is the worst possible failure shape. The tests that fail are whichever
/// ones lost the race, so the same test fails on one run and passes on the next
/// and the report is a list of unrelated storage tests.
///
/// A `tokio::sync::Mutex`, not a `std::sync::Mutex`: it is held across `.await`
/// points, and clippy is right that the blocking one is unsound there. It is
/// held only for the *build* — `postgres_store` drops it on return, so the tests
/// themselves still run in parallel and only the migration applies queue up.
static PG_MIGRATION_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Take the gate, waiting however long it takes.
///
/// A future, not a guard: the call site is `let _slot = pg_migration_slot().await`,
/// and the guard is bound to the returned future's parent scope. An `async fn`
/// that returned the guard would tie the guard's lifetime to the future, which
/// is the shape that does not compile.
fn pg_migration_slot() -> impl std::future::Future<Output = tokio::sync::MutexGuard<'static, ()>> {
    PG_MIGRATION_GATE.lock()
}

pub async fn postgres_store() -> Store {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        panic!(
            "DATABASE_URL must be set and reachable. §3.5 makes two engines a \
             property of the product, so a parity test that skips when the \
             database is absent is a parity test that never runs -- and the \
             whole ticket is the equality. scripts/verify.sh sets it."
        )
    });
    let _slot = pg_migration_slot().await;
    let admin = PgPool::connect(&url).await.unwrap();
    let schema = format!("search_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;

    // `search_path` is a connection setting, so it goes in the URL: a `SET` on
    // one pooled connection does not reach the next.
    let scoped = if url.contains('?') {
        format!("{url}&options=-csearch_path%3D{schema}")
    } else {
        format!("{url}?options=-csearch_path%3D{schema}")
    };

    // The migration tree cannot build a Postgres schema from nothing yet, and
    // this is the workaround rather than the fix. Two defects, both pre-existing
    // and both invisible until something tried to migrate an *empty* Postgres
    // database -- which nothing had, because the parity test is the first code
    // that asks for one:
    //
    //   1. `0001_core.sql` creates `performer_alias` with a foreign key to
    //      `producer` and creates `producer` five statements later. SQLite does
    //      not check foreign keys until a write, so the SQLite tree has always
    //      applied cleanly. Postgres checks at `CREATE TABLE`.
    //   2. `0002_appearance_nullable_cluster.sql` creates `appearance_cluster_idx`
    //      and `appearance_ambiguous_idx` with names `0001` already used. The
    //      SQLite mirror rebuilds the table instead, so SQLite never collides.
    //
    // Applied here rather than by editing the files: both migrations are applied
    // and immutable, and § the plan's migration rules say a fix is a new
    // migration, not a reorder. The follow-up is recorded under T-P5-001 in
    // `docs/plans/implementation-plan.md`.
    let conn = PgPool::connect(&scoped).await.unwrap();
    sqlx::query(PRODUCER_STUB).execute(&conn).await.unwrap();
    for stmt in index_drops() {
        sqlx::query(&stmt).execute(&conn).await.unwrap();
    }
    apply_migrations(&conn).await;
    conn.close().await;
    // Connect only. `open_index_url` would run the migrator, which does not
    // recognise a schema it did not create and fails on the first
    // `CREATE TABLE`.
    Store::connect_index_url(&scoped).await.unwrap()
}

pub async fn sqlite_store() -> Store {
    Store::open_memory().await.unwrap()
}
