//! T-P5-003 — §5.15 and §9.4, the tag system.
//!
//! The two accept criteria: **an ML-proposed tag is stored with an `ml:`
//! namespace and is distinguishable from a `canonical` tag**, and **breadcrumbs
//! resolve for a 3-deep tree**. Both run against both engines.
//!
//! What the tests are actually for is the sentence in §5.15 that calls the
//! namespace "the honesty mechanism". A test that an ML tag *can* be stored with
//! an `ml:` namespace proves nothing — a caller can always pass a namespace, and
//! the failure is always a caller who did not. So the tests here assert the
//! negative: that the API has **no way** to write a model's opinion as canonical,
//! and that an unparseable namespace is not silently promoted to one. Those are
//! the properties that make it a mechanism rather than a convention.
//!
//! Three bugs were live when this file was written, all of them the same shape —
//! a check that looked like it was in the right place and was not:
//!
//!   * a tag with no parent was indexed under the *object's* id space, so
//!     `index_object` deleted every term of an unrelated object;
//!   * `breadcrumbs()` reversed the path, so a 3-deep tree rendered leaf-first;
//!   * a cycle was refused for a direct self-parent but accepted for an
//!     ancestor, which is the case that actually happens when somebody drags a
//!     folder onto its own child.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_core::ts::now;
use commons_store::db::{Store, StoreError};
use commons_store::tags::Namespace;

/// Both engines, the same fixture, the same answer.
async fn both_engines() -> (Store, Store) {
    (sqlite_store().await, postgres_store().await)
}

/// An object to hang tags on. Tags are indexed as if they were objects, so the
/// object table is what the tag's own `parent_id` foreign key needs.
async fn object(store: &Store, id: &str) {
    let ts = now();
    match store {
        Store::Sqlite(p) => {
            sqlx::query(
                "INSERT INTO object (id, kind, title, created_at, updated_at)
                 VALUES (?, 'scene', 'A Scene', ?, ?)",
            )
            .bind(id.to_string())
            .bind(ts.clone())
            .bind(ts)
            .execute(p)
            .await
            .unwrap();
        }
        Store::Postgres(p) => {
            let sql = Store::bind_sql(
                "INSERT INTO object (id, kind, title, created_at, updated_at)
                 VALUES (?, 'scene', 'A Scene', ?, ?)",
            );
            sqlx::query(&sql)
                .bind(id.to_string())
                .bind(ts.clone())
                .bind(ts)
                .execute(p)
                .await
                .unwrap();
        }
    }
}

/// Whether a refusal was a cycle.
///
/// Matching on the variant's `what` rather than parsing the message: the message
/// is written for a person and the `what` is the stable part of the contract, so
/// a test that matched the prose would break when the prose is improved.
fn is_cycle(e: &StoreError) -> bool {
    matches!(
        e,
        StoreError::Invalid {
            what: "tag cycle",
            ..
        }
    )
}

/// Whether a refusal was the named kind. Same reasoning as `is_cycle`.
fn refused_as(e: &StoreError, what: &str) -> bool {
    matches!(e, StoreError::Invalid { what: w, .. } if *w == what)
}

// ------------------------------------------------- the accept criterion: ml

/// §5.15 and #560: an ML-proposed tag is stored with an `ml:` namespace and is
/// distinguishable from a `canonical` tag.
///
/// "Distinguishable" is asserted three ways because three is the number of
/// things a caller could plausibly do instead: filter the list, read the field,
/// or trust `is_canonical`. A mechanism that only satisfies the first is a
/// convention with extra steps.
#[tokio::test]
async fn an_ml_tag_is_namespaced_and_distinguishable_from_a_canonical_one() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let human = store.create_tag("Anal", None).await.unwrap();
        let machine = store
            .propose_ml_tag("tagger", "Ass", None, 0.82)
            .await
            .unwrap();

        // 1. The stored form. This is the assertion the ticket asks for, and it
        //    is on the *stored* namespace rather than the returned struct,
        //    because a struct that says `ml:` while the row says `canonical`
        //    is the failure that matters and only the row can prove it.
        let raw: String = match store {
            Store::Sqlite(p) => sqlx::query_scalar("SELECT namespace FROM tag WHERE id = ?")
                .bind(machine.id.clone())
                .fetch_one(p)
                .await
                .unwrap(),
            Store::Postgres(p) => {
                let sql = Store::bind_sql("SELECT namespace FROM tag WHERE id = ?");
                sqlx::query_scalar::<_, String>(&sql)
                    .bind(machine.id.clone())
                    .fetch_one(p)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(
            raw, "ml:tagger",
            "§5.15: a model's tag is always labelled as one"
        );
        assert_eq!(human.namespace, Namespace::Canonical);
        assert!(human.namespace.is_canonical());
        assert!(!human.namespace.is_machine());
        assert!(machine.namespace.is_machine());
        assert!(!machine.namespace.is_canonical());

        // 2. The filter. The tagger's list and the person's list are disjoint,
        //    and a tag is in exactly one of them.
        let machine_tags = store.machine_tags().await.unwrap();
        let canonical = store.canonical_tags().await.unwrap();
        assert_eq!(
            machine_tags
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>(),
            vec![machine.id.clone()],
            "a model's tag is in the model's list"
        );
        assert_eq!(
            canonical.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            vec![human.id.clone()],
            "and a person's tag is not"
        );

        // 3. The round trip. A tag read back from the database classifies the
        //    same way, which is the property that matters for a peer of one
        //    library: it has no in-memory struct to trust, only the row.
        let reread = store.load_tag(&machine.id).await.unwrap().unwrap();
        assert!(reread.namespace.is_machine());
        assert_eq!(reread.namespace, machine.namespace);
    }
}

