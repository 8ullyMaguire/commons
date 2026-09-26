//! The acceptance test for T-P2-002, and the cases around it.
//!
//! The plan is explicit about what "done" means here: "the move case asserts
//! artifact rows are *not* regenerated -- that is the expensive-to-get-right
//! part." A move test that only checks the path changed would pass with an
//! implementation that also threw the thumbnails away, which is the thing the
//! ticket exists to prevent.

use commons_core::FileState;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::PathBuf;

use commons_scan::reconcile::{apply, plan, Move, Reconciled, ScanInput};
use commons_store::{
    artifact_kinds, file_path_and_state, file_rows, insert_artifact, insert_file, insert_object,
    NewFile, Store, StoredFile,
};
use tempfile::TempDir;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

/// An object plus one file row, so the `file` table's foreign key is happy.
async fn seed(store: &Store, file_id: &str, object_id: &str, path: &str, blake3: &str) {
    insert_object(store, object_id, "scene").await.unwrap();
    insert_file(
        store,
        &NewFile {
            id: file_id,
            object_id,
            path,
            size_bytes: 10,
            mtime_ns: 1,
            hash_xxh128: None,
            hash_blake3: Some(blake3),
        },
    )
    .await
    .unwrap();
}

/// One artifact row, standing in for a thumbnail. The point of the test is
/// that this row survives a move untouched, so it needs a real row.
async fn seed_artifact(store: &Store, file_id: &str, kind: &str) {
    insert_artifact(
        store,
        &format!("{file_id}-{kind}"),
        file_id,
        kind,
        &format!("/artifacts/{file_id}/{kind}.webp"),
    )
    .await
    .unwrap();
}

async fn artifacts_of(store: &Store, file_id: &str) -> Vec<String> {
    artifact_kinds(store, file_id).await.unwrap()
}

async fn file_at(store: &Store, file_id: &str) -> Option<(String, String)> {
    file_path_and_state(store, file_id).await.unwrap()
}

async fn all_files(store: &Store) -> Vec<StoredFile> {
    file_rows(store).await.unwrap()
}

// ------------------------------------------------------- the plan's case 1

/// Rename a file, rescan, and assert: same row id, same object, path
/// rewritten, and **artifacts untouched**.
#[tokio::test]
async fn a_renamed_file_keeps_its_row_and_its_artifacts() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-1", "obj-1", "before.mp4", "aaa").await;
    seed_artifact(&store, "file-1", "thumbnail").await;
    seed_artifact(&store, "file-1", "sprite").await;

    // The file is now at a new path; the old one is gone.
    let input = ScanInput {
        present: BTreeSet::from([p("after.mp4")]),
        hashes: [(p("after.mp4"), "aaa".to_string())].into_iter().collect(),
        modified: BTreeSet::new(),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };

    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(plan.report.moved, 1, "{}", plan.report.summary());
    assert_eq!(plan.report.new, 0);
    assert_eq!(
        plan.needs_artifacts,
        Vec::<String>::new(),
        "a move must NOT regenerate artifacts -- the content is the same bytes"
    );

    apply(&store, &plan).await.unwrap();

    let files = all_files(&store).await;
    assert_eq!(files.len(), 1, "a move is not a delete plus an add");
    assert_eq!(files[0].id, "file-1", "the row id survives");
    assert_eq!(files[0].object_id, "obj-1", "and so does its object");
    assert_eq!(files[0].path, "after.mp4", "only the path changed");

    assert_eq!(
        artifacts_of(&store, "file-1").await,
        vec!["sprite".to_string(), "thumbnail".to_string()],
        "artifacts are the expensive part and must be untouched"
    );
}

// ------------------------------------------------------- the plan's case 2

