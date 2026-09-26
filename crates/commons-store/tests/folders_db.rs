//! Folders against a real database, on both engines.
//!
//! T-P5-006 item 6. Spec §5.14, §9.5. Companion to `folders.rs`, which tests
//! the resolver over a literal map. This file tests the *schema*, and the
//! distinction matters:
//!
//! `folders.rs` proves the rewrite is correct. It cannot prove the table exists,
//! that the partial index covers the roots, or that the cycle trigger fires — all
//! of which are migration behaviour, and a migration that silently fails to
//! create its constraint is a migration that passes every other test in the
//! repository.
//!
//! The harness's own header says it plainly: "What this harness does *not* prove:
//! that the tree applies for the right reasons. It proves the tree applies. A
//! migration that applies for the wrong reason still applies." So the assertions
//! here are about what the schema *does* — insert a cycle, read the roots back,
//! try two folders with the same name — rather than about whether the file ran.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_core::ts::now;
use commons_store::db::Store;
// `Row` for `.get::<String, _>` on both `SqliteRow` and `PgRow` — the trait has to
// be in scope for the macro's expansion to see it, and the macro is the only
// place in this file that touches a row.
use sqlx::Row;

fn ts() -> (String, String) {
    let t = now();
    (t.clone(), t)
}

/// Run a block against both engines.
///
/// # Why a macro and not ten `match`es
///
/// `sqlx` is generic over its backend and the two result types are *different*
/// Rust types: `SqliteQueryResult` and `PgQueryResult` share no variant a caller
/// can name, and neither is `()`. So any
/// `match store { Sqlite(p) => .., Postgres(p) => .. }` that lets a query result
/// escape fails to compile, and the fix is `.map(|_| ())` in both arms, by hand,
/// every time. This file had four such matches and three type errors before the
/// macro existed.
///
/// The macro does not fix that on its own: a closure returning
/// `Result<PgQueryResult, _>` is still not a `Result<SqliteQueryResult, _>`. The
/// bound has to be on the *output*, which is why this takes a block rather than
/// an expression — the block's value is `()`, so there is no type to reconcile.
/// That is also the right shape for the assertions, which are about "the write
/// happened" or "the write was refused" and never about which engine answered.
macro_rules! on_each_store {
    (|$store:ident| $body:block) => {{
        for $store in [sqlite_store().await, postgres_store().await] {
            $body
        }
    }};
}

// ---------------------------------------------------------------- writes

/// Insert, returning `Ok` or `Err` — for the assertions that *expect* a refusal.
///
/// The error is discarded deliberately. A test needs to know the write was
/// refused, not why, and the two engines word it differently; asserting on the
/// message would make this file a test of two error formats.
async fn insert_result(
    store: &Store,
    id: &str,
    name: &str,
    parent: Option<&str>,
    pos: i32,
) -> Result<(), ()> {
    let (c, u) = ts();
    let filter = r#"{"all":null}"#;
    // Eight binds, but the sixth column is the literal `0` (notify), so the
    // placeholders are 1..5 then 6,7 — seven of them, and Postgres needs each
    // *numbered*. Writing `$ph` seven times gives `VALUES ($, $, $, ...)`,
    // which is a syntax error rather than a wrong result, so the caller passes
    // the numbered set and the two engines differ in the arguments rather than
    // in the query text.
    macro_rules! insert {
        ($p:expr, $a:literal, $b:literal, $c:literal, $d:literal, $e:literal, $f:literal, $g:literal) => {
            sqlx::query(concat!(
                "INSERT INTO saved_filters (id, name, filter, parent_id, position, notify, created_at, updated_at) ",
                "VALUES (", $a, ", ", $b, ", ", $c, ", ", $d, ", ", $e, ", 0, ", $f, ", ", $g, ")"
            ))
            .bind(id.to_string())
            .bind(name.to_string())
            .bind(filter.to_string())
            .bind(parent.map(str::to_string))
            .bind(pos)
            .bind(c.clone())
            .bind(u.clone())
            .execute($p)
            .await
            .map(|_| ())
            .map_err(|_| ())
        };
    }
    match store {
        Store::Sqlite(p) => insert!(p, "?", "?", "?", "?", "?", "?", "?"),
        Store::Postgres(p) => insert!(p, "$1", "$2", "$3", "$4", "$5", "$6", "$7"),
    }
}

async fn insert_folder(store: &Store, id: &str, name: &str, parent: Option<&str>, pos: i32) {
    insert_result(store, id, name, parent, pos)
        .await
        .expect("a valid folder must insert");
}