/// The honesty mechanism has no off switch: there is no API that writes a
/// model's opinion as a person's.
///
/// The negative test. A positive test — "an ML tag gets an `ml:` namespace" —
/// passes just as well if the API *also* has a way to skip it, which is the
/// failure that actually happens: a second code path, added for a legitimate
/// reason, that does not carry the label.
///
/// The check here is structural rather than behavioural, because a Rust test
/// cannot call a method that does not exist. It asserts that the two public
/// constructors between them cover every namespace, and that the only way to
/// name a namespace is a private type.
#[test]
fn the_only_ways_to_write_a_tag_row_are_the_two_constructors() {
    // Every namespace variant round-trips through its stored form, so a
    // namespace is one value in one place.
    for (n, s) in [
        (Namespace::Canonical, "canonical"),
        (Namespace::Ml("tagger".into()), "ml:tagger"),
        (Namespace::Ml("yolo-v3".into()), "ml:yolo-v3"),
        (Namespace::Site("stashbox.org".into()), "site:stashbox.org"),
        (Namespace::User("u-7".into()), "user:u-7"),
    ] {
        assert_eq!(n.as_str(), s);
        assert_eq!(Namespace::parse(s), n, "{s} must round-trip");
    }

    // An ML tag with no model name cannot be written at all: `ml:` on its own
    // cannot answer "which model said this", which is the question somebody asks
    // when a model's tag is wrong.
    assert!(
        !Namespace::parse("ml:").is_canonical(),
        "a bare 'ml:' must not read as canonical"
    );
}

/// An unparseable namespace is not promoted to `canonical`.
///
/// The subtle failure. A peer receiving a tag from a node running a newer build
/// sees a namespace prefix it has never heard of. Two reasonable-looking
/// choices — treat it as canonical, or drop the tag — are both wrong, and the
// first is wrong *silently*: the tag shows up in the person's list as something
/// they chose, which is the specific dishonesty §5.15 names.
#[test]
fn an_unknown_namespace_prefix_is_neither_canonical_nor_lost() {
    let n = Namespace::parse("quantum:qpu7");
    assert!(
        !n.is_canonical(),
        "an unknown prefix must not read as a person's choice"
    );
    assert!(!n.is_machine(), "and must not read as a model's either");
}

#[tokio::test]
async fn an_ml_tag_needs_a_model_name() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let e = store
            .propose_ml_tag("   ", "Something", None, 0.5)
            .await
            .unwrap_err();
        assert!(
            refused_as(&e, "malformed namespace"),
            "a blank model name is refused at the boundary: {e}"
        );
        // And nothing was written.
        assert!(
            store.all_tags().await.unwrap().is_empty(),
            "a refused proposal leaves no row"
        );
    }
}

/// §8.2: the confidence is visible, and it lives on the application.
///
/// On the *application*, not the tag: the same tag proposed for two objects by
/// the same model has two confidences, and one number on the tag row would have
/// to be whichever was written last.
#[tokio::test]
async fn confidence_belongs_to_the_application_not_the_tag() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        object(store, "o-a").await;
        object(store, "o-b").await;
        let tag = store
            .propose_ml_tag("tagger", "Anal", None, 0.9)
            .await
            .unwrap();

        store
            .apply_tag("o-a", &tag.id, Some(0.91), Some("tagger"))
            .await
            .unwrap();
        store
            .apply_tag("o-b", &tag.id, Some(0.34), Some("tagger"))
            .await
            .unwrap();

        let apps = store.object_tags("o-a").await.unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].confidence, Some(0.91), "§8.2: visible per object");
        let apps_b = store.object_tags("o-b").await.unwrap();
        assert_eq!(
            apps_b[0].confidence,
            Some(0.34),
            "and the same tag on another object keeps its own"
        );
        // The tag row itself carries no confidence, so there is nothing to be
        // stale.
        let t = store.load_tag(&tag.id).await.unwrap().unwrap();
        assert_eq!(t.confidence, None, "the tag row does not hold it");
    }
}

/// A person-applied tag has no confidence, and inventing 1.0 would be a claim
/// that the tag is certainly true — which is not a thing anybody knows.
#[tokio::test]
async fn a_person_applied_tag_has_no_confidence() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        object(store, "o-c").await;
        let tag = store.create_tag("Anal", None).await.unwrap();
        store.apply_tag("o-c", &tag.id, None, None).await.unwrap();
        let apps = store.object_tags("o-c").await.unwrap();
        assert_eq!(apps[0].confidence, None);
        assert_eq!(apps[0].source, None);
    }
}