/// Overwrite in place with the same length, rescan, and assert the content is
/// treated as changed.
#[tokio::test]
async fn an_in_place_overwrite_of_the_same_length_is_a_change() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-1", "obj-1", "clip.mp4", "old-hash").await;
    seed_artifact(&store, "file-1", "thumbnail").await;

    // Same path, so not a move. The caller has already compared the
    // `(mtime, size)` hint and found it moved.
    let input = ScanInput {
        present: BTreeSet::from([p("clip.mp4")]),
        hashes: [(p("clip.mp4"), "new-hash".to_string())]
            .into_iter()
            .collect(),
        modified: BTreeSet::from(["file-1".to_string()]),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };

    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(plan.report.modified, 1, "{}", plan.report.summary());
    assert_eq!(plan.report.moved, 0);
    assert_eq!(
        plan.needs_artifacts,
        vec!["file-1".to_string()],
        "changed content DOES need its artifacts rebuilt"
    );

    let files = all_files(&store).await;
    assert_eq!(files[0].id, "file-1", "a modification keeps the row id too");
    assert_eq!(files[0].hash_blake3.as_deref(), Some("old-hash"));
}

// ---------------------------------------------------------- the surrounding

#[tokio::test]
async fn an_unrelated_new_file_is_new_not_a_move() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-1", "obj-1", "existing.mp4", "aaa").await;

    let input = ScanInput {
        present: BTreeSet::from([p("existing.mp4"), p("brand-new.mp4")]),
        hashes: [
            (p("existing.mp4"), "aaa".to_string()),
            (p("brand-new.mp4"), "zzz".to_string()),
        ]
        .into_iter()
        .collect(),
        modified: BTreeSet::new(),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };
    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(plan.report.unchanged, 1);
    assert_eq!(plan.report.new, 1);
    assert_eq!(
        plan.report.moved, 0,
        "nothing went missing, so nothing moved"
    );
    assert!(plan.moves.is_empty());
}

/// Two missing files with identical content, and one new path with that
/// content. Claiming either would destroy the other's identity.
#[tokio::test]
async fn two_missing_files_with_the_same_hash_are_not_merged() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-a", "obj-a", "copy-1.mp4", "same").await;
    seed(&store, "file-b", "obj-b", "copy-2.mp4", "same").await;
    seed_artifact(&store, "file-a", "thumbnail").await;
    seed_artifact(&store, "file-b", "thumbnail").await;

    // Both copies are gone; a third file with identical content appears.
    let input = ScanInput {
        present: BTreeSet::from([p("copy-3.mp4")]),
        hashes: [(p("copy-3.mp4"), "same".to_string())]
            .into_iter()
            .collect(),
        modified: BTreeSet::new(),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };
    let plan = plan(&store, &input).await.unwrap();

    assert_eq!(plan.report.moved, 0, "an ambiguous hash is not a move");
    assert_eq!(plan.report.ambiguous, 1, "and it is reported");
    assert_eq!(plan.report.new, 1, "so the new path is just new");

    apply(&store, &plan).await.unwrap();
    // Nothing was merged, so both original objects survive intact.
    assert_eq!(artifacts_of(&store, "file-a").await.len(), 1);
    assert_eq!(artifacts_of(&store, "file-b").await.len(), 1);
}

/// A file on an unmounted volume stops being seen. Its row must survive.
#[tokio::test]
async fn a_file_that_stopped_being_seen_is_marked_absent_not_deleted() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-1", "obj-1", "on-the-nas.mp4", "aaa").await;
    seed_artifact(&store, "file-1", "thumbnail").await;

    // The volume is not mounted, so the scan sees an empty library.
    let input = ScanInput {
        present: BTreeSet::new(),
        // A complete scan that saw nothing. Without this the reconciler
        // correctly refuses to call anything missing, and the test would be
        // asserting the opposite of what it says it is testing.
        complete: true,
        ..Default::default()
    };
    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(plan.report.marked_absent, 1);
    assert_eq!(plan.report.moved, 0, "nothing appeared, so nothing moved");

    apply(&store, &plan).await.unwrap();
    let (path, state) = file_at(&store, "file-1").await.unwrap();
    assert_eq!(path, "on-the-nas.mp4", "the row is not deleted");
    assert_eq!(state, FileState::Missing.as_str());
    assert_eq!(
        artifacts_of(&store, "file-1").await.len(),
        1,
        "and its artifacts are still there for when the volume comes back"
    );
}

