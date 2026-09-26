//! T-P5-006 item 3 — §10.7, §14.1, bulk writes.
//!
//! # What these tests are actually for
//!
//! The obvious tests — "tag three objects, all three get the tag" — would pass
//! against a bulk path with no consent check at all, because a test fixture is
//! usually visible to everybody. So the tests here are for the three properties
//! a bulk write has that a single write does not:
//!
//! 1. **The consent clause is in the data layer, not the caller's care.** The
//!    test that matters is the negative one: a caller who may not see an object
//!    gets it *not* written, with the count saying so. A missing clause does not
//!    error — it silently over-reports — so "it returned Ok" proves nothing and
//!    only the count proves anything.
//! 2. **The returned count is the server's.** It is the number a modal shows a
//!    user, so it has to be what the database did rather than what the client
//!    asked for.
//! 3. **A batch is atomic.** A bulk edit that half-finishes is worse than one
//!    that failed, because the user cannot tell which half.
//!
//! Both engines, the same fixture, the same answer. §3.5.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_store::bulk::{BulkError, BulkOutcome, Target};
use commons_store::db::{insert_object, Store};
use commons_store::filter_ast::{CallerId, ConsentTiers, Filter};
use commons_store::index::set_consent_tier;
use uuid::Uuid;

/// Both engines, the same fixture, the same answer.
async fn both_engines() -> (Store, Store) {
    (sqlite_store().await, postgres_store().await)
}

/// A stable, valid object id for slot `n`.
///
/// Object ids are uuids in the schema (`set_consent_tier` takes a `Uuid`), so a
/// fixture that invents `"o-1"` needs a parse and a fallback. Deriving from a
/// fixed namespace instead keeps the ids valid, deterministic, and distinct
/// across tests sharing a database -- a hard-coded uuid would collide with
/// itself in a second fixture and produce a test that passes for the wrong
/// reason.
/// An attested tier the public tiers include, and one they do not.
///
/// Named from [`ConsentTiers`] rather than written as `"public"` and
/// `"unverified"` literals: there is no `public` tier — the public tiers are
/// `self_published`, `performer_claimed` and `third_party_permitted` — so a
/// fixture written from memory makes every object invisible to every caller, and
/// the tests pass vacuously with `applied: 0`.
const VISIBLE_TIER: &str = ConsentTiers::PUBLIC[0];
const HIDDEN_TIER: &str = "unverified";

fn oid(n: u32) -> String {
    assert!(
        n <= u32::from(u8::MAX),
        "oid({n}) does not fit the fixture's namespace"
    );

    // Built from `from_fields` rather than `new_v5`: v5 is behind the `v5`
    // feature, and adding a feature to a dev-dependency graph to make a test id
    // deterministic is a poor trade. The namespace is a constant of this file,
    // which is all the uniqueness the fixture needs.
    Uuid::from_fields(0x6d9a_0000, 0x0000, 0x0000, &[0, 0, 0, 0, 0, 0, 0, n as u8]).to_string()
}

/// An object with a consent tier.
///
/// Built from [`insert_object`] and [`set_consent_tier`] rather than from
/// hand-written SQL, because a fixture that spells out the column list is a
/// fixture that breaks when the schema moves -- and it breaks by saying
/// `NOT NULL constraint failed: consent_record.updated_at`, which reads like a
/// bug in the code under test rather than a stale fixture. It cost one round of
/// eight identical failures to learn.
async fn object_with_tier(store: &Store, id: &str, tier: &str) {
    insert_object(store, id, "scene").await.unwrap();
    set_consent_tier(store, Uuid::parse_str(id).unwrap(), tier, "test")
        .await
        .unwrap();
}