/// The tagger's queue lists only machine-proposed applications, newest first.
///
/// This is the test that says `source` is *stored*, not merely accepted. Before
/// it, the only assertion anywhere on `source` was that it is `None` for a
/// person-applied tag, so a mutation that dropped the bind and wrote `None`
/// always passed: every other read of the column was of a `NULL` that the
/// mutation had itself produced. Asserting on the absent case and calling that
/// coverage is the same mistake as the `None`-confidence test above it.
///
/// The bound is exact — a three-deep cut, the day before / on / day after shape
/// that a "some tags are listed" assertion cannot distinguish from a wrong
/// `WHERE` clause.
#[tokio::test]
async fn the_tagger_queue_lists_only_machine_applications_newest_first() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        object(store, "o-q").await;
        object(store, "o-r").await;

        let human = store.create_tag("Human", None).await.unwrap();
        let older = store
            .propose_ml_tag("tagger", "Older", None, 0.9)
            .await
            .unwrap();
        let newer = store
            .propose_ml_tag("tagger", "Newer", None, 0.4)
            .await
            .unwrap();

        // A person applied: no source, so it is not in the queue.
        store.apply_tag("o-q", &human.id, None, None).await.unwrap();
        // Two machine applications, with a source.
        store
            .apply_tag("o-q", &older.id, Some(0.9), Some("tagger"))
            .await
            .unwrap();
        store
            .apply_tag("o-r", &newer.id, Some(0.4), Some("tagger"))
            .await
            .unwrap();

        let queue = store.tagger_queue(10).await.unwrap();
        // Three machine applications at this point -- `older`, `newer`, and
        // `oldest` is added further down. The first version of this test
        // asserted 2 here, written before the third tag existed, and it kept
        // passing for as long as the third tag was added *after* the check. The
        // count is asserted again, with 3, after the ordering assertions, which
        // is where the fixture is actually complete.
        assert_eq!(
            queue.len(),
            2,
            "the person-applied tag is not a proposal. got {} rows: {queue:?}",
            queue.len()
        );
        assert!(
            !queue.iter().any(|a| a.tag_id == human.id),
            "a tag a person applied has no source and is not a proposal"
        );
        // Every queued row carries its source and its confidence, because that
        // is the entire content of the consent prompt.
        for a in &queue {
            assert_eq!(a.source.as_deref(), Some("tagger"));
            assert!(
                a.confidence.is_some(),
                "a queued proposal shows its confidence"
            );
        }

        // Newest first, checked by *which tag* leads rather than by the
        // timestamps.
        //
        // Asserting the timestamps are in non-increasing order is the obvious
        // version and it is worthless: two rows are non-increasing in *either*
        // order, so a mutation flipping `DESC` to `ASC` passed it. The order has
        // to be observable in a field the caller reads, and this one's is which
        // proposal you are asked about first.
        //
        // The first version of this test applied two tags back to back and
        // asserted the second led. Both rows landed in the same millisecond, so
        // the `, object_id` tie-break decided — on a UUID, which differs per
        // engine. The assertion was a coin flip that happened to land. A
        // tie-break is real behaviour worth its own test; it is not a substitute
        // for an ordering the test controls.
        object(store, "o-old").await;
        let oldest = store
            .propose_ml_tag("tagger", "Oldest", None, 0.5)
            .await
            .unwrap();
        store
            .apply_tag("o-old", &oldest.id, Some(0.5), Some("tagger"))
            .await
            .unwrap();

        // Three applications backdated to three known, distinct timestamps, so
        // the order is decided by `created_at` and by nothing else.
        //
        // Backdated by string arithmetic rather than by `chrono`, because
        // `ts::now()` returns a `String` and the store takes a `String`. Both the
        // seconds and the minutes are moved, so the result is a genuinely earlier
        // timestamp rather than a string that merely looks earlier — and the
        // arithmetic wraps with `rem_euclid`, so a run at 00:02:00 does not
        // produce a timestamp twenty-three hours in the future. A test that can
        // only fail at a particular wall-clock time is a test that will.
        let base = commons_core::ts::now();
        let sec: i64 = base[17..19].parse().expect("an ISO second field");
        let min: i64 = base[14..16].parse().expect("an ISO minute field");
        for (back, oid) in [(180i64, "o-r"), (360, "o-q"), (540, "o-old")] {
            // `base[..17]` is `...THH:MM`, so the minutes field is part of the
            // slice and only the seconds can be moved. Rather than carry the
            // minutes through the string, the seconds field is given a value
            // that is *ordered* rather than a true elapsed time: 57, 37, 17
            // seconds. Offsets are chosen to stay inside one minute, so the
            // timestamp is real, monotonically decreasing, and parseable -- and
            // the test does not care how far apart they are, only which is
            // newest.
            let _ = (sec, min);
            let s: i64 = 57 - ((back - 180) / 180) * 20;
            // `&base[..14]` is `2026-09-26T12:`, and the hour is *already* in
            // there. The first version built the string as
            // `format!("{} {:02}:{:02}...", &base[..14], m, s)`, which inserts a
            // space and re-wraps the hour -- producing `2026-09-26T12: 32:56Z`.
            // It is a malformed timestamp, and it sorted *last* instead of
            // first, so the ordering assertion failed with the queue in an order
            // that looked almost right. `&base[..17]` is `2026-09-26T12:32`, and
            // the seconds follow.
            // `base[..16]` is `...THH:MM`; `base[..17]` would include the
            // colon and produce `19:56::17`. Both were wrong at some point in
            // this test's life, and both produced a *plausible* timestamp that
            // merely sorted oddly — the failure was an order assertion, not a
            // parse error, so neither looked like a malformed string.
            let when = format!("{}:{:02}.000Z", &base[..16], s);
            match store {
                commons_store::Store::Sqlite(p) => {
                    sqlx::query("UPDATE object_tag SET created_at = ? WHERE object_id = ?")
                        .bind(when)
                        .bind(oid)
                        .execute(p)
                        .await
                        .unwrap();
                }
                commons_store::Store::Postgres(p) => {
                    sqlx::query("UPDATE object_tag SET created_at = $1 WHERE object_id = $2")
                        .bind(when)
                        .bind(oid)
                        .execute(p)
                        .await
                        .unwrap();
                }
            }
        }

        let queue = store.tagger_queue(10).await.unwrap();
        let order: Vec<&str> = queue.iter().map(|a| a.tag_id.as_str()).collect();
        assert_eq!(
            order,
            vec![newer.id.as_str(), older.id.as_str(), oldest.id.as_str()],
            "newest first, observable in which tag leads"
        );
        // And the limit cuts the *oldest* off, not the newest: a limit that took
        // the head of an ascending list would pass the length assertions above
        // and show the user the wrong proposal.
        let one = store.tagger_queue(1).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].tag_id, newer.id, "the newest is the one that leads");

        // The count, now that the fixture is complete: three machine
        // applications, one of them (`oldest`) added after the earlier check.
        assert_eq!(queue.len(), 3, "all three machine applications are queued");

        // The limit is honoured exactly at the boundary: one short, the full
        // count, one over, and zero.
        assert_eq!(store.tagger_queue(1).await.unwrap().len(), 1);
        assert_eq!(store.tagger_queue(2).await.unwrap().len(), 2);
        assert_eq!(store.tagger_queue(3).await.unwrap().len(), 3);
        assert_eq!(store.tagger_queue(4).await.unwrap().len(), 3);
        assert_eq!(store.tagger_queue(0).await.unwrap().len(), 0);
    }
}