/// A file that is both missing and the source of a claimed move is not also
/// marked absent. Getting this wrong would mark a moved file absent and then
/// rewrite its path, so it would look present at the new path and absent at
/// the old one.
#[tokio::test]
async fn a_moved_file_is_not_also_marked_absent() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-1", "obj-1", "old.mp4", "aaa").await;
    seed_artifact(&store, "file-1", "thumbnail").await;

    let input = ScanInput {
        present: BTreeSet::from([p("new.mp4")]),
        hashes: [(p("new.mp4"), "aaa".to_string())].into_iter().collect(),
        modified: BTreeSet::new(),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };
    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(plan.report.moved, 1);
    assert_eq!(
        plan.report.marked_absent, 0,
        "the file moved; it did not also vanish"
    );
    apply(&store, &plan).await.unwrap();
    let (_, state) = file_at(&store, "file-1").await.unwrap();
    assert_eq!(
        state, "present",
        "the schema default, because nothing touched it"
    );
}

/// A file that moved AND whose hint moved.
///
/// This is the case the `needs_artifacts` filter exists for, and the only
/// one: a file in `modified` and in `moves` at once. It happens routinely --
/// a file moved out of a folder that a sync tool then rewrites, or a copy
/// that preserved the mtime, or a move within the same filesystem tick. The
/// hint says "changed", the content says "identical", and the content wins:
/// regenerating a thumbnail for bytes that did not change is exactly the cost
/// this ticket exists to remove.
///
/// Without this test the filter is dead code. A move test that only moves an
/// unmodified file passes whether or not the filter is there, because nothing
/// was ever in `needs_artifacts` to remove.
#[tokio::test]
async fn a_file_that_moved_and_whose_hint_moved_is_not_re_artificacted() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-1", "obj-1", "old.mp4", "aaa").await;
    seed_artifact(&store, "file-1", "thumbnail").await;

    let input = ScanInput {
        present: BTreeSet::from([p("new.mp4")]),
        hashes: [(p("new.mp4"), "aaa".to_string())].into_iter().collect(),
        // The hint moved as well.
        modified: BTreeSet::from(["file-1".to_string()]),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };
    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(plan.report.moved, 1, "{}", plan.report.summary());
    assert_eq!(plan.report.modified, 1, "the hint did move");
    assert_eq!(
        plan.needs_artifacts,
        Vec::<String>::new(),
        "same bytes, so the existing thumbnail is still correct"
    );
    apply(&store, &plan).await.unwrap();
    assert_eq!(artifacts_of(&store, "file-1").await, vec!["thumbnail"]);
}

/// A new path whose hash matches a file that is STILL PRESENT is a copy, not
/// a move. This is the case that prevents a user's deliberate duplicate from
/// being silently merged.
#[tokio::test]
async fn a_duplicate_of_a_present_file_is_not_a_move() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-1", "obj-1", "original.mp4", "same").await;
    seed_artifact(&store, "file-1", "thumbnail").await;

    let input = ScanInput {
        present: BTreeSet::from([p("original.mp4"), p("duplicate.mp4")]),
        hashes: [
            (p("original.mp4"), "same".to_string()),
            (p("duplicate.mp4"), "same".to_string()),
        ]
        .into_iter()
        .collect(),
        modified: BTreeSet::new(),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };
    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(plan.report.moved, 0, "the original is still there");
    assert_eq!(plan.report.new, 1, "so the duplicate is a new file");
}

/// A file whose hash could not be computed is new, never a move. Claiming a
/// move without content identity is how two different files get merged.
#[tokio::test]
async fn a_path_with_no_computed_hash_is_never_a_move() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-1", "obj-1", "gone.mp4", "aaa").await;

    let input = ScanInput {
        present: BTreeSet::from([p("unreadable.mp4")]),
        hashes: Default::default(),
        modified: BTreeSet::new(),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };
    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(plan.report.moved, 0);
    assert_eq!(plan.report.new, 1, "no hash means no identity claim");
    assert_eq!(plan.report.ambiguous, 0, "and not an error either");
}

