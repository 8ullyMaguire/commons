//! Sort and keyset pagination against a real database, on both engines.
//!
//! T-P5-006 item 8. Spec §10.4 (§4 of `docs/spec/t-p5-006-view-modes.md`).
//!
//! # Why this file exists at all
//!
//! `Store::query` hard-coded `ORDER BY o.date DESC, o.id` and took no offset, so
//! there was no sort to get wrong and no cursor to be non-total. The ticket names
//! "secondary-sort correctness", which presumes a sort already exists. The
//! honest order of work is therefore the sort first, and it must be built as a
//! *tuple* from the start, because retrofitting a second key onto a single-key
//! keyset cursor is the change that rewrites the predicate.
//!
//! # The three things only a real database can fail
//!
//! * **The ORDER BY is total.** A sort with ties and no trailing `id` produces
//!   two different orders for the same data, which is invisible in a unit test
//!   of a sort parser and catastrophic in a cursor.
//! * **The cursor predicate agrees with the ORDER BY.** These are two separate
//!   SQL fragments that must describe the same order. Getting them subtly
//!   different is the classic keyset bug: pages that overlap at one boundary and
//!   skip at another.
//! * **Row-value comparison is not portable.** Postgres compares `(a, b) > (?, ?)`
//!   as a tuple. SQLite does not have row values at all, so the explicit
//!   three-way spelling is required — and the same trap the undo CTE walked into
//!   recurs here.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_store::db::Store;
use commons_store::filter_ast::{CallerId, Filter};
use commons_store::sort::{Sort, SortKey, SortOrder};

macro_rules! on_each_store {
    (|$store:ident| $body:block) => {{
        for $store in [sqlite_store().await, postgres_store().await] {
            $body
        }
    }};
}

/// Every fixture write goes through this: the two engines' result types share no
/// variant a caller can name, so a block yielding `()` is what reconciles.
macro_rules! run_write {
    ($store:expr, $q:expr, $($b:expr),* $(,)?) => {{
        let sql: &str = $q;
        macro_rules! go {
            ($p:expr, $query:expr) => {{
                let mut qb = sqlx::query($query);
                $( qb = qb.bind($b); )*
                qb.execute($p).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
            }};
        }
        match $store {
            Store::Sqlite(p) => go!(p, sql),
            Store::Postgres(p) => {
                let bound = Store::bind_sql(sql);
                go!(p, &bound)
            }
        }
    }};
}

/// A consented object with the given id, date and rating.
///
/// `seed` names the id, so a fixture never writes a row another test's
/// assertions depend on: the shared ambient row is how a test ends up asserting
/// a value some other suite can set.
async fn seed_object(store: &Store, seed: &str, date: &str, rating: i32) -> String {
    let id = format!("o-{seed}");
    let t = commons_core::ts::now();
    run_write!(
        store,
        "INSERT INTO object (id, kind, title, date, rating_sum, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        id.clone(),
        "clip",
        format!("{seed}-obj"),
        date,
        rating,
        t.clone(),
        t.clone()
    );
    // A consent record per row, because `query` INNER JOINs `consent_record`:
    // a row without one is invisible, which looks exactly like a sort bug and
    // is not one. Each fixture gets its own record rather than a shared one.
    //
    // Two columns that are easy to get wrong and cost a whole debugging round:
    // `id` is the PRIMARY KEY and NOT NULL, so a record without one is a
    // constraint failure rather than an invisible row; and the tier must be a
    // real tier NAME. I wrote "public", which is not a tier -- the public tiers
    // are `self_published`, `performer_claimed`, `third_party_permitted`
    // (`ConsentTiers::PUBLIC`). A made-up tier satisfies no CHECK, matches no
    // clause, and produces zero rows with no error anywhere.
    run_write!(
        store,
        "INSERT INTO consent_record (id, object_id, tier, redistribution_permitted, updated_at)
         VALUES (?, ?, ?, ?, ?)",
        format!("cr-{seed}"),
        id.clone(),
        "self_published",
        1i32,
        t
    );
    id
}

/// The default sort: newest first.
fn date_desc() -> Sort {
    Sort::new(vec![(SortKey::Date, SortOrder::Desc)])
}