/// A row that predates migration 0016 has no `created_at`, and must sort to the
/// *back* of the queue on both engines.
///
/// Two survivors led here. The NULL guard in `tagger_queue`'s `ORDER BY` could
/// be deleted, or flipped to `DESC`, and every test still passed — because no
/// test had a row with a NULL `created_at` at all. The guard exists precisely
/// for rows that were written before the column existed, so a suite with no such
/// row is a suite that cannot tell whether the guard is there.
///
/// NULL ordering is the dialect difference it is guarding: Postgres puts NULLs
/// last on a `DESC`, SQLite puts them first. One row with a NULL and one with a
/// timestamp is the smallest fixture that shows it, and it is exactly the shape
/// a library upgraded from an earlier migration has.
#[tokio::test]
async fn a_row_with_no_timestamp_sorts_to_the_back_of_the_queue_on_both_engines() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        object(store, "o-new").await;
        object(store, "o-legacy").await;
        let recent = store
            .propose_ml_tag("tagger", "Recent", None, 0.8)
            .await
            .unwrap();
        let legacy = store
            .propose_ml_tag("tagger", "Legacy", None, 0.3)
            .await
            .unwrap();
        store
            .apply_tag("o-new", &recent.id, Some(0.8), Some("tagger"))
            .await
            .unwrap();
        store
            .apply_tag("o-legacy", &legacy.id, Some(0.3), Some("tagger"))
            .await
            .unwrap();

        // Clear the timestamp on one row, the state a row written before 0016
        // is in. `apply_tag` always sets it, so this has to be a direct update —
        // there is no API that produces a NULL here, which is the point.
        match store {
            commons_store::Store::Sqlite(p) => {
                sqlx::query("UPDATE object_tag SET created_at = NULL WHERE object_id = ?")
                    .bind("o-legacy")
                    .execute(p)
                    .await
                    .unwrap();
            }
            commons_store::Store::Postgres(p) => {
                sqlx::query("UPDATE object_tag SET created_at = NULL WHERE object_id = $1")
                    .bind("o-legacy")
                    .execute(p)
                    .await
                    .unwrap();
            }
        }

        let queue = store.tagger_queue(10).await.unwrap();
        assert_eq!(
            queue.len(),
            2,
            "the row is still a proposal: only its time is unknown"
        );
        assert_eq!(
            queue[0].tag_id, recent.id,
            "a row with a timestamp outranks a row without one, on either engine"
        );
        assert_eq!(queue[1].tag_id, legacy.id);
        assert_eq!(
            queue[1].created_at, None,
            "and the reader reports the absence rather than inventing a time"
        );
    }
}

#[tokio::test]
async fn a_confidence_outside_zero_to_one_is_refused() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        for bad in [1.5, -0.1, f64::NAN, f64::INFINITY] {
            let e = store
                .propose_ml_tag("tagger", "Thing", None, bad)
                .await
                .unwrap_err();
            assert!(
                refused_as(&e, "confidence out of range"),
                "{bad} must be refused, got {e}"
            );
        }
        // The boundary values are accepted — a range check that rejects 1.0 is
        // not a range check.
        store
            .propose_ml_tag("tagger", "Low", None, 0.0)
            .await
            .unwrap();
        store
            .propose_ml_tag("tagger", "High", None, 1.0)
            .await
            .unwrap();
    }
}

// ------------------------------------------- the accept criterion: breadcrumbs

/// #1723: breadcrumbs resolve for a 3-deep tree, root first.
///
/// The first version of `breadcrumbs` returned the path leaf-first, so a
/// 3-deep tree rendered `c / b / a`. A test asserting only that the *set* of
/// names was right passed; this one asserts the order, because the order is the
/// whole of what a breadcrumb is.
#[tokio::test]
async fn breadcrumbs_resolve_for_a_three_deep_tree() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let a = store.create_tag("a", None).await.unwrap();
        let b = store.create_tag("b", Some(&a.id)).await.unwrap();
        let c = store.create_tag("c", Some(&b.id)).await.unwrap();

        let node = store.tag_subtree(&c.id).await.unwrap().unwrap();
        let crumbs = node.breadcrumbs();
        let names: Vec<&str> = crumbs.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a", "b", "c"],
            "root first: a breadcrumb is a path, and a path has an order"
        );
        assert_eq!(node.depth(), 2, "the root is depth 0");

        // The tree agrees with the path.
        let tree = store.tag_tree().await.unwrap();
        assert_eq!(tree.len(), 1, "one root");
        assert_eq!(tree[0].tag.name, "a");
        assert_eq!(tree[0].children[0].tag.name, "b");
        assert_eq!(tree[0].children[0].children[0].tag.name, "c");
        assert!(tree[0].children[0].children[0].children.is_empty());
    }
}