/// A second scan of an unchanged library changes nothing. The idempotence the
/// ticket asks for, asserted through the reconciler rather than the walk.
#[tokio::test]
async fn reconciling_twice_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "file-1", "obj-1", "a.mp4", "aaa").await;
    seed(&store, "file-2", "obj-2", "b.mp4", "bbb").await;

    let input = ScanInput {
        present: BTreeSet::from([p("a.mp4"), p("b.mp4")]),
        hashes: [
            (p("a.mp4"), "aaa".to_string()),
            (p("b.mp4"), "bbb".to_string()),
        ]
        .into_iter()
        .collect(),
        modified: BTreeSet::new(),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };

    let first = plan(&store, &input).await.unwrap();
    assert_eq!(first.report.unchanged, 2);
    assert!(first.moves.is_empty());
    assert!(first.mark_absent.is_empty());
    apply(&store, &first).await.unwrap();

    let second = plan(&store, &input).await.unwrap();
    assert_eq!(second, first, "the same input must produce the same plan");
    apply(&store, &second).await.unwrap();

    assert_eq!(
        all_files(&store).await.len(),
        2,
        "and no rows were duplicated"
    );
}

/// Three files move in one scan, all at once -- the case a user gets when
/// they reorganise a folder tree.
#[tokio::test]
async fn many_moves_in_one_scan() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let mut present = BTreeSet::new();
    let mut hashes = std::collections::HashMap::new();
    for i in 0..20 {
        seed(
            &store,
            &format!("f{i}"),
            &format!("o{i}"),
            &format!("old/{i}.mp4"),
            &format!("h{i}"),
        )
        .await;
        seed_artifact(&store, &format!("f{i}"), "thumbnail").await;
        present.insert(p(&format!("new/{i}.mp4")));
        hashes.insert(p(&format!("new/{i}.mp4")), format!("h{i}"));
    }

    let input = ScanInput {
        present,
        hashes,
        modified: BTreeSet::new(),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };
    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(plan.report.moved, 20, "{}", plan.report.summary());
    assert_eq!(plan.report.marked_absent, 0);
    assert!(plan.needs_artifacts.is_empty(), "no artifact work at all");

    apply(&store, &plan).await.unwrap();
    for i in 0..20 {
        let (path, state) = file_at(&store, &format!("f{i}")).await.unwrap();
        assert_eq!(path, format!("new/{i}.mp4"));
        assert_eq!(state, "present");
        assert_eq!(artifacts_of(&store, &format!("f{i}")).await.len(), 1);
    }
}

/// The `Move` value the plan hands back carries everything an audit log needs,
/// and nothing it does not.
#[tokio::test]
async fn a_move_records_the_object_it_belongs_to() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    seed(&store, "f1", "o1", "x.mp4", "h").await;

    let input = ScanInput {
        present: BTreeSet::from([p("y.mp4")]),
        hashes: [(p("y.mp4"), "h".to_string())].into_iter().collect(),
        modified: BTreeSet::new(),
        stats: HashMap::new(),
        kinds: HashMap::new(),
        complete: true,
    };
    let plan = plan(&store, &input).await.unwrap();
    assert_eq!(
        plan.moves,
        vec![Move {
            file_id: "f1".into(),
            object_id: "o1".into(),
            from: "x.mp4".into(),
            to: "y.mp4".into(),
        }]
    );
}

/// The empty library: no rows, nothing seen, and a plan that says so.
#[tokio::test]
async fn an_empty_library_reconciles_to_nothing() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let plan: Reconciled = plan(&store, &ScanInput::default()).await.unwrap();
    assert_eq!(plan, Reconciled::default());
    assert_eq!(
        plan.report.summary(),
        "0 new, 0 unchanged, 0 modified, 0 moved, 0 absent"
    );
}
