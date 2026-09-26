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
        .filter(|n| n.ends_with(".sql") && !n.ends_with(".sqlite.sql"))
        .collect();
    names.sort();
    names
}

/// Every migration for `engine` *up to but not including* `name`.
///
/// Migrations apply in filename order, so this is the schema as it stood before
/// `name` ran -- which is what a file that only *alters* a table has to be
/// compared against, since it creates no `CREATE TABLE` of its own.
fn read_all_upto(engine: &str, name: &str) -> String {
    let mut out = String::new();
    for n in migration_files(engine) {
        if n == name {
            break;
        }
        out.push_str(&read(engine, &n));
        out.push('\n');
    }
    out
}

fn read(engine: &str, name: &str) -> String {
    fs::read_to_string(migration_dir(engine).join(name))
        .unwrap_or_else(|e| panic!("cannot read {engine}/{name}: {e}"))
}

/// Every table a migration file creates.
///
/// One exception, and it is the only one: a SQLite table rebuild creates
/// `foo_new` and then renames it to `foo`. That intermediate name is how one
/// engine spells a change the other spells with an `ALTER`, so counting it as a
/// new table makes a correct migration look like a schema divergence. Dropping
/// it here -- and only here -- leaves every other assertion in this file seeing
/// every real table, so a genuine divergence still fails.
fn table_names(sql: &str) -> BTreeSet<String> {
    regex::Regex::new(r"(?i)CREATE TABLE (?:IF NOT EXISTS )?(\w+)")
        .unwrap()
        .captures_iter(sql)
        .map(|c| c[1].to_lowercase())
        .filter(|name| !is_rebuild_temp_name(name, sql))
        .collect()
}

fn non_empty(s: BTreeSet<String>) -> Option<BTreeSet<String>> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// A column set with `dropped` removed from it.
fn apply_changes(
    cols: Option<BTreeSet<String>>,
    dropped: &BTreeSet<String>,
) -> Option<BTreeSet<String>> {
    cols.map(|c| {
        c.into_iter()
            .filter(|x| !dropped.contains(x))
            .collect::<BTreeSet<_>>()
    })
}

/// Union of two optional column sets.
fn merge(a: Option<BTreeSet<String>>, b: Option<BTreeSet<String>>) -> Option<BTreeSet<String>> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => Some(x),
        (Some(x), Some(y)) => {
            let mut out = x;
            out.extend(y);
            Some(out)
        }
    }
}

/// Columns described by *this* file's own statements, ignoring earlier ones.
///
/// `column_names` needs a `CREATE TABLE` body; a file whose only statement about
/// a table is `ALTER TABLE ... ADD COLUMN` has none, so this variant reads the
/// `ADD`/`DROP` statements directly. Without it, a one-file column addition is
/// compared against a schema that does not contain it on either side and the
/// difference cancels out.
/// Columns this file *adds*, and columns it *drops*, as `(add, drop)`.
///
/// `DROP COLUMN` is the mirror of `ADD COLUMN` and has to be modelled as a
/// removal, not as "no information": a column dropped on one engine and not the
/// other is a divergence, and a reader that only collects additions sees both
/// sides as identical.
fn column_changes(sql: &str, table: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    // `ADD CONSTRAINT` and `ADD PRIMARY KEY` add a *constraint*, not a column,
    // and they are spelled without the `COLUMN` keyword -- which is what makes the
    // `(?:COLUMN\s+)?` dangerous: it captures the word `constraint`, and the
    // parity test then reports a column Postgres has and SQLite does not.
    //
    // Filtered in code rather than with a negative lookahead because the `regex`
    // crate has no look-around, and a keyword list in a `filter` reads more
    // clearly than it would as a lookahead anyway.
    const NOT_A_COLUMN: [&str; 5] = ["constraint", "primary", "foreign", "unique", "check"];
    let add = regex::Regex::new(&format!(
        r"(?i)ALTER\s+TABLE\s+{table}\s+ADD\s+(?:COLUMN\s+)?(\w+)"
    ))
    .expect("a literal with a known shape");
    let drop = regex::Regex::new(&format!(
        r"(?i)ALTER\s+TABLE\s+{table}\s+DROP\s+(?:COLUMN\s+)?(\w+)"
    ))
    .expect("a literal with a known shape");
    let added = add
        .captures_iter(sql)
        .map(|c| c[1].to_lowercase())
        .filter(|w| !NOT_A_COLUMN.contains(&w.as_str()))
        .collect();
    let dropped = drop
        .captures_iter(sql)
        .map(|c| c[1].to_lowercase())
        .collect();
    (added, dropped)
}

/// Every table this file issues a *schema-changing* `ALTER TABLE` against.
///
/// A `RENAME` is excluded: renaming a table does not change what columns it has,
/// and the rebuild's `ALTER TABLE foo_new RENAME TO foo` would otherwise pull the
/// temporary name into the comparison as a table the other engine lacks. What is
/// wanted here is the statement that *widens* a table -- `ADD COLUMN`,
/// `DROP COLUMN`, `ALTER COLUMN` -- because those change the column set.
fn altered_tables(sql: &str) -> BTreeSet<String> {
    let re =
        regex::Regex::new(r"(?i)ALTER\s+TABLE\s+(?:IF\s+EXISTS\s+)?(\w+)\s+(ADD|DROP|ALTER)\b")
            .expect("a literal with a known shape");
    re.captures_iter(sql).map(|c| c[1].to_lowercase()).collect()
}

