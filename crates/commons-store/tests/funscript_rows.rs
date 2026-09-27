//! The funscript row, on both engines (T-P6-003, spec §5.6).
//!
//! # What is worth testing here, given the table is four columns
//!
//! The interesting properties are all about what the row does NOT hold, and
//! about the one constraint that keeps a re-scan honest:
//!
//! - **`axis_count` decodes as `i32` on both engines.** `INTEGER` is INT4 on
//!   Postgres and INT8 on SQLite, so a field typed `i64` compiles, passes the
//!   whole SQLite suite, and fails on Postgres at decode time. Every test here
//!   reads a row back, which is the only kind that can see it.
//! - **`UNIQUE (object_id, path)` is the conflict target**, so a re-scan that
//!   finds the same sidecar replaces the row rather than duplicating it. A
//!   duplicate would show the user the same script twice and make "which one
//!   does the player load" unanswerable.
//! - **`axis_count` is updated on conflict.** A script that gained an axis is a
//!   different script at the same path, and the row that says "1 axis" for a
//!   2-axis script sends a single-axis player down the wrong branch.
//! - **Ordering is total.** `axis_count DESC, path` — a list that reorders
//!   between two identical requests makes "the first script" unanswerable.
//! - **`metadata` is a projection, not a contract.** A corrupt blob reads as
//!   `None` rather than failing the read, because one bad row must not take the
//!   whole list down.
//!
//! Every fixture creates its OWN object with a UUID-derived title. The funscript
//! table is keyed on `object_id` and read per object, so a fixture that named a
//! shared object would have another test's rows decide its counts.

#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_store::create::{create, ObjectDraft};
use commons_store::db::Store;
use commons_store::filter_ast::CallerId;
use commons_store::funscript::{
    count_for_object, delete_for_object, get, list_for_object, primary_path, put,
    FunscriptMetadata, FunscriptRow,
};

macro_rules! on_each_store {
    (|$store:ident| $body:block) => {{
        for $store in [sqlite_store().await, postgres_store().await] {
            $body
        }
    }};
}

/// A seed this test alone uses, so no other fixture's rows decide its counts.
fn seed() -> String {
    format!("fs{}", uuid::Uuid::new_v4().simple())
}

fn owner() -> CallerId {
    CallerId {
        account_id: Some(uuid::Uuid::new_v4().to_string()),
        ..CallerId::anonymous()
    }
}

/// An object that belongs to this test alone.
async fn own_object(store: &Store, tag: &str) -> String {
    create(
        store,
        &ObjectDraft::new("scene").title(format!("{tag}-{}", seed())),
        &owner(),
    )
    .await
    .expect("create the object this fixture owns")
}

fn row(object_id: &str, path: &str, axis_count: i32) -> FunscriptRow {
    FunscriptRow {
        id: format!("fsr-{}", uuid::Uuid::new_v4().simple()),
        object_id: object_id.to_string(),
        path: path.to_string(),
        axis_count,
        metadata: None,
        created_at: commons_core::ts::now(),
    }
}

fn with_meta(mut r: FunscriptRow, m: &FunscriptMetadata) -> FunscriptRow {
    r.metadata = Some(serde_json::to_string(m).expect("metadata serialises"));
    r
}

// ---------------------------------------------------------------------------
// The row survives a round trip, on both engines
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_written_funscript_reads_back_unchanged() {
    on_each_store!(|store| {
        let object_id = own_object(&store, "roundtrip").await;
        let r = row(&object_id, "/library/clip.mp4.funscript", 1);
        let id = r.id.clone();
        put(&store, &r).await.unwrap();

        let back = get(&store, &id).await.unwrap().expect("the row is there");
        assert_eq!(back, r, "every field survives the round trip");
        // Spelled out because this is the field whose TYPE is the trap: a
        // column declared INTEGER must be read as i32 on both engines.
        assert_eq!(back.axis_count, 1);
    });
}

#[tokio::test]
async fn a_multi_axis_count_round_trips() {
    on_each_store!(|store| {
        // #6339. A count above 1 is what makes a single-axis player know it
        // cannot use this script.
        let object_id = own_object(&store, "multiaxis").await;
        let r = row(&object_id, "/library/clip.funscript/a.json", 7);
        let id = r.id.clone();
        put(&store, &r).await.unwrap();
        assert_eq!(get(&store, &id).await.unwrap().unwrap().axis_count, 7);
    });
}