/// The tags on one object, as a sorted list, for an assertion.
async fn tags_on(store: &Store, object_id: &str) -> Vec<String> {
    let sql = commons_store::db::Store::bind_sql(
        "SELECT tag_id FROM object_tag WHERE object_id = ? ORDER BY tag_id",
    );
    let rows: Vec<(String,)> = match store {
        Store::Sqlite(p) => sqlx::query_as(&sql)
            .bind(object_id)
            .fetch_all(p)
            .await
            .unwrap(),
        Store::Postgres(p) => sqlx::query_as(&sql)
            .bind(object_id)
            .fetch_all(p)
            .await
            .unwrap(),
    };
    rows.into_iter().map(|r| r.0).collect()
}

#[tokio::test]
async fn a_bulk_tag_writes_to_every_named_object() {
    let (sqlite, postgres) = both_engines().await;
    for store in [&sqlite, &postgres] {
        for i in 0..3 {
            object_with_tier(store, &oid(i + 1), VISIBLE_TIER).await;
        }
        let tag = store.create_tag("beach", None).await.unwrap();

        let ids: Vec<String> = (1..=3u32).map(oid).collect();
        let out = store
            .bulk_apply_tag(
                &Target::Ids(&ids),
                &tag.id,
                None,
                Some("bulk"),
                &CallerId::anonymous(),
            )
            .await
            .unwrap();

        assert_eq!(
            out,
            BulkOutcome {
                applied: 3,
                skipped_invisible: 0,
                requested: 3
            },
            "{}",
            store.engine()
        );
        for i in 0..3 {
            assert_eq!(
                tags_on(store, &oid(i + 1)).await,
                vec![tag.id.clone()],
                "oid({}) on {}",
                i + 1,
                store.engine()
            );
        }
    }
}

#[tokio::test]
async fn a_bulk_tag_does_not_write_an_object_the_caller_cannot_see() {
    // The test this module exists for. A bulk path with no consent clause
    // returns `applied: 3` here and writes three rows, and reports success --
    // which is the failure mode, because nothing about it looks wrong.
    let (sqlite, postgres) = both_engines().await;
    for store in [&sqlite, &postgres] {
        object_with_tier(store, &oid(1), VISIBLE_TIER).await;
        object_with_tier(store, &oid(2), HIDDEN_TIER).await;
        let tag = store.create_tag("beach", None).await.unwrap();

        let ids = vec![oid(1), oid(2)];
        let out = store
            .bulk_apply_tag(
                &Target::Ids(&ids),
                &tag.id,
                None,
                Some("bulk"),
                &CallerId::anonymous(),
            )
            .await
            .unwrap();

        assert_eq!(
            out.applied,
            1,
            "an anonymous caller may not write an unverified object ({})",
            store.engine()
        );
        assert_eq!(
            out.skipped_invisible,
            1,
            "and it is told one was skipped ({})",
            store.engine()
        );
        assert_eq!(out.requested, 2);

        assert_eq!(tags_on(store, &oid(1)).await, vec![tag.id.clone()]);
        assert_eq!(
            tags_on(store, &oid(2)).await,
            Vec::<String>::new(),
            "the hidden object must have no tag at all ({})",
            store.engine()
        );
    }
}

#[tokio::test]
async fn the_count_is_what_the_database_did_not_what_the_client_asked_for() {
    // The two differ as soon as one object is missing from the library, which
    // is what a stale selection looks like. A modal reporting the request size
    // tells the user 3 were tagged when 2 were.
    let (sqlite, postgres) = both_engines().await;
    for store in [&sqlite, &postgres] {
        object_with_tier(store, &oid(1), VISIBLE_TIER).await;
        object_with_tier(store, &oid(2), VISIBLE_TIER).await;
        let tag = store.create_tag("beach", None).await.unwrap();

        // oid(3) is never inserted: a stale selection naming an object that is
        // no longer in the library. That is the case the count exists for.
        let ids = vec![oid(1), oid(2), oid(3)];
        let out = store
            .bulk_apply_tag(
                &Target::Ids(&ids),
                &tag.id,
                None,
                Some("bulk"),
                &CallerId::anonymous(),
            )
            .await
            .unwrap();

        assert_eq!(out.applied, 2, "{}", store.engine());
        assert_eq!(out.requested, 3, "the request was 3 ({})", store.engine());
    }
}