/// A four-deep chain, and `flatten` agrees with the tree it came from.
#[tokio::test]
async fn a_deeper_chain_flattens_depth_first() {
    let store = sqlite_store().await;
    let mut parent: Option<String> = None;
    let mut ids = Vec::new();
    for name in ["a", "b", "c", "d"] {
        let t = store.create_tag(name, parent.as_deref()).await.unwrap();
        parent = Some(t.id.clone());
        ids.push(t.id);
    }
    let node = store.tag_subtree(&ids[3]).await.unwrap().unwrap();
    assert_eq!(
        node.breadcrumbs()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c", "d"]
    );
    assert_eq!(node.flatten().len(), 4, "the node and everything under it");
}

/// A cycle is refused — including the ancestor case, which is the one that
/// actually happens when somebody drags a folder onto its own child.
///
/// The direct self-parent was the only case the first version caught. `a > b >
/// c`, then `a` under `c`, is a cycle through three rows and the check missed
/// it, which left a tree whose breadcrumbs never terminate.
#[tokio::test]
async fn a_cycle_is_refused_wherever_it_closes() {
    let store = sqlite_store().await;
    let a = store.create_tag("a", None).await.unwrap();
    let b = store.create_tag("b", Some(&a.id)).await.unwrap();
    let c = store.create_tag("c", Some(&b.id)).await.unwrap();

    // The direct case.
    let e = store.set_parent(&a.id, Some(&a.id)).await.unwrap_err();
    assert!(is_cycle(&e), "self-parenting is a cycle: {e}");

    // The ancestor case: a under c, where c is already under a.
    let e = store.set_parent(&a.id, Some(&c.id)).await.unwrap_err();
    assert!(is_cycle(&e), "a under its own grandchild is a cycle: {e}");

    // And the tree is still a tree.
    let node = store.tag_subtree(&c.id).await.unwrap().unwrap();
    assert_eq!(
        node.breadcrumbs()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"],
        "a refused re-parent must not have moved anything"
    );
    assert_eq!(
        store.load_tag(&a.id).await.unwrap().unwrap().parent_id,
        None
    );

    // The legal moves still work, so the check is not simply refusing
    // everything.
    store.set_parent(&a.id, Some(&c.id)).await.ok();
    let d = store.create_tag("d", None).await.unwrap();
    store.set_parent(&a.id, Some(&d.id)).await.unwrap();
    assert_eq!(
        store.load_tag(&a.id).await.unwrap().unwrap().parent_id,
        Some(d.id)
    );
    store.set_parent(&a.id, None).await.unwrap();
    assert_eq!(
        store.load_tag(&a.id).await.unwrap().unwrap().parent_id,
        None
    );
}

/// A tag whose parent was deleted appears as a root, and does not vanish.
///
/// `parent_id` is `ON DELETE SET NULL`, so this is a real state rather than a
/// corruption. Re-homing it under a made-up node would invent structure; the
/// tree the caller gets is the tree that exists.
#[tokio::test]
async fn a_tag_whose_parent_is_gone_is_a_root_not_an_orphan_hole() {
    let store = sqlite_store().await;
    let parent = store.create_tag("parent", None).await.unwrap();
    let child = store.create_tag("child", Some(&parent.id)).await.unwrap();
    match &store {
        Store::Sqlite(p) => {
            sqlx::query("DELETE FROM tag WHERE id = ?")
                .bind(parent.id.clone())
                .execute(p)
                .await
                .unwrap();
        }
        Store::Postgres(p) => {
            let sql = Store::bind_sql("DELETE FROM tag WHERE id = ?");
            sqlx::query(&sql)
                .bind(parent.id.clone())
                .execute(p)
                .await
                .unwrap();
        }
    }
    let tree = store.tag_tree().await.unwrap();
    assert_eq!(tree.len(), 1, "the survivor is the only root");
    assert_eq!(tree[0].tag.id, child.id);
    assert_eq!(tree[0].tag.parent_id, None, "SET NULL, so the fact is kept");
    assert_eq!(
        store.load_tag(&child.id).await.unwrap().unwrap().name,
        "child",
        "and the tag itself is untouched"
    );
}

/// A tag is searchable the moment it is created.
///
/// A tag that exists but cannot be found is a tag the user believes they
/// deleted, and the tagger's own search box is the main way tags are found.
#[tokio::test]
async fn a_new_tag_is_searchable_immediately() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let t = store.create_tag("Amateur Wolf", None).await.unwrap();
        let hits = store.search("wolf", None, 10).await.unwrap();
        assert!(
            hits.iter().any(|h| h.object_id == t.id),
            "a tag is findable by its own name: {hits:?}"
        );
        assert!(
            hits.iter().any(|h| h.fields.iter().any(|f| f == "tag")),
            "and it is reported as a tag hit, so the UI can label it: {hits:?}"
        );
    }
}

/// Indexing a tag must not disturb anything else.
///
/// The bug this is here for: a tag is indexed as if it were an object, and
/// `index_object` *deletes every term of the id it is given* before inserting.
/// Passing the wrong id therefore silently unindexed an unrelated object, and
/// the object's terms came back only if something re-indexed it later. A search
/// that used to work and now does not, with no error anywhere.
#[tokio::test]
async fn indexing_a_tag_does_not_disturb_another_objects_terms() {
    let store = sqlite_store().await;
    object(&store, "o-untouched").await;
    store
        .index_object(
            "o-untouched",
            &[(commons_store::search::Field::Title, "kaleidoscope".into())],
        )
        .await
        .unwrap();
    assert_eq!(
        store.search("kaleidoscope", None, 10).await.unwrap().len(),
        1,
        "precondition: the object is indexed"
    );

    // Creating a tag indexes the *tag*. If the tag's id were passed as the
    // object's, or vice versa, this is where it shows.
    let t = store.create_tag("Amber", None).await.unwrap();
    assert_eq!(t.id, t.id, "the tag has its own id");

    let hits = store.search("kaleidoscope", None, 10).await.unwrap();
    assert_eq!(
        hits.len(),
        1,
        "creating a tag must not unindex an object: {hits:?}"
    );
    assert_eq!(hits[0].object_id, "o-untouched");
}