// ---------------------------------------------------------------------------
// The constraint that makes a re-scan idempotent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_same_path_twice_is_one_row() {
    on_each_store!(|store| {
        let object_id = own_object(&store, "samepath").await;
        let first = row(&object_id, "/library/clip.mp4.funscript", 1);
        let id = first.id.clone();
        put(&store, &first).await.unwrap();

        // A different id, the same (object_id, path): a re-scan, which finds
        // the same sidecar and mints a fresh row id.
        let second = row(&object_id, "/library/clip.mp4.funscript", 1);
        assert_ne!(second.id, id, "the ids differ, which is the point");
        put(&store, &second).await.unwrap();

        assert_eq!(
            count_for_object(&store, &object_id).await.unwrap(),
            1,
            "two rows would show the user the same script twice"
        );
    });
}

#[tokio::test]
async fn a_rescan_that_adds_an_axis_updates_the_count() {
    on_each_store!(|store| {
        let object_id = own_object(&store, "gainedaxis").await;
        let path = "/library/clip.mp4.funscript";

        put(&store, &row(&object_id, path, 1)).await.unwrap();
        // The author re-exports with a second axis. Same path, different
        // script -- and a row that still said "1 axis" would send a
        // single-axis player down the wrong branch with no complaint.
        put(&store, &row(&object_id, path, 2)).await.unwrap();

        let rows = list_for_object(&store, &object_id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].axis_count, 2, "the count followed the re-export");
    });
}

#[tokio::test]
async fn two_different_paths_are_two_rows() {
    on_each_store!(|store| {
        // The sidecar AND the directory form, which `discover` checks in that
        // order and both of which are real.
        let object_id = own_object(&store, "twopaths").await;
        put(&store, &row(&object_id, "/library/clip.mp4.funscript", 1))
            .await
            .unwrap();
        put(
            &store,
            &row(&object_id, "/library/clip.funscript/a.json", 2),
        )
        .await
        .unwrap();
        assert_eq!(count_for_object(&store, &object_id).await.unwrap(), 2);
    });
}

#[tokio::test]
async fn the_same_path_on_two_objects_is_two_rows() {
    on_each_store!(|store| {
        // The constraint is (object_id, path), NOT path alone. A library with
        // two copies of the same file in different folders has two scripts at
        // the same relative name, and keying on path alone would make the
        // second one collide with the first.
        let a = own_object(&store, "sharedpath-a").await;
        let b = own_object(&store, "sharedpath-b").await;
        put(&store, &row(&a, "clip.mp4.funscript", 1))
            .await
            .unwrap();
        put(&store, &row(&b, "clip.mp4.funscript", 1))
            .await
            .unwrap();
        assert_eq!(count_for_object(&store, &a).await.unwrap(), 1);
        assert_eq!(count_for_object(&store, &b).await.unwrap(), 1);
    });
}

// ---------------------------------------------------------------------------
// Ordering, and the reason it is total
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_list_is_ordered_by_axis_count_then_path() {
    on_each_store!(|store| {
        let object_id = own_object(&store, "ordered").await;
        put(&store, &row(&object_id, "/z/one-axis.funscript", 1))
            .await
            .unwrap();
        put(&store, &row(&object_id, "/a/two-axis.funscript", 2))
            .await
            .unwrap();
        put(&store, &row(&object_id, "/m/two-axis.funscript", 2))
            .await
            .unwrap();

        let got: Vec<(i32, String)> = list_for_object(&store, &object_id)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.axis_count, r.path))
            .collect();
        assert_eq!(
            got,
            vec![
                (2, "/a/two-axis.funscript".to_string()),
                (2, "/m/two-axis.funscript".to_string()),
                (1, "/z/one-axis.funscript".to_string()),
            ],
            "multi-axis first, then by name -- a total order, so 'the first \
             script' is answerable"
        );
    });
}

#[tokio::test]
async fn the_primary_path_is_the_one_the_list_would_put_first() {
    on_each_store!(|store| {
        // Two readers of the same question must not disagree: the scanner wants
        // "the file I just found", a library view wants "the best one", and
        // both are answered by the same ORDER BY.
        let object_id = own_object(&store, "primary").await;
        put(&store, &row(&object_id, "/z/one.funscript", 1))
            .await
            .unwrap();
        put(&store, &row(&object_id, "/a/two.funscript", 2))
            .await
            .unwrap();

        let first = list_for_object(&store, &object_id).await.unwrap()[0]
            .path
            .clone();
        assert_eq!(primary_path(&store, &object_id).await.unwrap(), Some(first));
    });
}

