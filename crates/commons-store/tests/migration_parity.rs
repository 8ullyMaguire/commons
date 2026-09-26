//! Migration parity: the Postgres and SQLite schemas must describe the same
//! tables and columns (plan §0.4, T-P0-007).
//!
//! One logical schema, two physical engines. This test is what stops a
//! half-remembered migration from leaving the two engines quietly disagreeing,
//! which is the single most likely way this project's portability rule breaks.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn migration_dir(engine: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("migrations")
        .join(engine)
}

fn migration_files(engine: &str) -> Vec<String> {
    let dir = migration_dir(engine);
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".sql"))
        .collect();
    names.sort();
    names
}

fn read(engine: &str, name: &str) -> String {
    fs::read_to_string(migration_dir(engine).join(name))
        .unwrap_or_else(|e| panic!("cannot read {engine}/{name}: {e}"))
}

fn table_names(sql: &str) -> BTreeSet<String> {
    regex::Regex::new(r"(?i)CREATE TABLE (?:IF NOT EXISTS )?(\w+)")
        .unwrap()
        .captures_iter(sql)
        .map(|c| c[1].to_lowercase())
        .collect()
}

fn column_names(sql: &str, table: &str) -> Option<BTreeSet<String>> {
    let pattern = format!(r"(?is)CREATE TABLE {table}\s*\((.*?)\n\);");
    let caps = regex::Regex::new(&pattern).unwrap().captures(sql)?;
    let body = &caps[1];
    // A column definition starts at the beginning of a line with two spaces
    // and is followed by a type. Skip table-level constraints (UNIQUE, CHECK,
    // PRIMARY, FOREIGN), which also start two spaces in but are not columns.
    let col_re = regex::Regex::new(r"(?m)^\s{2}(\w+)\s+\w").unwrap();
    let constraint_re =
        regex::Regex::new(r"(?m)^\s{2}(UNIQUE|PRIMARY|FOREIGN|CHECK|CONSTRAINT)\b").unwrap();
    let mut cols = BTreeSet::new();
    for c in col_re.captures_iter(body) {
        let name = c[1].to_lowercase();
        // Match the constraint keywords against the *whole line* this match
        // started on, not a fixed 20-byte window: a slice past the end of the
        // body panics, and a short window can miss a long keyword.
        let start = c.get(0).unwrap().start();
        let line_end = body[start..]
            .find('\n')
            .map(|off| start + off)
            .unwrap_or(body.len());
        if !constraint_re.is_match(&body[start..line_end]) {
            cols.insert(name);
        }
    }
    Some(cols)
}

fn index_names(sql: &str) -> BTreeSet<String> {
    regex::Regex::new(r"(?i)CREATE (?:UNIQUE )?INDEX (\w+)")
        .unwrap()
        .captures_iter(sql)
        .map(|c| c[1].to_lowercase())
        .collect()
}

#[test]
fn every_migration_has_a_mirror() {
    let pg = migration_files("postgres");
    let lite = migration_files("sqlite");
    assert!(
        !pg.is_empty(),
        "no postgres migrations found; the schema is missing"
    );
    assert_eq!(
        pg, lite,
        "migration file sets differ.\n  postgres: {pg:?}\n  sqlite:   {lite:?}\n\
         Run scripts/sync-migrations.py after editing a postgres migration."
    );
}

#[test]
fn both_engines_describe_the_same_tables_and_columns() {
    for name in migration_files("postgres") {
        let pg = read("postgres", &name);
        let lite = read("sqlite", &name);

        let pg_tables = table_names(&pg);
        let lite_tables = table_names(&lite);
        assert_eq!(
            pg_tables,
            lite_tables,
            "{name}: table sets differ.\n  only in postgres: {:?}\n  only in sqlite:   {:?}",
            pg_tables.difference(&lite_tables).collect::<Vec<_>>(),
            lite_tables.difference(&pg_tables).collect::<Vec<_>>(),
        );

        for table in &pg_tables {
            let a = column_names(&pg, table);
            let b = column_names(&lite, table);
            assert_eq!(
                a, b,
                "{name}: columns of `{table}` differ.\n  postgres: {a:?}\n  sqlite:   {b:?}"
            );
        }

        let pg_idx = index_names(&pg);
        let lite_idx = index_names(&lite);
        assert_eq!(
            pg_idx,
            lite_idx,
            "{name}: index sets differ.\n  only in postgres: {:?}\n  only in sqlite:   {:?}",
            pg_idx.difference(&lite_idx).collect::<Vec<_>>(),
            lite_idx.difference(&pg_idx).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn sqlite_migrations_declare_no_postgres_only_types() {
    // SQLite will happily accept a column type it does not understand, so this
    // is the only guard that a native `uuid` or `timestamptz` has not leaked
    // into the embedded-engine schema where it would not behave identically.
    let banned = [
        "UUID",
        "TIMESTAMPTZ",
        "TIMESTAMP ",
        "SERIAL",
        "BIGSERIAL",
        "JSONB",
        "BYTEA",
    ];
    for name in migration_files("sqlite") {
        let sql = read("sqlite", &name).to_uppercase();
        // Strip comments so prose about types does not trip the check.
        let stripped: String = sql
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        for b in banned {
            assert!(
                !stripped.contains(b),
                "{name}: SQLite migration contains Postgres-only type `{b}`.\n\
                 Use TEXT/INTEGER per plan section 0.4."
            );
        }
    }
}

#[test]
fn no_embedding_columns_anywhere() {
    // Vectors live in a sidecar ANN file (plan section 0.4). A `vector(N)`
    // column in either engine would make a query depend on pgvector and break
    // the embedded engine.
    for engine in ["postgres", "sqlite"] {
        for name in migration_files(engine) {
            let sql = read(engine, &name).to_lowercase();
            let stripped: String = sql
                .lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !stripped.contains("vector("),
                "{engine}/{name}: embedding column found. Vectors belong in the \
                 sidecar ANN file, not in SQL."
            );
        }
    }
}