#[tokio::test]
async fn a_sort_with_ties_is_total_and_breaks_them_by_id() {
    on_each_store!(|store| {
        // Six objects in three groups of two, and the two groups of a tie carry
        // DIFFERENT ratings. A total order has to place all six the same way on
        // every call and on both engines. Without the trailing `id`, the two
        // rows inside each tie have no defined relative order, so the group is
        // emitted in whatever order the query plan produced -- which differs
        // between SQLite and Postgres, and between plans on the same engine.
        for (i, (date, rating)) in [
            ("2026-01-01", 1),
            ("2026-01-01", 2),
            ("2026-01-02", 3),
            ("2026-01-02", 4),
            ("2026-01-03", 5),
            ("2026-01-03", 6),
        ]
        .iter()
        .enumerate()
        {
            seed_object(&store, &format!("total-{i}"), date, *rating).await;
        }

        let ids_of = |rows: Vec<commons_store::query::ObjectRow>| -> Vec<String> {
            rows.into_iter()
                .filter(|r| r.id.starts_with("o-total-"))
                .map(|r| r.id)
                .collect()
        };

        let first = ids_of(
            store
                .query_sorted(&Filter::All, &caller(), date_desc(), None, 50)
                .await
                .expect("query")
                .rows,
        );
        let second = ids_of(
            store
                .query_sorted(&Filter::All, &caller(), date_desc(), None, 50)
                .await
                .expect("query again")
                .rows,
        );

        assert_eq!(first.len(), 6, "every seeded row is visible");
        assert_eq!(first, second, "a tied sort gives the same order twice");
        // Newest first, so the 03 group leads, and inside each group the two rows
        // are ordered by id because nothing else separates them.
        assert_eq!(
            first,
            vec![
                "o-total-4",
                "o-total-5", // 2026-01-03
                "o-total-2",
                "o-total-3", // 2026-01-02
                "o-total-0",
                "o-total-1", // 2026-01-01
            ],
            "date DESC, then id"
        );
    });
}

#[tokio::test]
async fn a_secondary_key_orders_inside_the_ties() {
    on_each_store!(|store| {
        for (i, (date, rating)) in [
            ("2026-01-01", 1),
            ("2026-01-02", 1),
            ("2026-01-02", 5),
            ("2026-01-02", 3),
        ]
        .iter()
        .enumerate()
        {
            seed_object(&store, &format!("sec-{i}"), date, *rating).await;
        }

        // Date descending, and INSIDE each date, rating descending. The whole
        // point: without the secondary key the three 2026-01-02 rows are in id
        // order, so this is a different order from the primary sort alone.
        let sort = Sort::new(vec![
            (SortKey::Date, SortOrder::Desc),
            (SortKey::RatingSum, SortOrder::Desc),
        ]);
        let ids: Vec<String> = store
            .query_sorted(&Filter::All, &caller(), sort, None, 50)
            .await
            .expect("query")
            .rows
            .iter()
            .map(|r| r.id.clone())
            .collect();

        // Filter to just this fixture: another suite's rows must not change the
        // assertion, which is the shared-ambient-row trap.
        let mine: Vec<String> = ids
            .iter()
            .filter(|id| id.starts_with("o-sec-"))
            .cloned()
            .collect();
        assert_eq!(mine.len(), 4);
        // DESC means NEWEST FIRST, so the 01-02 group leads -- and inside it
        // rating_sum DESC gives 5, 3, 1. The lone 01-01 row is last.
        //
        // I wrote this backwards first (`o-sec-0` first, on the reasoning that
        // "the oldest date is last under DESC" should therefore be asserted
        // about the head of the list). The message was right and the position
        // was not: the assertion about the oldest row belongs at the TAIL. The
        // test failing is how I found it -- the whole order was inverted in my
        // head, not just one index.
        assert_eq!(
            mine,
            ["o-sec-2", "o-sec-3", "o-sec-1", "o-sec-0"],
            "date DESC (newest first), rating_sum DESC inside the tie"
        );
    });
}

#[tokio::test]
async fn a_page_boundary_splits_the_ties_without_repeating_or_skipping() {
    on_each_store!(|store| {
        // Ten rows, all sharing ONE date and one rating. Every row is a tie, so
        // the trailing `id` is the only thing making the order total -- and the
        // only thing that can paginate them without loss. A page of 3 across a
        // set of 10 identical keys is the exact case that shows up as a repeated
        // or missing row.
        for i in 0..10 {
            seed_object(&store, &format!("page-{i:02}"), "2026-05-05", 3).await;
        }

        let mut seen: Vec<String> = Vec::new();
        let mut cursor = None;
        for _ in 0..10 {
            let page = store
                .query_sorted(&Filter::All, &caller(), date_desc(), cursor.clone(), 3)
                .await
                .expect("query");
            if page.rows.is_empty() {
                break;
            }
            for r in &page.rows {
                if r.id.starts_with("o-page-") {
                    seen.push(r.id.clone());
                }
            }
            if !page.has_more {
                break;
            }
            cursor = page.next_cursor();
        }

        seen.sort();
        let mut expected: Vec<String> = (0..10).map(|i| format!("o-page-{i:02}")).collect();
        expected.sort();
        assert_eq!(
            seen, expected,
            "paging through 10 identical keys at 3 per page must yield each exactly once"
        );
    });
}

/// A caller that can see the seeded rows' tier.
fn caller() -> CallerId {
    CallerId::anonymous()
}
