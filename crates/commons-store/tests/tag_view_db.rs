//! The tag view's data: every tag with how many objects carry it.
//!
//! T-P5-006 item 8, spec §10.4 (#773 — "display all tags in a single page").
//! Companion to the Playwright spec; this covers the query, which is where the
//! two things that can go wrong live.
//!
//! # Why a count and not just a list
//!
//! `Store::all_tags` already returns every tag in name order. A grid of tags
//! whose tiles show only a name is a list, and the count is what makes it a
//! view: a tag applied to 40,000 objects and a tag applied to 3 are different
//! things, and a person reorganising a library needs to see which is which
//! before clicking.
//!
//! # The two traps
//!
//! **A LEFT JOIN, not an inner one.** An inner join against `object_tag` drops
//! every tag nobody has applied yet, and a tag with zero objects is precisely
//! the one a person needs to see — it is a tag that was created and never used,
//! and it is invisible in a list that only counts. This is the same shape as the
//! "absent and NULL are different" rule in `undo_db.rs`.
//!
//! **A count of a LEFT JOIN is 1 for an unmatched row, not 0.** `COUNT(t.object_id)`
//! is right and `COUNT(*)` is wrong, and the wrong one is off by one on exactly
//! the rows the LEFT JOIN was written to preserve. A test that seeds only tags
//! that HAVE objects cannot see the difference, so this file seeds an unused tag
//! and asserts it is present with a count of zero.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_store::db::Store;

macro_rules! on_each_store {
    (|$store:ident| $body:block) => {{
        for $store in [sqlite_store().await, postgres_store().await] {
            $body
        }
    }};
}

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

/// A tag, `applied` objects carrying it, and `n` objects in total.
async fn seed_tag_with(store: &Store, seed: &str, applied: usize, n: usize) -> String {
    let tag_id = format!("t-{seed}");
    run_write!(
        store,
        "INSERT INTO tag (id, name) VALUES (?, ?)",
        tag_id.clone(),
        format!("{seed}-tag")
    );
    for i in 0..n {
        let t = commons_core::ts::now();
        run_write!(
            store,
            "INSERT INTO object (id, kind, title, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
            format!("o-{seed}-{i}"),
            "clip",
            format!("{seed}-obj-{i}"),
            t.clone(),
            t
        );
    }
    for i in 0..applied {
        run_write!(
            store,
            "INSERT INTO object_tag (object_id, tag_id) VALUES (?, ?)",
            format!("o-{seed}-{i}"),
            tag_id.clone()
        );
    }
    tag_id
}

#[tokio::test]
async fn a_tag_nobody_has_applied_is_still_listed_with_a_count_of_zero() {
    on_each_store!(|store| {
        // THE test. An inner join drops this row entirely; a LEFT JOIN keeps it
        // with a count of 0. A tag created and never used is exactly what a
        // person reorganising a library is looking for, and it is the row a
        // counting inner join makes un-findable.
        seed_tag_with(&store, "unused", 0, 0).await;
        let tags = store.all_tags_with_counts().await.expect("list");
        let unused = tags
            .iter()
            .find(|t| t.id == "t-unused")
            .expect("the tag is listed");
        assert_eq!(unused.count, 0, "listed, and honestly counted as zero");
    });
}

#[tokio::test]
async fn the_count_is_the_number_of_objects_carrying_the_tag() {
    on_each_store!(|store| {
        seed_tag_with(&store, "counted", 3, 5).await;
        let tags = store.all_tags_with_counts().await.expect("list");
        let t = tags.iter().find(|t| t.id == "t-counted").unwrap();
        // 3 of 5 objects carry it. A `COUNT(*)` over the join would be right
        // here and wrong only on the zero case, which is why the previous test
        // is the one that matters and this one is the sanity check.
        assert_eq!(t.count, 3);
        assert_eq!(t.name, "counted-tag");
    });
}

#[tokio::test]
async fn removing_the_last_object_leaves_the_tag_at_zero_rather_than_deleting_it() {
    on_each_store!(|store| {
        // The lifecycle question, and the reason a count is computed rather than
        // inferred from presence. Deleting an object cascades to `object_tag`,
        // and a view that joins inward would now show nothing at all — the tag
        // would appear to have been deleted along with its last use, which it
        // was not.
        seed_tag_with(&store, "emptied", 2, 2).await;
        assert_eq!(
            store
                .all_tags_with_counts()
                .await
                .expect("list")
                .iter()
                .find(|t| t.id == "t-emptied")
                .unwrap()
                .count,
            2
        );

        for i in 0..2 {
            run_write!(
                &store,
                "DELETE FROM object WHERE id = ?",
                format!("o-emptied-{i}")
            );
        }
        let after = store.all_tags_with_counts().await.expect("list");
        let t = after
            .iter()
            .find(|t| t.id == "t-emptied")
            .expect("still listed");
        assert_eq!(t.count, 0, "the tag survived its objects");
    });
}

#[tokio::test]
async fn the_list_is_in_name_order_on_both_engines() {
    on_each_store!(|store| {
        // Ordering in the query rather than in Rust, so the two engines agree —
        // `tags.rs` says this in a comment on `all_tags` and this is the same
        // rule applied to the same join. Sorting in Rust is what `search.rs`
        // warns against, and the warning is about exactly this.
        for name in ["zulu", "alpha", "mike"] {
            run_write!(
                &store,
                "INSERT INTO tag (id, name) VALUES (?, ?)",
                format!("t-order-{name}"),
                name
            );
        }
        let tags = store.all_tags_with_counts().await.expect("list");
        let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "name order, not insertion or id order");
    });
}