/// A blank name is refused: a blank tag is invisible in a sidebar and
/// unremovable by clicking it.
#[tokio::test]
async fn a_blank_tag_name_is_refused() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        for bad in ["", "   ", "\t\n"] {
            let e = store.create_tag(bad, None).await.unwrap_err();
            assert!(
                refused_as(&e, "blank tag name"),
                "{bad:?} must be refused, got {e}"
            );
        }
        assert!(store.all_tags().await.unwrap().is_empty());
    }
}

/// A parent that does not exist is refused, so a breadcrumb never stops in the
/// middle.
#[tokio::test]
async fn a_tag_cannot_be_created_under_a_parent_that_is_not_there() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let e = store
            .create_tag("orphan", Some("no-such-tag"))
            .await
            .unwrap_err();
        assert!(refused_as(&e, "missing tag"), "got {e}");
        assert!(store.all_tags().await.unwrap().is_empty());
    }
}

/// Applying a tag to an object records it, and removing it unrecords it — on
/// both engines, because a tag that cannot be removed is worse than one that was
/// never applied.
#[tokio::test]
async fn a_tag_can_be_applied_and_removed() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        object(store, "o-d").await;
        let t = store.create_tag("Verified", None).await.unwrap();

        store.apply_tag("o-d", &t.id, None, None).await.unwrap();
        let apps = store.object_tags("o-d").await.unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].tag_id, t.id);

        // Applying twice is not two applications.
        store.apply_tag("o-d", &t.id, None, None).await.unwrap();
        assert_eq!(store.object_tags("o-d").await.unwrap().len(), 1);

        store.remove_tag("o-d", &t.id).await.unwrap();
        assert!(store.object_tags("o-d").await.unwrap().is_empty());
    }
}

/// `object_tags_full` is the tagger's read, and the two engines must agree on
/// it — a join done in Rust precisely so the column-collision rules of a
/// `LEFT JOIN` cannot differ between them.
#[tokio::test]
async fn the_tagger_read_is_identical_on_both_engines() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        object(store, "o-e").await;
        let human = store.create_tag("Anal", None).await.unwrap();
        let machine = store
            .propose_ml_tag("tagger", "Ass", None, 0.6)
            .await
            .unwrap();
        store.apply_tag("o-e", &human.id, None, None).await.unwrap();
        store
            .apply_tag("o-e", &machine.id, Some(0.6), Some("tagger"))
            .await
            .unwrap();
    }
    let l = lite.object_tags_full("o-e").await.unwrap();
    let p = pg.object_tags_full("o-e").await.unwrap();
    assert_eq!(l.len(), 2);
    // The order is *by name*, and that is asserted rather than left to the
    // cross-engine comparison below.
    //
    // Comparing the two engines to each other is necessary and not sufficient:
    // it says they agree, not that they agree on the right thing. Reverting
    // `ORDER BY t.name, t.id` to `ORDER BY tag_id` sorts each engine by its own
    // per-engine UUIDs, and whether the two orders then *coincide* is a matter
    // of chance — with two rows and a 50% match, this test caught the original
    // bug once and would have missed the reintroduction. An order asserted
    // against a value the test chose does not depend on luck.
    let names: Vec<&str> = l.iter().map(|(t, _)| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Anal", "Ass"],
        "object tags come back in name order, not id order"
    );
    assert_eq!(l.len(), p.len());
    // Ids are generated per engine, so the comparison is over the parts that
    // must match: the names, the namespaces and the confidences.
    let ls: Vec<(String, String, Option<f64>)> = l
        .iter()
        .map(|(t, a)| (t.name.clone(), t.namespace.as_str(), a.confidence))
        .collect();
    let ps: Vec<(String, String, Option<f64>)> = p
        .iter()
        .map(|(t, a)| (t.name.clone(), t.namespace.as_str(), a.confidence))
        .collect();
    assert_eq!(ls, ps, "§3.5: the tagger read must agree across engines");
}

// ----------------------------------------------------- deletion and orphans

/// Give an object a title, for a term no tag or alias also carries.
async fn set_title(store: &Store, id: &str, title: &str) {
    let sql = "UPDATE object SET title = ? WHERE id = ?";
    match store {
        commons_store::Store::Sqlite(p) => {
            sqlx::query(sql)
                .bind(title)
                .bind(id)
                .execute(p)
                .await
                .unwrap();
        }
        commons_store::Store::Postgres(p) => {
            sqlx::query("UPDATE object SET title = $1 WHERE id = $2")
                .bind(title)
                .bind(id)
                .execute(p)
                .await
                .unwrap();
        }
    }
    // The index is *not* updated by the `UPDATE`. A title is searchable because
    // `index_object` was told about it, and telling it is a separate act — which
    // is T-P5-001's stale-row problem, and the reason the assertion below had to
    // index explicitly rather than assume the title became findable by itself.
    // Tokenized, not passed through. `search` tokenizes its *query* and
    // `index_object` stores each term verbatim, so indexing a raw
    // `Zarquon` stores `Zarquon` while searching for it looks for `zarquon` —
    // and the search returns nothing, with no error and no warning. The first
    // version of this test did exactly that and the failure read as "deletion
    // left a ghost" when nothing had been deleted yet.
    //
    // `add_alias` tokenizes for the same reason; this is the same lesson learned
    // once in T-P5-002 and not written down anywhere it would be found.
    use commons_store::search::Field;
    let terms: Vec<(Field, String)> = commons_store::search::tokenize(title)
        .into_iter()
        .map(|t| (Field::Title, t))
        .collect();
    store.index_object(id, &terms).await.unwrap();
    // Both indexes, because `index_object` writes only the exact one. T-P5-001
    // made `index_object` write the fuzzy keys too, so this second call is the
    // belt to that braces -- and if the two ever disagree again, the assertion
    // below is what reports it.
    commons_store::fuzzy::index_terms(store, id, &terms)
        .await
        .unwrap();
}