#[tokio::test]
async fn an_empty_id_list_writes_nothing_and_does_not_become_everything() {
    // The dangerous one. An empty list compiled to "no predicate" would tag the
    // whole library, and a user who selected nothing and clicked apply would
    // have just tagged everything.
    let (sqlite, postgres) = both_engines().await;
    for store in [&sqlite, &postgres] {
        object_with_tier(store, &oid(1), VISIBLE_TIER).await;
        let tag = store.create_tag("beach", None).await.unwrap();

        let out = store
            .bulk_apply_tag(
                &Target::Ids(&[]),
                &tag.id,
                None,
                Some("bulk"),
                &CallerId::anonymous(),
            )
            .await
            .unwrap();

        assert_eq!(out.applied, 0, "{}", store.engine());
        assert!(!out.wrote_anything());
        assert_eq!(
            tags_on(store, &oid(1)).await,
            Vec::<String>::new(),
            "nothing may be written for an empty selection ({})",
            store.engine()
        );
    }
}

#[tokio::test]
async fn a_filter_target_is_consent_filtered_too() {
    // The id path is the one that will be forgotten -- it "obviously" needs no
    // filter. This is the other shape, and it has to carry the same clause.
    let (sqlite, postgres) = both_engines().await;
    for store in [&sqlite, &postgres] {
        object_with_tier(store, &oid(1), VISIBLE_TIER).await;
        object_with_tier(store, &oid(2), HIDDEN_TIER).await;
        let tag = store.create_tag("beach", None).await.unwrap();

        let filter = Filter::default();
        let out = store
            .bulk_apply_tag(
                &Target::Filter(&filter),
                &tag.id,
                None,
                Some("bulk"),
                &CallerId::anonymous(),
            )
            .await
            .unwrap();

        assert_eq!(
            out.applied,
            1,
            "a filter target must not reach past the consent clause ({})",
            store.engine()
        );
        assert_eq!(tags_on(store, &oid(2)).await, Vec::<String>::new());
    }
}

#[tokio::test]
async fn a_bulk_tag_records_its_provenance() {
    // A bulk tag that cannot say where it came from makes the tagger's
    // provenance unreadable for exactly the rows it touched, and a human tag
    // and a model tag become indistinguishable after a bulk edit.
    let (sqlite, postgres) = both_engines().await;
    for store in [&sqlite, &postgres] {
        object_with_tier(store, &oid(1), VISIBLE_TIER).await;
        let tag = store.create_tag("beach", None).await.unwrap();

        store
            .bulk_apply_tag(
                &Target::Ids(&[oid(1)]),
                &tag.id,
                Some(0.5),
                Some("bulk-edit"),
                &CallerId::anonymous(),
            )
            .await
            .unwrap();

        let sql = commons_store::db::Store::bind_sql(
            "SELECT source, confidence FROM object_tag WHERE object_id = ?",
        );
        let rows: Vec<(Option<String>, Option<f64>)> = match store {
            Store::Sqlite(p) => sqlx::query_as(&sql)
                .bind(oid(1))
                .fetch_all(p)
                .await
                .unwrap(),
            Store::Postgres(p) => sqlx::query_as(&sql)
                .bind(oid(1))
                .fetch_all(p)
                .await
                .unwrap(),
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].0.as_deref(),
            Some("bulk-edit"),
            "{}",
            store.engine()
        );
        assert_eq!(rows[0].1, Some(0.5));
    }
}