/// Is `name` a `foo_new` that this same file renames to `foo`?
fn is_rebuild_temp_name(name: &str, sql: &str) -> bool {
    let Some(stem) = name.strip_suffix("_new") else {
        return false;
    };
    // Case-insensitive: the caller lower-cases the name, the file is upper-case.
    regex::Regex::new(&format!(
        r"(?i)ALTER\s+TABLE\s+{name}\s+RENAME\s+TO\s+{stem}\b"
    ))
    .expect("a literal with a known shape")
    .is_match(sql)
}

fn column_names(sql: &str, table: &str) -> Option<BTreeSet<String>> {
    // A rebuilt table's columns live in the `CREATE TABLE foo_new` body. Looking
    // for `foo` alone would find nothing, and the fallback below would then
    // compare the *pre-rebuild* definition on both sides -- so a column the
    // rebuild adds would be invisible on both and cancel out.
    let pattern = format!(r"(?is)CREATE TABLE {table}(?:_new)?\s*\((.*?)\n\);");
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
    // A column added by a later `ALTER TABLE ... ADD COLUMN` is not in the
    // `CREATE TABLE` body, so without this a column added to a table defined in
    // an *earlier* migration is invisible to the comparison. That is not
    // hypothetical: it is exactly the shape of a nullable-column correction,
    // where the table is created in 0001 and widened in 0002.
    let add = regex::Regex::new(&format!(
        r"(?i)ALTER\s+TABLE\s+{table}\s+ADD\s+(?:COLUMN\s+)?(\w+)"
    ))
    .expect("a literal with a known shape");
    for c in add.captures_iter(sql) {
        cols.insert(c[1].to_lowercase());
    }
    Some(cols)
}

fn index_names(sql: &str) -> BTreeSet<String> {
    let created = regex::Regex::new(r"(?i)CREATE (?:UNIQUE )?INDEX (\w+)")
        .unwrap()
        .captures_iter(sql)
        .map(|c| c[1].to_lowercase())
        .collect::<BTreeSet<_>>();
    // An index dropped by this file is not created by it, and a `DROP INDEX` on
    // one engine only is exactly the divergence the index comparison exists to
    // find. Modelled as a removal so it is visible.
    let drop = regex::Regex::new(r"(?i)DROP\s+(?:INDEX|UNIQUE\s+INDEX)\s+(?:IF\s+EXISTS\s+)?(\w+)")
        .unwrap()
        .captures_iter(sql)
        .map(|c| c[1].to_lowercase())
        .collect::<BTreeSet<_>>();
    created.difference(&drop).cloned().collect()
}

#[test]
fn every_migration_has_a_mirror() {
    // `*.sqlite.sql` files are sidecars: the SQLite form of one statement inside
    // a migration, read by scripts/sync-migrations.py and never applied. Counting
    // them as migrations would fail for every migration that needs an
    // engine-specific statement, which is the mechanism working rather than
    // breaking -- and they only exist in the postgres directory, so the
    // comparison below is not symmetric without this filter.
    let pg: Vec<String> = migration_files("postgres")
        .into_iter()
        .filter(|n| !n.ends_with(".sqlite.sql"))
        .collect();
    let lite: Vec<String> = migration_files("sqlite")
        .into_iter()
        .filter(|n| !n.ends_with(".sqlite.sql"))
        .collect();
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

        // `pg_tables` is what *this* file creates. A file may also only *alter*
        // a table an earlier migration created -- a nullable-column correction,
        // for instance -- and then the column comparison has to read the
        // accumulated schema rather than this file alone, or the column it adds
        // is invisible on both sides and a genuine one-sided `ADD COLUMN` is
        // never seen. The fallback is the whole accumulated migration set.
        let pg_before = read_all_upto("postgres", &name);
        let lite_before = read_all_upto("sqlite", &name);
        // Every table this file *touches*: the ones it creates, and the ones
        // it only alters. A file that adds a column to a table created by an
        // earlier migration creates nothing, so iterating only what it creates
        // would skip the very file whose divergence matters most -- and a
        // nullable-column correction has exactly that shape.
        let altered = altered_tables(&pg)
            .into_iter()
            .chain(altered_tables(&lite))
            .collect::<BTreeSet<_>>();
        let mut touched = pg_tables.clone();
        touched.extend(altered);

        // A file that widens a table has no `CREATE TABLE` of its own for it, so
        // the comparison is: every column this file's text describes, which is
        // the accumulated schema *plus* anything this file adds. Reading only
        // "this file, else the previous" hides exactly the columns the file
        // exists to add -- the fallback discards the migration's whole point.
        for table in &touched {
            // Precedence: this file's own definition wins, because a rebuild
            // *replaces* the table rather than adding to it, and reading the
            // previous schema first would silently compare the pre-rebuild
            // columns on both sides and miss a column the rebuild adds or drops.
            let a = column_names(&pg, table).or_else(|| column_names(&pg_before, table));
            let b = column_names(&lite, table).or_else(|| column_names(&lite_before, table));
            // This file's own statements, added on top of whatever the file
            // created, is the third term and the one that sees an `ADD COLUMN`.
            let (a_add, a_drop) = column_changes(&pg, table);
            let (b_add, b_drop) = column_changes(&lite, table);
            let a = apply_changes(merge(a, non_empty(a_add)), &a_drop);
            let b = apply_changes(merge(b, non_empty(b_add)), &b_drop);
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