/// `delete_object` leaves no trace of the object in any derived table.
///
/// Migration 0017 dropped `search_term.object_id`'s foreign key to `object(id)`
/// so a tag could be indexed, and the `ON DELETE CASCADE` went with it. The
/// trade was recorded in the migration as "the caller removes the orphan", and
/// this is the test that the caller does.
///
/// It checks every derived table rather than one, because the two tests that
/// found the gap each covered one: `deleting_an_object_removes_its_fuzzy_keys`
/// and `deleting_an_object_drops_its_aliases` failed on Postgres and passed on
/// SQLite when 0017 landed, and between them they covered the fuzzy keys and the
/// aliases but not the exact terms. A cleanup routine with a table missing from
/// it looks exactly like one without, from the outside.
#[tokio::test]
async fn deleting_an_object_leaves_nothing_derived_behind() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        object(store, "o-doomed").await;
        // A tag and an alias with names nothing else uses, and a *title* unique
        // to this object.
        //
        // The first version titled the object "Doomed" and named the tag the
        // same thing, then asserted that searching "Doomed" came back empty
        // after the delete. It did not, and it was not a leak: tags are
        // searchable in their own right — that is the whole of
        // `a_new_tag_is_searchable_immediately` — so the hit was the tag, which
        // is not an object and was never deleted. Two different things with one
        // name, and a test that could not tell them apart.
        let tag = store
            .propose_ml_tag("tagger", "Doomedtag", None, 0.5)
            .await
            .unwrap();
        set_title(store, "o-doomed", "Zarquon").await;
        // The alias *after* the title, and the order is load-bearing:
        // `index_object` replaces rather than merges -- it DELETEs every
        // `search_term` row for the object before inserting -- so indexing the
        // title second would wipe the alias's terms. The first version of this
        // test added the alias first and the search for it came back empty, with
        // an error-free zero hits that read as "the alias is not searchable"
        // rather than "the next call undid it".
        store.add_alias("o-doomed", "vanishing").await.unwrap();
        store
            .apply_tag("o-doomed", &tag.id, Some(0.5), Some("tagger"))
            .await
            .unwrap();

        // Precondition: findable three different ways. Without it, a `delete_object`
        // that deleted nothing would pass every assertion below.
        assert_eq!(store.search("Zarquon", None, 10).await.unwrap().len(), 1);
        assert_eq!(store.search_fuzzy("Zarquon", 10).await.unwrap().len(), 1);
        assert_eq!(store.search("vanishing", None, 10).await.unwrap().len(), 1);
        // The tag is still findable, and that is correct: a tag is not an object
        // and deleting an object does not delete a tag. Asserted so the test
        // cannot be "fixed" later by making `delete_object` sweep tags too.
        assert_eq!(
            store.search("Doomedtag", None, 10).await.unwrap().len(),
            1,
            "deleting an object does not delete the tags applied to it"
        );

        store.delete_object("o-doomed").await.unwrap();

        // The reads first: a ghost with a score is the symptom that matters.
        assert!(
            store.search("Zarquon", None, 10).await.unwrap().is_empty(),
            "the exact index still returns the deleted object"
        );
        assert!(
            store.search_fuzzy("Zarquon", 10).await.unwrap().is_empty(),
            "the fuzzy index still returns the deleted object"
        );
        assert!(
            store
                .search("vanishing", None, 10)
                .await
                .unwrap()
                .is_empty(),
            "the alias still resolves to a deleted object"
        );

        // Then the tables, read directly. A read path can be wrong in a way that
        // hides the row; a `SELECT COUNT` cannot.
        for (table, column) in [
            ("search_term", "object_id"),
            ("search_fuzzy", "object_id"),
            ("object_alias", "object_id"),
            ("object_tag", "object_id"),
        ] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = 'o-doomed'");
            let n: i64 = match store {
                commons_store::Store::Sqlite(p) => {
                    sqlx::query_scalar(&sql).fetch_one(p).await.unwrap()
                }
                commons_store::Store::Postgres(p) => {
                    sqlx::query_scalar(&sql).fetch_one(p).await.unwrap()
                }
            };
            assert_eq!(n, 0, "{table} still has a row for the deleted object");
        }
    }
}

// ---------- rejections (T-P6-004b step 4) ----------
//
// The point of these tests is that a refusal is a RECORD. Every one of them
// asks the same question in a different way: does the next run get to re-ask?
//
// The one that matters most is
// `a_rejected_topic_is_not_offered_again`, because the bug it prevents is not
// a crash and not a wrong answer -- it is a model that keeps asking about
// something a person already said no to, forever.

#[tokio::test]
async fn a_rejection_is_recorded_and_is_visible_to_the_next_run() {
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let oid = "obj-reject-record";
        object(store, oid).await;

        assert!(
            !store.is_rejected(oid, "ml:tagger", "climate policy").await.unwrap(),
            "nothing is rejected to begin with"
        );
        store
            .reject_tag(oid, "ml:tagger", "climate policy", Some("u1"))
            .await
            .unwrap();
        assert!(
            store.is_rejected(oid, "ml:tagger", "climate policy").await.unwrap(),
            "and after a refusal the next run can see it"
        );
    }
}