// ---------------------------------------------------------------------------
// metadata: a projection, not a contract
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metadata_round_trips_through_the_column() {
    on_each_store!(|store| {
        let object_id = own_object(&store, "meta").await;
        let m = FunscriptMetadata {
            title: Some("A title".into()),
            author: Some("someone".into()),
            // The format's version arrives as a number from some exporters and
            // a string from others; storing it as text is why both work.
            version: Some("2".into()),
            source: Some("sidecar".into()),
        };
        let r = with_meta(row(&object_id, "/a.funscript", 1), &m);
        let id = r.id.clone();
        put(&store, &r).await.unwrap();

        let back = get(&store, &id).await.unwrap().unwrap();
        assert_eq!(back.metadata().unwrap(), m);
    });
}

#[tokio::test]
async fn a_row_with_no_metadata_reads_as_none() {
    on_each_store!(|store| {
        let object_id = own_object(&store, "nometa").await;
        let r = row(&object_id, "/a.funscript", 1);
        let id = r.id.clone();
        put(&store, &r).await.unwrap();
        let back = get(&store, &id).await.unwrap().unwrap();
        assert_eq!(back.metadata, None);
        assert_eq!(back.metadata(), None);
    });
}

/// A corrupt blob must not take the list down. One bad row reading as None is
/// recoverable; a route that 500s on it is not.
#[tokio::test]
async fn a_corrupt_metadata_blob_reads_as_none_rather_than_failing() {
    on_each_store!(|store| {
        let object_id = own_object(&store, "corrupt").await;
        let mut r = row(&object_id, "/a.funscript", 1);
        r.metadata = Some("{not json at all".into());
        let id = r.id.clone();
        put(&store, &r).await.unwrap();

        let back = get(&store, &id)
            .await
            .unwrap()
            .expect("the row still reads");
        assert_eq!(
            back.metadata(),
            None,
            "unparseable means absent, not an error"
        );
        // And the list still works, which is the actual claim.
        assert_eq!(list_for_object(&store, &object_id).await.unwrap().len(), 1);
    });
}

// ---------------------------------------------------------------------------
// Deletion, and absence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deleting_removes_every_script_for_the_object() {
    on_each_store!(|store| {
        // A re-scan that found nothing: a video whose sidecar was deleted must
        // stop offering a script, or the player opens a file that is gone and
        // reports an empty timeline -- which a user reads as "my script broke".
        let object_id = own_object(&store, "deleted").await;
        put(&store, &row(&object_id, "/a.funscript", 1))
            .await
            .unwrap();
        put(&store, &row(&object_id, "/b.funscript", 2))
            .await
            .unwrap();

        assert_eq!(delete_for_object(&store, &object_id).await.unwrap(), 2);
        assert_eq!(count_for_object(&store, &object_id).await.unwrap(), 0);
        assert!(list_for_object(&store, &object_id)
            .await
            .unwrap()
            .is_empty());
    });
}

#[tokio::test]
async fn deleting_an_object_with_no_scripts_reports_nothing_removed() {
    on_each_store!(|store| {
        // The absence case, which a delete test that only ever deletes would
        // never see: "removed 0" and "removed 2" are different facts and a
        // scanner reporting progress needs the difference.
        let object_id = own_object(&store, "nothing").await;
        assert_eq!(delete_for_object(&store, &object_id).await.unwrap(), 0);
    });
}

#[tokio::test]
async fn an_object_with_no_scripts_lists_nothing_and_counts_zero() {
    on_each_store!(|store| {
        let object_id = own_object(&store, "absent").await;
        assert!(list_for_object(&store, &object_id)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(count_for_object(&store, &object_id).await.unwrap(), 0);
        assert_eq!(primary_path(&store, &object_id).await.unwrap(), None);
    });
}

#[tokio::test]
async fn an_unknown_id_reads_as_none() {
    on_each_store!(|store| {
        // Not an error. A player asking for a script that was deleted between
        // the list and the load should show "no script", not a failure.
        assert!(get(&store, "fsr-does-not-exist").await.unwrap().is_none());
    });
}

#[tokio::test]
async fn a_list_is_scoped_to_its_object() {
    on_each_store!(|store| {
        // The consent-adjacent property: reading another object's scripts by
        // passing its id is the ONLY thing standing between a caller and a
        // list of every script in the library, so the scope is asserted here
        // rather than assumed from the WHERE clause.
        let mine = own_object(&store, "scope-mine").await;
        let theirs = own_object(&store, "scope-theirs").await;
        put(&store, &row(&theirs, "/secret.funscript", 1))
            .await
            .unwrap();

        assert!(list_for_object(&store, &mine).await.unwrap().is_empty());
        assert_eq!(list_for_object(&store, &theirs).await.unwrap().len(), 1);
    });
}