/// Point a folder at a new parent — the operation the cycle trigger exists for.
async fn reparent(store: &Store, id: &str, parent: Option<&str>) -> Result<(), ()> {
    macro_rules! go {
        ($p:expr, $ph:literal, $n:literal) => {
            sqlx::query(concat!(
                "UPDATE saved_filters SET parent_id = ",
                $ph,
                " WHERE id = ",
                $n
            ))
            .bind(parent.map(str::to_string))
            .bind(id.to_string())
            .execute($p)
            .await
            .map(|_| ())
            .map_err(|_| ())
        };
    }
    match store {
        Store::Sqlite(p) => go!(p, "?", "?"),
        Store::Postgres(p) => go!(p, "$1", "$2"),
    }
}

async fn delete(store: &Store, id: &str) {
    macro_rules! go {
        ($p:expr, $ph:literal) => {
            sqlx::query(concat!("DELETE FROM saved_filters WHERE id = ", $ph))
                .bind(id.to_string())
                .execute($p)
                .await
                .map(|_| ())
                .map_err(|_| ())
        };
    }
    let _ = match store {
        Store::Sqlite(p) => go!(p, "?"),
        Store::Postgres(p) => go!(p, "$1"),
    };
}

// ---------------------------------------------------------------- reads

/// How many folders have this id.
async fn count(store: &Store, id: &str) -> i64 {
    macro_rules! go {
        ($p:expr, $ph:literal) => {
            sqlx::query_scalar::<_, i64>(concat!(
                "SELECT COUNT(*) FROM saved_filters WHERE id = ",
                $ph
            ))
            .bind(id.to_string())
            .fetch_one($p)
            .await
            .expect("count")
        };
    }
    match store {
        Store::Sqlite(p) => go!(p, "?"),
        Store::Postgres(p) => go!(p, "$1"),
    }
}

/// `i32`, not `i64`: the column is `INTEGER`, which Postgres stores as INT4 and
/// SQLite as INT8, and `i64` decodes only the latter. A decode mismatch here
/// reads as a type error, not as a wrong answer, so it is worth stating.
async fn notify_flag(store: &Store, id: &str) -> i32 {
    macro_rules! go {
        ($p:expr, $ph:literal) => {
            sqlx::query_scalar::<_, i32>(concat!(
                "SELECT notify FROM saved_filters WHERE id = ",
                $ph
            ))
            .bind(id.to_string())
            .fetch_one($p)
            .await
            .expect("notify")
        };
    }
    match store {
        Store::Sqlite(p) => go!(p, "?"),
        Store::Postgres(p) => go!(p, "$1"),
    }
}

/// The root listing, in the order `folders::order_by_position_sql` specifies.
///
/// The `ORDER BY` is written out here rather than shared with the module, and
/// that is deliberate: if this test called the constant, a change to the
/// constant would change the query and the test would go on passing while
/// testing the new behaviour. The point of a schema test is to check the schema
/// against an expectation stated somewhere else.
async fn roots_in_order(store: &Store) -> Vec<String> {
    // Each arm maps to `Vec<String>` before the match ends, because
    // `Vec<SqliteRow>` and `Vec<PgRow>` are unrelated types. The mapping is
    // identical, so the only thing the macro cannot help with here is the row
    // type itself — and the fix is to not let it out of the arm, which is the
    // same discipline as `.map(|_| ())` on a write.
    macro_rules! go {
        ($p:expr) => {
            sqlx::query(
                "SELECT name FROM saved_filters WHERE parent_id IS NULL
                 ORDER BY position ASC, id ASC",
            )
            .fetch_all($p)
            .await
            .expect("roots query")
            .into_iter()
            .map(|r| r.get::<String, _>(0))
            .collect::<Vec<String>>()
        };
    }
    match store {
        Store::Sqlite(p) => go!(p),
        Store::Postgres(p) => go!(p),
    }
}

// ---------------------------------------------------------------- the schema

#[tokio::test]
async fn the_table_exists_and_a_folder_round_trips() {
    on_each_store!(|store| {
        insert_folder(&store, "f1", "Beach", None, 0).await;
        assert_eq!(
            count(&store, "f1").await,
            1,
            "0018 must create saved_filters"
        );
    });
}

#[tokio::test]
async fn roots_are_unique_by_name() {
    // The reason 0018 has a *partial* index. A plain `UNIQUE (parent_id, name)`
    // treats NULL as distinct in both engines, so without the partial index two
    // root folders can share a name and the sidebar shows two identical entries
    // with no way to tell them apart.
    on_each_store!(|store| {
        insert_folder(&store, "r1", "Favourites", None, 0).await;
        let second = insert_result(&store, "r2", "Favourites", None, 0).await;
        assert!(
            second.is_err(),
            "two root folders named 'Favourites' must not both insert"
        );
    });
}