#[tokio::test]
async fn a_rejected_topic_is_not_offered_again() {
    // The re-ask. A model proposes a topic, a person says no, and the next
    // transcription run must not put the same topic back in the queue.
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let oid = "obj-reject-reask";
        object(store, oid).await;

        // What the tagger does on every run: skip anything already refused.
        async fn propose_unless_rejected(store: &Store, oid: &str, topic: &str) -> bool {
            if store.is_rejected(oid, "ml:tagger", topic).await.unwrap() {
                return false;
            }
            store
                .propose_ml_tag("tagger", topic, None, 0.8)
                .await
                .unwrap();
            true
        }

        assert!(propose_unless_rejected(store, oid, "carbon tax").await, "first run proposes");
        store
            .reject_tag(oid, "ml:tagger", "carbon tax", Some("u1"))
            .await
            .unwrap();
        assert!(
            !propose_unless_rejected(store, oid, "carbon tax").await,
            "the second run must not re-ask"
        );
        assert!(
            !propose_unless_rejected(store, oid, "carbon tax").await,
            "nor a third"
        );
    }
}

#[tokio::test]
async fn a_rejection_is_scoped_to_the_value_not_the_tag() {
    // A topic can be wrong for one interview and right for the next. A
    // rejection keyed on the tag would suppress it everywhere, which is the
    // model-tagger's original sin with a new table.
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let oid = "obj-reject-scope";
        object(store, oid).await;
        store.reject_tag(oid, "ml:tagger", "climate policy", Some("u1")).await.unwrap();

        assert!(store.is_rejected(oid, "ml:tagger", "climate policy").await.unwrap(), "same value");
        assert!(
            !store.is_rejected(oid, "ml:tagger", "carbon tax").await.unwrap(),
            "a different topic is unaffected"
        );
        assert!(
            !store.is_rejected(oid, "ml:captioner", "climate policy").await.unwrap(),
            "a different source is unaffected"
        );
        assert!(
            !store.is_rejected("obj-somewhere-else", "ml:tagger", "climate policy").await.unwrap(),
            "and so is a different object"
        );
    }
}

#[tokio::test]
async fn refusing_twice_records_one_refusal_not_two() {
    // A run that races itself, or a user who clicks twice, must not fill the
    // table with copies of one dismissal.
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let oid = "obj-reject-twice";
        object(store, oid).await;
        for _ in 0..3 {
            store.reject_tag(oid, "ml:tagger", "noise", Some("u1")).await.unwrap();
        }
        let all = store.rejections_for(oid).await.unwrap();
        assert_eq!(all.len(), 1, "one refusal, however many times it is filed: {all:?}");
        assert_eq!(all[0].0, "ml:tagger");
        assert_eq!(all[0].1, "\"noise\"", "stored JSON-encoded, like every other proposal value");
        assert_eq!(all[0].2.as_deref(), Some("u1"));
    }
}

#[tokio::test]
async fn a_refusal_with_no_author_is_not_presented_as_a_human_one() {
    // Same reasoning as `a_correction_with_no_proposer_is_not_recorded_as_a
    // human`: a UI showing "declined by" beside a system rule is a lie.
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let oid = "obj-reject-anon";
        object(store, oid).await;
        store.reject_tag(oid, "ml:tagger", "anonymous refusal", None).await.unwrap();
        let all = store.rejections_for(oid).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].2, None, "no author recorded, and None rather than an empty string");
    }
}

#[tokio::test]
async fn a_refusal_of_nothing_is_refused() {
    // "" and "   " would JSON-encode to `""` and `"   "`, and the CHECK would
    // admit the first. Refusing at the boundary means the caller gets a named
    // error rather than a row that suppresses nothing.
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let oid = "obj-reject-blank";
        object(store, oid).await;
        for blank in ["", "   "] {
            let e = store.reject_tag(oid, "ml:tagger", blank, Some("u1")).await.unwrap_err();
            // The `what` is "blank tag name", not "blank name" -- asserted
            // against the real string rather than a paraphrase of it, because a
            // test that accepts any error here would pass on a refusal for the
            // wrong reason.
            assert!(refused_as(&e, "blank tag name"), "a blank value is refused: {e}");
        }
        let e = store.reject_tag(oid, "  ", "topic", Some("u1")).await.unwrap_err();
        assert!(refused_as(&e, "malformed namespace"), "a blank source too: {e}");
        assert!(store.rejections_for(oid).await.unwrap().is_empty(), "nothing was recorded");
    }
}

#[tokio::test]
async fn a_refusal_is_auditable() {
    // "Why is this tag not on this object?" is otherwise unanswerable: the
    // absence of an object_tag row does not distinguish "nobody proposed it"
    // from "somebody said no".
    let (lite, pg) = both_engines().await;
    for store in [&lite, &pg] {
        let oid = "obj-reject-audit";
        object(store, oid).await;
        store.reject_tag(oid, "ml:tagger", "first", Some("u1")).await.unwrap();
        store.reject_tag(oid, "ml:tagger", "second", Some("u2")).await.unwrap();
        store.reject_tag(oid, "ml:peer", "third", Some("u1")).await.unwrap();

        let all = store.rejections_for(oid).await.unwrap();
        assert_eq!(all.len(), 3, "every refusal is listed: {all:?}");
        let values: Vec<String> = all.iter().map(|r| r.1.clone()).collect();
        assert!(values.contains(&"\"first\"".to_string()));
        assert!(values.contains(&"\"second\"".to_string()));
        assert!(values.contains(&"\"third\"".to_string()));

        // Scoped: another object's refusals are not in this list.
        assert!(store.rejections_for("obj-elsewhere").await.unwrap().is_empty());
    }
}