#[tokio::test]
async fn a_second_bulk_tag_overwrites_the_provenance_of_the_first() {
    // The `ON CONFLICT DO UPDATE` half of the statement.
    //
    // The provenance test above only ever *inserts*, and a fresh insert never
    // reaches the conflict branch -- so a mutant that deletes the `SET source`
    // clause entirely survives it. Mutation testing found exactly that: a
    // re-tagged object kept the provenance of the tag that was replaced, which
    // is the specific wrongness "a human tag and a model tag become
    // indistinguishable" is about. The only way to prove the update branch is to
    // take the same conflict path a second time.
    let (sqlite, postgres) = both_engines().await;
    for store in [&sqlite, &postgres] {
        object_with_tier(store, &oid(1), VISIBLE_TIER).await;
        let tag = store.create_tag("beach", None).await.unwrap();
        let ids = vec![oid(1)];

        store
            .bulk_apply_tag(
                &Target::Ids(&ids),
                &tag.id,
                Some(0.2),
                Some("model"),
                &CallerId::anonymous(),
            )
            .await
            .unwrap();
        store
            .bulk_apply_tag(
                &Target::Ids(&ids),
                &tag.id,
                Some(0.9),
                Some("bulk-edit"),
                &CallerId::anonymous(),
            )
            .await
            .unwrap();

        let sql = commons_store::db::Store::bind_sql(
            "SELECT source, confidence FROM object_tag WHERE object_id = ?",
        );
        let rows: Vec<(Option<String>, Option<f64>)> = match store {
            Store::Sqlite(p) => sqlx::query_as(&sql)
                .bind(oid(1))
                .fetch_all(p)
                .await
                .unwrap(),
            Store::Postgres(p) => sqlx::query_as(&sql)
                .bind(oid(1))
                .fetch_all(p)
                .await
                .unwrap(),
        };
        assert_eq!(rows.len(), 1, "still one row ({})", store.engine());
        assert_eq!(
            rows[0].0.as_deref(),
            Some("bulk-edit"),
            "the conflict branch must overwrite the provenance, not keep the old ({})",
            store.engine()
        );
        assert_eq!(
            rows[0].1,
            Some(0.9),
            "and the confidence ({})",
            store.engine()
        );
    }
}

#[tokio::test]
async fn a_bulk_tag_is_idempotent_and_does_not_duplicate_rows() {
    let (sqlite, postgres) = both_engines().await;
    for store in [&sqlite, &postgres] {
        object_with_tier(store, &oid(1), VISIBLE_TIER).await;
        let tag = store.create_tag("beach", None).await.unwrap();
        let ids = vec![oid(1)];

        for _ in 0..3 {
            store
                .bulk_apply_tag(
                    &Target::Ids(&ids),
                    &tag.id,
                    None,
                    Some("bulk"),
                    &CallerId::anonymous(),
                )
                .await
                .unwrap();
        }

        assert_eq!(
            tags_on(store, &oid(1)).await,
            vec![tag.id.clone()],
            "three applications, one row ({})",
            store.engine()
        );
    }
}

#[tokio::test]
async fn a_tag_that_does_not_exist_writes_nothing() {
    // Checked before the batch, not discovered half way through. A bulk write
    // that fails on row 2,999 leaves a state the user cannot describe.
    let (sqlite, postgres) = both_engines().await;
    for store in [&sqlite, &postgres] {
        object_with_tier(store, &oid(1), VISIBLE_TIER).await;

        let r = store
            .bulk_apply_tag(
                &Target::Ids(&[oid(1)]),
                "no-such-tag",
                None,
                Some("bulk"),
                &CallerId::anonymous(),
            )
            .await;
        // The *kind* of the error, not merely that there was one. A missing tag
        // is a client error about the request; a foreign key violation is the
        // database noticing the same thing afterwards. Asserting only `is_err`
        // is satisfied by both, so a mutant that removes the pre-check passes
        // it -- the caller then has to parse a constraint violation to tell a
        // bad tag from a broken database, which is the distinction the
        // `BulkError` split exists to preserve.
        match r {
            Err(BulkError::NoSuchTag(id)) => assert_eq!(id, "no-such-tag", "{}", store.engine()),
            other => panic!(
                "expected NoSuchTag, got {other:?} -- a missing tag must be refused \
                 before the batch runs ({})",
                store.engine()
            ),
        }
        assert_eq!(tags_on(store, &oid(1)).await, Vec::<String>::new());
    }
}