#[tokio::test]
async fn siblings_are_unique_by_name() {
    on_each_store!(|store| {
        insert_folder(&store, "p", "Parent", None, 0).await;
        insert_folder(&store, "c1", "Child", Some("p"), 0).await;
        let second = insert_result(&store, "c2", "Child", Some("p"), 1).await;
        assert!(
            second.is_err(),
            "two siblings named 'Child' must not both insert"
        );
    });
}

#[tokio::test]
async fn a_name_may_repeat_across_different_parents() {
    // Uniqueness is scoped to siblings, not global: two unrelated parts of a
    // library may each have a "Favourites", and a global UNIQUE would make the
    // second one impossible to create.
    on_each_store!(|store| {
        insert_folder(&store, "a", "A", None, 0).await;
        insert_folder(&store, "b", "B", None, 1).await;
        insert_folder(&store, "a1", "Favourites", Some("a"), 0).await;
        insert_folder(&store, "b1", "Favourites", Some("b"), 0).await;
    });
}

#[tokio::test]
async fn a_folder_cannot_be_its_own_parent() {
    on_each_store!(|store| {
        // Insert as a root, then try to make it its own parent. An UPDATE
        // rather than an INSERT, because the insert form is only reachable with
        // a self-referencing row, which the primary key would reject first.
        insert_folder(&store, "s", "Self", None, 0).await;
        let r = reparent(&store, "s", Some("s")).await;
        assert!(r.is_err(), "a folder must not become its own parent");
    });
}

#[tokio::test]
async fn a_folder_cannot_be_reparented_under_its_own_descendant() {
    // The case that actually happens: dragging a folder onto its own child. A
    // direct self-parent is the easy one to catch and the rare one.
    on_each_store!(|store| {
        insert_folder(&store, "p", "Parent", None, 0).await;
        insert_folder(&store, "c", "Child", Some("p"), 0).await;
        let r = reparent(&store, "p", Some("c")).await;
        assert!(r.is_err(), "p -> c -> p must be refused");
    });
}

#[tokio::test]
async fn a_missing_parent_is_refused_rather_than_creating_an_orphan() {
    on_each_store!(|store| {
        let r = insert_result(&store, "o", "Orphan", Some("nope"), 0).await;
        assert!(
            r.is_err(),
            "a folder with a non-existent parent must not insert"
        );
    });
}

#[tokio::test]
async fn siblings_come_back_in_position_order() {
    on_each_store!(|store| {
        // Inserted in reverse alphabetical order on purpose, so an `ORDER BY
        // name` would fail this and only `ORDER BY position` passes.
        insert_folder(&store, "z", "Zebra", None, 0).await;
        insert_folder(&store, "a", "Apple", None, 1).await;
        let names = roots_in_order(&store).await;
        assert_eq!(
            names,
            vec!["Zebra".to_string(), "Apple".to_string()],
            "position, not the alphabet: a tree that re-sorts on rename is \
             re-ordering under the cursor"
        );
    });
}

#[tokio::test]
async fn deleting_a_parent_cascades_to_its_children() {
    // A tree with a hole in it is worse than a shorter tree: the breadcrumb for
    // the child names a parent that is not there.
    on_each_store!(|store| {
        insert_folder(&store, "p", "Parent", None, 0).await;
        insert_folder(&store, "c", "Child", Some("p"), 0).await;
        delete(&store, "p").await;
        assert_eq!(
            count(&store, "c").await,
            0,
            "the child went with the parent"
        );
    });
}

#[tokio::test]
async fn notify_defaults_off() {
    on_each_store!(|store| {
        insert_folder(&store, "n", "Noisy", None, 0).await;
        assert_eq!(
            notify_flag(&store, "n").await,
            0,
            "a subscription on by default is a job per folder"
        );
    });
}

#[tokio::test]
async fn a_filter_cannot_be_null() {
    // `filter` is NOT NULL, and the reason is the dangerous direction: a NULL
    // reaching the query means "no constraint", which is `1 = 1` -- a folder
    // that silently matches the whole library. An empty result would be the
    // safe failure, which is why the column is NOT NULL rather than defaulting
    // to an empty object.
    on_each_store!(|store| {
        let (c, u) = ts();
        let ok = match &store {
            Store::Sqlite(p) => sqlx::query(
                "INSERT INTO saved_filters (id, name, filter, position, created_at, updated_at)
                 VALUES ('nf', 'NullFilter', NULL, 0, ?, ?)",
            )
            .bind(c)
            .bind(u)
            .execute(p)
            .await
            .is_ok(),
            Store::Postgres(p) => sqlx::query(
                "INSERT INTO saved_filters (id, name, filter, position, created_at, updated_at)
                 VALUES ('nf', 'NullFilter', NULL, 0, $1, $2)",
            )
            .bind(c)
            .bind(u)
            .execute(p)
            .await
            .is_ok(),
        };
        assert!(!ok, "a folder with no filter must not insert");
    });
}
